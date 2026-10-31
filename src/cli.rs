// `spot <command>`: sends its arguments to the running player (ctl.rs) and prints the answer.
// `start` and `quit` are handled here, since they start and stop the player process itself.

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
};

use crate::*;

pub const COMMANDS: &[&str] = &[
    "start", "quit", "status", "play", "resume", "pause", "toggle", "next", "prev", "seek", "vol", "volume", "shuffle", "repeat", "queue",
    "search", "playlists", "watch", "wait",
];

pub const USAGE: &str = "\
spot: tiny Spotify player, and a command line for the one that's running

  spot                       open the TUI, starting the player in the background if needed (and
                             taking the TUI from any other terminal); d closes it, music playing
  spot start                 start it in the background, without a window, and return once
                             it's ready (what scripts and agents should use)
  spot --headless            the same in the foreground, for a launchd / systemd service
  spot quit                  stop the player, either kind (it resumes where it was next start)
  spot clean                 clear cached songs and lists (keeps login)
  spot -v, --version         which spot this is

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
  spot play 3                the 3rd result of the last search or `spot playlists` (queue add
                             takes these too)
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
  spot queue [add] <what>... add to the end of the queue: result numbers, uris, or words as in
                             play (`spot queue 2` = result 2). Songs play before the list carries
                             on; an album or playlist queues all its songs
  spot queue add --at 2 <what>...   the same, in at that position of the queue (1 plays next)
  spot queue rm 2 5-8        remove songs from the queue: numbers in `spot queue`, ranges, or uris
                             (a uri still means the same song if the queue has moved on)
  spot queue mv 5 1          move songs to a position: the same numbers, ranges or uris, then
                             where the first goes (`spot queue mv 3-5 1`); they stay in order
  spot queue clear           empty the queue
  spot playlists             your playlists, numbered, with uris
  spot watch [--before 30]   a line per event as it happens: track (new song), playing,
                             paused, skip (a media key skipped or seeked), end (nothing left to
                             play), ending (the song ends in 30s; says what's next, so there's
                             time to `queue add` something else)
  spot wait <event> [--before 30]   wait for the next such event, print it, exit

Finding music: `spot play <words>` plays the top hit and says so; to choose, `spot search` and
`spot play N`. Several of your playlists matching lists them, numbered for `spot play N`.
Spotify links (https://open.spotify.com/...) work anywhere a uri does.";

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

/// Send the arguments, print the reply: an exit code line, then text (an error's on stderr).
/// `watch` and `wait` keep printing event lines as they come.
pub fn client(args: Vec<String>) -> i32 {
    match args[0].as_str() {
        "start" => return start(true),
        "quit" => return quit(),
        _ => {}
    }
    let conn = match connect() {
        Ok(c) => c,
        Err(code) => return code,
    };
    let watching = matches!(args[0].as_str(), "watch" | "wait");
    if !watching {
        let _ = conn.set_read_timeout(Some(Duration::from_secs(100)));
    }
    if (&conn).write_all(format!("{}\n", args.join("\t")).as_bytes()).is_err() {
        eprintln!("spot: couldn't send the request");
        return 1;
    }
    let mut lines = BufReader::new(&conn).lines().map_while(Result::ok);
    let Some(code) = lines.next().and_then(|l| l.parse::<i32>().ok().or(l.starts_with('{').then_some(-1))) else {
        eprintln!("spot: no reply (did it quit?)");
        return 1;
    };
    if code < 0 {
        eprintln!("spot: the running spot is an older version: `spot quit`, then start it again");
        return 1;
    }
    let mut printed = false;
    for line in lines {
        match (code, printed) {
            (0, _) | (_, true) => println!("{line}"),
            _ => eprintln!("spot: {line}"),
        }
        printed = true;
    }
    // the player hung up on a watcher: it quit (a `wait` is hung up on once its event came)
    if watching && code == 0 && !(args[0] == "wait" && printed) {
        println!("spot quit");
        return 3;
    }
    code
}

/// `spot start`: run `spot --headless` detached from this terminal (its own process group, no
/// stdio, so whatever started it can exit or close), and wait until it answers. `say` prints how
/// that went; the TUI doesn't, or the line would be waiting in the terminal when it closes.
pub fn start(say: bool) -> i32 {
    if UnixStream::connect(sock_path()).is_ok() {
        if say {
            println!("spot is already running");
        }
        return 0;
    }
    let id = std::env::var_os("SPOT_CLIENT_ID").is_some() || settings::load().0.client_id.is_some() || old_client_id().is_some();
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
            if say {
                println!("spot is running in the background (pid {}); `spot quit` stops it", child.id());
            }
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
    let _ = (&conn).write_all(b"quit\n");
    let mut reply = String::new();
    let _ = BufReader::new(&conn).read_line(&mut reply);
    // a player from before the text protocol talks JSON; ask it that way, so `quit` works across an upgrade
    if reply.starts_with('{')
        && let Ok(old) = connect()
    {
        let _ = (&old).write_all(b"{\"cmd\":\"quit\"}\n");
        let _ = BufReader::new(&old).read_line(&mut reply);
    }
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
