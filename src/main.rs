// spot: a tiny Spotify player. librespot for audio, the Web API for library and search, Rune for
// the window. This file: the command line's way in, startup (logins, the player, the threads
// feeding it) and shutdown. The player lives on the main thread with the window (gui.rs), which
// keeps running with the window closed; the command line talks to it over a socket (ctl.rs).
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
use serde_json::Value;
use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig, SeekDirection};

mod app;
mod art;
mod artist;
mod fmt;
mod gui;
mod auth;
mod cache;
mod cli;
mod ctl;
mod keys;
mod lastfm;
mod lists;
mod metadata;
mod model;
mod playback;
mod settings;
mod theme;
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
use theme::*;
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
const NOT_SET_UP: &str = "not set up yet: run `spot` once in a terminal, or open Spot.app, to log in";

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

/// Always writes a detailed log (spot + librespot at debug) to ~/.cache/spot/log: the player
/// runs without a terminal. The previous run's log is kept as log.1.
fn init_log() {
    let spec = "info,spot=debug,librespot=debug,symphonia_bundle_mp3=error";
    let path = cache_dir().join("log");
    let _ = fs::create_dir_all(cache_dir());
    let _ = fs::rename(&path, path.with_extension("1"));
    let file = match fs::File::create(&path) {
        Ok(f) => f,
        Err(e) => return eprintln!("spot: can't open {}: {e}", path.display()),
    };
    env_logger::Builder::new()
        .parse_filters(spec)
        .target(env_logger::Target::Pipe(Box::new(file)))
        .write_style(env_logger::WriteStyle::Never)
        .format_timestamp_millis()
        .init();
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("panic: {info}");
        hook(info);
    }));
    log::info!("spot {} starting, log filter {spec:?}", env!("CARGO_PKG_VERSION"));
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        // from a terminal: open the window (starting spot if needed, after any first-run setup);
        // from Finder or `open`: this is the app
        None if std::io::stdin().is_terminal() => std::process::exit(cli::open()),
        None => player(true),
        Some("--app") if args.len() <= 2 && args.get(1).is_none_or(|a| a == "--show") => player(args.len() == 2),
        Some("-v" | "--version") if args.len() == 1 => println!("spot {}", env!("CARGO_PKG_VERSION")),
        Some(c) if cli::COMMANDS.contains(&c) => std::process::exit(cli::client(args)),
        Some(a) => {
            let ok = matches!(a, "-h" | "--help" | "help");
            if ok { println!("{}", cli::USAGE) } else { eprintln!("spot: unknown argument '{a}'\n\n{}", cli::USAGE) }
            std::process::exit(if ok { 0 } else { 2 });
        }
    }
}

/// The player, with its window (shown now if `show`). Returns only to quit.
fn player(show: bool) {
    // One player at a time: a second would fight the first over the audio session and the
    // resume point. The lock goes when the process does, crash or not.
    let _ = fs::create_dir_all(cache_dir());
    let lock = match fs::File::create(cache_dir().join("lock")) {
        Ok(f) => match f.try_lock() {
            Ok(()) => Some(f),
            Err(fs::TryLockError::WouldBlock) => {
                // it's running: bring its window up instead
                if show {
                    let _ = cli::send(&["show"]);
                } else {
                    eprintln!("spot is already running. Control it with `spot status`, `spot play …` (see `spot help`).");
                }
                std::process::exit(if show { 0 } else { 1 });
            }
            Err(_) => None,
        },
        Err(_) => None,
    };
    init_log();
    rune::prewarm_gpu();
    match start(false, show) {
        Ok(Some((app, rx))) => gui::run(app, rx, show, lock),
        Ok(None) => {}
        Err(e) => {
            log::error!("exiting: {e}");
            eprintln!("spot: {e}");
            // opened from Finder there's no terminal to say it in
            if !std::io::stderr().is_terminal() {
                gui::problem(e);
            }
            std::process::exit(1);
        }
    }
}

