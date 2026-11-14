// spot: a tiny Spotify TUI. librespot for audio, the Web API for library and search, ratatui for
// the UI. This file: the command line's way in, startup (logins, the player, the threads feeding
// it) and the main loop. The player stays in the background; terminal.rs handles the disposable
// TUI connections, and the command line talks to it over the same socket (ctl.rs).
// The imports here are shared by every module (`use crate::*`).

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::IsTerminal,
    path::PathBuf,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use librespot_core::{
    Session, SessionConfig, SpotifyUri, authentication::Credentials, cache::Cache,
};
use librespot_oauth::{OAuthClient, OAuthClientBuilder};
use librespot_playback::{
    audio_backend,
    config::{AudioFormat, Bitrate, PlayerConfig},
    mixer::{self, Mixer, MixerConfig},
    player::{Player, PlayerEvent},
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Cell, List, ListItem, ListState, Paragraph, Row, Table, TableState},
};
use serde_json::Value;

mod app;
mod art;
mod artist;
mod fmt;
mod auth;
mod cache;
mod cli;
mod ctl;
mod keys;
mod lastfm;
mod launch;
mod lists;
mod metadata;
mod model;
mod nowplaying;
mod playback;
mod settings;
mod terminal;
mod theme;
mod ui;
mod views;
mod webapi;

use app::*;
use auth::*;
use cache::*;
use fmt::*;
use keys::*;
use lastfm::*;
use lists::*;
use metadata::*;
use model::*;
use nowplaying::*;
use theme::*;
use ui::*;
use webapi::*;

// Each user brings their own Spotify developer app (the shared librespot client id is permanently
// rate limited): SPOT_CLIENT_ID, else client_id in config.toml, else asked for on first run.
// SPOT_REDIRECT overrides the redirect, which must be registered on the app.
const REDIRECT: &str = "http://127.0.0.1:19872/login";
const SCOPES: &[&str] = &[
    "streaming",
    "user-read-private",
    "playlist-read-private",
    "playlist-read-collaborative",
    "user-library-read",
];
const SESSION_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
const SESSION_REDIRECT: &str = "http://127.0.0.1:8898/login";
const API: &str = "https://api.spotify.com/v1";
/// n/p only loads the track once skipping has paused this long, so mashing next doesn't fetch
/// (and ask Spotify for an audio key for) every track passed over; too many gets keys refused.
const SKIP_SETTLE: Duration = Duration::from_millis(500);
/// View key of the queue (`Q`); has a colon, so it's never a playlist id.
const QUEUE_KEY: &str = "queue:";
const NOT_SET_UP: &str = "not set up yet: run `spot` once in a terminal to log in";

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRunLoopRun();
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
}

/// Hand freed heap pages back to the OS. macOS malloc otherwise keeps them dirty (and counted
/// against us) after bursts like loading a big library or starting a track.
pub fn trim_heap() {
    #[cfg(target_os = "macos")]
    unsafe {
        malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
    }
}

/// Log levels by module path prefix; the longest matching prefix wins, else the first entry.
const LOG_LEVELS: &[(&str, log::LevelFilter)] = {
    use log::LevelFilter::*;
    &[("", Info), ("spot", Debug), ("librespot", Debug), ("symphonia_bundle_mp3", Error)]
};

/// Lines like `[2026-11-10T12:34:56.789Z INFO  spot::app] text`, to a file.
struct FileLog(std::sync::Mutex<std::io::LineWriter<fs::File>>);

impl log::Log for FileLog {
    fn enabled(&self, m: &log::Metadata) -> bool {
        let level = LOG_LEVELS.iter().filter(|(p, _)| m.target().starts_with(p)).max_by_key(|(p, _)| p.len());
        level.is_some_and(|&(_, l)| m.level() <= l)
    }

    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        let line = format!("[{} {:<5} {}] {}\n", utc_time(now), r.level(), r.target(), r.args());
        if let Ok(mut f) = self.0.lock() {
            let _ = std::io::Write::write_all(&mut *f, line.as_bytes());
        }
    }

    fn flush(&self) {
        if let Ok(mut f) = self.0.lock() {
            let _ = std::io::Write::flush(&mut *f);
        }
    }
}

