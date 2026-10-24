// Spotify's Web API (HTTP + JSON): the token, the user's playlists and Liked Songs, search.

use crate::*;

pub struct Tok {
    pub access: String,
    pub refresh: String,
    pub exp: Instant,
    pub failed_login: Option<Instant>,
}

pub struct Api {
    pub http: reqwest::Client,
    pub oauth: Arc<OAuthClient>,
    pub redirect: String,
    pub tok: tokio::sync::Mutex<Tok>,
    pub path: PathBuf,
    pub tx: mpsc::Sender<Msg>,
}

impl Api {
    pub async fn token(&self, force: bool) -> Result<String, String> {
        let mut t = self.tok.lock().await;
        if force || Instant::now() + Duration::from_secs(60) >= t.exp {
            let n = match self.oauth.refresh_token_async(&t.refresh).await {
                Ok(n) => n,
                // Spotify no longer accepts the refresh token (revoked, or rotated by another
                // instance): log in through the browser again. Holding the lock keeps it to one.
                Err(e) if e.to_string().contains("invalid_") => {
                    if t.failed_login.is_some_and(|at| at.elapsed() < Duration::from_secs(60)) {
                        return Err("Spotify login expired; restart spot to log in".into());
                    }
                    let _ = self.tx.send(Msg::Relogin(true));
                    let r = self.relogin().await;
                    let _ = self.tx.send(Msg::Relogin(false));
                    t.failed_login = r.is_err().then(Instant::now);
                    r?
                }
                Err(e) => return Err(format!("token refresh: {e}")),
            };
            if !n.refresh_token.is_empty() && n.refresh_token != t.refresh {
                t.refresh = n.refresh_token;
                save_refresh(&self.path, &t.refresh);
            }
            t.access = n.access_token;
            t.exp = n.expires_at;
        }
        Ok(t.access.clone())
    }

    pub async fn relogin(&self) -> Result<librespot_oauth::OAuthToken, String> {
        browser_login(self.oauth.clone(), &self.redirect).await
    }

    pub async fn get(&self, url: &str) -> Result<Value, String> {
        let mut force = false;
        for _ in 0..4 {
            let tok = self.token(force).await?;
            let r = self
                .http
                .get(url)
                .bearer_auth(tok)
                .send()
                .await
                .map_err(|e| format!("network: {e}"))?;
            let status = r.status().as_u16();
            match status {
                200..=299 => {
                    let body = r.text().await.map_err(|e| e.to_string())?;
                    return serde_json::from_str(&body).map_err(|e| e.to_string());
                }
                401 => force = true,
                429 => {
                    let wait = r
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok()?.parse().ok())
                        .unwrap_or(2u64);
                    if wait > 30 {
                        return Err(format!("rate limited by Spotify, retry in {wait}s"));
                    }
                    tokio::time::sleep(Duration::from_secs(wait)).await;
                }
                500..=599 => tokio::time::sleep(Duration::from_millis(500)).await,
                _ => {
                    let body = r.text().await.unwrap_or_default();
                    return Err(format!("HTTP {status}: {}", body.chars().take(120).collect::<String>()));
                }
            }
        }
        Err("request failed after retries".into())
    }
}

/// Browser login, off the runtime: librespot's callback listener blocks its thread.
pub async fn browser_login(oauth: Arc<OAuthClient>, redirect: &str) -> Result<librespot_oauth::OAuthToken, String> {
    let (otx, orx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let r = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())
            .and_then(|rt| rt.block_on(oauth.get_access_token_async()).map_err(|e| format!("login: {e}")));
        let _ = otx.send(r);
    });
    match tokio::time::timeout(Duration::from_secs(300), orx).await {
        Ok(r) => r.map_err(|_| "login: aborted".to_string())?,
        Err(_) => {
            // nobody came back from the browser; poke the listener so it frees the port
            if let Some(addrs) = reqwest::Url::parse(redirect).ok().and_then(|u| u.socket_addrs(|| None).ok()) {
                for a in addrs {
                    let _ = std::net::TcpStream::connect_timeout(&a, Duration::from_secs(1));
                }
            }
            Err("login timed out".into())
        }
    }
}