/// Log in and start the player: the app and the channel it runs on. `setup_only` completes the
/// interactive first-run approvals in a terminal, then stops (None), before `spot` starts the app.
/// Call it on the main thread: macOS sends Control Center the supported commands from there, once;
/// attaching media controls from another, it snapshots them part way and the rest stay greyed out.
pub fn start(setup_only: bool, window: bool) -> Result<Option<(App, mpsc::Receiver<Msg>)>, String> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    // This thread (the window's, for good) runs inside the runtime from here on, so whatever the
    // app starts finds it, after this returns too.
    let handle: &'static tokio::runtime::Handle = Box::leak(Box::new(rt.handle().clone()));
    std::mem::forget(handle.enter());

    settings::write_template();
    let (mut settings, problems) = settings::load();
    problems.iter().for_each(|p| log::warn!("{p}"));

    let dir = cache_dir();
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // Played songs are kept (encrypted, as streamed) so replays start instantly and skip the
    // network, least recently played dropped first past the limit: audio_cache_gb in config.toml (SPOT_AUDIO_CACHE_GB overrides), 0 = off.
    let audio_gb: f64 = std::env::var("SPOT_AUDIO_CACHE_GB").ok().and_then(|v| v.parse().ok()).unwrap_or(settings.audio_cache_gb);
    let audio = (audio_gb > 0.0).then(|| dir.join("audio"));
    let limit = audio.as_ref().map(|_| (audio_gb * 1e9) as u64);
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
    // a terminal, or the window (opened by hand, so a browser tab is expected), can log in;
    // `spot start` and the login item can't
    let interactive = std::io::stdin().is_terminal() || window;
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

    let media = MediaControls::new(PlatformConfig { display_name: "spot", dbus_name: "spot", hwnd: None })
        .ok()
        .and_then(|mut m| {
            let mtx = tx.clone();
            m.attach(move |ev| {
                let _ = mtx.send(Msg::Media(ev));
            })
            .ok()?;
            Some(m)
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
        sh_state: Sel::with(Some(0)),
        podcasts: false,
        view: View { loading: false, ..View::new(0, String::new(), "Tracks".into(), Kind::Tracks) },
        view_seq: 0,
        history: vec![],
        grid_cols: std::cell::Cell::new(4),
        releases: Releases::All,
        tr_state: Sel::default(),
        focus: Focus::Playlists,
        input: None,
        live_search: false,
        search_at: None,
        search_task: None,
        find: None,
        matches: vec![],
        sort: Sort::from_name(&fs::read_to_string(cache_dir().join("sort")).unwrap_or_default()),
        search_kind: Kind::Tracks,
        shuffle: false,
        repeat: Repeat::Off,
        fails: 0,
        queue: None,
        upnext: vec![],
        queued: None,
        cur: None,
        episode: None,
        continuing: read_tracks(&continuing_path()),
        recent: read_links(&recent_path()),
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
        finished: false,
        ctl: ctl::State::default(),
        quit: false,
        art: Default::default(),
        show: false,
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
    /// typing stopping. True when that changed what the window shows.
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
        self.tick_watchers();
        changed
    }

    /// When `tick` next needs to run: the earliest timer, and once a second while playing if
    /// `every_second` (a clock on show, or `spot watch`ers waiting for a song to be ending).
    pub fn next_wake(&self, every_second: bool) -> Option<Instant> {
        let mut next = [self.note.as_ref().map(|n| n.1), self.vol_until, self.load_at, self.search_at].into_iter().flatten().min();
        if self.playing && (every_second || !self.ctl.watchers().is_empty()) {
            // on the clock's next whole second
            let into = self.position() % 1000;
            let tick = Instant::now() + Duration::from_millis(1000 - into + 5);
            next = Some(next.map_or(tick, |n| n.min(tick)));
        }
        next
    }

    /// Quit, and open the window again once this process is gone (it holds the player's lock):
    /// for settings that only apply at start, and logging in again.
    pub fn restart(&mut self) -> ! {
        use std::os::unix::process::CommandExt;
        if let Ok(exe) = std::env::current_exe() {
            let r = std::process::Command::new("/bin/sh")
                .args(["-c", "while kill -0 \"$0\" 2>/dev/null; do sleep 0.1; done; exec \"$1\" --app --show"])
                .arg(std::process::id().to_string())
                .arg(exe)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .process_group(0) // outlives this one, and a login item's job
                .spawn();
            if let Err(e) = r {
                log::warn!("restart: {e}");
            }
        }
        self.shutdown()
    }

    /// Stop: save where it was, send the last scrobble, and exit.
    pub fn shutdown(&mut self) -> ! {
        ctl::unlink();
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
