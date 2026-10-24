// spot: a tiny Spotify TUI. librespot for audio, Web API for library/search, ratatui for UI.
// This file: startup (logins, the player, the threads feeding the main loop) and the main loop.
// The imports here are shared by every module (`use crate::*`).

use std::{
    collections::HashMap,
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
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    layout::{Constraint, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Cell, List, ListItem, ListState, Paragraph, Row, Table, TableState},
};
use serde_json::Value;
use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig, SeekDirection};

mod app;
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
mod theme;
mod ui;
mod views;
mod webapi;

use app::*;
use auth::*;
use cache::*;
use keys::*;
use lastfm::*;
use lists::*;
use metadata::*;
use model::*;
use theme::*;
use ui::*;
use webapi::*;

// Each user brings their own Spotify developer app (the shared librespot client id is permanently
// rate limited): SPOT_CLIENT_ID, else ~/.config/spot/client_id, else asked for on first run.
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

/// Running without the window (`spot --headless`): nothing draws or reads the terminal.
static HEADLESS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn restore_terminal() {
    if HEADLESS.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    use std::io::Write;
    let _ = std::io::stdout().write_all(b"\x1b[?2031l");
    ratatui::restore();
    let _ = ratatui::crossterm::execute!(std::io::stdout(), ratatui::crossterm::cursor::Show);
}

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

/// Always writes a detailed log (spot + librespot at debug) to ~/.cache/spot/log, since the TUI
/// owns the terminal. The previous run's log is kept as log.1.
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
    let headless = args.first().is_some_and(|a| a == "--headless");
    match args.first().map(String::as_str) {
        None => {}
        Some("--headless") if args.len() == 1 => {}
        Some("clean") => return clean_cache(),
        Some(c) if cli::COMMANDS.contains(&c) => std::process::exit(cli::client(args)),
        Some(a) => {
            let ok = matches!(a, "-h" | "--help" | "help");
            if ok { println!("{}", cli::USAGE) } else { eprintln!("spot: unknown argument '{a}'\n\n{}", cli::USAGE) }
            std::process::exit(if ok { 0 } else { 2 });
        }
    }
    // One player at a time: a second would fight the first over the audio session and the
    // resume point. The lock goes when the process does, crash or not.
    let _ = fs::create_dir_all(cache_dir());
    let _lock = match fs::File::create(cache_dir().join("lock")) {
        Ok(f) => match f.try_lock() {
            Ok(()) => Some(f),
            Err(fs::TryLockError::WouldBlock) => {
                eprintln!("spot is already running (in another terminal, or headless). Control it from here with `spot status`, `spot play …` (see `spot help`).");
                std::process::exit(1);
            }
            Err(_) => None,
        },
        Err(_) => None,
    };
    init_log();
    HEADLESS.store(headless, std::sync::atomic::Ordering::Relaxed);
    let ui = std::thread::spawn(move || {
        if let Err(e) = run(headless) {
            log::error!("exiting: {e}");
            restore_terminal();
            eprintln!("spot: {e}");
            std::process::exit(1);
        }
    });
    // macOS delivers media key / Now Playing commands on the main run loop.
    #[cfg(target_os = "macos")]
    unsafe {
        CFRunLoopRun()
    };
    let _ = ui.join();
}

