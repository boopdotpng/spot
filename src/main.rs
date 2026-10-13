// spot: a tiny Spotify TUI. librespot for audio, Web API for library/search, ratatui for UI.

use std::{
    collections::HashMap,
    fs,
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

// ---------- data ----------

#[derive(Clone)]
struct Track {
    uri: String,
    name: String,
    artists: String,
    album: String,
    cover: Option<String>,
    ms: u64,
}

/// An album or artist search result.
#[derive(Clone)]
struct Link {
    uri: String,
    name: String,
    sub: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Tracks,
    Albums,
    Artists,
}

impl Kind {
    fn next(self) -> Self {
        match self {
            Kind::Tracks => Kind::Albums,
            Kind::Albums => Kind::Artists,
            Kind::Artists => Kind::Tracks,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Kind::Tracks => "songs",
            Kind::Albums => "albums",
            Kind::Artists => "artists",
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Repeat {
    Off,
    All, // loop the list (reshuffled each pass when shuffling)
    One, // replay the current song
}

#[derive(Clone)]
struct Playlist {
    id: Option<String>, // None = Liked Songs
    name: String,
}

impl Playlist {
    fn key(&self) -> String {
        self.id.clone().unwrap_or_else(|| "liked".into())
    }
}

enum Msg {
    Key(KeyEvent),
    Redraw,
    Scheme(bool), // terminal switched color scheme; true = light
    Player(PlayerEvent),
    Playlists(Vec<Playlist>),
    Tracks { view: u64, tracks: Vec<Track>, done: bool },
    Links { view: u64, links: Vec<Link>, done: bool },
    Error(String),
    Media(MediaControlEvent),
}

// ---------- Web API ----------

struct Tok {
    access: String,
    refresh: String,
    exp: Instant,
}

struct Api {
    http: reqwest::Client,
    oauth: OAuthClient,
    tok: tokio::sync::Mutex<Tok>,
    path: PathBuf,
}

impl Api {
    async fn token(&self, force: bool) -> Result<String, String> {
        let mut t = self.tok.lock().await;
        if force || Instant::now() + Duration::from_secs(60) >= t.exp {
            let n = self
                .oauth
                .refresh_token_async(&t.refresh)
                .await
                .map_err(|e| format!("token refresh: {e}"))?;
            if !n.refresh_token.is_empty() && n.refresh_token != t.refresh {
                t.refresh = n.refresh_token;
                save_refresh(&self.path, &t.refresh);
            }
            t.access = n.access_token;
            t.exp = n.expires_at;
        }
        Ok(t.access.clone())
    }

    async fn get(&self, url: &str) -> Result<Value, String> {
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

fn parse_track(v: &Value) -> Option<Track> {
    let uri = v["uri"].as_str()?;
    if !uri.starts_with("spotify:track:") {
        return None;
    }
    let artists = v["artists"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x["name"].as_str()).collect::<Vec<_>>().join(", "))
        .unwrap_or_default();
    Some(Track {
        uri: uri.into(),
        name: v["name"].as_str().unwrap_or("?").into(),
        artists,
        album: v["album"]["name"].as_str().unwrap_or("").into(),
        cover: v["album"]["images"].as_array().and_then(|i| i.first()?["url"].as_str()).map(String::from),
        ms: v["duration_ms"].as_u64().unwrap_or(0),
    })
}

async fn load_playlists(api: Arc<Api>, tx: mpsc::Sender<Msg>) {
    let mut out = vec![Playlist { id: None, name: "♥ Liked Songs".into() }];
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
                let _ = tx.send(Msg::Error(format!("playlists: {e}")));
                break;
            }
        }
    }
    let _ = tx.send(Msg::Playlists(out));
}

async fn load_tracks(api: Arc<Api>, session: Session, tx: mpsc::Sender<Msg>, view: u64, pl: Playlist) {
    let res = match &pl.id {
        Some(id) => playlist_tracks(&session, id, &tx, view).await,
        None => liked_tracks(&api, &tx, view).await,
    };
    if let Err(e) = res {
        let _ = tx.send(Msg::Error(e));
    }
    let _ = tx.send(Msg::Tracks { view, tracks: vec![], done: true });
}

async fn liked_tracks(api: &Api, tx: &mpsc::Sender<Msg>, view: u64) -> Result<(), String> {
    let mut url = Some(format!("{API}/me/tracks?limit=50"));
    while let Some(u) = url {
        let v = api.get(&u).await?;
        let tracks = v["items"].as_array().into_iter().flatten().filter_map(|it| parse_track(&it["track"])).collect();
        url = v["next"].as_str().map(String::from);
        let _ = tx.send(Msg::Tracks { view, tracks, done: false });
    }
    Ok(())
}

// Playlists, albums and artists are read from Spotify's internal playlist + extended-metadata
// endpoints (what the official client uses): dev-mode Web API apps can't read playlists you
// don't own, and batch lookups there were removed.
use librespot_protocol::{
    extension_kind::ExtensionKind,
    metadata::{Album as PbAlbum, Artist as PbArtist, Track as PbTrack},
};

async fn batch_meta<M: protobuf::Message>(
    session: &Session,
    uris: &[String],
    kind: ExtensionKind,
) -> Result<HashMap<String, M>, String> {
    use librespot_protocol::extended_metadata::{BatchedEntityRequest, EntityRequest, ExtensionQuery};
    use protobuf::EnumOrUnknown;

    let mut out = HashMap::new();
    for chunk in uris.chunks(200) {
        let req = BatchedEntityRequest {
            entity_request: chunk
                .iter()
                .map(|u| EntityRequest {
                    entity_uri: u.clone(),
                    query: vec![ExtensionQuery { extension_kind: EnumOrUnknown::new(kind), ..Default::default() }],
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let res = session.spclient().get_extended_metadata(req).await.map_err(|e| format!("metadata: {e}"))?;
        for arr in &res.extended_metadata {
            for d in &arr.extension_data {
                if let Some(any) = d.extension_data.as_ref()
                    && let Ok(m) = M::parse_from_bytes(&any.value)
                {
                    out.insert(d.entity_uri.clone(), m);
                }
            }
        }
    }
    Ok(out)
}

fn gid_uri(kind: &str, gid: &[u8]) -> Option<String> {
    let b62 = librespot_core::SpotifyId::from_raw(gid).ok()?.to_base62().ok()?;
    Some(format!("spotify:{kind}:{b62}"))
}

fn pb_track(uri: &str, t: &PbTrack) -> Track {
    Track {
        uri: uri.into(),
        name: t.name().to_string(),
        artists: t.artist.iter().map(|a| a.name()).collect::<Vec<_>>().join(", "),
        album: t.album.name().to_string(),
        cover: t.album.cover_group.image.iter().max_by_key(|i| i.width()).map(|i| {
            let hex: String = i.file_id().iter().map(|b| format!("{b:02x}")).collect();
            format!("https://i.scdn.co/image/{hex}")
        }),
        ms: t.duration() as u64,
    }
}

/// Resolve track uris to names in batches, streaming them to the view in order.
/// `dedupe` drops repeats of the same song (single + album versions in a discography).
async fn send_tracks(session: &Session, uris: &[String], dedupe: bool, tx: &mpsc::Sender<Msg>, view: u64) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for chunk in uris.chunks(200) {
        let mut found = batch_meta::<PbTrack>(session, chunk, ExtensionKind::TRACK_V4).await?;
        let tracks = chunk
            .iter()
            .filter_map(|u| found.remove(u).map(|t| pb_track(u, &t)))
            .filter(|t| !dedupe || seen.insert((t.name.to_lowercase(), t.artists.to_lowercase())))
            .collect();
        let _ = tx.send(Msg::Tracks { view, tracks, done: false });
    }
    Ok(())
}

async fn playlist_tracks(session: &Session, id: &str, tx: &mpsc::Sender<Msg>, view: u64) -> Result<(), String> {
    use librespot_metadata::Metadata;
    let uri = SpotifyUri::from_uri(&format!("spotify:playlist:{id}")).map_err(|e| e.to_string())?;
    let pl = librespot_metadata::Playlist::get(session, &uri).await.map_err(|e| format!("playlist: {e}"))?;
    let uris: Vec<String> = pl
        .tracks()
        .filter(|u| matches!(u, SpotifyUri::Track { .. }))
        .filter_map(|u| u.to_uri().ok())
        .collect();
    send_tracks(session, &uris, false, tx, view).await
}

async fn album_track_uris(session: &Session, albums: &[String]) -> Result<Vec<String>, String> {
    let found = batch_meta::<PbAlbum>(session, albums, ExtensionKind::ALBUM_V4).await?;
    Ok(albums
        .iter()
        .filter_map(|a| found.get(a))
        .flat_map(|a| a.disc.iter().flat_map(|d| d.track.iter()))
        .filter_map(|t| gid_uri("track", t.gid()))
        .collect())
}

/// Album or artist (albums + singles/EPs, newest first) as a flat track list.
async fn link_tracks(session: Session, tx: mpsc::Sender<Msg>, view: u64, uri: String) {
    let res = async {
        let (albums, dedupe) = if uri.starts_with("spotify:artist:") {
            let a = batch_meta::<PbArtist>(&session, std::slice::from_ref(&uri), ExtensionKind::ARTIST_V4)
                .await?
                .remove(&uri)
                .ok_or("artist not found")?;
            // each group lists regional/edition variants of one release; the first is the canonical one
            let albums: Vec<String> = a
                .album_group
                .iter()
                .chain(&a.single_group)
                .filter_map(|g| g.album.first())
                .filter_map(|al| gid_uri("album", al.gid()))
                .collect();
            (albums, true)
        } else {
            (vec![uri.clone()], false)
        };
        let tracks = album_track_uris(&session, &albums).await?;
        send_tracks(&session, &tracks, dedupe, &tx, view).await
    }
    .await;
    if let Err(e) = res {
        let _ = tx.send(Msg::Error(e));
    }
    let _ = tx.send(Msg::Tracks { view, tracks: vec![], done: true });
}

fn parse_link(kind: Kind, v: &Value) -> Option<Link> {
    let names = |a: &Value| a.as_array().map(|a| a.iter().filter_map(|x| x["name"].as_str()).collect::<Vec<_>>().join(", "));
    let sub = match kind {
        Kind::Albums => {
            let year = v["release_date"].as_str().unwrap_or("").get(..4).unwrap_or("");
            let kind = v["album_type"].as_str().unwrap_or("");
            format!("{} · {year} {kind}", names(&v["artists"]).unwrap_or_default())
        }
        _ => v["genres"].as_array().map(|g| g.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default(),
    };
    Some(Link { uri: v["uri"].as_str()?.into(), name: v["name"].as_str().unwrap_or("?").into(), sub })
}

// Dev-mode apps get max 10 results per search call, so page through a few.
async fn search(api: Arc<Api>, tx: mpsc::Sender<Msg>, view: u64, q: String, kind: Kind) {
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

// ---------- auth / session ----------

fn cache_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache"));
    base.join("spot")
}

fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("spot")
}

/// The user's Spotify app client id: env, then config file, then a one-time prompt that saves it.
fn client_id(redirect: &str) -> Result<String, String> {
    if let Ok(id) = std::env::var("SPOT_CLIENT_ID") {
        return Ok(id);
    }
    let path = config_dir().join("client_id");
    if let Some(id) = fs::read_to_string(&path).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        return Ok(id);
    }
    println!("spot needs your own Spotify app (one-time setup, requires Premium):");
    println!("  1. Open https://developer.spotify.com/dashboard and create an app.");
    println!("  2. Add this redirect URI: {redirect}");
    println!("  3. Tick \"Web API\" and save.");
    println!("  4. Copy the app's Client ID from its settings page.");
    let id = loop {
        use std::io::Write;
        print!("\nClient ID: ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Err("no client id given".into());
        }
        let id = line.trim().to_string();
        if id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit()) {
            break id;
        }
        println!("That doesn't look like a client id (32 hex characters).");
    };
    fs::create_dir_all(config_dir()).map_err(|e| e.to_string())?;
    fs::write(&path, &id).map_err(|e| e.to_string())?;
    println!("Saved to {}.\n", path.display());
    Ok(id)
}

fn save_refresh(path: &PathBuf, tok: &str) {
    let _ = fs::write(path, tok);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
}

async fn connect(cache: &Cache, creds: Option<Credentials>) -> Result<Session, String> {
    let creds = creds.or_else(|| cache.credentials()).ok_or("no saved login")?;
    let s = Session::new(SessionConfig::default(), Some(cache.clone()));
    s.connect(creds, true).await.map_err(|e| format!("login failed: {e}"))?;
    Ok(s)
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
}

/// Hand freed heap pages back to the OS. macOS malloc otherwise keeps them dirty (and counted
/// against us) after bursts like loading a big library or starting a track.
fn trim_heap() {
    #[cfg(target_os = "macos")]
    unsafe {
        malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
    }
}

// ---------- app ----------

#[derive(PartialEq)]
enum Focus {
    Playlists,
    Tracks,
}

struct View {
    id: u64,
    key: String,
    title: String,
    kind: Kind, // Tracks, or a list of album/artist links
    tracks: Arc<Vec<Track>>, // shared with the queue, so playing a big list doesn't copy it
    links: Vec<Link>,
    loading: bool,
}

impl View {
    fn new(id: u64, key: String, title: String, kind: Kind) -> Self {
        View { id, key, title, kind, tracks: Arc::default(), links: vec![], loading: true }
    }
    fn len(&self) -> usize {
        if self.kind == Kind::Tracks { self.tracks.len() } else { self.links.len() }
    }
    fn haystack(&self, i: usize) -> String {
        match self.kind {
            Kind::Tracks => {
                let t = &self.tracks[i];
                format!("{} {} {}", t.name, t.artists, t.album)
            }
            _ => format!("{} {}", self.links[i].name, self.links[i].sub),
        }
        .to_lowercase()
    }
}

/// Rows currently listed: the whole view, or the ctrl+f matches.
enum Shown<'a> {
    All(usize),
    Matches(&'a [usize]),
}

impl Shown<'_> {
    fn len(&self) -> usize {
        match self {
            Shown::All(n) => *n,
            Shown::Matches(m) => m.len(),
        }
    }
    /// Original index of the r-th listed row.
    fn get(&self, r: usize) -> Option<usize> {
        match self {
            Shown::All(n) => (r < *n).then_some(r),
            Shown::Matches(m) => m.get(r).copied(),
        }
    }
}

struct Queue {
    key: String,
    name: String,
    tracks: Arc<Vec<Track>>,
    order: Vec<usize>, // play order (indices into tracks); shuffled when shuffle is on
    pos: usize,
}

impl Queue {
    fn idx(&self) -> usize {
        self.order[self.pos]
    }

    /// Uniform Fisher-Yates over every track (OS-seeded ChaCha RNG), no "smart" weighting.
    /// `first` stays at the front so the current/chosen song keeps playing.
    fn reorder(&mut self, shuffle: bool, first: Option<usize>) {
        use rand::seq::SliceRandom;
        let n = self.tracks.len();
        if shuffle {
            let mut rest: Vec<usize> = (0..n).filter(|&i| Some(i) != first).collect();
            rest.shuffle(&mut rand::rng());
            self.order = first.into_iter().chain(rest).collect();
            self.pos = 0;
        } else {
            self.order = (0..n).collect();
            self.pos = first.unwrap_or(0);
        }
    }
}

struct App {
    rt: tokio::runtime::Runtime,
    api: Arc<Api>,
    tx: mpsc::Sender<Msg>,
    cache: Cache,
    session: Session,
    player: Arc<Player>,
    mixer: Arc<dyn Mixer>,
    media: Option<MediaControls>,

    playlists: Vec<Playlist>,
    pl_state: ListState,
    view: View,
    tr_state: TableState,
    focus: Focus,
    input: Option<String>,
    find: Option<String>, // ctrl+f filter over the open track list
    matches: Vec<usize>,  // view indices matching `find`, kept current by refilter()
    search_kind: Kind,
    shuffle: bool,
    repeat: Repeat,
    fails: usize, // consecutive unplayable tracks, so repeat-all can't spin forever
    queue: Option<Queue>,
    cur: Option<SpotifyUri>,
    playing: bool,
    pos_ms: u64,
    pos_at: Option<Instant>,
    status: String,
    note: Option<(String, Instant)>, // brief popup and when it disappears
    retried: bool,
    vol: u16,               // user volume; the mixer dips below it during fades
    want_play: bool,        // intent, flips immediately (playing lags until the player confirms)
    fade_gen: Arc<std::sync::atomic::AtomicU64>, // bumps cancel an in-flight fade
    quit: bool,
}

impl App {
    fn open_playlist(&mut self, i: usize) {
        let Some(pl) = self.playlists.get(i).cloned() else { return };
        self.view = View::new(self.view.id + 1, pl.key(), pl.name.clone(), Kind::Tracks);
        self.tr_state = TableState::default().with_selected(0);
        self.find = None;
        self.rt.spawn(load_tracks(self.api.clone(), self.session.clone(), self.tx.clone(), self.view.id, pl));
    }

    fn open_search(&mut self, q: String) {
        let kind = self.search_kind;
        let title = format!("Search {}: {q}", kind.label());
        self.view = View::new(self.view.id + 1, format!("search:{}:{q}", kind.label()), title, kind);
        self.tr_state = TableState::default().with_selected(0);
        self.find = None;
        self.focus = Focus::Tracks;
        self.rt.spawn(search(self.api.clone(), self.tx.clone(), self.view.id, q, kind));
    }

    /// Open an album (track list) or artist (whole discography) from a search result.
    fn open_link(&mut self, i: usize) {
        let Some(l) = self.view.links.get(i).cloned() else { return };
        let title = if self.view.kind == Kind::Artists { format!("{} · discography", l.name) } else { format!("{} — {}", l.name, l.sub) };
        self.view = View::new(self.view.id + 1, l.uri.clone(), title, Kind::Tracks);
        self.tr_state = TableState::default().with_selected(0);
        self.find = None;
        self.rt.spawn(link_tracks(self.session.clone(), self.tx.clone(), self.view.id, l.uri));
    }

    /// Enter on a row: play a track (queueing the whole list) or open an album/artist.
    fn activate(&mut self, i: usize) {
        if self.view.kind == Kind::Tracks { self.play_from_view(Some(i)) } else { self.open_link(i) }
    }

    fn toggle_shuffle(&mut self) {
        self.shuffle = !self.shuffle;
        if let Some(q) = &mut self.queue {
            let cur = q.idx();
            q.reorder(self.shuffle, Some(cur));
        }
    }

    fn ensure_session(&mut self) {
        if !self.session.is_invalid() {
            return;
        }
        self.status = "reconnecting…".into();
        match self.rt.block_on(connect(&self.cache, None)) {
            Ok(s) => {
                self.player.set_session(s.clone());
                self.session = s;
                self.status.clear();
            }
            Err(e) => self.status = e,
        }
    }

    fn play_current(&mut self) {
        let Some(q) = &self.queue else { return };
        let Some(t) = q.tracks.get(q.idx()).cloned() else { return };
        let Ok(uri) = SpotifyUri::from_uri(&t.uri) else { return };
        self.ensure_session();
        if let Some(m) = &mut self.media {
            let _ = m.set_metadata(MediaMetadata {
                title: Some(&t.name),
                artist: Some(&t.artists),
                album: Some(&t.album),
                cover_url: t.cover.as_deref(),
                duration: Some(Duration::from_millis(t.ms)),
            });
        }
        self.reset_fade();
        self.cur = Some(uri.clone());
        self.pos_ms = 0;
        self.pos_at = None;
        self.player.load(uri, true, 0);
    }

    fn sync_media(&mut self) {
        let progress = Some(MediaPosition(Duration::from_millis(self.position())));
        let state = match (&self.cur, self.playing) {
            (None, _) => MediaPlayback::Stopped,
            (_, true) => MediaPlayback::Playing { progress },
            (_, false) => MediaPlayback::Paused { progress },
        };
        if let Some(m) = &mut self.media {
            let _ = m.set_playback(state);
        }
    }

    fn toggle(&mut self) {
        if self.want_play {
            self.want_play = false;
            self.fade(false);
        } else if self.cur.is_some() {
            self.ensure_session();
            self.want_play = true;
            self.fade(true);
        }
    }

    /// Ramp volume down then pause (250ms), or resume and ramp back up (400ms).
    fn fade(&self, up: bool) {
        use std::sync::atomic::Ordering::SeqCst;
        let id = self.fade_gen.fetch_add(1, SeqCst) + 1;
        let (fade_gen, mixer, player, vol) = (self.fade_gen.clone(), self.mixer.clone(), self.player.clone(), self.vol);
        if up {
            player.play();
        }
        self.rt.spawn(async move {
            let (from, to, ms) = (mixer.volume() as f64, if up { vol as f64 } else { 0.0 }, if up { 400 } else { 250 });
            const STEPS: u64 = 25;
            for i in 1..=STEPS {
                tokio::time::sleep(Duration::from_millis(ms / STEPS)).await;
                if fade_gen.load(SeqCst) != id {
                    return;
                }
                // the soft mixer's curve is logarithmic, so a linear ramp here sounds even
                mixer.set_volume((from + (to - from) * i as f64 / STEPS as f64) as u16);
            }
            if !up {
                player.pause();
            }
        });
    }

    /// Cancel any fade and put the mixer back at the user's volume (new track starting).
    fn reset_fade(&mut self) {
        self.fade_gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.mixer.set_volume(self.vol);
        self.want_play = true;
    }

    fn seek_to(&mut self, ms: u64) {
        self.player.seek(ms as u32);
    }

    fn on_media(&mut self, ev: MediaControlEvent) {
        use MediaControlEvent as E;
        let step = Duration::from_secs(10);
        match ev {
            E::Toggle => self.toggle(),
            E::Play if !self.want_play => self.toggle(),
            E::Pause if self.want_play => self.toggle(),
            E::Next => self.skip(1),
            E::Previous => self.prev(),
            E::Seek(d) => self.seek_by(d, step),
            E::SeekBy(d, by) => self.seek_by(d, by),
            E::SetPosition(MediaPosition(p)) => self.seek_to(p.as_millis() as u64),
            _ => {}
        }
    }

    fn seek_by(&mut self, d: SeekDirection, by: Duration) {
        let by = by.as_millis() as u64;
        let pos = self.position();
        self.seek_to(match d {
            SeekDirection::Forward => pos + by,
            SeekDirection::Backward => pos.saturating_sub(by),
        });
    }

    fn prev(&mut self) {
        if self.position() > 3000 {
            self.seek_to(0);
        } else {
            self.skip(-1);
        }
    }

    /// Queue the whole open list. `start` = chosen track; None = from the top (or a random track if shuffling).
    fn play_from_view(&mut self, start: Option<usize>) {
        if self.view.kind != Kind::Tracks || self.view.tracks.is_empty() {
            return;
        }
        let mut q = Queue {
            key: self.view.key.clone(),
            name: self.view.title.clone(),
            tracks: self.view.tracks.clone(),
            order: vec![],
            pos: 0,
        };
        q.reorder(self.shuffle, start);
        self.queue = Some(q);
        self.retried = false;
        self.play_current();
    }

    fn skip(&mut self, d: isize) {
        let Some(q) = &mut self.queue else { return };
        let len = q.order.len() as isize;
        let mut n = q.pos as isize + d;
        if self.repeat == Repeat::All && (n < 0 || n >= len) {
            if n >= len && self.shuffle {
                // fresh order each pass; don't open the new pass with the song that just ended
                let last = q.idx();
                q.reorder(true, None);
                if q.order.len() > 1 && q.order[0] == last {
                    q.order.swap(0, 1);
                }
                n = 0;
            } else {
                n = n.rem_euclid(len);
            }
        }
        if n < 0 || n >= len {
            self.player.stop();
            self.playing = false;
            self.want_play = false;
            return;
        }
        q.pos = n as usize;
        self.retried = false;
        self.play_current();
    }

    fn position(&self) -> u64 {
        self.pos_ms + self.pos_at.map_or(0, |t| t.elapsed().as_millis() as u64)
    }

    fn set_volume(&mut self, delta: i32) {
        self.vol = (self.vol as i32 + delta).clamp(0, 65535) as u16;
        if self.want_play {
            self.mixer.set_volume(self.vol);
        }
        self.cache.save_volume(self.vol);
    }

    fn on_player(&mut self, ev: PlayerEvent) {
        let sync = matches!(
            ev,
            PlayerEvent::Playing { .. } | PlayerEvent::Paused { .. } | PlayerEvent::Stopped { .. } | PlayerEvent::Seeked { .. }
        );
        self.on_player_inner(ev);
        if sync {
            self.sync_media();
        }
    }

    fn on_player_inner(&mut self, ev: PlayerEvent) {
        let is_cur = |id: &SpotifyUri, cur: &Option<SpotifyUri>| cur.as_ref() == Some(id);
        match ev {
            PlayerEvent::Playing { track_id, position_ms, .. } if is_cur(&track_id, &self.cur) => {
                self.playing = true;
                self.fails = 0;
                self.pos_ms = position_ms as u64;
                self.pos_at = Some(Instant::now());
                if self.status.starts_with("can't play") {
                    self.status.clear();
                }
                trim_heap();
            }
            PlayerEvent::Paused { track_id, position_ms, .. } if is_cur(&track_id, &self.cur) => {
                self.playing = false;
                self.pos_ms = position_ms as u64;
                self.pos_at = None;
            }
            PlayerEvent::Seeked { track_id, position_ms, .. }
            | PlayerEvent::PositionCorrection { track_id, position_ms, .. }
                if is_cur(&track_id, &self.cur) =>
            {
                self.pos_ms = position_ms as u64;
                self.pos_at = self.playing.then(Instant::now);
            }
            PlayerEvent::TimeToPreloadNextTrack { track_id, .. } if is_cur(&track_id, &self.cur) => {
                let next = self.queue.as_ref().and_then(|q| match self.repeat {
                    Repeat::One => Some(q.pos),
                    _ if q.pos + 1 < q.order.len() => Some(q.pos + 1),
                    Repeat::All if !self.shuffle => Some(0), // shuffled wrap picks a new order later
                    _ => None,
                });
                if let Some(q) = &self.queue
                    && let Some(t) = next.map(|p| &q.tracks[q.order[p]])
                    && let Ok(u) = SpotifyUri::from_uri(&t.uri)
                {
                    self.player.preload(u);
                }
            }
            PlayerEvent::EndOfTrack { track_id, .. } if is_cur(&track_id, &self.cur) => {
                if self.repeat == Repeat::One {
                    self.play_current();
                } else {
                    self.skip(1);
                }
            }
            PlayerEvent::Unavailable { track_id, .. } if is_cur(&track_id, &self.cur) => {
                // dead session after sleep/network change shows up as unavailable: retry once
                if !self.retried && self.session.is_invalid() {
                    self.retried = true;
                    self.play_current();
                } else {
                    self.fails += 1;
                    if self.fails > self.queue.as_ref().map_or(0, |q| q.order.len()) {
                        self.status = "nothing in this list is playable".into();
                        self.player.stop();
                        self.want_play = false;
                    } else {
                        self.status = "can't play that track, skipping".into();
                        self.skip(1);
                    }
                }
            }
            PlayerEvent::Stopped { .. } => {
                self.playing = false;
                self.pos_at = None;
            }
            _ => {}
        }
    }

    /// Rows currently listed (all of them unless filtering).
    fn shown(&self) -> Shown<'_> {
        match self.find.as_deref() {
            Some(q) if !q.is_empty() => Shown::Matches(&self.matches),
            _ => Shown::All(self.view.len()),
        }
    }

    /// Recompute the ctrl+f matches; call when the query or the open list changes.
    fn refilter(&mut self) {
        self.matches.clear();
        let Some(q) = self.find.as_deref().filter(|q| !q.is_empty()) else { return };
        let words: Vec<String> = q.to_lowercase().split_whitespace().map(String::from).collect();
        let view = &self.view;
        self.matches.extend((0..view.len()).filter(|&i| {
            let hay = view.haystack(i);
            words.iter().all(|w| hay.contains(w.as_str()))
        }));
    }

    /// Original track index under the cursor.
    fn selected_track(&self) -> Option<usize> {
        self.shown().get(self.tr_state.selected()?)
    }

    fn close_find(&mut self) {
        let orig = self.selected_track();
        self.find = None;
        self.tr_state.select(orig.or(Some(0)));
    }

    fn on_find_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Esc => self.close_find(),
            KeyCode::Enter => {
                // play the match, but queue the whole list so playback continues through the playlist
                if let Some(i) = self.selected_track() {
                    self.close_find();
                    self.activate(i);
                }
            }
            KeyCode::Down => self.move_sel(1),
            KeyCode::Up => self.move_sel(-1),
            KeyCode::Char('n') if ctrl => self.move_sel(1),
            KeyCode::Char('p') if ctrl => self.move_sel(-1),
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Backspace => {
                if let Some(f) = &mut self.find {
                    f.pop();
                }
                self.refilter();
                self.tr_state.select(Some(0));
            }
            KeyCode::Char(c) if !ctrl => {
                if let Some(f) = &mut self.find {
                    f.push(c);
                }
                self.refilter();
                self.tr_state.select(Some(0));
            }
            _ => {}
        }
    }

    fn on_key(&mut self, k: KeyEvent) {
        if self.find.is_some() {
            return self.on_find_key(k);
        }
        if let Some(buf) = &mut self.input {
            match k.code {
                KeyCode::Enter => {
                    let q = buf.trim().to_string();
                    self.input = None;
                    if !q.is_empty() {
                        self.open_search(q);
                    }
                }
                KeyCode::Esc => self.input = None,
                KeyCode::Tab => self.search_kind = self.search_kind.next(),
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) => buf.push(c),
                _ => {}
            }
            return;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char('/') => self.input = Some(String::new()),
            KeyCode::Char('t') if ctrl => {
                self.note = Some((format!("theme: {}", cycle_family()), Instant::now() + Duration::from_millis(1500)))
            }
            KeyCode::Char('f') if ctrl => {
                self.focus = Focus::Tracks;
                self.find = Some(String::new());
                self.tr_state.select(Some(0));
            }
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l') => {
                self.focus = match (&self.focus, k.code) {
                    (_, KeyCode::Left | KeyCode::Char('h')) => Focus::Playlists,
                    (_, KeyCode::Right | KeyCode::Char('l')) => Focus::Tracks,
                    (Focus::Playlists, _) => Focus::Tracks,
                    (Focus::Tracks, _) => Focus::Playlists,
                }
            }
            KeyCode::Char(' ') => self.toggle(),
            KeyCode::Char('s') => self.toggle_shuffle(),
            KeyCode::Char('r') => {
                self.repeat = match self.repeat {
                    Repeat::Off => Repeat::All,
                    Repeat::All => Repeat::One,
                    Repeat::One => Repeat::Off,
                }
            }
            KeyCode::Char('P') => self.play_from_view(None),
            KeyCode::Char('n') => self.skip(1),
            KeyCode::Char('p') => self.prev(),
            KeyCode::Char('.') => self.seek_by(SeekDirection::Forward, Duration::from_secs(10)),
            KeyCode::Char(',') => self.seek_by(SeekDirection::Backward, Duration::from_secs(10)),
            KeyCode::Char('+') | KeyCode::Char('=') => self.set_volume(3277),
            KeyCode::Char('-') => self.set_volume(-3277),
            KeyCode::Char('o') => {
                // jump to what's playing
                if let Some(q) = &self.queue {
                    let idx = q.idx();
                    let mut v = View::new(self.view.id + 1, q.key.clone(), q.name.clone(), Kind::Tracks);
                    v.tracks = q.tracks.clone();
                    v.loading = false;
                    self.view = v;
                    self.tr_state = TableState::default().with_selected(idx);
                    self.focus = Focus::Tracks;
                    self.find = None;
                }
            }
            KeyCode::Enter => match self.focus {
                Focus::Playlists => {
                    if let Some(i) = self.pl_state.selected() {
                        self.open_playlist(i);
                        self.focus = Focus::Tracks;
                    }
                }
                Focus::Tracks => {
                    if let Some(i) = self.selected_track() {
                        self.activate(i);
                    }
                }
            },
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::PageDown | KeyCode::Char('d') if k.code == KeyCode::PageDown || ctrl => self.move_sel(15),
            KeyCode::PageUp | KeyCode::Char('u') if k.code == KeyCode::PageUp || ctrl => self.move_sel(-15),
            KeyCode::Char('g') | KeyCode::Home => self.move_sel(isize::MIN / 2),
            KeyCode::Char('G') | KeyCode::End => self.move_sel(isize::MAX / 2),
            _ => {}
        }
    }

    fn move_sel(&mut self, d: isize) {
        let (len, cur) = match self.focus {
            Focus::Playlists => (self.playlists.len(), self.pl_state.selected()),
            Focus::Tracks => (self.shown().len(), self.tr_state.selected()),
        };
        if len == 0 {
            return;
        }
        let n = (cur.unwrap_or(0) as isize).saturating_add(d).clamp(0, len as isize - 1) as usize;
        match self.focus {
            Focus::Playlists => self.pl_state.select(Some(n)),
            Focus::Tracks => self.tr_state.select(Some(n)),
        }
    }
}

