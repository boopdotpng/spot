// The command line: `spot status`, `spot play …`, `spot watch` and friends talk to the running
// spot over a Unix socket (~/.cache/spot/sock), one JSON line per request and reply. The player
// is the server: every request becomes a Msg in its main loop, so keys, media keys and the CLI
// all drive the same state, and the window shows whatever the CLI changed.

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
};

use serde_json::json;

use super::*;

/// Where a request's answer goes (the connection's thread waits on it).
pub type Reply = mpsc::Sender<Value>;

/// What a background lookup was for, carried back to the main loop with its tracks.
pub enum Then {
    Play { from: Option<String>, show: bool },
    Queue { shown: usize },
}

/// What a `play` / `queue add` names: a uri (and its name if we know it), or a search.
enum Target {
    Uri(String, Option<String>),
    Search(&'static str, String),
}

/// A `spot watch` / `spot wait` connection, sent events until it hangs up.
pub struct Watcher {
    out: UnixStream,
    before_ms: u64, // `ending` fires this long before a song ends
    warned: u64,    // play number it was last sent `ending` for
}

/// Event bookkeeping: what the watchers were last told.
#[derive(Default)]
pub struct State {
    watchers: Vec<Watcher>,
    plays: u64,
    playing: bool,
}

/// True once no player holds the lock: after a quit, the old one may still be finishing up
/// (sending a scrobble) for a moment after its socket is gone.
fn lock_free() -> bool {
    let f = fs::OpenOptions::new().create(true).truncate(false).write(true).open(cache_dir().join("lock"));
    f.is_ok_and(|f| f.try_lock().is_ok())
}

/// Wait up to `secs` for `ok`.
fn wait_for(secs: u64, ok: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !ok() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    true
}

fn sock_path() -> PathBuf {
    cache_dir().join("sock")
}

fn err(e: impl Into<String>) -> Value {
    json!({ "ok": false, "error": e.into() })
}

fn send(mut w: &UnixStream, v: &Value) -> bool {
    let mut s = v.to_string();
    s.push('\n');
    w.write_all(s.as_bytes()).is_ok()
}

// ---------- server (inside the running spot) ----------

/// Listen on the socket. Only the instance holding the lock calls this, so a socket file
/// that's already there belongs to a spot that's gone.
pub fn serve(tx: mpsc::Sender<Msg>) {
    let path = sock_path();
    let _ = fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => return log::error!("control socket {}: {e}", path.display()),
    };
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let tx = tx.clone();
            std::thread::spawn(move || handle(conn, tx));
        }
    });
}

pub fn unlink() {
    let _ = fs::remove_file(sock_path());
}

fn handle(conn: UnixStream, tx: mpsc::Sender<Msg>) {
    let mut line = String::new();
    if BufReader::new(&conn).read_line(&mut line).is_err() {
        return;
    }
    let req: Value = match serde_json::from_str(&line) {
        Ok(v) => v,
        Err(e) => {
            send(&conn, &err(format!("bad request: {e}")));
            return;
        }
    };
    log::debug!("cli: {req}");
    if req["cmd"] == "watch" {
        // a watcher that stops reading mustn't stall the player
        let _ = conn.set_write_timeout(Some(Duration::from_millis(200)));
        let before_ms = req["before_ms"].as_u64().unwrap_or(30_000);
        let _ = tx.send(Msg::Watch(Watcher { out: conn, before_ms, warned: 0 }));
        return;
    }
    let (rtx, rrx) = mpsc::channel();
    if tx.send(Msg::Ctl(req, rtx)).is_err() {
        return;
    }
    let res = rrx.recv_timeout(Duration::from_secs(90)).unwrap_or_else(|_| err("spot didn't answer"));
    send(&conn, &res);
}

fn track_json(t: &Track) -> Value {
    json!({ "uri": t.uri, "name": t.name, "artists": t.artists, "album": t.album, "ms": t.ms })
}

/// The uri a queue key stands for: Liked Songs, a playlist id, or an album / artist / track uri.
fn key_uri(key: &str) -> Option<String> {
    if key == "liked" || key.starts_with("spotify:") {
        Some(key.into())
    } else if key.contains(':') {
        None // a search
    } else {
        Some(format!("spotify:playlist:{key}"))
    }
}

/// The queue key for a uri, matching what the window uses (so the sidebar marks it playing).
fn uri_queue_key(uri: &str) -> String {
    uri.strip_prefix("spotify:playlist:").unwrap_or(uri).into()
}

fn playlist_name(p: &Playlist) -> &str {
    if p.id.is_none() { "Liked Songs" } else { &p.name }
}

