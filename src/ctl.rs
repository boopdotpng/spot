// The player's end of the command line. `spot status`, `spot play …` and the rest (cli.rs) send
// their arguments over a Unix socket (~/.cache/spot/sock) as one tab-separated line; the reply is
// an exit code on the first line, then the text to print. Every request becomes a Msg in the main
// loop, so keys, media keys and the CLI all drive the same state, and the window shows whatever
// the CLI changed.

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
};

use crate::*;

/// Where a request's answer goes: exit code and text. The connection's thread waits on it.
pub type Reply = mpsc::Sender<(i32, String)>;

/// A failed request: exit code and message.
type Fail = (i32, String);

fn fail(e: impl Into<String>) -> Fail {
    (1, e.into())
}

fn usage(e: &str) -> Fail {
    (2, format!("{e} (see `spot help`)"))
}

/// What a background lookup was for, carried back to the main loop with its songs.
pub enum Then {
    Play { from: Option<String>, show: bool },
    Queue { shown: usize },
}

/// A list a request named, fetched: its uri, name and songs, and what was searched to find it.
pub struct List {
    uri: String,
    name: String,
    tracks: Vec<Track>,
    found: Option<String>,
}

/// What a `play` / `queue add` names: a uri (and its name if we know it), or a search.
enum Target {
    Uri(String, Option<String>),
    Search(&'static str, String),
}

/// A `spot watch` / `spot wait` connection, sent event lines until it hangs up.
pub struct Watcher {
    out: UnixStream,
    before_ms: u64,        // `ending` fires this long before a song ends
    until: Option<String>, // `wait`: the event it's waiting for, then it's done
    warned: u64,           // play number it was last sent `ending` for
}

/// Event bookkeeping: what the watchers were last told.
#[derive(Default)]
pub struct State {
    watchers: Vec<Watcher>,
    plays: u64,
    playing: bool,
}

/// The last search's (or `spot playlists`') uris, one per line, so `spot play 3` can pick one.
fn results_path() -> PathBuf {
    cache_dir().join("last_search")
}

fn save_results(uris: &[String]) {
    write_atomic(&results_path(), &(uris.join("\n") + "\n"));
}

/// Rows numbered from 1, for `spot play N`.
fn numbered(rows: &[String]) -> String {
    rows.iter().enumerate().map(|(i, r)| format!("{:>3}  {r}", i + 1)).collect::<Vec<_>>().join("\n")
}

fn song(t: &Track) -> String {
    format!("{} — {}", t.name, t.artists)
}

/// A number from the last search, an open.spotify.com link as a uri, or anything else as is.
fn uri_arg(a: &str) -> Result<String, Fail> {
    if let Ok(n) = a.parse::<usize>()
        && let Ok(saved) = fs::read_to_string(results_path())
    {
        let uris: Vec<&str> = saved.lines().collect();
        return match n.checked_sub(1).and_then(|i| uris.get(i)) {
            Some(u) => Ok(u.to_string()),
            None => Err(usage(&format!("the last search had {}, so there's no {n}", plural(uris.len(), "result")))),
        };
    }
    let Some(rest) = a.strip_prefix("https://open.spotify.com/").or_else(|| a.strip_prefix("open.spotify.com/")) else {
        return Ok(a.into());
    };
    let mut parts = rest.split(['/', '?']).filter(|p| !p.starts_with("intl-"));
    Ok(match (parts.next(), parts.next()) {
        (Some(kind), Some(id)) => format!("spotify:{kind}:{id}"),
        _ => a.into(),
    })
}

/// A search result's image closest to 300px wide (sharp at thumbnail size on a retina screen), or "".
fn cover(v: &Value) -> String {
    let images = v["images"].as_array().map_or(&[][..], Vec::as_slice);
    let near = images.iter().min_by_key(|i| i["width"].as_u64().unwrap_or(0).abs_diff(300));
    near.and_then(|i| i["url"].as_str()).unwrap_or("").into()
}

/// Fields joined by tabs, with any tabs or newlines inside them made spaces.
fn tsv_row(fields: &[&str]) -> String {
    fields.iter().map(|f| f.replace(['\t', '\n'], " ")).collect::<Vec<_>>().join("\t")
}

/// Take `--flag` out of the arguments.
fn flag(args: &mut Vec<String>, f: &str) -> bool {
    let i = args.iter().position(|a| a == f);
    i.map(|i| args.remove(i)).is_some()
}

/// Take `--opt value` out of the arguments.
fn opt(args: &mut Vec<String>, f: &str) -> Result<Option<String>, Fail> {
    let Some(i) = args.iter().position(|a| a == f) else { return Ok(None) };
    args.remove(i);
    if i >= args.len() {
        return Err(usage(&format!("{f} needs a value")));
    }
    Ok(Some(args.remove(i)))
}

fn num_opt(args: &mut Vec<String>, f: &str) -> Result<Option<u64>, Fail> {
    opt(args, f)?.map(|n| n.parse().map_err(|_| usage(&format!("{f} takes a number")))).transpose()
}

/// "1:30" or "90" (seconds) to ms.
fn parse_time(s: &str) -> Option<i64> {
    let secs = match s.split_once(':') {
        Some((m, s)) => m.parse::<f64>().ok()? * 60.0 + s.parse::<f64>().ok()?,
        None => s.parse::<f64>().ok()?,
    };
    Some((secs * 1000.0) as i64)
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

fn playlist_name(p: &Playlist) -> &str {
    if p.id.is_none() { "Liked Songs" } else { &p.name } // not the sidebar's heart
}

fn list_name(q: &Queue) -> &str {
    if q.key == "liked" { "Liked Songs" } else { &q.name }
}

// ---------- the socket ----------

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

fn handle(mut conn: UnixStream, tx: mpsc::Sender<Msg>) {
    let mut line = String::new();
    if BufReader::new(&conn).read_line(&mut line).is_err() {
        return;
    }
    let mut args: Vec<String> = line.trim_end_matches('\n').split('\t').map(String::from).collect();
    log::debug!("cli: {args:?}");
    if matches!(args[0].as_str(), "watch" | "wait") {
        match watcher(&mut args) {
            Ok((before_ms, until)) => {
                // a watcher that stops reading mustn't stall the player
                let _ = conn.set_write_timeout(Some(Duration::from_millis(200)));
                let _ = tx.send(Msg::Watch(Watcher { out: conn, before_ms, until, warned: 0 }));
            }
            Err((code, e)) => {
                let _ = writeln!(conn, "{code}\n{e}");
            }
        }
        return;
    }
    let (rtx, rrx) = mpsc::channel();
    if tx.send(Msg::Ctl(args, rtx)).is_err() {
        return;
    }
    let (code, text) = rrx.recv_timeout(Duration::from_secs(90)).unwrap_or_else(|_| fail("spot didn't answer"));
    let _ = writeln!(conn, "{code}\n{text}");
}

/// `watch [--before 30]` or `wait <event> [--before 30]`.
fn watcher(args: &mut Vec<String>) -> Result<(u64, Option<String>), Fail> {
    let before = num_opt(args, "--before")?.unwrap_or(30);
    let until = match (args[0].as_str(), args.get(1).map(String::as_str)) {
        ("watch", _) => None,
        (_, Some(e @ ("track" | "ending" | "playing" | "paused" | "end"))) => Some(e.to_string()),
        _ => return Err(usage("wait: for track, ending, playing, paused or end")),
    };
    Ok((before * 1000, until))
}

// ---------- commands ----------

impl App {
    pub fn on_ctl(&mut self, mut args: Vec<String>, reply: Reply) {
        let cmd = args.remove(0);
        match self.command(&cmd, args, &reply) {
            Ok(Some(text)) => drop(reply.send((0, text))),
            Ok(None) => {} // answered once the lookup is done
            Err(e) => drop(reply.send(e)),
        }
    }

    /// Run a command; its output, or None when a background lookup will answer.
    fn command(&mut self, cmd: &str, mut args: Vec<String>, reply: &Reply) -> Result<Option<String>, Fail> {
        let nothing = || fail("nothing to play: start something with `spot play <words>`");
        match cmd {
            // for programs: playing (1 or empty), name, artists, album, uri, shuffle (1 or empty),
            // repeat (off, all, one); nothing playing is no line
            "status" if flag(&mut args, "--tsv") => {
                let repeat = match self.repeat {
                    Repeat::Off => "off",
                    Repeat::All => "all",
                    Repeat::One => "one",
                };
                let on = |b: bool| if b { "1" } else { "" };
                let row = |t: &Track| tsv_row(&[on(self.want_play), &t.name, &t.artists, &t.album, &t.uri, on(self.shuffle), repeat]);
                return Ok(Some(self.now().map(row).unwrap_or_default()));
            }
            "status" => {}
            "play" | "resume" => {
                let show = flag(&mut args, "--show");
                let from = opt(&mut args, "--from")?.map(|f| uri_arg(&f)).transpose()?;
                if !args.is_empty() {
                    let target = uri_arg(&args.join(" "))?;
                    self.load(vec![target], Then::Play { from, show }, reply.clone());
                    return Ok(None);
                }
                if self.cur.is_none() {
                    return Err(nothing());
                }
                if !self.want_play {
                    self.toggle();
                }
            }
            "pause" => {
                if self.want_play {
                    self.toggle();
                }
            }
            "toggle" | "next" | "prev" if self.now().is_none() && self.upnext.is_empty() => return Err(nothing()),
            "toggle" => self.toggle(),
            "next" => self.skip_soon(1),
            "prev" => self.prev(),
            "seek" => {
                let len = self.now().ok_or_else(nothing)?.ms;
                let a = args.first().map_or("", String::as_str);
                let ms = parse_time(a.trim_start_matches(['+', '-'])).ok_or_else(|| usage("seek: 1:30, 90, +10 or -10"))?;
                let to = match a.chars().next() {
                    Some('+') => self.position() as i64 + ms,
                    Some('-') => self.position() as i64 - ms,
                    _ => ms,
                };
                self.seek_to((to.max(0) as u64).min(len.saturating_sub(1000)));
            }
            "vol" | "volume" => {
                if let Some(a) = args.first() {
                    let n: i64 = a.parse().map_err(|_| usage("vol: 0-100, +10 or -10"))?;
                    let to = if a.starts_with(['+', '-']) { self.vol_pct() as i64 + n } else { n };
                    self.set_volume((to.clamp(0, 100) * 65535 / 100) as i32 - self.vol as i32);
                }
            }
            "shuffle" => match args.first().map(String::as_str) {
                None => self.toggle_shuffle(),
                Some(a @ ("on" | "off")) => {
                    if (a == "on") != self.shuffle {
                        self.toggle_shuffle();
                    }
                }
                Some(_) => return Err(usage("shuffle: on or off")),
            },
            "repeat" => match args.first().map(String::as_str) {
                None => self.cycle_repeat(),
                Some("off") => self.repeat = Repeat::Off,
                Some("all") => self.repeat = Repeat::All,
                Some("one") => self.repeat = Repeat::One,
                Some(m) => return Err(usage(&format!("repeat: off, all or one, not '{m}'"))),
            },
            "queue" => return self.queue_cmd(args, reply),
            "search" => return self.search_cmd(args, reply),
            "playlists" => {
                let uris: Vec<String> = self.playlists.iter().map(Playlist::uri).collect();
                let rows: Vec<String> = self.playlists.iter().zip(&uris).map(|(p, u)| format!("{}  {u}", playlist_name(p))).collect();
                save_results(&uris);
                return Ok(Some(numbered(&rows)));
            }
            "quit" => {
                self.quit = true;
                return Ok(Some(String::new()));
            }
            c => return Err(usage(&format!("unknown command '{c}'"))),
        }
        Ok(Some(self.status_text()))
    }

    /// `queue [-n 10]`, `queue [add] <what>…`, `queue rm N…`, `queue clear`. Each shows the queue
    /// after (after a change, just a peek at the list).
    fn queue_cmd(&mut self, mut args: Vec<String>, reply: &Reply) -> Result<Option<String>, Fail> {
        let n = num_opt(&mut args, "-n")?.map(|n| n as usize);
        let shown = n.unwrap_or(3);
        match args.first().map(String::as_str) {
            None => return Ok(Some(self.queue_text(n.unwrap_or(10)))),
            Some("clear") => {
                self.upnext.clear();
                self.queue_changed();
            }
            Some("rm") => {
                let mut ns = args[1..]
                    .iter()
                    .map(|a| a.parse::<usize>().map_err(|_| usage("queue rm: song numbers from `spot queue`")))
                    .collect::<Result<Vec<_>, _>>()?;
                if ns.is_empty() {
                    return Err(usage("queue rm: which? (numbers from `spot queue`)"));
                }
                let len = self.upnext.len();
                if let Some(bad) = ns.iter().find(|&&n| n == 0 || n > len) {
                    return Err(fail(format!("the queue has {}, so there's no {bad}", plural(len, "song"))));
                }
                ns.sort_unstable();
                ns.dedup();
                for i in ns.into_iter().rev() {
                    self.dequeue(i - 1);
                }
            }
            Some(_) => {
                if args[0] == "add" {
                    args.remove(0);
                }
                if args.is_empty() {
                    return Err(usage("queue add: what?"));
                }
                // several numbers or uris queue each; anything else is one thing to find
                let one = |a: &String| a.parse::<usize>().is_ok() || a.starts_with("spotify:") || a.contains("open.spotify.com/");
                let targets = if args.iter().all(one) {
                    args.iter().map(|a| uri_arg(a)).collect::<Result<_, _>>()?
                } else {
                    vec![uri_arg(&args.join(" "))?]
                };
                self.load(targets, Then::Queue { shown }, reply.clone());
                return Ok(None);
            }
        }
        Ok(Some(self.queue_text(shown)))
    }

    /// `search <words> [--albums | --artists | --playlists] [-n 10]`: numbered, saved for `play N`.
    fn search_cmd(&mut self, mut args: Vec<String>, reply: &Reply) -> Result<Option<String>, Fail> {
        let kinds = [("--tracks", "track"), ("--songs", "track"), ("--albums", "album"), ("--artists", "artist"), ("--playlists", "playlist")];
        let ty = kinds.iter().filter(|(f, _)| flag(&mut args, f)).map(|(_, k)| *k).last().unwrap_or("track");
        let n = num_opt(&mut args, "-n")?.unwrap_or(10).clamp(1, 50) as usize;
        // for programs (the macOS launcher): uri, name, subtitle, cover url, ms, explicit; tab-separated,
        // unnumbered, and the last search's numbers are left alone
        let tsv = flag(&mut args, "--tsv");
        if args.is_empty() {
            return Err(usage("search: for what?"));
        }
        let (api, q, reply) = (self.api.clone(), args.join(" "), reply.clone());
        self.rt.spawn(async move {
            let (mut rows, mut uris) = (vec![], vec![]);
            let res = search(&api, &q, ty, n, |items, _| {
                for i in items {
                    if ty == "track" {
                        if let Some(t) = parse_track(i) {
                            rows.push(if tsv {
                                let sub = format!("{} · {}", t.artists, t.album);
                                tsv_row(&[&t.uri, &t.name, &sub, &cover(&i["album"]), &t.ms.to_string(), if t.explicit { "1" } else { "" }])
                            } else {
                                format!("{} · {}  {}  {}", song(&t), t.album, mmss(t.ms), t.uri)
                            });
                            uris.push(t.uri);
                        }
                    } else if let Some(l) = parse_link(ty, i) {
                        rows.push(if tsv {
                            tsv_row(&[&l.uri, &l.name, &l.sub, &cover(i), "", ""])
                        } else {
                            format!("{} · {}  {}", l.name, l.sub, l.uri)
                        });
                        uris.push(l.uri);
                    }
                }
            })
            .await;
            let _ = reply.send(match res {
                Err(e) => fail(e),
                Ok(()) if tsv => (0, rows.join("\n")),
                Ok(()) if rows.is_empty() => (0, "no results".into()),
                Ok(()) => {
                    save_results(&uris);
                    (0, numbered(&rows))
                }
            });
        });
        Ok(None)
    }

    /// What `t` names: `liked`, a Spotify uri, one of their playlists, or failing those words to
    /// search for (`song` / `album` / `artist` / `playlist` first picks what to search; songs
    /// otherwise). Several matching playlists is an error that lists them, numbered.
    fn target(&self, t: &str) -> Result<Target, Fail> {
        let t = t.trim();
        let low = t.to_lowercase();
        if matches!(low.as_str(), "liked" | "liked songs" | "spotify:collection") {
            return Ok(Target::Uri("liked".into(), None));
        }
        if let Some(rest) = t.strip_prefix("spotify:") {
            let kind = rest.split(':').next().unwrap_or("");
            if !matches!(kind, "track" | "album" | "artist" | "playlist") {
                return Err(fail(format!("can't play a {kind} uri (songs, albums, artists and playlists work)")));
            }
            let name = self.playlists.iter().find(|p| p.uri() == t).map(|p| p.name.clone());
            return Ok(Target::Uri(t.into(), name));
        }
        let name_of = |p: &&Playlist| playlist_name(p).to_lowercase();
        let exact: Vec<&Playlist> = self.playlists.iter().filter(|p| name_of(p) == low).collect();
        if let [p] = exact.as_slice() {
            return Ok(Target::Uri(p.uri(), Some(p.name.clone())));
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
                [p] => return Ok(Target::Uri(p.uri(), Some(p.name.clone()))),
                many => {
                    let uris: Vec<String> = many.iter().map(|p| p.uri()).collect();
                    let rows: Vec<String> = many.iter().zip(&uris).map(|(p, u)| format!("{}  {u}", playlist_name(p))).collect();
                    save_results(&uris);
                    let instead = if kind.is_none() { format!(", `spot play song {q}` searches songs instead") } else { String::new() };
                    return Err(fail(format!(
                        "'{q}' matches {} of your playlists; `spot play N` picks one{instead}:\n{}",
                        many.len(),
                        numbered(&rows)
                    )));
                }
            }
        }
        if q.is_empty() {
            return Err(usage(&format!("play which {}?", kind.unwrap_or("song"))));
        }
        Ok(Target::Search(kind.unwrap_or("track"), q.into()))
    }

    /// Fetch the songs behind each target in the background (searching first for words);
    /// on_loaded acts on them.
    fn load(&mut self, targets: Vec<String>, then: Then, reply: Reply) {
        let targets = match targets.iter().map(|t| self.target(t)).collect::<Result<Vec<_>, _>>() {
            Ok(t) => t,
            Err(e) => return drop(reply.send(e)),
        };
        let (api, sess, tx) = (self.api.clone(), self.sess.subscribe(), self.tx.clone());
        self.rt.spawn(async move {
            let mut list = List { uri: String::new(), name: String::new(), tracks: vec![], found: None };
            for t in targets {
                let (uri, name, found) = match t {
                    Target::Uri(uri, name) => (uri, name, None),
                    Target::Search(ty, q) => match top_hit(&api, ty, &q).await {
                        Ok(hit) => hit,
                        Err(e) => return drop(reply.send(e)),
                    },
                };
                match resolve(api.clone(), sess.clone(), &uri, name).await {
                    Ok((name, tracks)) => {
                        list.tracks.extend(tracks);
                        list = List { uri, name, found, ..list };
                    }
                    Err(e) => return drop(reply.send(fail(e))),
                }
            }
            let _ = tx.send(Msg::Loaded(reply, then, list));
        });
    }

    pub fn on_loaded(&mut self, reply: Reply, then: Then, list: List) {
        let List { uri, name, tracks, found } = list;
        let text = match then {
            Then::Play { from, show } => {
                if uri.starts_with("spotify:track:") && self.queue.is_some() {
                    // a single song plays now, then the queue and the list carry on
                    self.queued = tracks.into_iter().next();
                    self.retried = false;
                    self.load_at = None;
                    self.play_current();
                } else {
                    let start = match from {
                        Some(f) => match tracks.iter().position(|t| t.uri == f) {
                            Some(i) => Some(i),
                            None => return drop(reply.send(fail(format!("{f} isn't in {name}")))),
                        },
                        None => None,
                    };
                    // the same key the window uses, so the sidebar marks it playing
                    let key = uri.strip_prefix("spotify:playlist:").unwrap_or(&uri).into();
                    self.play_queue(Queue { key, name, tracks: Arc::new(tracks), order: vec![], pos: 0 }, start);
                }
                if show {
                    self.show_queue();
                }
                self.status_text()
            }
            Then::Queue { shown } => {
                self.enqueue(tracks);
                self.queue_text(shown)
            }
        };
        let _ = reply.send((0, match found {
            Some(f) => format!("{f}:\n{text}"),
            None => text,
        }));
    }

    // ---------- output ----------

    /// What's playing, where from, the modes and what's next.
    fn status_text(&self) -> String {
        let cur = self.now();
        let mut out = vec![];
        match cur {
            None => out.push("nothing playing".to_string()),
            Some(t) => {
                let mark = if self.want_play { "▶" } else { "‖" };
                out.push(format!("{mark} {}  {} / {}", song(t), mmss(self.position().min(t.ms)), mmss(t.ms)));
                out.push(format!("  album: {}  ·  {}", t.album, t.uri));
            }
        }
        let list = self.queue.as_ref().map(|q| {
            let uri = key_uri(&q.key).filter(|u| u.starts_with("spotify:")).map(|u| format!(" {u}")).unwrap_or_default();
            format!("{}{uri} ({} of {})", list_name(q), q.pos + 1, q.order.len())
        });
        let mut modes = vec![];
        match (self.queued.is_some(), list) {
            (true, Some(l)) => modes.push(format!("from the queue, then {l}")),
            (true, None) => modes.push("from the queue".into()),
            (false, Some(l)) if cur.is_some() => modes.push(format!("from {l}")),
            _ => {}
        }
        if !self.upnext.is_empty() {
            modes.push(format!("{} queued", self.upnext.len()));
        }
        if self.shuffle {
            modes.push("shuffle".into());
        }
        match self.repeat {
            Repeat::All => modes.push("repeat".into()),
            Repeat::One => modes.push("repeat one".into()),
            Repeat::Off => {}
        }
        modes.push(format!("vol {}%", self.vol_pct()));
        if !self.session().is_some_and(|s| !s.is_invalid()) {
            modes.push("not connected yet".into());
        }
        out.push(format!("  {}", modes.join("  ·  ")));
        if !self.status.is_empty() {
            out.push(format!("  ! {}", self.status)); // e.g. a login to approve
        }
        if cur.is_some() {
            out.push(format!("  {}", self.next_text()));
        }
        out.join("\n")
    }

    fn next_text(&self) -> String {
        match self.up_next() {
            None => "next: nothing (the queue ends)".into(),
            Some(n) => format!("next: {}  {}", song(n), n.uri),
        }
    }

    /// The status line, the queue, then the list's next `n` songs after it.
    fn queue_text(&self, n: usize) -> String {
        let row = |t: &Track| format!("{} ({})  {}", song(t), mmss(t.ms), t.uri);
        let mut out = vec![self.status_text().lines().next().unwrap_or("").to_string()];
        if !self.upnext.is_empty() {
            out.push("queue (`spot queue rm N` removes):".into());
            out.push(numbered(&self.upnext.iter().map(row).collect::<Vec<_>>()));
        }
        let upcoming: Vec<String> =
            self.queue.iter().flat_map(|q| q.order.iter().skip(q.pos + 1).take(n).map(|&i| format!("     {}", row(&q.tracks[i])))).collect();
        match &self.queue {
            Some(q) if !upcoming.is_empty() => {
                out.push(format!("then {}:", list_name(q)));
                out.extend(upcoming);
            }
            _ if self.upnext.is_empty() => out.push("nothing after this".into()),
            _ => {}
        }
        out.join("\n")
    }

    // ---------- watchers ----------

    pub fn watch(&mut self, mut w: Watcher) {
        // `watch` starts with the status line; `wait` only prints its event
        let hello = match w.until {
            None => format!("0\n{}\n", self.status_text().lines().next().unwrap_or("")),
            Some(_) => "0\n".into(),
        };
        // already inside its warning window, `ending` fires on the next tick
        if w.out.write_all(hello.as_bytes()).is_ok() {
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
        let now = || cur.map_or("nothing".into(), |t| format!("{}  {}", song(t), t.uri));
        let next = self.next_text();
        let mut events: Vec<(&str, String)> = vec![];
        if new_track {
            events.push(("track", format!("track: {} ({})  ·  {next}", now(), mmss(cur.map_or(0, |t| t.ms)))));
        }
        if toggled {
            events.push(match (self.want_play, self.finished) {
                (true, _) => ("playing", format!("playing: {}", now())),
                (false, true) => ("end", "end: the queue ran out".into()),
                (false, false) => ("paused", format!("paused: {}", now())),
            });
        }
        let ending = cur.filter(|t| t.ms > 0 && self.want_play).map(|t| {
            let left = t.ms.saturating_sub(self.position());
            (left, format!("ending in {}s: {}  ·  {next}", left.div_ceil(1000), song(t)))
        });
        let plays = self.plays;
        self.ctl.watchers.retain_mut(|w| {
            let wants = |e: &str| w.until.as_deref().is_none_or(|u| u == e);
            let mut lines: Vec<&str> = events.iter().filter(|(e, _)| wants(e)).map(|(_, l)| l.as_str()).collect();
            if let Some((left, l)) = &ending
                && *left <= w.before_ms
                && w.warned != plays
                && wants("ending")
            {
                w.warned = plays;
                lines.push(l);
            }
            let ok = lines.iter().all(|l| writeln!(w.out, "{l}").is_ok());
            // a `wait` is done once its event came
            ok && (w.until.is_none() || lines.is_empty())
        });
    }
}

/// Spotify's top hit of a type for some words: its uri, its name if it's a playlist, and what
/// was searched (to say so). Asks for 10 like `spot search`: Spotify ranks differently for 1.
async fn top_hit(api: &Api, ty: &'static str, q: &str) -> Result<(String, Option<String>, Option<String>), Fail> {
    let what = if ty == "track" { "song" } else { ty };
    let mut hit = None;
    search(api, q, ty, 10, |items, _| hit = items.iter().find(|i| i["uri"].is_string()).cloned()).await.map_err(fail)?;
    let hit = hit.ok_or_else(|| fail(format!("no {what} on Spotify matches '{q}'")))?;
    let name = (ty == "playlist").then(|| hit["name"].as_str().unwrap_or("playlist").to_string());
    Ok((hit["uri"].as_str().unwrap_or_default().into(), name, Some(format!("the top {what} for '{q}'"))))
}

/// The songs behind a uri, and the list's name. Albums and artists come from the cache when
/// they're in it; Liked Songs and playlists change, so they're refreshed.
async fn resolve(api: Arc<Api>, sess: Sess, uri: &str, name: Option<String>) -> Result<(String, Vec<Track>), String> {
    if uri.starts_with("spotify:track:") {
        let session = ready(sess).await?;
        let t = batch_meta::<PbTrack>(&session, &[uri.to_string()], ExtensionKind::TRACK_V4).await?.remove(uri).ok_or("song not found")?;
        let t = pb_track(uri, &t);
        return Ok((song(&t), vec![t]));
    }
    let cached = read_tracks(&tracks_path(&list_key(uri)));
    let changes = uri == "liked" || uri.starts_with("spotify:playlist:");
    let tracks = if changes || cached.is_empty() {
        let (nobody, _) = mpsc::channel(); // the songs would stream to a view; none is showing this
        fetch_list(api, sess.clone(), uri, &cached, &nobody, 0).await?.unwrap_or(cached)
    } else {
        cached
    };
    let name = match name {
        Some(n) => n,
        None if uri == "liked" => "Liked Songs".into(),
        None if uri.starts_with("spotify:playlist:") => {
            use librespot_metadata::Metadata;
            let session = ready(sess).await?;
            let u = SpotifyUri::from_uri(uri).map_err(|e| e.to_string())?;
            librespot_metadata::Playlist::get(&session, &u).await.map_or("playlist".into(), |p| p.name().to_string())
        }
        None if uri.starts_with("spotify:artist:") => {
            let by = tracks.iter().find(|t| id_uri("artist", t.artist_id).as_deref() == Some(uri));
            format!("{} · all songs", by.map_or("artist", |t| &t.artists[..t.lead]))
        }
        None => tracks.first().map_or("album".into(), |t| format!("{} — {}", t.album, t.artists)),
    };
    if tracks.is_empty() {
        return Err(format!("{name} has no songs to play"));
    }
    Ok((name, tracks))
}