// ---------- drawing ----------

fn mmss(ms: u64) -> String {
    format!("{}:{:02}", ms / 60000, ms / 1000 % 60)
}

/// One palette per theme family and light/dark; ANSI fallback without truecolor.
struct Theme {
    accent: Color,  // focus, playing
    accent2: Color, // context name
    deep: Color,    // unfocused selection, progress track
    hl: Color,      // focused selection background
    text: Color,
    sub: Color,
    dim: Color, // borders, durations
    gold: Color, // shuffle / repeat / prompt labels
    pink: Color, // Liked Songs
    err: Color,
}

/// Light vs dark terminal background: `SPOT_THEME=light|dark` wins, otherwise ask the terminal (OSC 11).
/// Must run before the input thread starts reading stdin, or it eats the reply.
fn is_light() -> bool {
    match std::env::var("SPOT_THEME").as_deref() {
        Ok("light") => true,
        Ok("dark") => false,
        _ => {
            use terminal_colorsaurus::{QueryOptions, ThemeMode, theme_mode};
            matches!(theme_mode(QueryOptions::default()), Ok(ThemeMode::Light))
        }
    }
}

/// Current light/dark mode; flipped live when the terminal reports a color scheme change.
static LIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Index into FAMILIES, cycled with ctrl+t and remembered in the cache dir.
static FAMILY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Theme families as (name, dark, light). Each palette is
/// [accent, accent2, deep, hl, text, sub, dim, gold, pink, err] (see Theme).
const FAMILIES: [(&str, [u32; 10], [u32; 10]); 7] = [
    // muted desert sunset: amber sky, dusky mountains, dark scrub; at midday for light
    ("desert",
     [0xd0924a, 0xb08a66, 0x2e2722, 0x5c4128, 0xd6c8b4, 0x8f8679, 0x4f4a55, 0xd9b066, 0xa58b98, 0xc26a50],
     [0xa8621c, 0x8a6440, 0xe8dfd3, 0xf0d3a8, 0x3a3029, 0x6e655a, 0xa39a90, 0x9a7014, 0x8f5f78, 0xb0472f]),
    // shallow water: teal, slate blue, coral reef
    ("lagoon",
     [0x4fb3a9, 0x7fa8b8, 0x1d2b2e, 0x1f4a4c, 0xcfdcda, 0x7f9796, 0x3f5256, 0xe39a7b, 0xd98ba0, 0xe0685e],
     [0x16857c, 0x3f6f86, 0xdcebea, 0xbfe3df, 0x23393b, 0x56706f, 0x9db3b2, 0xc0623f, 0xb4506e, 0xc23b30]),
    // forest floor: sage, lichen, wild plum
    ("moss",
     [0x9cbf6b, 0xb5a77a, 0x262b22, 0x3d4a2c, 0xd8dccb, 0x8d9581, 0x4a5243, 0xe0b45e, 0xc08aa8, 0xd46a5a],
     [0x5b8a2a, 0x7d6e3f, 0xe5eadb, 0xd6e6bd, 0x2e3326, 0x626b55, 0xa7ae98, 0xa8781a, 0x96507a, 0xb5402f]),
    // twilight: violet, periwinkle, peach
    ("iris",
     [0xa98be0, 0x8fa0d8, 0x26223a, 0x3e3466, 0xdcd6ec, 0x928ba8, 0x4b4663, 0xf0b48a, 0xe58fb6, 0xe86f7a],
     [0x6c4cc4, 0x4a5fb0, 0xe8e3f4, 0xd9cff5, 0x2d2840, 0x625b78, 0xaaa3c0, 0xb8673a, 0xb03f7a, 0xc2364a]),
    // pastels; light variants are deepened so they stay readable on white
    // cherry blossom: rose, lavender, cream
    ("blossom",
     [0xf5a6c8, 0xc6a8f0, 0x2a2430, 0x4a3550, 0xeadff0, 0xa597b0, 0x564b60, 0xf6d3a1, 0xe8a0d8, 0xf28b9a],
     [0xc4467e, 0x7f5cc4, 0xf3e6ef, 0xf6d4e5, 0x3d2f3f, 0x7a6a80, 0xbfaec4, 0xb27a28, 0xa850a0, 0xcc3b55]),
    // mint and sky
    ("mint",
     [0x94e2c0, 0x89c4f4, 0x1f2a2a, 0x2f4a48, 0xdcefea, 0x92aaa8, 0x46585a, 0xf9e2af, 0xf5b8d0, 0xf38ba8],
     [0x1f9a73, 0x2f7fc4, 0xe0f1ec, 0xc6ebde, 0x26393a, 0x5d7473, 0xa5bab8, 0xb8861c, 0xc0507e, 0xd0304f]),
    // peach sorbet with lilac
    ("sorbet",
     [0xfab387, 0xcba6f7, 0x2c2626, 0x4d3a36, 0xf0e2da, 0xab9a92, 0x5a4e4c, 0xf9e2af, 0xf5c2e7, 0xf38ba8],
     [0xd0601c, 0x8839ef, 0xf6e8e0, 0xfad8c4, 0x40302a, 0x7d6a62, 0xc4b2aa, 0xb07a10, 0xc0479a, 0xd20f39]),
];