/// `2026-11-10T12:34:56.789Z` for a time since the Unix epoch.
fn utc_time(since_epoch: Duration) -> String {
    let secs = since_epoch.as_secs() as i64;
    // days since 1970 to a civil date: Howard Hinnant's civil_from_days
    let z = secs.div_euclid(86400) + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (d, m) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    let y = yoe + era * 400 + (m <= 2) as i64;
    let t = secs.rem_euclid(86400);
    let ms = since_epoch.subsec_millis();
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{ms:03}Z", t / 3600, t / 60 % 60, t % 60)
}

/// Always writes a detailed log (spot + librespot at debug) to ~/.cache/spot/log: the player
/// runs without a terminal of its own. The previous run's log is kept as log.1.
fn init_log() {
    let path = cache_dir().join("log");
    let _ = fs::create_dir_all(cache_dir());
    let _ = fs::rename(&path, path.with_extension("1"));
    let file = match fs::File::create(&path) {
        Ok(f) => f,
        Err(e) => return eprintln!("spot: can't open {}: {e}", path.display()),
    };
    let max = LOG_LEVELS.iter().map(|&(_, l)| l).max().unwrap_or(log::LevelFilter::Info);
    let logger = Box::leak(Box::new(FileLog(std::sync::Mutex::new(std::io::LineWriter::new(file)))));
    if log::set_logger(logger).is_ok() {
        log::set_max_level(max);
    }
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("panic: {info}");
        hook(info);
    }));
    log::info!("spot {} starting", env!("CARGO_PKG_VERSION"));
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        // the TUI, starting the player in the background first if it isn't running
        None => std::process::exit(terminal::launch()),
        Some("--headless") if args.len() == 1 => player(),
        Some("clean") if args.len() == 1 => std::process::exit(clean_cache()),
        Some("-v" | "--version") if args.len() == 1 => println!("spot {}", env!("CARGO_PKG_VERSION")),
        Some(c) if cli::COMMANDS.contains(&c) => std::process::exit(cli::client(args)),
        Some(a) => {
            let ok = matches!(a, "-h" | "--help" | "help");
            if ok { println!("{}", cli::USAGE) } else { eprintln!("spot: unknown argument '{a}'\n\n{}", cli::USAGE) }
            std::process::exit(if ok { 0 } else { 2 });
        }
    }
}

/// The background player (`spot --headless`, which `spot start` and plain `spot` run detached).
/// Returns only to quit.
fn player() {
    // One player at a time: a second would fight the first over the audio session and the
    // resume point. The lock goes when the process does, crash or not.
    let _ = fs::create_dir_all(cache_dir());
    let _lock = match fs::File::create(cache_dir().join("lock")) {
        Ok(f) => match f.try_lock() {
            Ok(()) => Some(f),
            Err(fs::TryLockError::WouldBlock) => {
                eprintln!("spot is already running. Open it with `spot`, or control it with `spot status`, `spot play <uri>` (see `spot help`).");
                std::process::exit(1);
            }
            Err(_) => None,
        },
        Err(_) => None,
    };
    init_log();
    let main_loop = std::thread::spawn(move || match start(false) {
        Ok(Some((app, rx))) => run(app, rx),
        Ok(None) => {}
        Err(e) => {
            log::error!("exiting: {e}");
            eprintln!("spot: {e}");
            std::process::exit(1);
        }
    });
    // macOS delivers media key / Now Playing commands on the main run loop.
    #[cfg(target_os = "macos")]
    unsafe {
        CFRunLoopRun()
    };
    let _ = main_loop.join();
}

/// macOS sends Control Center the supported commands from the main thread, once: attaching from
/// another, it snapshots them part way through and the rest stay greyed out for good.
#[cfg(target_os = "macos")]
fn on_main<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    let mut out = None;
    dispatch2::DispatchQueue::main().exec_sync(|| out = Some(f()));
    out.expect("ran on the main thread")
}