fn run(headless: bool) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let _guard = rt.enter();

    let dir = cache_dir();
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // Played songs are kept (encrypted, as streamed) so replays start instantly and skip the
    // network, least recently played dropped first past the limit: 1 GB, SPOT_AUDIO_CACHE_GB to change, 0 = off.
    let audio_gb: f64 = std::env::var("SPOT_AUDIO_CACHE_GB").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0);
    let audio = (audio_gb > 0.0).then(|| dir.join("audio"));
    let limit = audio.as_ref().map(|_| (audio_gb * 1e9) as u64);
    let cache = Cache::new(Some(dir.clone()), Some(dir.clone()), audio, limit).map_err(|e| e.to_string())?;
    let refresh_path = dir.join("refresh_token");

    let redirect = std::env::var("SPOT_REDIRECT").unwrap_or_else(|_| REDIRECT.into());
    let client_id = client_id(&redirect)?;
    let oauth = OAuthClientBuilder::new(&client_id, &redirect, SCOPES.to_vec())
        .open_in_browser()
        .build()
        .map_err(|e| e.to_string())?;

    // Web API token: refreshed lazily from the saved refresh token; browser login only the first time.
    let saved = fs::read_to_string(&refresh_path).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
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
    ctl::serve(tx.clone());
    let http = reqwest::Client::new();
    let lastfm = lastfm_setup(&rt, &http, &redirect, tx.clone());
    if let Some(fm) = lastfm.clone() {
        rt.spawn(async move { fm.drain().await }); // whatever didn't get sent last time
    }
    let api = Arc::new(Api {
        http,
        oauth: Arc::new(oauth),
        redirect,
        tok: tokio::sync::Mutex::new(tok),
        path: refresh_path,
        tx: tx.clone(),
    });

    let mixer = mixer::find(None).unwrap()(MixerConfig::default()).map_err(|e| e.to_string())?;
    let vol = cache.volume().unwrap_or(u16::MAX);
    mixer.set_volume(vol);
    let backend = audio_backend::find(None).ok_or("no audio backend")?;
    let player = Player::new(
        PlayerConfig { bitrate: Bitrate::Bitrate320, ..Default::default() } /* max librespot can play; no FLAC */,
        session.clone(),
        mixer.get_soft_volume(),
        move || backend(None, AudioFormat::default()),
    );

    let (sess, _) = tokio::sync::watch::channel(first_session.clone());
    if first_session.is_none() {
        rt.spawn(start_session(session, cache.clone(), tx.clone()));
    }
    rt.spawn(load_playlists(api.clone(), tx.clone()));

    // probe light/dark before anything reads stdin
    if !headless {
        probe_terminal();
    }
    let theme = fs::read_to_string(dir.join("theme")).unwrap_or_default();
    if let Some(i) = FAMILIES.iter().position(|(name, ..)| *name == theme.trim()) {
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

    let ktx = tx.clone();
    let rtx = tx.clone();
    if !headless {
        std::thread::spawn(move || read_input(ktx));
    }
    std::thread::spawn(move || {
        if headless {
            return;
        }
        // we read stdin ourselves, so nothing reports SIGWINCH; poll the size instead
        let mut last = ratatui::crossterm::terminal::size().ok();
        loop {
            std::thread::sleep(Duration::from_millis(200));
            let now = ratatui::crossterm::terminal::size().ok();
            if now != last {
                last = now;
                if rtx.send(Msg::Redraw).is_err() {
                    break;
                }
            }
        }
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
        pl_state: ListState::default().with_selected(Some(0)),
        view: View { loading: false, ..View::new(0, String::new(), "Tracks".into(), Kind::Tracks) },
        view_seq: 0,
        history: vec![],
        artist_songs: true,
        releases: Releases::All,
        tr_state: TableState::default(),
        focus: Focus::Playlists,
        input: None,
        find: None,
        matches: vec![],
        search_kind: Kind::Tracks,
        shuffle: false,
        repeat: Repeat::Off,
        fails: 0,
        queue: None,
        upnext: vec![],
        queued: None,
        cur: None,
        loaded: false,
        load_at: None,
        playing: false,
        pos_ms: 0,
        pos_at: None,
        status: String::new(),
        note: None,
        retried: false,
        vol,
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
    };

    if let Some(last) = read_last() {
        app.restore(last);
    }
    app.restore_upnext();
    app.open_playlist(0);

    let mut term = (!headless).then(ratatui::init);
    if !headless && std::env::var_os("SPOT_THEME").is_none() {
        // color scheme reports: subscribe to changes (2031) and ask for the current one (996)
        use std::io::Write;
        let _ = std::io::stdout().write_all(b"\x1b[?2031h\x1b[?996n").and_then(|_| std::io::stdout().flush());
    }
    while !app.quit {
        if let Some(term) = &mut term {
            term.draw(|f| draw(f, &mut app)).map_err(|e| e.to_string())?;
        }
        // Only wake up once a second while playing (progress clock); otherwise sleep until input.
        let mut timeout = if app.playing { Duration::from_secs(1) } else { Duration::from_secs(3600) };
        // wake up in time to take the popup / volume meter down
        for until in [app.note.as_ref().map(|n| n.1), app.vol_until, app.load_at].into_iter().flatten() {
            timeout = timeout.min(until.saturating_duration_since(Instant::now()));
        }
        let first = match rx.recv_timeout(timeout) {
            Ok(m) => Some(m),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(_) => break,
        };
        if app.note.as_ref().is_some_and(|(_, until)| Instant::now() >= *until) {
            app.note = None;
        }
        if app.vol_until.is_some_and(|until| Instant::now() >= until) {
            app.vol_until = None;
        }
        if app.load_at.is_some_and(|until| Instant::now() >= until) {
            app.load_at = None;
            app.load_cur();
        }
        for m in first.into_iter().chain(std::iter::from_fn(|| rx.try_recv().ok())) {
            app.on_msg(m);
        }
        app.tick_watchers();
    }
    ctl::unlink();
    restore_terminal();
    app.save_last();
    app.player.stop();
    app.end_listen();
    if let Some(fm) = app.lastfm.clone() {
        // give the last scrobble a moment to go out; otherwise it's sent next start
        let _ = app.rt.block_on(tokio::time::timeout(Duration::from_secs(3), async move { fm.drain().await }));
    }
    std::process::exit(0);
}