fn theme() -> &'static Theme {
    use std::sync::atomic::Ordering::Relaxed;
    static T: std::sync::OnceLock<Vec<Theme>> = std::sync::OnceLock::new();
    let t = T.get_or_init(|| {
        let truecolor = std::env::var("COLORTERM").is_ok_and(|v| v.contains("truecolor") || v.contains("24bit"));
        FAMILIES.iter().flat_map(|(_, dark, light)| [palette(truecolor.then_some(dark), false), palette(truecolor.then_some(light), true)]).collect()
    });
    &t[FAMILY.load(Relaxed) * 2 + LIGHT.load(Relaxed) as usize]
}

fn palette(hex: Option<&[u32; 10]>, light: bool) -> Theme {
    let rgb = |h: u32| Color::Rgb((h >> 16) as u8, (h >> 8) as u8, h as u8);
    match (hex, light) {
        (Some(h), _) => Theme {
            accent: rgb(h[0]),
            accent2: rgb(h[1]),
            deep: rgb(h[2]),
            hl: rgb(h[3]),
            text: rgb(h[4]),
            sub: rgb(h[5]),
            dim: rgb(h[6]),
            gold: rgb(h[7]),
            pink: rgb(h[8]),
            err: rgb(h[9]),
        },
        (None, false) => Theme {
            accent: Color::Yellow,
            accent2: Color::Yellow,
            deep: Color::DarkGray,
            hl: Color::DarkGray,
            text: Color::White,
            sub: Color::Gray,
            dim: Color::DarkGray,
            gold: Color::LightYellow,
            pink: Color::Magenta,
            err: Color::Red,
        },
        (None, true) => Theme {
            accent: Color::Yellow,
            accent2: Color::Yellow,
            deep: Color::Gray,
            hl: Color::Gray,
            text: Color::Black,
            sub: Color::DarkGray,
            dim: Color::Gray,
            gold: Color::Red,
            pink: Color::Magenta,
            err: Color::Red,
        },
    }
}