#[cfg(not(target_os = "macos"))]
fn on_main<T>(f: impl FnOnce() -> T) -> T {
    f()
}

/// The main loop: messages in, the attached terminal (if any) redrawn after each batch.
fn run(mut app: App, rx: mpsc::Receiver<Msg>) {
    while !app.quit {
        app.draw_terminal();
        // only wake for timers (and the clock while a terminal shows it); otherwise sleep until a message
        let timeout = app.next_wake(app.terminal.is_some()).map_or(Duration::from_secs(3600), |t| t.saturating_duration_since(Instant::now()));
        let first = match rx.recv_timeout(timeout) {
            Ok(m) => Some(m),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(_) => break,
        };
        for m in first.into_iter().chain(std::iter::from_fn(|| rx.try_recv().ok())) {
            app.on_msg(m);
        }
        app.tick();
    }
    app.shutdown()
}

/// Log in and start the player: the app and the channel it runs on. `setup_only` completes the
/// interactive first-run approvals in a terminal, then stops (None), before `spot` starts the
/// player in the background.
pub fn start(setup_only: bool) -> Result<Option<(App, mpsc::Receiver<Msg>)>, String> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    // This thread (the main loop's, for good) runs inside the runtime from here on, so whatever
    // the app starts finds it, after this returns too.
    let handle: &'static tokio::runtime::Handle = Box::leak(Box::new(rt.handle().clone()));
    std::mem::forget(handle.enter());

    settings::write_template();
    let (mut settings, problems) = settings::load();
    use_themes(std::mem::take(&mut settings.themes));
    problems.iter().for_each(|p| log::warn!("{p}"));

    let dir = cache_dir();
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // The whole cache stays under cache_gb in config.toml (SPOT_CACHE_GB overrides). Covers get a
    // tenth of it, least recently shown dropped first; played songs (kept encrypted, as streamed,
    // so replays start instantly and skip the network) fill what's left, least recently played
    // dropped first. 0 keeps no songs.
    let gb: f64 = ["SPOT_CACHE_GB", "SPOT_AUDIO_CACHE_GB"].iter().find_map(|k| std::env::var(k).ok()?.parse().ok()).unwrap_or(settings.cache_gb);
    let total = (gb * 1e9) as u64;
    set_art_budget(total / 10);
    prune_art();
    let others = disk_size(&dir).saturating_sub(disk_size(&dir.join("audio")));
    let audio = (gb > 0.0).then(|| dir.join("audio"));
    let limit = audio.as_ref().map(|_| total.saturating_sub(others));
    log::info!("cache: {:.0} MB besides songs, {:.0} MB left for songs", others as f64 / 1e6, limit.unwrap_or(0) as f64 / 1e6);
    let cache = Cache::new(Some(dir.clone()), Some(dir.clone()), audio, limit).map_err(|e| e.to_string())?;
    let refresh_path = dir.join("refresh_token");

    let redirect = std::env::var("SPOT_REDIRECT").unwrap_or_else(|_| REDIRECT.into());
    let client_id = client_id(&redirect, settings.client_id.take())?;
    let oauth = OAuthClientBuilder::new(&client_id, &redirect, SCOPES.to_vec())
        .open_in_browser()
        .build()
        .map_err(|e| e.to_string())?;

    // Web API token: refreshed lazily from the saved refresh token; browser login only the first time.
    let saved = fs::read_to_string(&refresh_path).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    // a terminal can log in; `spot start` can't
    let interactive = std::io::stdin().is_terminal();
    let tok = match saved {
        Some(refresh) => Tok { access: String::new(), refresh, exp: Instant::now(), failed_login: None },
        None if !interactive => return Err(NOT_SET_UP.into()),
        None => {
            println!("Log in to Spotify in your browser (opening now)…");
            let t = rt.block_on(oauth.get_access_token_async()).map_err(|e| format!("login: {e}"))?;
            if !t.refresh_token.is_empty() {
                save_refresh(&refresh_path, &t.refresh_token);
            }
            Tok { access: t.access_token, refresh: t.refresh_token, exp: t.expires_at, failed_login: None }
        }
    };

    // The audio session connects in the background unless this is the first run.
    let first_session = if cache.credentials().is_none() {
        if !interactive {
            return Err(NOT_SET_UP.into());
        }
        println!("One more browser approval for the audio session…");
        Some(rt.block_on(approve_session(&cache))?)
    } else {
        None
    };
    let session = first_session.clone().unwrap_or_else(|| Session::new(SessionConfig::default(), Some(cache.clone())));

    let (tx, rx) = mpsc::channel::<Msg>();
    let http = reqwest::Client::new();
    let lastfm = lastfm_setup(&rt, &http, &redirect, tx.clone());
    let api = Arc::new(Api {
        http,
        oauth: Arc::new(oauth),
        redirect,
        tok: tokio::sync::Mutex::new(tok),
        path: refresh_path,
        tx: tx.clone(),
    });

    if setup_only {
        return Ok(None);
    }
    if let Some(fm) = lastfm.clone() {
        rt.spawn(async move { fm.drain().await }); // whatever didn't get sent last time
    }
    ctl::serve(tx.clone());

    let mixer = mixer::find(None).unwrap()(MixerConfig::default()).map_err(|e| e.to_string())?;
    let vol = cache.volume().unwrap_or(u16::MAX);
    mixer.set_volume(vol);
    let backend = audio_backend::find(None).ok_or("no audio backend")?;
    let player = Player::new(
        // 320 is the most librespot can play; no FLAC
        PlayerConfig {
            bitrate: match settings.quality {
                96 => Bitrate::Bitrate96,
                160 => Bitrate::Bitrate160,
                _ => Bitrate::Bitrate320,
            },
            normalisation: settings.normalize,
            ..Default::default()
        },
        session.clone(),
        mixer.get_soft_volume(),
        move || backend(None, AudioFormat::default()),
    );

    let (sess, _) = tokio::sync::watch::channel(first_session.clone());
    if first_session.is_none() {
        rt.spawn(start_session(session, cache.clone(), tx.clone()));
    }
    rt.spawn(load_playlists(api.clone(), tx.clone()));
    rt.spawn(load_shows(api.clone(), tx.clone()));

    let theme = fs::read_to_string(dir.join("theme")).unwrap_or_default();
    if let Some(i) = families().iter().position(|(name, ..)| name == theme.trim()) {
        FAMILY.store(i, std::sync::atomic::Ordering::Relaxed);
    }

    let mut events = player.get_player_event_channel();
    let ptx = tx.clone();
    rt.spawn(async move {
        while let Some(ev) = events.recv().await {
            if ptx.send(Msg::Player(ev)).is_err() {
                break;
            }
        }
    });

    // quit cleanly (saving the resume point) when killed
    let stx = tx.clone();
    rt.spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};
        let (Ok(mut term), Ok(mut int)) = (signal(SignalKind::terminate()), signal(SignalKind::interrupt())) else { return };
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
        let _ = stx.send(Msg::Quit);
    });

    let mtx = tx.clone();
    let media = on_main(move || {
        MediaControls::attach(move |ev| {
            let _ = mtx.send(Msg::Media(ev));
        })
    });

    let mut app = App {
        rt,
        api,
        tx,
        cache,
        sess,
        player,
        mixer,
        media,
        playlists: read_playlists(),
        pinned: read_pinned(),
        pl_state: Sel::with(Some(0)),
        shows: podcast_home().into_iter().chain(read_links(&shows_path())).collect(),
        recent: read_links(&recent_path()),
        sh_state: Sel::with(Some(0)),
        podcasts: false,
        view: View { loading: false, ..View::new(0, String::new(), "Tracks".into(), Kind::Tracks) },
        view_seq: 0,
        history: vec![],
        ahead: vec![],
        grid_cols: std::cell::Cell::new(1),
        releases: Releases::All,
        tr_state: Sel::default(),
        tr_off: 0,
        side_off: 0,
        focus: Focus::Playlists,
        input: None,
        live_search: false,
        search_at: None,
        search_task: None,
        find: None,
        matches: vec![],
        sort: Sort::from_name(&fs::read_to_string(cache_dir().join("sort")).unwrap_or_default()),
        search_kind: None,
        shuffle: false,
        repeat: Repeat::Off,
        fails: 0,
        queue: None,
        upnext: vec![],
        queued: None,
        cur: None,
        episode: None,
        continuing: read_tracks(&continuing_path()),
        progress: read_progress(),
        loaded: false,
        load_at: None,
        playing: false,
        pos_ms: 0,
        pos_at: None,
        status: problems.join("; "),
        note: None,
        retried: false,
        vol,
        settings,
        want_play: false,
        fade_gen: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        vol_until: None,
        help: false,
        lastfm,
        listen: None,
        plays: 0,
        heard: false,
        finished: false,
        ctl: ctl::State::default(),
        quit: false,
        terminal: None,
        art: Default::default(),
        act_on_load: None,
        shuffle_on_load: None,
    };

    if let Some(last) = read_last() {
        app.restore(last);
    }
    app.restore_upnext();
    app.open_playlist(0);
    Ok(Some((app, rx)))
}

