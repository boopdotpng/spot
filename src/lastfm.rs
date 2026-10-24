// Last.fm scrobbling, optional: on when ~/.config/spot/lastfm holds an API key and shared secret (one per
// line), or LASTFM_API_KEY / LASTFM_SECRET are set. The session key from the one-time browser
// login is kept in ~/.cache/spot/lastfm_session. Plays are queued in ~/.cache/spot/scrobbles
// first, so ones that couldn't be sent (offline, quit mid-request) go out later.

use crate::*;

pub const LASTFM_API: &str = "https://ws.audioscrobbler.com/2.0/";

pub struct LastFm {
    pub http: reqwest::Client,
    pub key: String,
    pub secret: String,
    pub sk: String,
    pub queue: PathBuf,
    pub file: std::sync::Mutex<()>,          // queue file read-modify-writes
    pub draining: tokio::sync::Mutex<()>,    // one drain at a time, so nothing is sent twice
    pub tx: mpsc::Sender<Msg>,
}

/// A play in progress. Only time spent actually playing counts, so seeking can't fake a listen.
pub struct Listen {
    pub track: Track,
    pub started: u64, // unix seconds, set when it first starts playing
    pub listened: Duration,
    pub since: Option<Instant>, // playing since
    pub announced: bool,        // sent as "now playing"
}