pub fn parse_track(v: &Value) -> Option<Track> {
    let uri = v["uri"].as_str()?;
    if !uri.starts_with("spotify:track:") {
        return None;
    }
    let artists: Vec<&str> =
        v["artists"].as_array().map(|a| a.iter().filter_map(|x| x["name"].as_str()).collect()).unwrap_or_default();
    let id = |v: &Value| v["id"].as_str().and_then(|s| librespot_core::SpotifyId::from_base62(s).ok()).map_or(0, |i| i.id);
    Some(Track {
        uri: uri.into(),
        name: v["name"].as_str().unwrap_or("?").into(),
        artists: artists.join(", "),
        lead: artists.first().map_or(0, |a| a.len()),
        album: v["album"]["name"].as_str().unwrap_or("").into(),
        cover: v["album"]["images"].as_array().and_then(|i| i.first()?["url"].as_str()).map(String::from),
        ms: v["duration_ms"].as_u64().unwrap_or(0),
        album_id: id(&v["album"]),
        artist_id: id(&v["artists"][0]),
        disc: v["disc_number"].as_u64().unwrap_or(0) as u32,
    })
}

pub fn liked() -> Playlist {
    Playlist { id: None, name: "♥  Liked Songs".into() } // the heart overhangs its cell in many fonts
}

pub async fn load_playlists(api: Arc<Api>, tx: mpsc::Sender<Msg>) {
    let mut out = vec![liked()];
    let mut url = Some(format!("{API}/me/playlists?limit=50"));
    while let Some(u) = url {
        match api.get(&u).await {
            Ok(v) => {
                for p in v["items"].as_array().into_iter().flatten() {
                    let Some(id) = p["id"].as_str() else { continue };
                    out.push(Playlist {
                        id: Some(id.into()),
                        name: p["name"].as_str().unwrap_or("?").into(),
                    });
                }
                url = v["next"].as_str().map(String::from);
            }
            Err(e) => {
                // keep showing the cached list
                let _ = tx.send(Msg::Error(format!("playlists: {e}")));
                return;
            }
        }
    }
    save_playlists(&out);
    let _ = tx.send(Msg::Playlists(out));
}

/// Refresh a playlist / Liked Songs. With nothing cached the tracks stream in as they load;
/// otherwise the cached copy is already showing and gets replaced only if it changed.
pub async fn load_tracks(api: Arc<Api>, sess: Sess, tx: mpsc::Sender<Msg>, view: u64, pl: Playlist, cached: Arc<Vec<Track>>) {
    let res = match &pl.id {
        Some(id) => playlist_tracks(sess, id, &cached, &tx, view).await,
        None => liked_tracks(api, &cached, &tx, view).await,
    };
    match res {
        Ok(Some(tracks)) => {
            save_tracks(&tracks_path(&pl.key()), &tracks);
            if !cached.is_empty() {
                let _ = tx.send(Msg::Replace { view, tracks: Arc::new(tracks) });
            }
        }
        Ok(None) => {}
        Err(e) => {
            let _ = tx.send(Msg::Error(e));
        }
    }
    let _ = tx.send(Msg::Tracks { view, tracks: vec![], done: true });
}

pub async fn liked_page(api: Arc<Api>, offset: usize) -> Result<(Vec<Track>, usize), String> {
    let v = api.get(&format!("{API}/me/tracks?limit=50&offset={offset}")).await?;
    let page = v["items"].as_array().into_iter().flatten().filter_map(|it| parse_track(&it["track"])).collect();
    Ok((page, v["total"].as_u64().unwrap_or(0) as usize))
}