impl App {
    /// Run the timers that are due: popups and the volume meter going away, a skip settling,
    /// typing stopping. True when that changed what the TUI shows.
    pub fn tick(&mut self) -> bool {
        let now = Instant::now();
        let mut changed = false;
        if self.note.as_ref().is_some_and(|(_, until)| now >= *until) {
            self.note = None;
            changed = true;
        }
        if self.vol_until.is_some_and(|until| now >= until) {
            self.vol_until = None;
            changed = true;
        }
        if self.load_at.is_some_and(|until| now >= until) {
            self.load_at = None;
            self.load_cur();
            changed = true;
        }
        if self.search_at.is_some_and(|until| now >= until) {
            self.search_at = None;
            self.run_search();
            changed = true;
        }
        changed |= self.note_heard();
        self.tick_watchers();
        changed
    }

    /// When `tick` next needs to run: the earliest timer, and once a second while playing if
    /// `every_second` (a clock on show, or `spot watch`ers waiting for a song to be ending).
    pub fn next_wake(&self, every_second: bool) -> Option<Instant> {
        let mut next = [self.note.as_ref().map(|n| n.1), self.vol_until, self.load_at, self.search_at].into_iter().flatten().min();
        // the moment the song playing has been heard long enough to be recently played
        if self.playing && !self.heard {
            let at = Instant::now() + Duration::from_millis(HEARD_MS.saturating_sub(self.position()) + 5);
            next = Some(next.map_or(at, |n| n.min(at)));
        }
        if self.playing && (every_second || !self.ctl.watchers().is_empty()) {
            // on the clock's next whole second
            let into = self.position() % 1000;
            let tick = Instant::now() + Duration::from_millis(1000 - into + 5);
            next = Some(next.map_or(tick, |n| n.min(tick)));
        }
        next
    }

    /// Stop: save where it was, send the last scrobble, and exit.
    pub fn shutdown(&mut self) -> ! {
        ctl::unlink();
        self.detach_terminal();
        self.save_last();
        self.player.stop();
        self.end_listen();
        if let Some(fm) = self.lastfm.clone() {
            // give the last scrobble a moment to go out; otherwise it's sent next start
            let _ = self.rt.block_on(tokio::time::timeout(Duration::from_secs(3), async move { fm.drain().await }));
        }
        std::process::exit(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_times() {
        let at = |s: u64, ms: u64| utc_time(Duration::from_millis(s * 1000 + ms));
        assert_eq!(at(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(at(951_782_400, 5), "2000-02-29T00:00:00.005Z");
        assert_eq!(at(1_000_000_000, 0), "2001-09-09T01:46:40.000Z");
        assert_eq!(at(1_793_836_799, 999), "2026-11-04T23:59:59.999Z");
        assert_eq!(at(4_107_542_400, 0), "2100-03-01T00:00:00.000Z");
    }
}