/// Tracks for a uri: from the disk cache when the window has loaded it before, otherwise fetched
/// (and cached). Returns the list's name too.
async fn resolve(api: Arc<Api>, sess: Sess, uri: String, name: Option<String>) -> Result<(String, Vec<Track>), String> {
    // the loaders stream into a view as they go; nobody is showing this one
    let (dtx, _) = mpsc::channel::<Msg>();
    let fresh = |key: &str, cached: Vec<Track>, got: Option<Vec<Track>>| match got {
        Some(t) => {
            save_tracks(&tracks_path(key), &t);
            t
        }
        None => cached,
    };
    let (name, tracks) = if uri == "liked" {
        let cached = read_tracks(&tracks_path("liked"));
        let got = liked_tracks(api, &cached, &dtx, 0).await?;
        ("Liked Songs".to_string(), fresh("liked", cached, got))
    } else if let Some(id) = uri.strip_prefix("spotify:playlist:") {
        let cached = read_tracks(&tracks_path(id));
        let got = playlist_tracks(sess.clone(), id, &cached, &dtx, 0).await?;
        let name = match name {
            Some(n) => n,
            None => {
                use librespot_metadata::Metadata;
                let session = ready(sess).await?;
                let u = SpotifyUri::from_uri(&uri).map_err(|e| e.to_string())?;
                librespot_metadata::Playlist::get(&session, &u).await.map_or("playlist".into(), |p| p.name().to_string())
            }
        };
        (name, fresh(id, cached, got))
    } else if uri.starts_with("spotify:track:") {
        let session = ready(sess).await?;
        let t = batch_meta::<PbTrack>(&session, std::slice::from_ref(&uri), ExtensionKind::TRACK_V4)
            .await?
            .remove(&uri)
            .ok_or("song not found")?;
        let t = pb_track(&uri, &t);
        (format!("{} — {}", t.name, t.artists), vec![t])
    } else {
        let artist = uri.starts_with("spotify:artist:");
        let mut tracks = read_tracks(&tracks_path(&uri_key(&uri)));
        if tracks.is_empty() {
            let session = ready(sess).await?;
            let albums = if artist { artist_albums(&session, &uri).await? } else { vec![uri.clone()] };
            let uris = album_track_uris(&session, &albums).await?;
            tracks = send_tracks(&session, &uris, artist, false, &dtx, 0).await?;
            save_tracks(&tracks_path(&uri_key(&uri)), &tracks);
        }
        let name = match tracks.first() {
            _ if artist => {
                let by = tracks.iter().find(|t| id_uri("artist", t.artist_id).as_deref() == Some(&uri));
                format!("{} · all songs", by.map_or("artist", |t| &t.artists[..t.lead]))
            }
            Some(t) => format!("{} — {}", t.album, t.artists),
            None => "album".into(),
        };
        (name, tracks)
    };
    if tracks.is_empty() {
        return Err(format!("nothing playable in {uri}"));
    }
    Ok((name, tracks))
}

/// One page of Spotify search per 10 results (dev-mode apps get at most 10 per call).
async fn search(api: Arc<Api>, q: String, ty: &'static str, n: usize) -> Value {
    let mut out = vec![];
    for offset in (0..n).step_by(10) {
        let limit = (n - offset).min(10).to_string();
        let url = reqwest::Url::parse_with_params(
            &format!("{API}/search"),
            &[("q", q.as_str()), ("type", ty), ("limit", &limit), ("offset", &offset.to_string())],
        )
        .unwrap();
        let v = match api.get(url.as_str()).await {
            Ok(v) => v,
            Err(e) => return err(format!("search: {e}")),
        };
        let page = &v[format!("{ty}s")];
        let items = page["items"].as_array().cloned().unwrap_or_default();
        out.extend(items.iter().filter_map(|i| match ty {
            "track" => parse_track(i).map(|t| track_json(&t)),
            "album" | "artist" => {
                let l = parse_link(if ty == "album" { Kind::Albums } else { Kind::Artists }, i)?;
                Some(json!({ "uri": l.uri, "name": l.name, "sub": l.sub }))
            }
            _ => Some(json!({
                "uri": i["uri"].as_str()?,
                "name": i["name"].as_str().unwrap_or("?"),
                "sub": format!("by {} · {} songs", i["owner"]["display_name"].as_str().unwrap_or("?"), i["tracks"]["total"].as_u64().unwrap_or(0)),
            })),
        }));
        if items.len() < 10 || page["next"].is_null() {
            break;
        }
    }
    json!({ "ok": true, "kind": ty, "results": out })
}