/// Switch to the next theme family and remember it.
fn cycle_family() -> &'static str {
    use std::sync::atomic::Ordering::Relaxed;
    let i = (FAMILY.load(Relaxed) + 1) % FAMILIES.len();
    FAMILY.store(i, Relaxed);
    let _ = fs::write(cache_dir().join("theme"), FAMILIES[i].0);
    FAMILIES[i].0
}

fn draw(f: &mut Frame, app: &mut App) {
    let th = theme();
    // bottom line only appears for prompts and errors
    let prompt = app.find.is_some() || app.input.is_some() || !app.status.is_empty();
    let [main, now, bar, bottom] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(prompt as u16),
    ])
    .areas(f.area());
    let [left, right] = Layout::horizontal([Constraint::Percentage(28), Constraint::Fill(1)]).areas(main);

    let block = |title: String, focused: bool| {
        let (border, title_style) = if focused {
            (Style::new().fg(th.accent), Style::new().fg(th.accent).bold())
        } else {
            (Style::new().fg(th.dim), Style::new().fg(th.sub))
        };
        Block::bordered()
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(border)
            .title(Span::styled(title, title_style))
    };
    let hl = |focused: bool| {
        if focused {
            Style::new().bg(th.hl).fg(th.text).bold()
        } else {
            Style::new().bg(th.deep).fg(th.text)
        }
    };
    let playing_key = app.queue.as_ref().map(|q| q.key.as_str());
    let pl_focus = app.focus == Focus::Playlists;

    let items: Vec<ListItem> = app
        .playlists
        .iter()
        .map(|p| {
            if Some(p.key().as_str()) == playing_key {
                ListItem::new(format!("▶ {}", p.name)).style(Style::new().fg(th.accent).bold())
            } else if p.id.is_none() {
                ListItem::new(format!("  {}", p.name)).style(Style::new().fg(th.pink))
            } else {
                ListItem::new(format!("  {}", p.name)).style(Style::new().fg(th.text))
            }
        })
        .collect();
    let title = if app.playlists.is_empty() { " Playlists (loading…) ".into() } else { " Playlists ".into() };
    let list = List::new(items).block(block(title, pl_focus)).highlight_style(hl(pl_focus));
    f.render_stateful_widget(list, left, &mut app.pl_state);

    let playing_idx = app.queue.as_ref().filter(|q| q.key == app.view.key).map(|q| q.idx());
    let shown = app.shown();
    let total = shown.len();
    // Build only the rows that fit: ratatui would otherwise lay out every row of a 10k-song list each frame.
    let height = right.height.saturating_sub(2) as usize;
    let sel = app.tr_state.selected().map(|s| s.min(total.saturating_sub(1)));
    let mut off = app.tr_state.offset().min(total.saturating_sub(height));
    if let Some(s) = sel {
        if s < off {
            off = s;
        } else if s >= off + height {
            off = s + 1 - height;
        }
    }
    let rows: Vec<Row> = (off..total.min(off + height))
        .filter_map(|r| shown.get(r))
        .map(|i| {
            if app.view.kind != Kind::Tracks {
                let l = &app.view.links[i];
                return Row::new([
                    Cell::from(" "),
                    Cell::from(l.name.as_str()).style(Style::new().fg(th.text)),
                    Cell::from(l.sub.as_str()).style(Style::new().fg(th.sub)),
                    Cell::from(""),
                ]);
            }
            let t = &app.view.tracks[i];
            if Some(i) == playing_idx {
                return Row::new([
                    Cell::from("▶"),
                    Cell::from(t.name.as_str()),
                    Cell::from(t.artists.as_str()),
                    Cell::from(mmss(t.ms)),
                ])
                .style(Style::new().fg(th.accent).bold());
            }
            Row::new([
                Cell::from(" "),
                Cell::from(t.name.as_str()).style(Style::new().fg(th.text)),
                Cell::from(t.artists.as_str()).style(Style::new().fg(th.sub)),
                Cell::from(mmss(t.ms)).style(Style::new().fg(th.dim)),
            ])
        })
        .collect();
    let mut title = format!(" {} ", app.view.title);
    if app.find.is_some() {
        title.push_str(&format!("· {}/{} match ", total, app.view.len()));
    } else if app.view.loading {
        title.push_str("(loading…) ");
    } else if !app.view.key.is_empty() {
        title.push_str(&format!("· {} ", app.view.len()));
    }
    let table = Table::new(
        rows,
        [Constraint::Length(1), Constraint::Fill(3), Constraint::Fill(2), Constraint::Length(6)],
    )
    .block(block(title, !pl_focus))
    .row_highlight_style(hl(!pl_focus));
    let mut window = TableState::default().with_selected(sel.map(|s| s - off));
    f.render_stateful_widget(table, right, &mut window);
    *app.tr_state.offset_mut() = off;

    // now playing + progress bar
    let modes = format!(
        "{}{}",
        if app.shuffle { "  ⤮ shuffle" } else { "" },
        match app.repeat {
            Repeat::Off => "",
            Repeat::All => "  ⟳ repeat",
            Repeat::One => "  ⟳ repeat one",
        }
    );
    let shuf = Span::styled(modes, Style::new().fg(th.gold).bold());
    let cur = app.queue.as_ref().and_then(|q| q.tracks.get(q.idx()).map(|t| (q, t)));
    let (np, ratio) = match cur {
        Some((q, t)) => {
            let pos = app.position().min(t.ms);
            let line = Line::from(vec![
                Span::styled(if app.playing { " ▶ " } else { " ⏸ " }, Style::new().fg(th.accent).bold()),
                Span::styled(t.name.clone(), Style::new().fg(th.text).bold()),
                Span::styled(format!("  {}", t.artists), Style::new().fg(th.sub)),
                Span::styled(format!("  {} / {}", mmss(pos), mmss(t.ms)), Style::new().fg(th.dim)),
                Span::styled(format!("  · {}", q.name), Style::new().fg(th.accent2)),
                Span::styled(
                    format!("  · vol {}%", (app.vol as u32 * 100 + 32767) / 65535),
                    Style::new().fg(th.dim),
                ),
                shuf,
            ]);
            (line, if t.ms > 0 { pos as f64 / t.ms as f64 } else { 0.0 })
        }
        None => (Line::from(vec![Span::styled(" nothing playing", Style::new().fg(th.dim)), shuf]), 0.0),
    };
    f.render_widget(Paragraph::new(np), now);
    let gauge = ratatui::widgets::LineGauge::default()
        .ratio(ratio.clamp(0.0, 1.0))
        .label("")
        .filled_style(Style::new().fg(th.accent))
        .unfilled_style(Style::new().fg(th.deep))
        .filled_symbol("━")
        .unfilled_symbol("━");
    f.render_widget(gauge, bar.inner(ratatui::layout::Margin { horizontal: 1, vertical: 0 }));

    // popup floating over the bottom-right corner of the track list
    if let Some((note, _)) = &app.note {
        let w = (note.chars().count() as u16 + 4).min(right.width);
        let h = 3.min(right.height);
        let area = ratatui::layout::Rect::new(right.right() - w, right.bottom() - h, w, h);
        f.render_widget(ratatui::widgets::Clear, area);
        f.render_widget(
            Paragraph::new(Span::styled(format!(" {note}"), Style::new().fg(th.text).bold())).block(
                Block::bordered()
                    .border_type(ratatui::widgets::BorderType::Rounded)
                    .border_style(Style::new().fg(th.accent)),
            ),
            area,
        );
    }

    if !prompt {
        return;
    }
    let bottom_line = if let Some(buf) = &app.find {
        Line::from(vec![
            Span::styled(" find: ", Style::new().fg(th.gold).bold()),
            Span::styled(format!("{buf}█"), Style::new().fg(th.text)),
        ])
    } else if let Some(buf) = &app.input {
        Line::from(vec![
            Span::styled(" search ", Style::new().fg(th.accent).bold()),
            Span::styled(app.search_kind.label(), Style::new().fg(th.gold).bold()),
            Span::styled(": ", Style::new().fg(th.accent)),
            Span::styled(format!("{buf}█"), Style::new().fg(th.text)),
            Span::styled("   tab: songs/albums/artists", Style::new().fg(th.dim)),
        ])
    } else {
        Line::from(Span::styled(format!(" {}", app.status), Style::new().fg(th.err)))
    };
    f.render_widget(Paragraph::new(bottom_line), bottom);

}

