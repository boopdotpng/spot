// The player's end of the command line, made for agents. `spot status`, `spot play <uri>` and the
// rest (cli.rs) send their arguments over a Unix socket (~/.cache/spot/sock) as one tab-separated
// line; the reply is an exit code on the first line, then the text to print. Every request
// becomes a Msg in the main loop, so keys, media keys and the CLI all drive the same state, and
// the TUI shows whatever the CLI changed. A TUI attaching (`spot` in a terminal) starts on this
// socket too, then the connection becomes its screen (terminal.rs).

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

/// How many of the list's songs `status` shows after the queue.
const SHOWN: usize = 5;

/// What a background lookup was for, carried back to the main loop with its songs.
pub enum Then {
    Play { from: Option<String> },
    /// `queue`: the new queue, None where the next fetched uri's songs go
    Queue(Vec<Option<Track>>),
}

/// What a request named, fetched: each uri's songs, and the last one's uri and name.
pub struct List {
    uri: String,
    name: String,
    parts: Vec<Vec<Track>>,
}

/// A `spot watch` connection, sent event lines until it hangs up.
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
    modes: (bool, Repeat),
    pub skipped: bool, // a media key skipped or seeked since the last tick
    /// The play number of the song `spot play` last started. It's held where a song taken
    /// off the queue is, but didn't come from the queue.
    played: Option<u64>,
}

impl State {
    pub fn watchers(&self) -> &[Watcher] {
        &self.watchers
    }
}

fn song(t: &Track) -> String {
    format!("{} — {}", t.name, t.artists)
}