impl App {
    pub fn on_ctl(&mut self, req: Value, reply: Reply) {
        let s = |k: &str| req[k].as_str().map(String::from);
        let shown = req["shown"].as_u64().unwrap_or(10) as usize;
        let none = || "nothing to play: start something with `spot play <uri>` (find one with `spot search`)".to_string();
        let res: Result<(), String> = match req["cmd"].as_str().unwrap_or("") {
            "status" => Ok(()),
            "play" => match s("target") {
                Some(t) => {
                    let show = req["show"].as_bool().unwrap_or(false);
                    return self.load(t, Then::Play { from: s("from"), show }, reply);
                }
                None if self.cur.is_none() => Err(none()),
                None => {
                    if !self.want_play {
                        self.toggle();
                    }
                    Ok(())
                }
            },
            "pause" => {
                if self.want_play {
                    self.toggle();
                }
                Ok(())
            }
            "toggle" | "next" | "prev" if self.now().is_none() && self.upnext.is_empty() => Err(none()),
            "toggle" => {
                self.toggle();
                Ok(())
            }
            "next" => {
                self.skip_soon(1);
                Ok(())
            }
            "prev" => {
                self.prev();
                Ok(())
            }
            "seek" => match self.now().map(|t| t.ms) {
                None => Err(none()),
                Some(len) => {
                    let to = match (req["to_ms"].as_i64(), req["by_ms"].as_i64()) {
                        (Some(to), _) => to,
                        (_, Some(by)) => self.position() as i64 + by,
                        _ => return void(reply.send(err("seek: to_ms or by_ms"))),
                    };
                    self.seek_to((to.max(0) as u64).min(len.saturating_sub(1000)));
                    Ok(())
                }
            },
            "vol" => {
                let pct = |v: u16| (v as i64 * 100 + 32767) / 65535;
                let to = match (req["set"].as_i64(), req["by"].as_i64()) {
                    (Some(to), _) => Some(to),
                    (_, Some(by)) => Some(pct(self.vol) + by),
                    _ => None,
                };
                if let Some(to) = to {
                    self.set_volume((to.clamp(0, 100) * 65535 / 100) as i32 - self.vol as i32);
                }
                Ok(())
            }
            "shuffle" => {
                if req["on"].as_bool() != Some(self.shuffle) {
                    self.toggle_shuffle();
                }
                Ok(())
            }
            "repeat" => {
                match s("mode").as_deref() {
                    None => self.cycle_repeat(),
                    Some("off") => self.repeat = Repeat::Off,
                    Some("all") => self.repeat = Repeat::All,
                    Some("one") => self.repeat = Repeat::One,
                    Some(m) => return void(reply.send(err(format!("repeat: off, all or one, not '{m}'")))),
                }
                Ok(())
            }
            "queue" => return void(reply.send(self.queue_json(req["n"].as_u64().unwrap_or(10) as usize))),
            "queue_rm" => {
                let mut ns: Vec<usize> = req["n"].as_array().into_iter().flatten().filter_map(Value::as_u64).map(|n| n as usize).collect();
                let len = self.upnext.len();
                if let Some(&bad) = ns.iter().find(|&&n| n == 0 || n > len) {
                    return void(reply.send(err(format!("the queue has {}, so there's no {bad}", plural(len, "song")))));
                }
                ns.sort_unstable();
                ns.dedup();
                for n in ns.into_iter().rev() {
                    self.dequeue(n - 1);
                }
                return void(reply.send(self.queue_json(shown)));
            }
            "queue_clear" => {
                self.upnext.clear();
                self.queue_changed();
                return void(reply.send(self.queue_json(shown)));
            }
            "queue_add" => match s("target") {
                Some(t) => return self.load(t, Then::Queue { shown }, reply),
                None => Err("queue add: which uri?".into()),
            },
            "search" => {
                let ty = match req["kind"].as_str().unwrap_or("track") {
                    "album" => "album",
                    "artist" => "artist",
                    "playlist" => "playlist",
                    _ => "track",
                };
                let (api, q, n) = (self.api.clone(), s("q").unwrap_or_default(), req["n"].as_u64().unwrap_or(10).clamp(1, 50));
                self.rt.spawn(async move {
                    let _ = reply.send(search(api, q, ty, n as usize).await);
                });
                return;
            }
            "playlists" => {
                let pls: Vec<Value> = self
                    .playlists
                    .iter()
                    .map(|p| json!({ "name": playlist_name(p), "uri": key_uri(&p.key()) }))
                    .collect();
                return void(reply.send(json!({ "ok": true, "playlists": pls })));
            }
            "quit" => {
                self.quit = true;
                Ok(())
            }
            c => Err(format!("unknown command '{c}' (see `spot help`)")),
        };
        let _ = reply.send(match res {
            Ok(()) => self.snapshot(),
            Err(e) => err(e),
        });
    }

    /// What `t` names: `liked`, a Spotify uri, one of their playlists, or failing those words to
    /// search for (`song` / `album` / `artist` / `playlist` first picks what to search; songs
    /// otherwise). Several matching playlists is an error that lists them, numbered.
    fn target(&self, t: &str) -> Result<Target, Value> {
        let t = t.trim();
        let low = t.to_lowercase();
        if matches!(low.as_str(), "liked" | "liked songs" | "spotify:collection") {
            return Ok(Target::Uri("liked".into(), None));
        }
        if let Some(rest) = t.strip_prefix("spotify:") {
            let kind = rest.split(':').next().unwrap_or("");
            if !matches!(kind, "track" | "album" | "artist" | "playlist") {
                return Err(err(format!("can't play a {kind} uri (songs, albums, artists and playlists work)")));
            }
            let name = self.playlists.iter().find(|p| key_uri(&p.key()).as_deref() == Some(t)).map(|p| p.name.clone());
            return Ok(Target::Uri(t.into(), name));
        }
        let name_of = |p: &&Playlist| playlist_name(p).to_lowercase();
        let exact: Vec<&Playlist> = self.playlists.iter().filter(|p| name_of(p) == low).collect();
        if let [p] = exact.as_slice() {
            return Ok(Target::Uri(key_uri(&p.key()).unwrap_or_default(), Some(p.name.clone())));
        }
        let (kind, q) = match low.split_once(' ') {
            Some(("song" | "track", q)) => (Some("track"), q),
            Some(("album", q)) => (Some("album"), q),
            Some(("artist", q)) => (Some("artist"), q),
            Some(("playlist", q)) => (Some("playlist"), q),
            _ => (None, low.as_str()),
        };
        let q = q.trim();
        if matches!(kind, None | Some("playlist")) {
            // the only one of theirs containing it
            let found: Vec<&Playlist> = self.playlists.iter().filter(|p| name_of(p).contains(q)).collect();
            match found.as_slice() {
                [] => {}
                [p] => return Ok(Target::Uri(key_uri(&p.key()).unwrap_or_default(), Some(p.name.clone()))),
                many => {
                    let mut e = err(format!(
                        "'{q}' matches {} of your playlists; `spot play N` picks one{}:",
                        many.len(),
                        if kind.is_none() { format!(", `spot play song {q}` searches songs instead") } else { String::new() }
                    ));
                    e["results"] = many
                        .iter()
                        .map(|p| json!({ "name": playlist_name(p), "uri": key_uri(&p.key()) }))
                        .collect::<Vec<_>>()
                        .into();
                    return Err(e);
                }
            }
        }
        if q.is_empty() {
            return Err(err(format!("play which {}?", kind.unwrap_or("song"))));
        }
        Ok(Target::Search(kind.unwrap_or("track"), q.into()))
    }