// ---------- main ----------

// ratatui hides the cursor; we exit via process::exit so Terminal's Drop never re-shows it.
/// Reads keys from stdin. Crossterm's reader can't be used: it treats the terminal's color
/// scheme report (`CSI ? 997 ; 1|2 n`) as an unfinished sequence and swallows the keys after it.
fn read_input(tx: mpsc::Sender<Msg>) {
    use std::io::Read;
    let mut stdin = std::io::stdin().lock();
    let mut buf = [0u8; 1024];
    loop {
        let n = match stdin.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        for m in parse_input(&buf[..n]) {
            if tx.send(m).is_err() {
                return;
            }
        }
    }
}

fn parse_input(b: &[u8]) -> Vec<Msg> {
    let key = |code| Msg::Key(KeyEvent::new(code, KeyModifiers::NONE));
    let ctrl = |c: u8| Msg::Key(KeyEvent::new(KeyCode::Char(c as char), KeyModifiers::CONTROL));
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            // a lone ESC at the end of a read is the Esc key; otherwise it starts a sequence
            0x1b if i + 1 == b.len() => {
                out.push(key(KeyCode::Esc));
                i += 1;
            }
            0x1b => match b[i + 1] {
                b'[' => {
                    let start = i + 2;
                    let Some(len) = b[start..].iter().position(|c| (0x40..=0x7e).contains(c)) else { break };
                    let (params, fin) = (&b[start..start + len], b[start + len]);
                    i = start + len + 1;
                    let first = params.split(|&c| c == b';').next().unwrap_or(b"");
                    out.extend(match (fin, first) {
                        (b'A', _) => Some(key(KeyCode::Up)),
                        (b'B', _) => Some(key(KeyCode::Down)),
                        (b'C', _) => Some(key(KeyCode::Right)),
                        (b'D', _) => Some(key(KeyCode::Left)),
                        (b'H', _) | (b'~', b"1" | b"7") => Some(key(KeyCode::Home)),
                        (b'F', _) | (b'~', b"4" | b"8") => Some(key(KeyCode::End)),
                        (b'~', b"5") => Some(key(KeyCode::PageUp)),
                        (b'~', b"6") => Some(key(KeyCode::PageDown)),
                        (b'Z', _) => Some(Msg::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT))),
                        (b'n', _) if params == b"?997;1" || params == b"?997;2" => {
                            Some(Msg::Scheme(params == b"?997;2"))
                        }
                        _ => None,
                    });
                }
                b'O' if i + 2 < b.len() => {
                    out.extend(match b[i + 2] {
                        b'A' => Some(key(KeyCode::Up)),
                        b'B' => Some(key(KeyCode::Down)),
                        b'C' => Some(key(KeyCode::Right)),
                        b'D' => Some(key(KeyCode::Left)),
                        b'H' => Some(key(KeyCode::Home)),
                        b'F' => Some(key(KeyCode::End)),
                        _ => None,
                    });
                    i += 3;
                }
                // OSC (e.g. a late color reply): skip to BEL or ST
                b']' => {
                    i += 2;
                    while i < b.len() && b[i] != 0x07 && !(b[i] == 0x1b && b.get(i + 1) == Some(&b'\\')) {
                        i += 1;
                    }
                    i += if b.get(i) == Some(&0x1b) { 2 } else { 1 };
                }
                // Alt+key: unused, drop both bytes
                _ => i += 2,
            },
            b'\r' | b'\n' => {
                out.push(key(KeyCode::Enter));
                i += 1;
            }
            b'\t' => {
                out.push(key(KeyCode::Tab));
                i += 1;
            }
            0x7f | 0x08 => {
                out.push(key(KeyCode::Backspace));
                i += 1;
            }
            c @ 0x01..=0x1a => {
                out.push(ctrl(b'a' + c - 1));
                i += 1;
            }
            c if c < 0x20 => i += 1,
            c => {
                let len = match c {
                    0xf0.. => 4,
                    0xe0.. => 3,
                    0xc0.. => 2,
                    _ => 1,
                };
                if let Some(ch) = b.get(i..i + len).and_then(|s| std::str::from_utf8(s).ok()).and_then(|s| s.chars().next()) {
                    out.push(key(KeyCode::Char(ch)));
                }
                i += len;
            }
        }
    }
    out
}