impl Listen {
    /// Last.fm's rule: tracks over 30s count once half of them (or 4 minutes) has been heard.
    pub fn counts(&self) -> bool {
        let heard = self.listened + self.since.map_or(Duration::ZERO, |s| s.elapsed());
        self.track.ms > 30_000 && heard >= Duration::from_millis(self.track.ms / 2).min(Duration::from_secs(240))
    }
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Signed POST to the Last.fm API. Errors carry Last.fm's error code (0 = network / bad reply).
pub async fn lastfm_call(
    http: &reqwest::Client,
    key: &str,
    secret: &str,
    method: &str,
    mut params: Vec<(String, String)>,
) -> Result<Value, (i64, String)> {
    use md5::Digest;
    params.push(("api_key".into(), key.into()));
    params.push(("method".into(), method.into()));
    params.sort();
    let base: String = params.iter().flat_map(|(k, v)| [k.as_str(), v.as_str()]).chain([secret]).collect();
    let sig = md5::Md5::digest(base.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    params.push(("api_sig".into(), sig));
    params.push(("format".into(), "json".into()));
    let r = http.post(LASTFM_API).form(&params).send().await.map_err(|e| (0, e.to_string()))?;
    let body = r.text().await.map_err(|e| (0, e.to_string()))?;
    let v: Value = serde_json::from_str(&body).map_err(|_| (0, format!("bad reply: {body:.200}")))?;
    match v["error"].as_i64() {
        Some(code) => Err((code, v["message"].as_str().unwrap_or("?").into())),
        None => Ok(v),
    }
}

pub fn lastfm_creds() -> Option<(String, String)> {
    if std::env::var("SPOT_LASTFM").is_ok_and(|v| v == "off") {
        return None;
    }
    if let (Ok(k), Ok(s)) = (std::env::var("LASTFM_API_KEY"), std::env::var("LASTFM_SECRET")) {
        return Some((k, s));
    }
    let f = fs::read_to_string(config_dir().join("lastfm")).ok()?;
    let mut lines = f.lines().map(str::trim).filter(|l| !l.is_empty());
    Some((lines.next()?.into(), lines.next()?.into()))
}

/// Scrobbling, if set up. Logs in through the browser the first time; a failed or skipped login
/// just means no scrobbling this run.
pub fn lastfm_setup(rt: &tokio::runtime::Runtime, http: &reqwest::Client, redirect: &str, tx: mpsc::Sender<Msg>) -> Option<Arc<LastFm>> {
    let (key, secret) = lastfm_creds()?;
    let path = cache_dir().join("lastfm_session");
    let sk = match fs::read_to_string(&path).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        Some(sk) => sk,
        None if !std::io::stdin().is_terminal() => {
            log::warn!("last.fm isn't connected yet, and there's no terminal to ask in; not scrobbling");
            return None;
        }
        None => {
            let r = lastfm_token(&key, redirect).and_then(|token| {
                let v = rt
                    .block_on(lastfm_call(http, &key, &secret, "auth.getSession", vec![("token".into(), token)]))
                    .map_err(|(_, e)| e)?;
                v["session"]["key"].as_str().map(String::from).ok_or_else(|| "no session key in reply".into())
            });
            match r {
                Ok(sk) => {
                    save_refresh(&path, &sk);
                    println!("Last.fm connected.\n");
                    sk
                }
                Err(e) => {
                    log::warn!("last.fm login: {e}");
                    println!("Last.fm login {}; not scrobbling this time (SPOT_LASTFM=off stops asking).\n",
                        if e == "skipped" { e } else { format!("failed ({e})") });
                    return None;
                }
            }
        }
    };
    Some(Arc::new(LastFm {
        http: http.clone(),
        key,
        secret,
        sk,
        queue: cache_dir().join("scrobbles"),
        file: Default::default(),
        draining: Default::default(),
        tx,
    }))
}

/// Browser approval: Last.fm redirects to <redirect host>/lastfm?token=…, caught here.
pub fn lastfm_token(key: &str, redirect: &str) -> Result<String, String> {
    use std::io::{BufRead, Write};
    let mut cb = reqwest::Url::parse(redirect).map_err(|e| e.to_string())?;
    cb.set_path("/lastfm");
    cb.set_query(None);
    let addr = cb.socket_addrs(|| None).ok().and_then(|a| a.first().copied()).ok_or("bad redirect address")?;
    let listener = std::net::TcpListener::bind(addr).map_err(|e| format!("listen on {addr}: {e}"))?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let auth = reqwest::Url::parse_with_params("https://www.last.fm/api/auth/", [("api_key", key), ("cb", cb.as_str())])
        .map_err(|e| e.to_string())?;
    println!("Log in to Last.fm in your browser (opening now), or press enter to skip…");
    if open::that(auth.as_str()).is_err() {
        println!("Open {auth}");
    }
    // Polled through crossterm (not a stdin thread) so nothing is left reading input once the TUI starts.
    let skipped = || {
        use ratatui::crossterm::event::{Event, poll, read};
        poll(Duration::from_millis(100)).unwrap_or(false)
            && matches!(read(), Ok(Event::Key(k)) if matches!(k.code, KeyCode::Enter | KeyCode::Esc))
    };
    let deadline = Instant::now() + Duration::from_secs(300);
    while Instant::now() < deadline {
        let mut c = match listener.accept() {
            Ok((c, _)) => c,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if skipped() {
                    return Err("skipped".into());
                }
                continue;
            }
            Err(e) => return Err(e.to_string()),
        };
        let _ = c.set_nonblocking(false);
        let _ = c.set_read_timeout(Some(Duration::from_secs(5)));
        // "GET /lastfm?token=… HTTP/1.1"
        let mut line = String::new();
        let _ = std::io::BufReader::new(&c).read_line(&mut line);
        let token = line
            .split_whitespace()
            .nth(1)
            .and_then(|p| cb.join(p).ok())
            .and_then(|u| u.query_pairs().find(|(k, _)| k == "token").map(|(_, v)| v.into_owned()));
        let body = if token.is_some() { "spot is connected to Last.fm. You can close this tab." } else { "Waiting for Last.fm…" };
        let _ = write!(
            c,
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        if let Some(t) = token {
            return Ok(t);
        }
    }
    Err("timed out waiting for the browser".into())
}

impl LastFm {
    pub async fn call(&self, method: &str, mut params: Vec<(String, String)>) -> Result<Value, (i64, String)> {
        params.push(("sk".into(), self.sk.clone()));
        lastfm_call(&self.http, &self.key, &self.secret, method, params).await
    }