    /// Fetch the tracks behind `t` in the background (searching first if it's words);
    /// on_loaded acts on them.
    fn load(&mut self, t: String, then: Then, reply: Reply) {
        let target = match self.target(&t) {
            Ok(x) => x,
            Err(e) => return void(reply.send(e)),
        };
        let (api, sess, tx) = (self.api.clone(), self.sess.subscribe(), self.tx.clone());
        self.rt.spawn(async move {
            let (uri, name, found) = match target {
                Target::Uri(uri, name) => (uri, name, None),
                Target::Search(ty, q) => {
                    let what = if ty == "track" { "song" } else { ty };
                    let res = search(api.clone(), q.clone(), ty, 1).await;
                    let hit = &res["results"][0];
                    let Some(uri) = hit["uri"].as_str() else {
                        let e = if res["ok"] == true { err(format!("no {what} on Spotify matches '{q}'")) } else { res.clone() };
                        return void(reply.send(e));
                    };
                    let name = (ty == "playlist").then(|| hit["name"].as_str().unwrap_or("playlist").to_string());
                    (uri.to_string(), name, Some(format!("the top {what} for '{q}'")))
                }
            };
            let res = resolve(api, sess, uri.clone(), name).await;
            let _ = tx.send(Msg::Loaded { reply, then, uri, found, res });
        });
    }

    pub fn on_loaded(&mut self, reply: Reply, then: Then, uri: String, found: Option<String>, res: Result<(String, Vec<Track>), String>) {
        let (name, tracks) = match res {
            Ok(x) => x,
            Err(e) => return void(reply.send(err(e))),
        };
        let shown = match then {
            Then::Queue { shown } => shown,
            Then::Play { .. } => 10,
        };
        let res: Result<(), String> = match then {
            // a single song plays now, then the queue and the list carry on
            Then::Play { show, .. } if uri.starts_with("spotify:track:") => {
                if self.queue.is_none() {
                    self.play_queue(Queue { key: uri, name, tracks: Arc::new(tracks), order: vec![], pos: 0 }, Some(0));
                } else {
                    self.queued = tracks.into_iter().next();
                    self.retried = false;
                    self.load_at = None;
                    self.play_current();
                }
                if show {
                    self.show_queue();
                }
                Ok(())
            }
            Then::Play { from, show } => {
                let start = match from {
                    Some(f) => match tracks.iter().position(|t| t.uri == f) {
                        Some(i) => Some(i),
                        None => return void(reply.send(err(format!("{f} isn't in {name}")))),
                    },
                    None => None,
                };
                let q = Queue { key: uri_queue_key(&uri), name, tracks: Arc::new(tracks), order: vec![], pos: 0 };
                self.play_queue(q, start);
                if show {
                    self.show_queue();
                }
                Ok(())
            }
            Then::Queue { .. } => {
                self.enqueue(tracks);
                Ok(())
            }
        };
        let _ = reply.send(match res {
            Ok(()) => {
                let mut v = self.queue_json(shown);
                v["found"] = found.into();
                v
            }
            Err(e) => err(e),
        });
    }

    fn snapshot(&self) -> Value {
        let cur = self.now();
        let repeat = match self.repeat {
            Repeat::Off => "off",
            Repeat::All => "all",
            Repeat::One => "one",
        };
        json!({
            "ok": true,
            "track": cur.map(track_json),
            "position_ms": cur.map(|t| self.position().min(t.ms)),
            "playing": self.want_play && cur.is_some(),
            "from_queue": self.queued.is_some(),
            "queued": self.upnext.len(),
            "list": self.queue.as_ref().map(|q| {
                let name = if q.key == "liked" { "Liked Songs" } else { &q.name }; // not the sidebar's heart
                json!({ "name": name, "uri": key_uri(&q.key), "pos": q.pos + 1, "len": q.order.len() })
            }),
            "next": self.up_next().map(track_json),
            "shuffle": self.shuffle,
            "repeat": repeat,
            "volume": (self.vol as u32 * 100 + 32767) / 65535,
            "connected": self.session().is_some_and(|s| !s.is_invalid()),
            "notice": (!self.status.is_empty()).then_some(&self.status), // e.g. a login to approve
        })
    }

    /// Status, the queue, and the list's next `n` songs after it.
    fn queue_json(&self, n: usize) -> Value {
        let mut v = self.snapshot();
        v["queue"] = self.upnext.iter().map(track_json).collect::<Vec<_>>().into();
        let upcoming: Vec<Value> = self
            .queue
            .iter()
            .flat_map(|q| q.order.iter().skip(q.pos + 1).take(n).map(|&i| track_json(&q.tracks[i])))
            .collect();
        v["upcoming"] = upcoming.into();
        v
    }

    pub fn watch(&mut self, w: Watcher) {
        let mut hello = self.snapshot();
        hello["event"] = "status".into();
        // already inside its warning window, `ending` fires on the next tick
        if send(&w.out, &hello) {
            self.ctl.watchers.push(w);
        }
    }