fn restore_terminal() {
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

fn main() {
    // probe light/dark before anything reads stdin
    LIGHT.store(is_light(), std::sync::atomic::Ordering::Relaxed);
    let saved = fs::read_to_string(cache_dir().join("theme")).unwrap_or_default();
    if let Some(i) = FAMILIES.iter().position(|(name, ..)| *name == saved.trim()) {
        FAMILY.store(i, std::sync::atomic::Ordering::Relaxed);
    }
    let ui = std::thread::spawn(|| {
        if let Err(e) = run() {
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

fn run() -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let _guard = rt.enter();

    let dir = cache_dir();
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let cache = Cache::new(Some(&dir), Some(&dir), None, None).map_err(|e| e.to_string())?;
    let refresh_path = dir.join("refresh_token");

    let redirect = std::env::var("SPOT_REDIRECT").unwrap_or_else(|_| REDIRECT.into());
    let client_id = client_id(&redirect)?;
    let oauth = OAuthClientBuilder::new(&client_id, &redirect, SCOPES.to_vec())
        .open_in_browser()
        .build()
        .map_err(|e| e.to_string())?;

    // Web API token: reuse saved refresh token, otherwise log in through the browser once.
    let saved = fs::read_to_string(&refresh_path).ok().filter(|s| !s.trim().is_empty());
    let tok = match saved.map(|r| rt.block_on(oauth.refresh_token_async(r.trim()))) {
        Some(Ok(t)) => t,
        _ => {
            println!("Log in to Spotify in your browser (opening now)…");
            rt.block_on(oauth.get_access_token_async()).map_err(|e| format!("login: {e}"))?
        }
    };
    if !tok.refresh_token.is_empty() {
        save_refresh(&refresh_path, &tok.refresh_token);
    }

    println!("Connecting…");
    let session = match rt.block_on(connect(&cache, None)) {
        Ok(s) => s,
        Err(_) => {
            // Streaming + internal APIs need a session from Spotify's own client id. Only needed once:
            // librespot stores reusable credentials afterwards. Never used for the Web API.
            println!("One more browser approval for the audio session…");
            let t = OAuthClientBuilder::new(SESSION_CLIENT_ID, SESSION_REDIRECT, vec!["streaming"])
                .open_in_browser()
                .build()
                .map_err(|e| e.to_string())?;
            let t = rt.block_on(t.get_access_token_async()).map_err(|e| format!("login: {e}"))?;
            rt.block_on(connect(&cache, Some(Credentials::with_access_token(t.access_token))))?
        }
    };

    let api = Arc::new(Api {
        http: reqwest::Client::new(),
        oauth,
        tok: tokio::sync::Mutex::new(Tok { access: tok.access_token, refresh: tok.refresh_token, exp: tok.expires_at }),
        path: refresh_path,
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

    let (tx, rx) = mpsc::channel::<Msg>();

    let mut events = player.get_player_event_channel();
    let ptx = tx.clone();
    rt.spawn(async move {
        while let Some(ev) = events.recv().await {
            if ptx.send(Msg::Player(ev)).is_err() {
                break;
            }
        }
    });

    let ktx = tx.clone();
    std::thread::spawn(move || read_input(ktx));
    let rtx = tx.clone();
    std::thread::spawn(move || {
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

    rt.spawn(load_playlists(api.clone(), tx.clone()));

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
        session,
        player,
        mixer,
        media,
        playlists: vec![],
        pl_state: ListState::default().with_selected(Some(0)),
        view: View { loading: false, ..View::new(0, String::new(), "Tracks".into(), Kind::Tracks) },
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
        cur: None,
        playing: false,
        pos_ms: 0,
        pos_at: None,
        status: String::new(),
        note: None,
        retried: false,
        vol,
        want_play: false,
        fade_gen: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        quit: false,
    };

    let mut term = ratatui::init();
    if std::env::var_os("SPOT_THEME").is_none() {
        // color scheme reports: subscribe to changes (2031) and ask for the current one (996)
        use std::io::Write;
        let _ = std::io::stdout().write_all(b"\x1b[?2031h\x1b[?996n").and_then(|_| std::io::stdout().flush());
    }
    while !app.quit {
        term.draw(|f| draw(f, &mut app)).map_err(|e| e.to_string())?;
        // Only wake up once a second while playing (progress clock); otherwise sleep until input.
        let mut timeout = if app.playing { Duration::from_secs(1) } else { Duration::from_secs(3600) };
        if let Some((_, until)) = app.note {
            // wake up in time to take the popup down
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
        for m in first.into_iter().chain(std::iter::from_fn(|| rx.try_recv().ok())) {
            match m {
                Msg::Key(k) => {
                    app.status.clear();
                    app.on_key(k);
                }
                Msg::Redraw => {}
                Msg::Scheme(light) => LIGHT.store(light, std::sync::atomic::Ordering::Relaxed),
                Msg::Player(ev) => app.on_player(ev),
                Msg::Playlists(p) => {
                    app.playlists = p;
                    if app.pl_state.selected().is_none() {
                        app.pl_state.select(Some(0));
                    }
                    if app.view.key.is_empty() {
                        app.open_playlist(0);
                    }
                }
                Msg::Tracks { view, tracks, done } if view == app.view.id => {
                    if !tracks.is_empty() {
                        Arc::make_mut(&mut app.view.tracks).extend(tracks);
                    }
                    app.view.loading = !done;
                    app.refilter();
                    if done {
                        trim_heap();
                    }
                    if app.tr_state.selected().is_none() && !app.view.tracks.is_empty() {
                        app.tr_state.select(Some(0));
                    }
                }
                Msg::Links { view, links, done } if view == app.view.id => {
                    app.view.links.extend(links);
                    app.view.loading = !done;
                    app.refilter();
                    // ratatui drops the selection while a list renders empty
                    if app.tr_state.selected().is_none() && !app.view.links.is_empty() {
                        app.tr_state.select(Some(0));
                    }
                }
                Msg::Tracks { .. } | Msg::Links { .. } => {}
                Msg::Error(e) => app.status = e,
                Msg::Media(ev) => app.on_media(ev),
            }
        }
    }
    restore_terminal();
    app.player.stop();
    std::process::exit(0);
}