/// An open.spotify.com link as a uri; anything else as is.
fn link_uri(a: &str) -> String {
    let Some(rest) = a.strip_prefix("https://open.spotify.com/").or_else(|| a.strip_prefix("open.spotify.com/")) else {
        return a.into();
    };
    let mut parts = rest.split(['/', '?']).filter(|p| !p.starts_with("intl-"));
    match (parts.next(), parts.next()) {
        (Some(kind), Some(id)) => format!("spotify:{kind}:{id}"),
        _ => a.into(),
    }
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

/// One `set` change.
#[cfg_attr(test, derive(PartialEq))]
enum Change {
    Paused(bool),
    Volume(i64, bool), // percent, and whether it's +/- the current
    Shuffle(bool),
    Repeat(Repeat),
    Seek(i64, Option<char>), // ms, from the start or + / - from where it is
}

/// `paused=yes`, `volume=40` / `+10`, `shuffle=on`, `repeat=all`, `seek=1:30` / `-10`.
fn parse_change(a: &str) -> Result<Change, Fail> {
    let yes = |v: &str| match v {
        "yes" | "true" | "on" | "1" => Some(true),
        "no" | "false" | "off" | "0" => Some(false),
        _ => None,
    };
    let sign = |v: &str| v.chars().next().filter(|c| matches!(c, '+' | '-'));
    let bad = |m: &str| usage(&format!("set {a}: {m}"));
    let Some((k, v)) = a.split_once('=') else { return Err(usage(&format!("set: key=value, not '{a}'"))) };
    Ok(match k {
        "paused" => Change::Paused(yes(v).ok_or_else(|| bad("yes or no"))?),
        "shuffle" => Change::Shuffle(yes(v).ok_or_else(|| bad("on or off"))?),
        "volume" | "vol" => Change::Volume(v.parse().map_err(|_| bad("0-100, +10 or -10"))?, sign(v).is_some()),
        "repeat" => Change::Repeat(match v {
            "off" => Repeat::Off,
            "all" => Repeat::All,
            "one" => Repeat::One,
            _ => return Err(bad("off, all or one")),
        }),
        "seek" => Change::Seek(parse_time(v.trim_start_matches(['+', '-'])).ok_or_else(|| bad("1:30, 90, +10 or -10"))?, sign(v)),
        _ => return Err(usage(&format!("set: paused, volume, shuffle, repeat or seek, not '{k}'"))),
    })
}

/// `queue <uri>…`: the new queue, with songs already in it kept as they are and Err(uri) for
/// what has to be fetched. A first uri that's the queued song playing now was read before it
/// started, so it's left out rather than queued again.
fn plan_queue(uris: &[String], upnext: &[Track], playing: Option<&Track>) -> Vec<Result<Track, String>> {
    let started = matches!((uris.first(), playing), (Some(u), Some(p)) if *u == p.uri && upnext.first().is_none_or(|n| n.uri != p.uri));
    uris[started as usize..].iter().map(|u| upnext.iter().find(|t| t.uri == *u).cloned().ok_or_else(|| u.clone())).collect()
}

/// The uri a list's key stands for: Liked Songs, a playlist id, or an album / artist / show uri.
fn key_uri(key: &str) -> Option<String> {
    if key == "liked" {
        Some("spotify:collection".into())
    } else if key.starts_with("spotify:") {
        Some(key.into())
    } else if key.contains(':') {
        None // a search
    } else {
        Some(format!("spotify:playlist:{key}"))
    }
}

fn playlist_name(p: &Playlist) -> &str {
    &p.name
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
    if args[0] == "attach" {
        return terminal::serve(conn, &args, tx);
    }
    if args[0] == "watch" {
        match num_opt(&mut args, "--before") {
            Ok(before) => {
                // a watcher that stops reading mustn't stall the player
                let _ = conn.set_write_timeout(Some(Duration::from_millis(200)));
                let _ = tx.send(Msg::Watch(Watcher { out: conn, before_ms: before.unwrap_or(30) * 1000, warned: 0 }));
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

    /// Run a command; its output, or None when a background lookup will answer. Whatever
    /// changes something answers with the status after.
    fn command(&mut self, cmd: &str, mut args: Vec<String>, reply: &Reply) -> Result<Option<String>, Fail> {
        let nothing = || fail("nothing to play: start something with `spot play <uri>`");
        match cmd {
            "status" => {
                let n = num_opt(&mut args, "-n")?.map_or(SHOWN, |n| n as usize);
                return Ok(Some(self.status_text(n)));
            }
            "library" => return Ok(Some(self.library_text())),
            "search" => return self.search_cmd(args, reply),
            "launcher" => return self.launcher_cmd(args, reply),
            "play" => {
                let from = opt(&mut args, "--from")?.map(|f| link_uri(&f));
                let [uri] = &args[..] else { return Err(usage("play: one uri (--from <song uri> starts a list at that song)")) };
                self.load(vec![uri.clone()], Then::Play { from }, reply.clone());
                return Ok(None);
            }
            "queue" => return self.queue_cmd(args, reply),
            "set" => self.set_cmd(&args)?,
            "next" | "prev" if self.now().is_none() && self.upnext.is_empty() => return Err(nothing()),
            "next" => self.skip_soon(1),
            "prev" => self.prev(),
            "quit" => {
                self.quit = true;
                return Ok(Some(String::new()));
            }
            c => return Err(usage(&format!("unknown command '{c}'"))),
        }
        Ok(Some(self.status_text(SHOWN)))
    }

    /// `set key=value…`: all of them checked before any is made.
    fn set_cmd(&mut self, args: &[String]) -> Result<(), Fail> {
        if args.is_empty() {
            return Err(usage("set: paused=, volume=, shuffle=, repeat= or seek="));
        }
        let changes = args.iter().map(|a| parse_change(a)).collect::<Result<Vec<_>, _>>()?;
        let needs_song = changes.iter().any(|c| matches!(c, Change::Seek(..) | Change::Paused(false)));
        if needs_song && self.now().is_none() {
            return Err(fail("nothing to play: start something with `spot play <uri>`"));
        }
        for c in changes {
            match c {
                Change::Paused(p) => {
                    if self.want_play == p {
                        self.toggle();
                    }
                }
                Change::Volume(n, rel) => {
                    let to = if rel { self.vol_pct() as i64 + n } else { n };
                    self.set_volume((to.clamp(0, 100) * 65535 / 100) as i32 - self.vol as i32);
                }
                Change::Shuffle(on) => {
                    if on != self.shuffle {
                        self.toggle_shuffle();
                    }
                }
                Change::Repeat(r) => self.repeat = r,
                Change::Seek(ms, sign) => {
                    let len = self.now().map_or(0, |t| t.ms);
                    let to = match sign {
                        Some('+') => self.position() as i64 + ms,
                        Some('-') => self.position() as i64 - ms,
                        _ => ms,
                    };
                    self.seek_to((to.max(0) as u64).min(len.saturating_sub(1000)));
                }
            }
        }
        Ok(())
    }

    /// `queue <uri>…` sets the whole queue; `queue clear` empties it.
    pub(crate) fn queue_cmd(&mut self, args: Vec<String>, reply: &Reply) -> Result<Option<String>, Fail> {
        if args.is_empty() {
            return Err(usage("queue: the uris it should hold, in order (`spot queue clear` empties it)"));
        }
        let uris: Vec<String> = if args == ["clear"] { vec![] } else { args.iter().map(|a| link_uri(a)).collect() };
        let from_queue = self.queued.as_ref().filter(|_| self.ctl.played != Some(self.plays));
        let plan = plan_queue(&uris, &self.upnext, from_queue);
        let fetch: Vec<String> = plan.iter().filter_map(|p| p.as_ref().err().cloned()).collect();
        let plan = plan.into_iter().map(Result::ok).collect();
        if fetch.is_empty() {
            self.set_queue(plan, vec![]);
            return Ok(Some(self.status_text(SHOWN)));
        }
        self.load(fetch, Then::Queue(plan), reply.clone());
        Ok(None)
    }

    /// Make the queue `plan`, its gaps filled in order with `parts`; with nothing playing (or
    /// everything played), its first song starts.
    fn set_queue(&mut self, plan: Vec<Option<Track>>, parts: Vec<Vec<Track>>) {
        let mut parts = parts.into_iter();
        self.upnext = plan.into_iter().flat_map(|p| p.map_or_else(|| parts.next().unwrap_or_default(), |t| vec![t])).collect();
        self.queue_changed();
        if !self.upnext.is_empty() && (self.cur.is_none() || self.finished) {
            self.skip(1);
        }
    }

    /// `search <words> [--type track|album|artist|playlist|show|episode] [-n 10]`.
    fn search_cmd(&mut self, mut args: Vec<String>, reply: &Reply) -> Result<Option<String>, Fail> {
        let ty: &'static str = match opt(&mut args, "--type")?.as_deref() {
            None | Some("track" | "song") => "track",
            Some("album") => "album",
            Some("artist") => "artist",
            Some("playlist") => "playlist",
            Some("show") => "show",
            Some("episode") => "episode",
            Some("podcast") => "show",
            Some(t) => return Err(usage(&format!("--type: track, album, artist, playlist, show or episode, not '{t}'"))),
        };
        let n = num_opt(&mut args, "-n")?.unwrap_or(10).clamp(1, 50) as usize;
        if args.is_empty() {
            return Err(usage("search: for what?"));
        }
        let (api, q, reply) = (self.api.clone(), args.join(" "), reply.clone());
        self.rt.spawn(async move {
            let mut rows = vec![];
            let res = search(&api, &q, ty, n, |items, _| {
                for i in items {
                    rows.extend(match ty {
                        "track" => parse_track(i).map(|t| format!("{} · {}  {}  {}", song(&t), t.album, mmss(t.ms), t.uri)),
                        "episode" => parse_episode(i, None).map(|t| format!("{} · {}  {}  {}", t.name, t.album, hours(t.ms), t.uri)),
                        _ => parse_link(ty, i).map(|l| format!("{} · {}  {}", l.name, l.sub, l.uri)),
                    });
                }
            })
            .await;
            let _ = reply.send(match res {
                Err(e) => fail(e),
                Ok(()) if rows.is_empty() => (0, "no results".into()),
                Ok(()) => (0, rows.join("\n")),
            });
        });
        Ok(None)
    }

    /// The uri `t` names (Liked Songs is `liked`), and its name if it's one of their playlists.
    fn target(&self, t: &str) -> Result<(String, Option<String>), Fail> {
        let t = link_uri(t.trim());
        if t == "liked" || (t.starts_with("spotify:") && t.ends_with(":collection")) {
            return Ok(("liked".into(), None));
        }
        match t.strip_prefix("spotify:").and_then(|r| r.split(':').next()) {
            Some("track" | "album" | "artist" | "playlist" | "show" | "episode") => {}
            Some(kind) => return Err(fail(format!("can't play a {kind} uri (songs, albums, artists, playlists, podcasts and episodes work)"))),
            None => return Err(usage(&format!("'{t}' isn't a spotify uri: `spot search` and `spot library` print them"))),
        }
        let name = self.playlists.iter().find(|p| p.uri() == t).map(|p| p.name.clone());
        Ok((t, name))
    }

    /// Play an album, artist or playlist from the TUI without opening it, as `spot play
    /// <uri>` would. Nobody waits on the answer.
    pub fn play_uri(&mut self, uri: String) {
        let (reply, _) = mpsc::channel();
        self.load(vec![uri], Then::Play { from: None }, reply);
    }

    /// Fetch the songs behind each uri in the background; on_loaded acts on them.
    pub fn load(&mut self, targets: Vec<String>, then: Then, reply: Reply) {
        if targets.is_empty() {
            return drop(reply.send(usage("which uris?")));
        }
        let targets = match targets.iter().map(|t| self.target(t)).collect::<Result<Vec<_>, _>>() {
            Ok(t) => t,
            Err(e) => return drop(reply.send(e)),
        };
        let (api, sess, tx) = (self.api.clone(), self.sess.subscribe(), self.tx.clone());
        self.rt.spawn(async move {
            let mut list = List { uri: String::new(), name: String::new(), parts: vec![] };
            for (uri, name) in targets {
                match resolve(api.clone(), sess.clone(), &uri, name).await {
                    Ok((name, tracks)) => {
                        list.parts.push(tracks);
                        (list.uri, list.name) = (uri, name);
                    }
                    Err(e) => return drop(reply.send(fail(e))),
                }
            }
            let _ = tx.send(Msg::Loaded(reply, then, list));
        });
    }

    pub fn on_loaded(&mut self, reply: Reply, then: Then, list: List) {
        let List { uri, name, mut parts } = list;
        match then {
            Then::Play { from } => {
                let tracks = parts.pop().unwrap_or_default();
                if (uri.starts_with("spotify:track:") || uri.starts_with("spotify:episode:")) && self.queue.is_some() {
                    // a single song plays now, then the queue and the list carry on
                    self.queued = tracks.into_iter().next();
                    self.play_fresh();
                    self.ctl.played = Some(self.plays);
                } else {
                    let start = match from {
                        Some(f) => match tracks.iter().position(|t| t.uri == f) {
                            Some(i) => Some(i),
                            None => return drop(reply.send(fail(format!("{f} isn't in {name}")))),
                        },
                        None => None,
                    };
                    // the same key the TUI uses, so the sidebar marks it playing
                    let key = uri.strip_prefix("spotify:playlist:").unwrap_or(&uri).into();
                    self.play_queue(Queue { key, name, tracks: Arc::new(tracks), order: vec![], pos: 0, sorted: vec![] }, start);
                }
            }
            Then::Queue(plan) => self.set_queue(plan, parts),
        }
        let _ = reply.send((0, self.status_text(SHOWN)));
    }

    // ---------- output ----------

    /// What's playing, the list it's from, the modes, the queue, then the list's next `n` songs.
    fn status_text(&self, n: usize) -> String {
        let row = |t: &Track| format!("  {} ({})  {}", song(t), mmss(t.ms), t.uri);
        let mut out = vec![];
        match self.now() {
            None => out.push("nothing playing".to_string()),
            Some(t) => {
                let state = if self.want_play { "playing" } else { "paused" };
                let queued = if self.queued.is_some() && self.ctl.played != Some(self.plays) { "  (from the queue)" } else { "" };
                out.push(format!("{state}: {} · {}  {} / {}  {}{queued}", song(t), t.album, mmss(self.position().min(t.ms)), mmss(t.ms), t.uri));
            }
        }
        if let Some(q) = &self.queue {
            let uri = key_uri(&q.key).filter(|u| u.starts_with("spotify:")).map(|u| format!("  {u}")).unwrap_or_default();
            out.push(format!("list: {}{uri}  (song {} of {})", list_name(q), q.pos + 1, q.order.len()));
        }
        let on = |b: bool| if b { "on" } else { "off" };
        out.push(format!("shuffle {} · repeat {} · volume {}%", on(self.shuffle), self.repeat.name(), self.vol_pct()));
        if !self.session().is_some_and(|s| !s.is_invalid()) {
            out.push("not connected yet".into());
        }
        if !self.status.is_empty() {
            out.push(format!("! {}", self.status)); // e.g. a login to approve
        }
        if self.upnext.is_empty() {
            out.push("queue: empty".into());
        } else {
            out.push("queue:".into());
            out.extend(self.upnext.iter().map(row));
        }
        let upcoming: Vec<String> = self.queue.iter().flat_map(|q| q.upcoming().take(n).map(row)).collect();
        match &self.queue {
            Some(q) if !upcoming.is_empty() => {
                out.push(format!("then {}:", list_name(q)));
                out.extend(upcoming);
            }
            _ => out.push("then nothing".into()),
        }
        out.join("\n")
    }

    /// Liked Songs, their playlists and the podcasts they follow.
    fn library_text(&self) -> String {
        let mut out = vec![];
        for p in &self.playlists {
            out.push(format!("{}  {}", playlist_name(p), key_uri(&p.uri()).unwrap_or_default()));
        }
        if out.is_empty() {
            out.push("no playlists yet (still loading?)".into());
        }
        let shows: Vec<String> = self.shows.iter().filter(|s| is_show(&s.uri)).map(|s| format!("  {} · {}  {}", s.name, s.sub, s.uri)).collect();
        if !shows.is_empty() {
            out.push("podcasts:".into());
            out.extend(shows);
        }
        out.join("\n")
    }

    fn next_text(&self) -> String {
        match self.up_next() {
            None => "next: nothing (the queue ends)".into(),
            Some(n) => format!("next: {}  {}", song(n), n.uri),
        }
    }

    // ---------- watchers ----------

    pub fn watch(&mut self, mut w: Watcher) {
        // it starts with what's playing; already inside its warning window, `ending` fires on the next tick
        let hello = format!("0\n{}\n", self.status_text(0).lines().next().unwrap_or(""));
        if w.out.write_all(hello.as_bytes()).is_ok() {
            self.ctl.watchers.push(w);
        }
    }

    /// Tell watchers what changed since the last tick: a new song, play / pause, shuffle or
    /// repeat, the queue running out, a media key skipping or seeking, or the song ending within their `--before`.
    pub fn tick_watchers(&mut self) {
        let new_track = std::mem::replace(&mut self.ctl.plays, self.plays) != self.plays;
        let toggled = std::mem::replace(&mut self.ctl.playing, self.want_play) != self.want_play;
        let modes = std::mem::replace(&mut self.ctl.modes, (self.shuffle, self.repeat)) != (self.shuffle, self.repeat);
        let skipped = std::mem::take(&mut self.ctl.skipped);
        if self.ctl.watchers.is_empty() {
            return;
        }
        let cur = self.now();
        let now = || cur.map_or("nothing".into(), |t| format!("{}  {}", song(t), t.uri));
        let next = self.next_text();
        let mut events: Vec<String> = vec![];
        if new_track {
            events.push(format!("track: {} ({})  ·  {next}", now(), mmss(cur.map_or(0, |t| t.ms))));
        }
        if toggled {
            events.push(match (self.want_play, self.finished) {
                (true, _) => format!("playing: {}", now()),
                (false, true) => "end: the queue ran out".into(),
                (false, false) => format!("paused: {}", now()),
            });
        }
        if modes {
            events.push(format!("modes: shuffle {}, repeat {}", if self.shuffle { "on" } else { "off" }, self.repeat.name()));
        }
        if skipped {
            events.push(format!("skip: {}", now()));
        }
        let ending = cur.filter(|t| t.ms > 0 && self.want_play).map(|t| {
            let left = t.ms.saturating_sub(self.position());
            (left, format!("ending in {}s: {}  ·  {next}", left.div_ceil(1000), song(t)))
        });
        let plays = self.plays;
        self.ctl.watchers.retain_mut(|w| {
            let mut lines: Vec<&str> = events.iter().map(String::as_str).collect();
            if let Some((left, l)) = &ending
                && *left <= w.before_ms
                && w.warned != plays
            {
                w.warned = plays;
                lines.push(l);
            }
            lines.iter().all(|l| writeln!(w.out, "{l}").is_ok())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(i: usize) -> Track {
        Track {
            uri: format!("spotify:track:{i}"),
            name: String::new(),
            artists: String::new(),
            lead: 0,
            album: String::new(),
            cover: None,
            ms: 0,
            album_id: 0,
            artist_id: 0,
            more: Vec::new(),
            disc: 0,
            number: 0,
            explicit: false,
            added: 0,
        }
    }

    fn uris(a: &[usize]) -> Vec<String> {
        a.iter().map(|i| format!("spotify:track:{i}")).collect()
    }

    /// A plan as uris, `?` for what has to be fetched.
    fn shape(plan: &[Result<Track, String>]) -> Vec<String> {
        plan.iter().map(|p| p.as_ref().map_or_else(|u| format!("?{u}"), |t| t.uri.clone())).collect()
    }

    #[test]
    fn queue_keeps_what_it_has_and_fetches_the_rest() {
        let upnext = vec![track(1), track(2)];
        let plan = plan_queue(&uris(&[2, 3, 1]), &upnext, None);
        assert_eq!(shape(&plan), ["spotify:track:2", "?spotify:track:3", "spotify:track:1"]);
    }

    #[test]
    fn queue_leaves_out_a_first_song_that_has_started() {
        // read as [1, 2]; 1 has started playing since, so the queue is [2]
        let playing = track(1);
        let plan = plan_queue(&uris(&[1, 2, 3]), &[track(2)], Some(&playing));
        assert_eq!(shape(&plan), ["spotify:track:2", "?spotify:track:3"]);
        // queued twice and still at the head: kept
        let plan = plan_queue(&uris(&[1, 2]), &[track(1), track(2)], Some(&playing));
        assert_eq!(shape(&plan), ["spotify:track:1", "spotify:track:2"]);
        // not the first: kept
        let plan = plan_queue(&uris(&[2, 1]), &[track(2)], Some(&playing));
        assert_eq!(shape(&plan), ["spotify:track:2", "?spotify:track:1"]);
    }

    #[test]
    fn set_parses_each_change() {
        assert!(parse_change("paused=yes").ok() == Some(Change::Paused(true)));
        assert!(parse_change("volume=+10").ok() == Some(Change::Volume(10, true)));
        assert!(parse_change("volume=40").ok() == Some(Change::Volume(40, false)));
        assert!(parse_change("shuffle=off").ok() == Some(Change::Shuffle(false)));
        assert!(parse_change("repeat=one").ok() == Some(Change::Repeat(Repeat::One)));
        assert!(parse_change("seek=1:30").ok() == Some(Change::Seek(90_000, None)));
        assert!(parse_change("seek=-10").ok() == Some(Change::Seek(10_000, Some('-'))));
        for bad in ["paused", "paused=maybe", "volume=loud", "repeat=twice", "seek=x", "speed=2"] {
            assert!(parse_change(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn links_become_uris() {
        assert_eq!(link_uri("https://open.spotify.com/intl-de/track/abc?si=x"), "spotify:track:abc");
        assert_eq!(link_uri("spotify:album:x"), "spotify:album:x");
    }
}