    /// Tell watchers what changed since the last tick: a new song, play / pause, the queue
    /// running out, or the song ending within their `--before`.
    pub fn tick_watchers(&mut self) {
        let new_track = std::mem::replace(&mut self.ctl.plays, self.plays) != self.plays;
        let toggled = std::mem::replace(&mut self.ctl.playing, self.want_play) != self.want_play;
        if self.ctl.watchers.is_empty() {
            return;
        }
        let cur = self.now();
        let mut events = vec![];
        let with_track = |ev: &str| {
            json!({ "event": ev, "track": cur.map(track_json), "next": self.up_next().map(track_json) })
        };
        if new_track {
            events.push(with_track("track"));
        }
        if toggled {
            let ev = if self.want_play {
                "playing"
            } else if self.finished {
                "end"
            } else {
                "paused"
            };
            events.push(with_track(ev));
        }
        let left = cur.filter(|t| t.ms > 0 && self.want_play).map(|t| t.ms.saturating_sub(self.position()));
        let ending = left.map(|left| {
            let mut v = with_track("ending");
            v["in_ms"] = left.into();
            (left, v)
        });
        let plays = self.plays;
        self.ctl.watchers.retain_mut(|w| {
            let mut ok = events.iter().all(|e| send(&w.out, e));
            if let Some((left, ev)) = &ending
                && *left <= w.before_ms
                && w.warned != plays
            {
                w.warned = plays;
                ok &= send(&w.out, ev);
            }
            ok
        });
    }
}

fn void<T>(_: T) {}

// ---------- client (`spot <command>`) ----------

pub const COMMANDS: &[&str] = &[
    "start", "quit", "status", "play", "resume", "pause", "toggle", "next", "prev", "seek", "vol", "volume", "shuffle", "repeat", "queue",
    "search", "playlists", "watch", "wait",
];

pub const USAGE: &str = "\
spot: tiny Spotify player, and a command line for the one that's running

  spot                       start the player in this terminal (one at a time; it holds the audio)
  spot start                 start it in the background, without a window, and return once
                             it's ready (what scripts and agents should use)
  spot --headless            the same in the foreground, for a launchd / systemd service
  spot quit                  stop the player, either kind (it resumes where it was next start)
  spot clean                 clear cached songs and lists (keeps login)