/// The full Liked Songs list, or None if `cached` is still current.
pub async fn liked_tracks(api: Arc<Api>, cached: &[Track], tx: &mpsc::Sender<Msg>, view: u64) -> Result<Option<Vec<Track>>, String> {
    let (mut got, total) = liked_page(api.clone(), 0).await?;
    if cached.is_empty() {
        let _ = tx.send(Msg::Tracks { view, tracks: got.clone(), done: false });
    } else if let Some(j) = (cached.len() + got.len()).checked_sub(total)
        && j > 0
        && j <= cached.len()
        && got.last().is_some_and(|t| t.uri == cached[j - 1].uri)
    {
        // Likes come newest first: new ones push the cache down, unlikes pull what's after them
        // up. If the first page's last song sits where the cache has it, shifted by exactly the
        // change in count, everything after it is unchanged.
        if j == got.len() && got.iter().zip(cached).all(|(a, b)| a.uri == b.uri) {
            return Ok(None);
        }
        got.extend_from_slice(&cached[j..]);
        return Ok(Some(got));
    }
    // Each page takes about a second, so fetch the rest a few at a time, keeping their order.
    const PARALLEL: usize = 16;
    let mut offsets = (got.len().max(50)..total).step_by(50);
    let mut pending = std::collections::VecDeque::new();
    loop {
        while pending.len() < PARALLEL
            && let Some(o) = offsets.next()
        {
            pending.push_back(tokio::spawn(liked_page(api.clone(), o)));
        }
        let Some(next) = pending.pop_front() else { break };
        let (page, _) = next.await.map_err(|e| e.to_string())??;
        if cached.is_empty() {
            let _ = tx.send(Msg::Tracks { view, tracks: page.clone(), done: false });
        }
        got.extend(page);
    }
    Ok(Some(got))
}

pub fn parse_link(kind: Kind, v: &Value) -> Option<Link> {
    let names = |a: &Value| a.as_array().map(|a| a.iter().filter_map(|x| x["name"].as_str()).collect::<Vec<_>>().join(", "));
    let sub = match kind {
        Kind::Albums => {
            let year = v["release_date"].as_str().unwrap_or("").get(..4).unwrap_or("");
            let kind = v["album_type"].as_str().unwrap_or("");
            format!("{} · {year} {kind}", names(&v["artists"]).unwrap_or_default())
        }
        _ => v["genres"].as_array().map(|g| g.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default(),
    };
    let single = v["album_type"] == "single";
    Some(Link { uri: v["uri"].as_str()?.into(), name: v["name"].as_str().unwrap_or("?").into(), sub, single })
}

// Dev-mode apps get max 10 results per search call, so page through a few.
pub async fn search(api: Arc<Api>, tx: mpsc::Sender<Msg>, view: u64, q: String, kind: Kind) {
    let (ty, key) = match kind {
        Kind::Tracks => ("track", "tracks"),
        Kind::Albums => ("album", "albums"),
        Kind::Artists => ("artist", "artists"),
    };
    for offset in (0..50).step_by(10) {
        let url = reqwest::Url::parse_with_params(
            &format!("{API}/search"),
            &[("q", q.as_str()), ("type", ty), ("limit", "10"), ("offset", &offset.to_string())],
        )
        .unwrap();
        match api.get(url.as_str()).await {
            Ok(v) => {
                let items = v[key]["items"].as_array().cloned().unwrap_or_default();
                let last = items.len() < 10 || v[key]["next"].is_null();
                if kind == Kind::Tracks {
                    let tracks = items.iter().filter_map(parse_track).collect();
                    let _ = tx.send(Msg::Tracks { view, tracks, done: last });
                } else {
                    let links = items.iter().filter_map(|i| parse_link(kind, i)).collect();
                    let _ = tx.send(Msg::Links { view, links, done: last });
                }
                if last {
                    return;
                }
            }
            Err(e) => {
                let _ = tx.send(Msg::Error(format!("search: {e}")));
                break;
            }
        }
    }
    let _ = tx.send(Msg::Tracks { view, tracks: vec![], done: true });
}