    pub fn now_playing(self: &Arc<Self>, rt: &tokio::runtime::Runtime, t: &Track) {
        let fm = self.clone();
        let mut params = vec![("artist".into(), lead_artist(t).into()), ("track".into(), t.name.clone())];
        if !t.album.is_empty() {
            params.push(("album".into(), t.album.clone()));
        }
        params.push(("duration".into(), (t.ms / 1000).to_string()));
        rt.spawn(async move {
            if let Err((_, e)) = fm.call("track.updateNowPlaying", params).await {
                log::warn!("last.fm now playing: {e}");
            }
        });
    }

    /// Queue a finished play; drain() sends it.
    pub fn queue(&self, l: &Listen) {
        use std::io::Write;
        let t = &l.track;
        let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
        let line = format!("{}\t{}\t{}\t{}\t{}\n", l.started, clean(lead_artist(t)), clean(&t.name), clean(&t.album), t.ms / 1000);
        log::info!("scrobble {} - {}", lead_artist(t), t.name);
        let _g = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let r = fs::OpenOptions::new().create(true).append(true).open(&self.queue).and_then(|mut f| f.write_all(line.as_bytes()));
        if let Err(e) = r {
            log::warn!("last.fm queue: {e}");
        }
    }

    /// Send queued plays, 50 at a time (Last.fm's batch limit).
    pub async fn drain(&self) {
        let _d = self.draining.lock().await;
        loop {
            let text = {
                let _g = self.file.lock().unwrap_or_else(|e| e.into_inner());
                fs::read_to_string(&self.queue).unwrap_or_default()
            };
            let batch: Vec<&str> = text.lines().take(50).collect();
            if batch.is_empty() {
                return;
            }
            let mut params = vec![];
            for (i, f) in batch.iter().map(|l| l.split('\t').collect::<Vec<_>>()).filter(|f| f.len() == 5).enumerate() {
                for (k, v) in ["timestamp", "artist", "track", "album", "duration"].into_iter().zip(f) {
                    if !v.is_empty() {
                        params.push((format!("{k}[{i}]"), v.to_string()));
                    }
                }
            }
            if !params.is_empty() {
                match self.call("track.scrobble", params).await {
                    Ok(v) => {
                        let a = &v["scrobbles"]["@attr"];
                        log::info!("last.fm: {} scrobbled, {} ignored", a["accepted"], a["ignored"]);
                    }
                    Err((9, e)) => {
                        // session revoked on last.fm: log in again next start, keep the queue
                        log::warn!("last.fm session rejected: {e}");
                        let _ = fs::remove_file(cache_dir().join("lastfm_session"));
                        let _ = self.tx.send(Msg::Status("Last.fm login was revoked; restart spot to log in again".into()));
                        return;
                    }
                    // offline, Last.fm down or rate limiting: keep them for later
                    Err((code @ (0 | 11 | 16 | 29), e)) => {
                        log::warn!("last.fm scrobble failed ({code}), will retry: {e}");
                        return;
                    }
                    Err((code, e)) => log::warn!("last.fm rejected {} scrobbles ({code}): {e}", batch.len()),
                }
            }
            // drop what was sent; plays queued meanwhile were appended after it
            let _g = self.file.lock().unwrap_or_else(|e| e.into_inner());
            let now = fs::read_to_string(&self.queue).unwrap_or_default();
            let rest: String = now.lines().skip(batch.len()).flat_map(|l| [l, "\n"]).collect();
            let _ = if rest.is_empty() { fs::remove_file(&self.queue) } else { fs::write(&self.queue, rest) };
        }
    }
}

/// First artist only: Last.fm matches "A" much better than "A, B, C".
pub fn lead_artist(t: &Track) -> &str {
    t.artists.get(..t.lead).filter(|a| !a.is_empty()).unwrap_or(&t.artists)
}