Control the running player, from any terminal, script or agent:
  spot status                what's playing, position, queue, modes, volume, what's next
  spot search <words>        search songs (the default), or with --albums, --artists or
                             --playlists one of those instead; -n 20 for more than 10.
                             Spotify's field filters narrow it down:
                             spot search 'track:one more time artist:daft punk'
                             spot search 'artist:radiohead year:1997' --albums
  spot play <uri>            a song plays now, then the queue and the list carry on; an album,
                             artist (all songs) or playlist replaces the list
                             (--from <song uri> starts there, --show opens it in the window)
  spot play 3                the 3rd result of the last search (queue add takes these too)
  spot play liked            Liked Songs
  spot play <words>          one of your playlists by name, else the top song on Spotify
  spot play album <words>    the top album (or artist, song, playlist: yours first, then Spotify's)
  spot play | pause | toggle
  spot next | prev
  spot seek 1:30 | +10 | -10
  spot vol [60 | +10 | -10]  volume in percent
  spot shuffle [on | off]    no argument toggles
  spot repeat [off | all | one]   no argument cycles
  spot queue [-n 20]         the queue, then what the list plays after it
  spot queue [add] <uri>...  add to the end of the queue (`spot queue 2` = result 2) (songs play before the list carries on;
                             an album or playlist queues all its songs); words work as in play
  spot queue rm 2 3          remove songs from the queue, by their number in `spot queue`
  spot queue clear           empty the queue
  spot playlists             your playlists, with uris
  spot watch [--before 30]   a line per event as it happens: track (new song), playing,
                             paused, end (nothing left to play), ending (the song ends in 30s; says
                             what's next, so there's time to `queue add` something else)
  spot wait <event> [--before 30]   wait for the next such event, print it, exit

Finding music: `spot play <words>` plays the top hit and says so; to choose, `spot search` and
`spot play N`. Several of your playlists matching lists them, numbered for `spot play N`.
Spotify links (https://open.spotify.com/...) work anywhere a uri does.
Exit codes: 0 ok, 1 error, 2 bad usage, 3 spot isn't running.";

fn results_path() -> PathBuf {
    cache_dir().join("last_search")
}

/// Keep a search's uris, one per line, so `spot play 3` can pick one.
fn save_results(res: &Value) {
    let uris: Vec<&str> = res["results"].as_array().into_iter().flatten().filter_map(|r| r["uri"].as_str()).collect();
    write_atomic(&results_path(), &(uris.join("\n") + "\n"));
}

/// A search result number, an open.spotify.com link, or anything else as is.
fn uri_arg(a: &str) -> String {
    if let Ok(n) = a.parse::<usize>()
        && let Ok(saved) = fs::read_to_string(results_path())
    {
        let uris: Vec<&str> = saved.lines().collect();
        return match n.checked_sub(1).and_then(|i| uris.get(i)) {
            Some(u) => u.to_string(),
            None => usage(&format!("the last search had {}, so there's no {n}", plural(uris.len(), "result"))),
        };
    }
    let Some(rest) = a.strip_prefix("https://open.spotify.com/").or_else(|| a.strip_prefix("open.spotify.com/")) else {
        return a.into();
    };
    let mut parts = rest.split(['/', '?']).filter(|p| !p.starts_with("intl-"));
    match (parts.next(), parts.next()) {
        (Some(kind), Some(id)) => format!("spotify:{kind}:{id}"),
        _ => a.into(),
    }
}

fn usage(e: &str) -> ! {
    eprintln!("spot: {e} (see `spot help`)");
    std::process::exit(2);
}

/// "1:30" or "90" (seconds) to ms.
fn parse_time(s: &str) -> Option<i64> {
    let secs = match s.split_once(':') {
        Some((m, s)) => m.parse::<f64>().ok()? * 60.0 + s.parse::<f64>().ok()?,
        None => s.parse::<f64>().ok()?,
    };
    Some((secs * 1000.0) as i64)
}

pub fn client(args: Vec<String>) -> i32 {
    let mut args = args;
    let cmd = args.remove(0);
    let flag = |args: &mut Vec<String>, f: &str| match args.iter().position(|a| a == f) {
        Some(i) => {
            args.remove(i);
            true
        }
        None => false,
    };
    let opt = |args: &mut Vec<String>, f: &str| -> Option<String> {
        let i = args.iter().position(|a| a == f)?;
        args.remove(i);
        if i >= args.len() {
            usage(&format!("{f} needs a value"));
        }
        Some(args.remove(i))
    };
    let num = |args: &mut Vec<String>, f: &str, default: u64| match opt(args, f) {
        Some(n) => n.parse::<u64>().unwrap_or_else(|_| usage(&format!("{f} takes a number"))),
        None => default,
    };
    let rest = |args: &[String]| (!args.is_empty()).then(|| args.join(" "));
    // every queue command shows the queue after, so they all take -n
    // `spot queue 1 3` is `spot queue add 1 3`
    if cmd == "queue" && args.first().is_some_and(|a| !a.starts_with('-') && !matches!(a.as_str(), "add" | "rm" | "clear")) {
        args.insert(0, "add".into());
    }
    // (after a change, just a peek at the list)
    let edit = args.first().is_some_and(|a| matches!(a.as_str(), "add" | "rm" | "clear"));
    let shown = if cmd == "queue" { num(&mut args, "-n", if edit { 3 } else { 10 }) } else { 10 };

    let req = match cmd.as_str() {
        "start" => return start(),
        "quit" => return quit(),
        "status" | "pause" | "toggle" | "next" | "prev" | "playlists" => json!({ "cmd": cmd }),
        "resume" => json!({ "cmd": "play" }),
        "play" => {
            let show = flag(&mut args, "--show");
            let from = opt(&mut args, "--from").map(|f| uri_arg(&f));
            let target = rest(&args).map(|t| uri_arg(&t));
            json!({ "cmd": "play", "target": target, "from": from, "show": show })
        }
        "seek" => match args.first().map(String::as_str) {
            Some(a) if a.starts_with(['+', '-']) => match parse_time(&a[1..]) {
                Some(ms) => json!({ "cmd": "seek", "by_ms": if a.starts_with('-') { -ms } else { ms } }),
                None => usage("seek: 1:30, 90, +10 or -10"),
            },
            Some(a) => match parse_time(a) {
                Some(ms) => json!({ "cmd": "seek", "to_ms": ms }),
                None => usage("seek: 1:30, 90, +10 or -10"),
            },
            None => usage("seek: where to? 1:30, 90, +10 or -10"),
        },
        "vol" | "volume" => match args.first().map(String::as_str) {
            None => json!({ "cmd": "vol" }),
            Some(a) => match a.parse::<i64>() {
                Ok(n) if a.starts_with(['+', '-']) => json!({ "cmd": "vol", "by": n }),
                Ok(n) => json!({ "cmd": "vol", "set": n }),
                Err(_) => usage("vol: 0-100, +10 or -10"),
            },
        },
        "shuffle" => match args.first().map(String::as_str) {
            None => json!({ "cmd": "shuffle" }),
            Some("on") => json!({ "cmd": "shuffle", "on": true }),
            Some("off") => json!({ "cmd": "shuffle", "on": false }),
            Some(_) => usage("shuffle: on or off"),
        },
        "repeat" => json!({ "cmd": "repeat", "mode": args.first() }),
        "queue" if args.first().is_some_and(|a| a == "add") => {
            args.remove(0);
            if args.is_empty() {
                usage("queue add: what?");
            }
            // several numbers or uris queue each; anything else is one thing to find
            let one = |a: &String| a.parse::<usize>().is_ok() || a.starts_with("spotify:") || a.contains("open.spotify.com/");
            if !args.iter().all(one) {
                args = vec![args.join(" ")];
            }
            let mut last = None;
            for a in &args {
                let res = match request(&json!({ "cmd": "queue_add", "target": uri_arg(a), "shown": shown })) {
                    Ok(r) => r,
                    Err(code) => return code,
                };
                if res["ok"] != true {
                    return finish(&cmd, &res);
                }
                last = Some(res);
            }
            return finish(&cmd, &last.unwrap());
        }
        "queue" if args.first().is_some_and(|a| a == "rm") => {
            let ns: Vec<u64> = args[1..].iter().map(|a| a.parse().unwrap_or_else(|_| usage("queue rm: song numbers from `spot queue`"))).collect();
            if ns.is_empty() {
                usage("queue rm: which? (numbers from `spot queue`)");
            }
            json!({ "cmd": "queue_rm", "n": ns, "shown": shown })
        }
        "queue" if args.first().is_some_and(|a| a == "clear") => json!({ "cmd": "queue_clear", "shown": shown }),
        "queue" => json!({ "cmd": "queue", "n": shown }),
        "search" => {
            let kinds = [("--tracks", "track"), ("--songs", "track"), ("--albums", "album"), ("--artists", "artist"), ("--playlists", "playlist")];
            let kind = kinds.iter().filter(|(f, _)| flag(&mut args, f)).map(|(_, k)| *k).last().unwrap_or("track");
            let n = num(&mut args, "-n", 10);
            let Some(q) = rest(&args) else { usage("search: for what?") };
            json!({ "cmd": "search", "q": q, "kind": kind, "n": n })
        }
        "watch" | "wait" => {
            let before = num(&mut args, "--before", 30);
            let until = match cmd.as_str() {
                "wait" => match args.first().map(String::as_str) {
                    Some(e @ ("track" | "ending" | "playing" | "paused" | "end")) => Some(e.to_string()),
                    _ => usage("wait: for track, ending, playing, paused or end"),
                },
                _ => None,
            };
            return watch(before * 1000, until);
        }
        _ => usage(&format!("unknown command '{cmd}'")),
    };
    match request(&req) {
        Ok(res) => {
            if res["results"].is_array() {
                save_results(&res);
            }
            finish(&cmd, &res)
        }
        Err(code) => code,
    }
}

/// `spot start`: run `spot --headless` detached from this terminal (its own process group, no
/// stdio, so whatever started it can exit or close), and wait until it answers.
fn start() -> i32 {
    if UnixStream::connect(sock_path()).is_ok() {
        println!("spot is already running");
        return 0;
    }
    let id = std::env::var_os("SPOT_CLIENT_ID").is_some() || config_dir().join("client_id").exists();
    if !id || !cache_dir().join("refresh_token").exists() || !cache_dir().join("credentials.json").exists() {
        eprintln!("spot: {NOT_SET_UP}");
        return 1;
    }
    if !wait_for(5, lock_free) {
        eprintln!("spot: another spot is still running (or shutting down); try again in a moment");
        return 1;
    }
    use std::{os::unix::process::CommandExt, process::Stdio};
    let exe = std::env::current_exe().unwrap_or_else(|_| "spot".into());
    let mut child = match std::process::Command::new(exe)
        .arg("--headless")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("spot: couldn't start: {e}");
            return 1;
        }
    };
    let log = cache_dir().join("log");
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        if let Ok(Some(st)) = child.try_wait() {
            // another spot took the lock first, or startup failed
            let why = fs::read_to_string(&log).ok().and_then(|l| l.lines().rev().find(|l| l.contains("exiting:")).map(String::from));
            eprintln!("spot: the player exited ({st}): {}", why.as_deref().unwrap_or("see the log"));
            eprintln!("log: {}", log.display());
            return 1;
        }
        if UnixStream::connect(sock_path()).is_ok() {
            println!("spot is running in the background (pid {}); `spot quit` stops it", child.id());
            return 0;
        }
    }
    eprintln!("spot: started (pid {}) but isn't answering yet; see {}", child.id(), log.display());
    1
}

/// `spot quit`: ask the player to stop, then wait until it has, so a `spot start` right after
/// doesn't find the old one.
fn quit() -> i32 {
    let conn = match connect() {
        Ok(c) => c,
        Err(code) => return code,
    };
    send(&conn, &json!({ "cmd": "quit" }));
    let _ = BufReader::new(&conn).read_line(&mut String::new());
    // gone once it lets go of the lock (it may send a last scrobble first, up to 3s)
    if wait_for(8, lock_free) {
        println!("spot quit");
        0
    } else {
        eprintln!("spot: asked it to quit, but it's still running");
        1
    }
}

fn connect() -> Result<UnixStream, i32> {
    UnixStream::connect(sock_path()).map_err(|e| {
        use std::io::ErrorKind::*;
        if matches!(e.kind(), NotFound | ConnectionRefused) {
            eprintln!("spot isn't running: `spot start` starts it in the background (or run `spot` in a terminal)");
            3
        } else {
            eprintln!("spot: {}: {e}", sock_path().display());
            1
        }
    })
}

fn request(req: &Value) -> Result<Value, i32> {
    let conn = connect()?;
    let _ = conn.set_read_timeout(Some(Duration::from_secs(100)));
    if !send(&conn, req) {
        eprintln!("spot: couldn't send the request");
        return Err(1);
    }
    let mut line = String::new();
    match BufReader::new(&conn).read_line(&mut line) {
        Ok(n) if n > 0 => serde_json::from_str(&line).map_err(|e| {
            eprintln!("spot: bad reply: {e}");
            1
        }),
        _ => {
            eprintln!("spot: no reply (did it quit?)");
            Err(1)
        }
    }
}

fn watch(before_ms: u64, until: Option<String>) -> i32 {
    let conn = match connect() {
        Ok(c) => c,
        Err(code) => return code,
    };
    if !send(&conn, &json!({ "cmd": "watch", "before_ms": before_ms })) {
        return 1;
    }
    for line in BufReader::new(&conn).lines() {
        let Ok(line) = line else { break };
        let Ok(ev) = serde_json::from_str::<Value>(&line) else { continue };
        let name = ev["event"].as_str().unwrap_or("");
        if until.as_deref().is_some_and(|u| u != name) {
            continue;
        }
        println!("{}", event_line(&ev));
        if until.is_some() {
            return 0;
        }
    }
    println!("spot quit");
    3
}

// ---------- text output ----------

fn song(t: &Value) -> String {
    format!("{} — {}", t["name"].as_str().unwrap_or("?"), t["artists"].as_str().unwrap_or("?"))
}

fn ms(v: &Value) -> String {
    mmss(v.as_u64().unwrap_or(0))
}

fn next_part(ev: &Value) -> String {
    match &ev["next"] {
        Value::Null => "next: nothing (the queue ends)".into(),
        n => format!("next: {}  {}", song(n), n["uri"].as_str().unwrap_or("")),
    }
}

fn event_line(ev: &Value) -> String {
    let t = &ev["track"];
    let now = || if t.is_null() { "nothing".to_string() } else { format!("{}  {}", song(t), t["uri"].as_str().unwrap_or("")) };
    match ev["event"].as_str().unwrap_or("") {
        "status" => status_text(ev).lines().next().unwrap_or("").to_string(),
        "track" => format!("track: {} ({})  ·  {}", now(), ms(&t["ms"]), next_part(ev)),
        "ending" => format!("ending in {}s: {}  ·  {}", ev["in_ms"].as_u64().unwrap_or(0).div_ceil(1000), song(t), next_part(ev)),
        "playing" => format!("playing: {}", now()),
        "paused" => format!("paused: {}", now()),
        "end" => "end: the queue ran out".into(),
        e => format!("{e}: {ev}"),
    }
}

fn status_text(v: &Value) -> String {
    let mut out = vec![];
    let t = &v["track"];
    if t.is_null() {
        out.push("nothing playing".to_string());
    } else {
        let mark = if v["playing"] == true { "▶" } else { "‖" };
        out.push(format!("{mark} {}  {} / {}", song(t), ms(&v["position_ms"]), ms(&t["ms"])));
        out.push(format!("  album: {}  ·  {}", t["album"].as_str().unwrap_or(""), t["uri"].as_str().unwrap_or("")));
    }
    let mut modes = vec![];
    let list = v["list"].as_object().map(|q| {
        let uri = q["uri"].as_str().filter(|u| u.starts_with("spotify:")).map(|u| format!(" {u}")).unwrap_or_default();
        format!("{}{uri} ({} of {})", q["name"].as_str().unwrap_or("?"), q["pos"], q["len"])
    });
    match (v["from_queue"] == true, list) {
        (true, Some(l)) => modes.push(format!("from the queue, then {l}")),
        (true, None) => modes.push("from the queue".into()),
        (false, Some(l)) if !t.is_null() => modes.push(format!("from {l}")),
        _ => {}
    }
    match v["queued"].as_u64() {
        Some(0) | None => {}
        Some(n) => modes.push(format!("{n} queued")),
    }
    if v["shuffle"] == true {
        modes.push("shuffle".into());
    }
    match v["repeat"].as_str() {
        Some("all") => modes.push("repeat".into()),
        Some("one") => modes.push("repeat one".into()),
        _ => {}
    }
    modes.push(format!("vol {}%", v["volume"]));
    if v["connected"] == false {
        modes.push("not connected yet".into());
    }
    out.push(format!("  {}", modes.join("  ·  ")));
    if let Some(n) = v["notice"].as_str() {
        out.push(format!("  ! {n}"));
    }
    if !t.is_null() {
        out.push(format!("  {}", next_part(v)));
    }
    out.join("\n")
}

fn finish(cmd: &str, res: &Value) -> i32 {
    let list = |items: &[Value], row: &dyn Fn(&Value) -> String| {
        for (i, it) in items.iter().enumerate() {
            println!("{:>3}  {}", i + 1, row(it));
        }
    };
    let none = Vec::new();
    let named = |p: &Value| format!("{}  {}", p["name"].as_str().unwrap_or("?"), p["uri"].as_str().unwrap_or(""));
    if res["ok"] != true {
        eprintln!("spot: {}", res["error"].as_str().unwrap_or("failed"));
        // the playlists a name matched, numbered for `spot play N`
        list(res["results"].as_array().unwrap_or(&none), &named);
        return 1;
    }
    if let Some(f) = res["found"].as_str() {
        println!("{f}:");
    }
    match cmd {
        "search" => {
            let items = res["results"].as_array().unwrap_or(&none);
            if items.is_empty() {
                println!("no results");
            }
            list(items, &|r| match res["kind"].as_str() {
                Some("track") => format!("{} · {}  {}  {}", song(r), r["album"].as_str().unwrap_or(""), ms(&r["ms"]), r["uri"].as_str().unwrap_or("")),
                _ => format!("{} · {}  {}", r["name"].as_str().unwrap_or("?"), r["sub"].as_str().unwrap_or(""), r["uri"].as_str().unwrap_or("")),
            });
        }
        "playlists" => list(res["playlists"].as_array().unwrap_or(&none), &named),
        "queue" => {
            let row = |t: &Value| format!("{} ({})  {}", song(t), ms(&t["ms"]), t["uri"].as_str().unwrap_or(""));
            println!("{}", status_text(res).lines().next().unwrap_or(""));
            let queued = res["queue"].as_array().unwrap_or(&none);
            if !queued.is_empty() {
                println!("queue (`spot queue rm N` removes):");
                list(queued, &row);
            }
            let up = res["upcoming"].as_array().unwrap_or(&none);
            match res["list"]["name"].as_str() {
                Some(name) if !up.is_empty() => {
                    println!("then {name}:");
                    for t in up {
                        println!("     {}", row(t));
                    }
                }
                _ if queued.is_empty() => println!("nothing after this"),
                _ => {}
            }
        }
        _ => println!("{}", status_text(res)),
    }
    0
}
