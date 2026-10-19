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
    lead: usize, // byte length of the first artist's name in `artists` (names can contain ", ")
    album: String,
    cover: Option<String>,
    ms: u64,
    album_id: u128, // SpotifyIds for goto album / artist (first artist); 0 = unknown
    artist_id: u128,
}

/// An album or artist search result.
#[derive(Clone)]
struct Link {
    uri: String,
    name: String,
    sub: String,
    single: bool, // a single release (EPs and compilations count as albums)
}

/// Which of an artist's releases their page lists, cycled with `f`.
#[derive(Clone, Copy, PartialEq)]
enum Releases {
    All,
    Albums,
    Singles,
}

impl Releases {
    fn next(self) -> Self {
        match self {
            Releases::All => Releases::Albums,
            Releases::Albums => Releases::Singles,
            Releases::Singles => Releases::All,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Releases::All => "albums & singles",
            Releases::Albums => "albums",
            Releases::Singles => "singles",
        }
    }
    fn keeps(self, l: &Link) -> bool {
        match self {
            Releases::All => true,
            Releases::Albums => !l.single,
            Releases::Singles => l.single,
        }
    }
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
    Background(u32), // terminal's background color (OSC 11 reply)
    Player(PlayerEvent),
    Playlists(Vec<Playlist>),
    Tracks { view: u64, tracks: Vec<Track>, done: bool },
    Replace { view: u64, tracks: Arc<Vec<Track>> }, // fresh copy of a list that was shown from cache
    Links { view: u64, links: Vec<Link>, done: bool },
    ReplaceLinks { view: u64, links: Vec<Link> }, // fresh copy of an artist's releases shown from cache
    Error(String),
    Media(MediaControlEvent),
    Relogin(bool), // true: waiting on browser login; false: it finished
    Connected(Result<Session, String>),
    Status(String),
}

/// The audio session, None until it has connected (in the background, so startup doesn't wait).
type Sess = tokio::sync::watch::Receiver<Option<Session>>;

async fn ready(mut s: Sess) -> Result<Session, String> {
    let s = s.wait_for(Option::is_some).await.map_err(|_| "not connected")?;
    Ok(s.clone().expect("checked by wait_for"))
}

// ---------- Web API ----------

struct Tok {
    access: String,
    refresh: String,
    exp: Instant,
    failed_login: Option<Instant>,
}

struct Api {
    http: reqwest::Client,
    oauth: Arc<OAuthClient>,
    redirect: String,
    tok: tokio::sync::Mutex<Tok>,
    path: PathBuf,
    tx: mpsc::Sender<Msg>,
}

impl Api {
    async fn token(&self, force: bool) -> Result<String, String> {
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

    async fn relogin(&self) -> Result<librespot_oauth::OAuthToken, String> {
        browser_login(self.oauth.clone(), &self.redirect).await
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

/// Browser login, off the runtime: librespot's callback listener blocks its thread.
async fn browser_login(oauth: Arc<OAuthClient>, redirect: &str) -> Result<librespot_oauth::OAuthToken, String> {
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

fn parse_track(v: &Value) -> Option<Track> {
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
    })
}

fn liked() -> Playlist {
    Playlist { id: None, name: "♥  Liked Songs".into() } // the heart overhangs its cell in many fonts
}

async fn load_playlists(api: Arc<Api>, tx: mpsc::Sender<Msg>) {
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
async fn load_tracks(api: Arc<Api>, sess: Sess, tx: mpsc::Sender<Msg>, view: u64, pl: Playlist, cached: Arc<Vec<Track>>) {
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

async fn liked_page(api: Arc<Api>, offset: usize) -> Result<(Vec<Track>, usize), String> {
    let v = api.get(&format!("{API}/me/tracks?limit=50&offset={offset}")).await?;
    let page = v["items"].as_array().into_iter().flatten().filter_map(|it| parse_track(&it["track"])).collect();
    Ok((page, v["total"].as_u64().unwrap_or(0) as usize))
}

/// The full Liked Songs list, or None if `cached` is still current.
async fn liked_tracks(api: Arc<Api>, cached: &[Track], tx: &mpsc::Sender<Msg>, view: u64) -> Result<Option<Vec<Track>>, String> {
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

/// spotify:<kind>:<base62> for a stored SpotifyId (0 = unknown).
fn id_uri(kind: &str, id: u128) -> Option<String> {
    if id == 0 {
        return None;
    }
    Some(format!("spotify:{kind}:{}", librespot_core::SpotifyId { id }.to_base62().ok()?))
}

fn pb_track(uri: &str, t: &PbTrack) -> Track {
    let id = |gid: &[u8]| librespot_core::SpotifyId::from_raw(gid).map_or(0, |i| i.id);
    Track {
        uri: uri.into(),
        name: t.name().to_string(),
        artists: t.artist.iter().map(|a| a.name()).collect::<Vec<_>>().join(", "),
        lead: t.artist.first().map_or(0, |a| a.name().len()),
        album: t.album.name().to_string(),
        cover: t.album.cover_group.image.iter().max_by_key(|i| i.width()).map(|i| {
            let hex: String = i.file_id().iter().map(|b| format!("{b:02x}")).collect();
            format!("https://i.scdn.co/image/{hex}")
        }),
        ms: t.duration() as u64,
        album_id: id(t.album.gid()),
        artist_id: t.artist.first().map_or(0, |a| id(a.gid())),
    }
}

/// Resolve track uris to names in batches, streaming them to the view in order (unless it's
/// showing a cached copy, `stream` false); returns them all.
/// `dedupe` drops repeats of the same song (single + album versions in a discography).
async fn send_tracks(
    session: &Session,
    uris: &[String],
    dedupe: bool,
    stream: bool,
    tx: &mpsc::Sender<Msg>,
    view: u64,
) -> Result<Vec<Track>, String> {
    let mut seen = std::collections::HashSet::new();
    let mut all = vec![];
    for chunk in uris.chunks(200) {
        let mut found = batch_meta::<PbTrack>(session, chunk, ExtensionKind::TRACK_V4).await?;
        let tracks: Vec<Track> = chunk
            .iter()
            .filter_map(|u| found.remove(u).map(|t| pb_track(u, &t)))
            .filter(|t| !dedupe || seen.insert((t.name.to_lowercase(), t.artists.to_lowercase())))
            .collect();
        all.extend_from_slice(&tracks);
        if stream {
            let _ = tx.send(Msg::Tracks { view, tracks, done: false });
        }
    }
    Ok(all)
}

/// The playlist's tracks, or None if `cached` is still current. Only songs the cache
/// doesn't have get looked up.
async fn playlist_tracks(sess: Sess, id: &str, cached: &[Track], tx: &mpsc::Sender<Msg>, view: u64) -> Result<Option<Vec<Track>>, String> {
    use librespot_metadata::Metadata;
    let session = ready(sess).await?;
    let uri = SpotifyUri::from_uri(&format!("spotify:playlist:{id}")).map_err(|e| e.to_string())?;
    let pl = librespot_metadata::Playlist::get(&session, &uri).await.map_err(|e| format!("playlist: {e}"))?;
    let uris: Vec<String> = pl
        .tracks()
        .filter(|u| matches!(u, SpotifyUri::Track { .. }))
        .filter_map(|u| u.to_uri().ok())
        .collect();
    if cached.is_empty() {
        return send_tracks(&session, &uris, false, true, tx, view).await.map(Some);
    }
    if uris.len() == cached.len() && uris.iter().zip(cached).all(|(u, t)| *u == t.uri) {
        return Ok(None);
    }
    let known: HashMap<&str, &Track> = cached.iter().map(|t| (t.uri.as_str(), t)).collect();
    let missing: Vec<String> = uris.iter().filter(|u| !known.contains_key(u.as_str())).cloned().collect();
    let found = batch_meta::<PbTrack>(&session, &missing, ExtensionKind::TRACK_V4).await?;
    Ok(Some(
        uris.iter()
            .filter_map(|u| known.get(u.as_str()).map(|t| (*t).clone()).or_else(|| found.get(u).map(|t| pb_track(u, t))))
            .collect(),
    ))
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

/// An artist's albums, then singles/EPs, each newest first.
async fn artist_albums(session: &Session, uri: &str) -> Result<Vec<String>, String> {
    let a = batch_meta::<PbArtist>(session, &[uri.to_string()], ExtensionKind::ARTIST_V4)
        .await?
        .remove(uri)
        .ok_or("artist not found")?;
    // each group lists regional/edition variants of one release; the first is the canonical one
    Ok(a.album_group
        .iter()
        .chain(&a.single_group)
        .filter_map(|g| g.album.first())
        .filter_map(|al| gid_uri("album", al.gid()))
        .collect())
}

/// Album or artist (albums + singles/EPs, newest first) as a flat track list. Like playlists,
/// a cached copy may already be showing; it's replaced only if the fresh one differs.
async fn link_tracks(sess: Sess, tx: mpsc::Sender<Msg>, view: u64, uri: String, cached: Arc<Vec<Track>>) {
    let res = async {
        let session = ready(sess).await?;
        let (albums, dedupe) = if uri.starts_with("spotify:artist:") {
            (artist_albums(&session, &uri).await?, true)
        } else {
            (vec![uri.clone()], false)
        };
        let uris = album_track_uris(&session, &albums).await?;
        let tracks = send_tracks(&session, &uris, dedupe, cached.is_empty(), &tx, view).await?;
        save_tracks(&tracks_path(&uri_key(&uri)), &tracks);
        if !cached.is_empty() && !same_uris(tracks.iter().map(|t| &t.uri), cached.iter().map(|t| &t.uri)) {
            let _ = tx.send(Msg::Replace { view, tracks: Arc::new(tracks) });
        }
        Ok::<_, String>(())
    }
    .await;
    if let Err(e) = res {
        let _ = tx.send(Msg::Error(e));
    }
    let _ = tx.send(Msg::Tracks { view, tracks: vec![], done: true });
}

/// An artist's releases as openable album links.
async fn artist_releases(sess: Sess, tx: mpsc::Sender<Msg>, view: u64, uri: String, cached: Vec<Link>) {
    use librespot_protocol::metadata::album::Type;
    let res = async {
        let session = ready(sess).await?;
        let albums = artist_albums(&session, &uri).await?;
        let found = batch_meta::<PbAlbum>(&session, &albums, ExtensionKind::ALBUM_V4).await?;
        let mut links: Vec<_> = albums
            .iter()
            .filter_map(|u| {
                let a = found.get(u)?;
                let kind = match a.type_() {
                    Type::SINGLE => "single",
                    Type::EP => "EP",
                    Type::COMPILATION => "compilation",
                    _ => "album",
                };
                let n: usize = a.disc.iter().map(|d| d.track.len()).sum();
                let year = if a.date.year() > 0 { format!("{} ", a.date.year()) } else { String::new() };
                let sub = format!("{year}{kind} · {n} song{}", if n == 1 { "" } else { "s" });
                let d = &a.date;
                let single = a.type_() == Type::SINGLE;
                Some(((d.year(), d.month(), d.day()), Link { uri: u.clone(), name: a.name().into(), sub, single }))
            })
            .collect();
        // Newest release first, mixing albums, EPs and singles.
        links.sort_by_key(|(date, _)| std::cmp::Reverse(*date));
        let links: Vec<Link> = links.into_iter().map(|(_, l)| l).collect();
        save_links(&releases_path(&uri), &links);
        if cached.is_empty() {
            let _ = tx.send(Msg::Links { view, links, done: true });
        } else {
            if !same_uris(links.iter().map(|l| &l.uri), cached.iter().map(|l| &l.uri))
                || links.iter().zip(&cached).any(|(a, b)| a.name != b.name || a.sub != b.sub)
            {
                let _ = tx.send(Msg::ReplaceLinks { view, links });
            }
            let _ = tx.send(Msg::Links { view, links: vec![], done: true });
        }
        Ok::<_, String>(())
    }
    .await;
    if let Err(e) = res {
        let _ = tx.send(Msg::Error(e));
        let _ = tx.send(Msg::Links { view, links: vec![], done: true });
    }
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
    let single = v["album_type"] == "single";
    Some(Link { uri: v["uri"].as_str()?.into(), name: v["name"].as_str().unwrap_or("?").into(), sub, single })
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
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache"));
    base.join("spot")
}

fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
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
    let s = Session::new(SessionConfig::default(), Some(cache.clone()));
    login(&s, cache, creds).await?;
    Ok(s)
}

async fn login(s: &Session, cache: &Cache, creds: Option<Credentials>) -> Result<(), String> {
    let creds = creds.or_else(|| cache.credentials()).ok_or("no saved login")?;
    s.connect(creds, true).await.map_err(|e| format!("login failed: {e}"))
}

/// Streaming + internal APIs need a session from Spotify's own client id. Only needed once:
/// librespot stores reusable credentials afterwards. Never used for the Web API.
async fn approve_session(cache: &Cache) -> Result<Session, String> {
    let oauth = OAuthClientBuilder::new(SESSION_CLIENT_ID, SESSION_REDIRECT, vec!["streaming"])
        .open_in_browser()
        .build()
        .map_err(|e| e.to_string())?;
    let t = browser_login(Arc::new(oauth), SESSION_REDIRECT).await?;
    connect(cache, Some(Credentials::with_access_token(t.access_token))).await
}

/// Log the player's (not yet connected) session in with the saved credentials, in the background.
async fn start_session(s: Session, cache: Cache, tx: mpsc::Sender<Msg>) {
    let r = match login(&s, &cache, None).await {
        Ok(()) => Ok(s),
        Err(_) => {
            let _ = tx.send(Msg::Status("approve the audio session in your browser…".into()));
            approve_session(&cache).await
        }
    };
    let _ = tx.send(Msg::Connected(r));
}

// ---------- Last.fm ----------
// Optional scrobbling: on when ~/.config/spot/lastfm holds an API key and shared secret (one per
// line), or LASTFM_API_KEY / LASTFM_SECRET are set. The session key from the one-time browser
// login is kept in ~/.cache/spot/lastfm_session. Plays are queued in ~/.cache/spot/scrobbles
// first, so ones that couldn't be sent (offline, quit mid-request) go out later.

const LASTFM_API: &str = "https://ws.audioscrobbler.com/2.0/";

struct LastFm {
    http: reqwest::Client,
    key: String,
    secret: String,
    sk: String,
    queue: PathBuf,
    file: std::sync::Mutex<()>,          // queue file read-modify-writes
    draining: tokio::sync::Mutex<()>,    // one drain at a time, so nothing is sent twice
    tx: mpsc::Sender<Msg>,
}

/// A play in progress. Only time spent actually playing counts, so seeking can't fake a listen.
struct Listen {
    track: Track,
    started: u64, // unix seconds, set when it first starts playing
    listened: Duration,
    since: Option<Instant>, // playing since
    announced: bool,        // sent as "now playing"
}

impl Listen {
    /// Last.fm's rule: tracks over 30s count once half of them (or 4 minutes) has been heard.
    fn counts(&self) -> bool {
        let heard = self.listened + self.since.map_or(Duration::ZERO, |s| s.elapsed());
        self.track.ms > 30_000 && heard >= Duration::from_millis(self.track.ms / 2).min(Duration::from_secs(240))
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Signed POST to the Last.fm API. Errors carry Last.fm's error code (0 = network / bad reply).
async fn lastfm_call(
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

fn lastfm_creds() -> Option<(String, String)> {
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
fn lastfm_setup(rt: &tokio::runtime::Runtime, http: &reqwest::Client, redirect: &str, tx: mpsc::Sender<Msg>) -> Option<Arc<LastFm>> {
    let (key, secret) = lastfm_creds()?;
    let path = cache_dir().join("lastfm_session");
    let sk = match fs::read_to_string(&path).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        Some(sk) => sk,
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
fn lastfm_token(key: &str, redirect: &str) -> Result<String, String> {
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
    async fn call(&self, method: &str, mut params: Vec<(String, String)>) -> Result<Value, (i64, String)> {
        params.push(("sk".into(), self.sk.clone()));
        lastfm_call(&self.http, &self.key, &self.secret, method, params).await
    }

    fn now_playing(self: &Arc<Self>, rt: &tokio::runtime::Runtime, t: &Track) {
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
    fn queue(&self, l: &Listen) {
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
    async fn drain(&self) {
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
fn lead_artist(t: &Track) -> &str {
    t.artists.get(..t.lead).filter(|a| !a.is_empty()).unwrap_or(&t.artists)
}

// ---------- disk cache ----------
// Track lists are plain text, one tab-separated track per line, so even a big library loads in
// a few ms at startup. Writes go through a temp file so a crash can't leave half a list.

fn tracks_path(key: &str) -> PathBuf {
    cache_dir().join("lists").join(key)
}

/// Cache file name for an album / artist uri (`spotify:album:…` → `album_…`).
fn uri_key(uri: &str) -> String {
    uri.trim_start_matches("spotify:").replace(':', "_")
}

/// An artist's releases (the albums & singles page), one `uri, single, name, sub` per line.
fn releases_path(uri: &str) -> PathBuf {
    cache_dir().join("releases").join(uri_key(uri))
}

fn same_uris<'a>(a: impl ExactSizeIterator<Item = &'a String>, b: impl ExactSizeIterator<Item = &'a String>) -> bool {
    a.len() == b.len() && a.zip(b).all(|(x, y)| x == y)
}

fn read_links(path: &std::path::Path) -> Vec<Link> {
    let s = fs::read_to_string(path).unwrap_or_default();
    s.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let (uri, single, name, sub) = (f.next()?, f.next()?, f.next()?, f.next()?);
            Some(Link { uri: uri.into(), single: single == "1", name: name.into(), sub: sub.into() })
        })
        .collect()
}

fn save_links(path: &std::path::Path, links: &[Link]) {
    use std::fmt::Write;
    let mut s = String::new();
    for l in links {
        let _ = writeln!(s, "{}\t{}\t{}\t{}", l.uri, l.single as u8, clean(&l.name), clean(&l.sub));
    }
    write_atomic(path, &s);
}

fn last_path() -> PathBuf {
    cache_dir().join("last")
}

fn write_atomic(path: &std::path::Path, data: &str) {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("tmp");
    if fs::write(&tmp, data).is_ok() {
        let _ = fs::rename(&tmp, path);
    }
}

/// Tabs / newlines would break the line format; same byte length, so `lead` stays valid.
fn clean(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains(['\t', '\n', '\r']) { s.replace(['\t', '\n', '\r'], " ").into() } else { s.into() }
}

fn push_tracks(out: &mut String, tracks: &[Track]) {
    use std::fmt::Write;
    for t in tracks {
        let _ = writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:x}\t{:x}",
            t.uri,
            clean(&t.name),
            clean(&t.artists),
            t.lead,
            clean(&t.album),
            t.cover.as_deref().unwrap_or(""),
            t.ms,
            t.album_id,
            t.artist_id
        );
    }
}

fn parse_line(l: &str) -> Option<Track> {
    let mut f = l.split('\t');
    let (uri, name, artists) = (f.next()?, f.next()?, f.next()?);
    let lead = f.next()?.parse().ok().filter(|&n| artists.is_char_boundary(n)).unwrap_or(0);
    let (album, cover, ms) = (f.next()?, f.next()?, f.next()?.parse().ok()?);
    let mut id = || f.next().and_then(|x| u128::from_str_radix(x, 16).ok()).unwrap_or(0);
    Some(Track {
        uri: uri.into(),
        name: name.into(),
        artists: artists.into(),
        lead,
        album: album.into(),
        cover: (!cover.is_empty()).then(|| cover.into()),
        ms,
        album_id: id(),
        artist_id: id(),
    })
}

fn read_tracks(path: &std::path::Path) -> Vec<Track> {
    fs::read_to_string(path).map(|s| s.lines().filter_map(parse_line).collect()).unwrap_or_default()
}

fn save_tracks(path: &std::path::Path, tracks: &[Track]) {
    let mut s = String::with_capacity(tracks.len() * 200);
    push_tracks(&mut s, tracks);
    write_atomic(path, &s);
}

/// Saved playlist list (Liked Songs first, which is never stored).
fn read_playlists() -> Vec<Playlist> {
    let s = fs::read_to_string(cache_dir().join("playlists")).unwrap_or_default();
    let saved = s.lines().filter_map(|l| {
        let (id, name) = l.split_once('\t')?;
        Some(Playlist { id: Some(id.into()), name: name.into() })
    });
    std::iter::once(liked()).chain(saved).collect()
}

fn save_playlists(pls: &[Playlist]) {
    use std::fmt::Write;
    let mut s = String::new();
    for p in pls {
        if let Some(id) = &p.id {
            let _ = writeln!(s, "{id}\t{}", clean(&p.name));
        }
    }
    write_atomic(&cache_dir().join("playlists"), &s);
}

/// The queue as it was at quit, with the playing song's position.
struct Last {
    queue: Queue,
    pos_ms: u64,
    shuffle: bool,
    repeat: Repeat,
}

fn read_last() -> Option<Last> {
    let s = fs::read_to_string(last_path()).ok()?;
    let mut lines = s.lines();
    let (key, name) = (lines.next()?.to_string(), lines.next()?.to_string());
    let mut nums = lines.next()?.split(' ').map(|n| n.parse::<u64>().ok());
    let (pos, pos_ms, shuffle, repeat) = (nums.next()??, nums.next()??, nums.next()??, nums.next()??);
    let order: Vec<usize> = lines.next()?.split(',').filter_map(|n| n.parse().ok()).collect();
    let tracks: Vec<Track> = lines.filter_map(parse_line).collect();
    let mut queue = Queue { key, name, tracks: Arc::new(tracks), order, pos: pos as usize };
    let n = queue.tracks.len();
    if n == 0 {
        return None;
    }
    if queue.order.len() != n || queue.order.iter().any(|&i| i >= n) || queue.pos >= n {
        queue.order = (0..n).collect();
        queue.pos = 0;
    }
    let repeat = match repeat {
        1 => Repeat::All,
        2 => Repeat::One,
        _ => Repeat::Off,
    };
    Some(Last { queue, pos_ms, shuffle: shuffle == 1, repeat })
}

fn save_last(q: &Queue, pos_ms: u64, shuffle: bool, repeat: Repeat) {
    let order: Vec<String> = q.order.iter().map(|i| i.to_string()).collect();
    let mut s = String::with_capacity(q.tracks.len() * 200);
    s += &format!(
        "{}\n{}\n{} {pos_ms} {} {}\n{}\n",
        clean(&q.key),
        clean(&q.name),
        q.pos,
        shuffle as u8,
        repeat as u8,
        order.join(",")
    );
    push_tracks(&mut s, &q.tracks);
    write_atomic(&last_path(), &s);
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
    artist: Option<String>, // artist page (key = artist uri): name, for switching songs/releases
    releases: Vec<Link>,    // artist page: every release, of which `links` is the `filter`ed part
    filter: Releases,
    want: Option<String>,   // track uri to select once it loads (goto album)
}

impl View {
    fn new(id: u64, key: String, title: String, kind: Kind) -> Self {
        View { id, key, title, kind, tracks: Arc::default(), links: vec![], loading: true, artist: None, releases: vec![], filter: Releases::All, want: None }
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
    sess: tokio::sync::watch::Sender<Option<Session>>, // None while connecting
    player: Arc<Player>,
    mixer: Arc<dyn Mixer>,
    media: Option<MediaControls>,

    playlists: Vec<Playlist>,
    pl_state: ListState,
    view: View,
    view_seq: u64,                  // last view id handed out
    history: Vec<(View, usize)>,    // views to go back to (esc), with their selected row
    artist_songs: bool,             // artist pages list every song (else albums & singles)
    releases: Releases,             // which releases new artist pages list (remembered)
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
    loaded: bool, // `cur` is loaded into the player (it waits for the session)
    playing: bool,
    pos_ms: u64,
    pos_at: Option<Instant>,
    status: String,
    note: Option<(String, Instant)>, // brief popup and when it disappears
    retried: bool,
    vol: u16,               // user volume; the mixer dips below it during fades
    want_play: bool,        // intent, flips immediately (playing lags until the player confirms)
    fade_gen: Arc<std::sync::atomic::AtomicU64>, // bumps cancel an in-flight fade
    vol_until: Option<Instant>, // volume meter shows until then, after a change
    help: bool,                 // key help on the bottom line
    lastfm: Option<Arc<LastFm>>,
    listen: Option<Listen>, // current play, for scrobbling (only with lastfm)
    quit: bool,
}

impl App {
    fn new_view(&mut self, key: String, title: String, kind: Kind) -> View {
        self.view_seq += 1;
        View::new(self.view_seq, key, title, kind)
    }

    /// Put `v` in the right pane. `back` keeps the current view for esc to return to.
    fn show(&mut self, v: View, back: bool) {
        let sel = self.selected_track().unwrap_or(0);
        self.find = None;
        let old = std::mem::replace(&mut self.view, v);
        if back && !old.key.is_empty() {
            self.history.push((old, sel));
            if self.history.len() > 50 {
                self.history.remove(0);
            }
        }
        self.tr_state = TableState::default().with_selected(0);
    }

    fn back(&mut self) {
        let Some((v, sel)) = self.history.pop() else { return };
        self.view = v;
        self.find = None;
        self.tr_state = TableState::default().with_selected(sel);
        self.focus = Focus::Tracks;
    }

    /// The view a message is for: the open one, or one waiting in the history (still loading).
    fn view_mut(&mut self, id: u64) -> Option<&mut View> {
        if self.view.id == id {
            return Some(&mut self.view);
        }
        self.history.iter_mut().map(|(v, _)| v).find(|v| v.id == id)
    }

    fn session(&self) -> Option<Session> {
        self.sess.borrow().clone()
    }

    /// Opens instantly from the disk cache (or the playing queue), then refreshes in the background.
    fn open_playlist(&mut self, i: usize) {
        let Some(pl) = self.playlists.get(i).cloned() else { return };
        let key = pl.key();
        let mut v = self.new_view(key.clone(), pl.name.clone(), Kind::Tracks);
        v.tracks = match &self.queue {
            Some(q) if q.key == key => q.tracks.clone(),
            _ => Arc::new(read_tracks(&tracks_path(&key))),
        };
        v.loading = v.tracks.is_empty();
        let cached = v.tracks.clone();
        self.rt.spawn(load_tracks(self.api.clone(), self.sess.subscribe(), self.tx.clone(), v.id, pl, cached));
        self.show(v, true);
    }

    fn open_search(&mut self, q: String) {
        let kind = self.search_kind;
        let title = format!("Search {}: {q}", kind.label());
        let v = self.new_view(format!("search:{}:{q}", kind.label()), title, kind);
        self.rt.spawn(search(self.api.clone(), self.tx.clone(), v.id, q, kind));
        self.show(v, true);
        self.focus = Focus::Tracks;
    }

    fn open_album(&mut self, uri: String, title: String, want: Option<String>) {
        let mut v = self.new_view(uri.clone(), title, Kind::Tracks);
        v.want = want;
        v.tracks = Arc::new(read_tracks(&tracks_path(&uri_key(&uri))));
        v.loading = v.tracks.is_empty();
        let cached = v.tracks.clone();
        self.rt.spawn(link_tracks(self.sess.subscribe(), self.tx.clone(), v.id, uri, cached));
        self.show(v, true);
        self.focus = Focus::Tracks;
    }

    /// Artist page in the current mode: every song (newest release first), or albums & singles.
    fn artist_view(&mut self, uri: String, name: String) -> View {
        let (title, kind) = if self.artist_songs {
            (format!("{name} · all songs"), Kind::Tracks)
        } else {
            (format!("{name} · {}", self.releases.label()), Kind::Albums)
        };
        let mut v = self.new_view(uri.clone(), title, kind);
        v.artist = Some(name);
        v.filter = self.releases;
        let (s, tx, id) = (self.sess.subscribe(), self.tx.clone(), v.id);
        if self.artist_songs {
            v.tracks = Arc::new(read_tracks(&tracks_path(&uri_key(&uri))));
            v.loading = v.tracks.is_empty();
            self.rt.spawn(link_tracks(s, tx, id, uri, v.tracks.clone()));
        } else {
            let cached = read_links(&releases_path(&uri));
            v.loading = cached.is_empty();
            v.links = cached.iter().filter(|l| v.filter.keeps(l)).cloned().collect();
            v.releases = cached.clone();
            self.rt.spawn(artist_releases(s, tx, id, uri, cached));
        }
        v
    }

    fn open_artist(&mut self, uri: String, name: String) {
        let v = self.artist_view(uri, name);
        self.show(v, true);
        self.focus = Focus::Tracks;
    }

    /// `v` on an artist page: switch between all songs and albums & singles (remembered).
    fn toggle_artist_mode(&mut self) {
        let Some(name) = self.view.artist.clone() else {
            self.note = Some(("v works on artist pages".into(), Instant::now() + Duration::from_millis(1500)));
            return;
        };
        self.artist_songs = !self.artist_songs;
        let v = self.artist_view(self.view.key.clone(), name);
        self.show(v, false);
    }

    /// `f` on an artist's releases: cycle all / albums / singles (remembered).
    fn cycle_releases(&mut self) {
        let Some(name) = self.view.artist.clone().filter(|_| self.view.kind == Kind::Albums) else {
            let msg = if self.view.artist.is_some() { "f works on releases (v)" } else { "f works on artist pages" };
            self.note = Some((msg.into(), Instant::now() + Duration::from_millis(1500)));
            return;
        };
        let keep = self.selected_track().and_then(|i| self.view.links.get(i)).map(|l| l.uri.clone());
        self.releases = self.releases.next();
        let v = &mut self.view;
        v.filter = self.releases;
        v.title = format!("{name} · {}", v.filter.label());
        v.links = v.releases.iter().filter(|l| v.filter.keeps(l)).cloned().collect();
        let sel = keep.and_then(|u| v.links.iter().position(|l| l.uri == u)).unwrap_or(0);
        self.find = None;
        self.matches.clear();
        self.tr_state = TableState::default().with_selected(sel);
    }

    /// Open an album or artist from a search result or an artist's releases.
    fn open_link(&mut self, i: usize) {
        let Some(l) = self.view.links.get(i).cloned() else { return };
        if self.view.kind == Kind::Artists {
            return self.open_artist(l.uri, l.name);
        }
        let by = self.view.artist.as_ref().unwrap_or(&l.sub);
        self.open_album(l.uri.clone(), format!("{} — {by}", l.name), None);
    }

    /// What goto album/artist acts on: the highlighted song, else the one playing.
    fn target_track(&self) -> Option<Track> {
        if self.focus == Focus::Tracks
            && self.view.kind == Kind::Tracks
            && let Some(t) = self.selected_track().and_then(|i| self.view.tracks.get(i))
        {
            return Some(t.clone());
        }
        self.queue.as_ref().and_then(|q| q.tracks.get(q.idx()).cloned())
    }

    fn goto_album(&mut self) {
        let Some(t) = self.target_track() else { return };
        let Some(uri) = id_uri("album", t.album_id) else { return };
        self.open_album(uri, format!("{} — {}", t.album, t.artists), Some(t.uri));
    }

    fn goto_artist(&mut self) {
        let Some(t) = self.target_track() else { return };
        let Some(uri) = id_uri("artist", t.artist_id) else { return };
        self.open_artist(uri, t.artists[..t.lead].to_string());
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

    /// Reconnect a session that died (sleep, network change). Still connecting: nothing to do.
    fn ensure_session(&mut self) {
        if !self.session().is_some_and(|s| s.is_invalid()) {
            return;
        }
        log::info!("session is invalid, reconnecting");
        self.status = "reconnecting…".into();
        match self.rt.block_on(connect(&self.cache, None)) {
            Ok(s) => {
                log::info!("reconnected");
                self.player.set_session(s.clone());
                self.sess.send_replace(Some(s));
                self.status.clear();
            }
            Err(e) => {
                log::error!("reconnect failed: {e}");
                self.status = e;
            }
        }
    }

    fn on_connected(&mut self, s: Session) {
        log::info!("audio session connected");
        self.player.set_session(s.clone());
        self.sess.send_replace(Some(s));
        if self.status.starts_with("approve the audio session") {
            self.status.clear();
        }
        // the song restored from last time (or picked while connecting): fetch it now, so
        // space starts it without waiting on the network
        self.load_cur();
    }

    /// Load `cur` into the player at the current position, playing if that's wanted.
    fn load_cur(&mut self) {
        if self.loaded || self.session().is_none() {
            return;
        }
        let Some(uri) = self.cur.clone() else { return };
        log::debug!("load {uri:?} play {} at {}ms", self.want_play, self.pos_ms);
        self.player.load(uri, self.want_play, self.pos_ms as u32);
        self.loaded = true;
    }

    fn set_cur(&mut self, t: &Track, pos_ms: u64) {
        let Ok(uri) = SpotifyUri::from_uri(&t.uri) else { return };
        self.end_listen();
        if self.lastfm.is_some() {
            self.listen = Some(Listen { track: t.clone(), started: 0, listened: Duration::ZERO, since: None, announced: false });
        }
        if let Some(m) = &mut self.media {
            let _ = m.set_metadata(MediaMetadata {
                title: Some(&t.name),
                artist: Some(&t.artists),
                album: Some(&t.album),
                cover_url: t.cover.as_deref(),
                duration: Some(Duration::from_millis(t.ms)),
            });
        }
        self.cur = Some(uri);
        self.loaded = false;
        self.pos_ms = pos_ms;
        self.pos_at = None;
    }

    /// Scrobble the play that just ended, if it counts.
    fn end_listen(&mut self) {
        if let Some(l) = self.listen.take()
            && l.counts()
            && let Some(fm) = &self.lastfm
        {
            fm.queue(&l);
            let fm = fm.clone();
            self.rt.spawn(async move { fm.drain().await });
        }
    }

    fn pause_listen(&mut self) {
        if let Some(l) = &mut self.listen
            && let Some(s) = l.since.take()
        {
            l.listened += s.elapsed();
        }
    }

    fn play_current(&mut self) {
        let Some(q) = &self.queue else { return };
        let Some(t) = q.tracks.get(q.idx()).cloned() else { return };
        log::info!("play {} ({} - {})", t.uri, t.name, t.artists);
        self.ensure_session();
        self.reset_fade();
        self.set_cur(&t, 0);
        self.load_cur();
    }

    /// Put last session's queue back, paused on the song that was playing.
    fn restore(&mut self, last: Last) {
        self.shuffle = last.shuffle;
        self.repeat = last.repeat;
        let t = last.queue.tracks[last.queue.idx()].clone();
        self.queue = Some(last.queue);
        self.set_cur(&t, last.pos_ms);
        self.sync_media();
    }

    fn save_last(&self) {
        match &self.queue {
            Some(q) => save_last(q, self.position(), self.shuffle, self.repeat),
            None => {
                let _ = fs::remove_file(last_path());
            }
        }
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
            if self.loaded {
                self.fade(false);
            }
        } else if self.cur.is_some() {
            self.ensure_session();
            if self.loaded {
                self.want_play = true;
                self.fade(true);
            } else {
                // still connecting: on_connected starts it
                self.reset_fade();
                self.load_cur();
            }
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
        if self.loaded {
            self.player.seek(ms as u32);
        } else {
            self.pos_ms = ms;
        }
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
        self.vol_until = Some(Instant::now() + Duration::from_millis(1500));
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
        log::debug!("player event: {ev:?}");
        match ev {
            PlayerEvent::Playing { track_id, position_ms, .. } if is_cur(&track_id, &self.cur) => {
                self.playing = true;
                self.fails = 0;
                self.pos_ms = position_ms as u64;
                self.pos_at = Some(Instant::now());
                if self.status.starts_with("can't play") {
                    self.status.clear();
                }
                if let Some(l) = &mut self.listen {
                    l.since.get_or_insert_with(Instant::now);
                    if !l.announced {
                        l.announced = true;
                        l.started = unix_now();
                        if let Some(fm) = &self.lastfm {
                            fm.now_playing(&self.rt, &l.track);
                        }
                    }
                }
                trim_heap();
            }
            PlayerEvent::Paused { track_id, position_ms, .. } if is_cur(&track_id, &self.cur) => {
                self.playing = false;
                self.pos_ms = position_ms as u64;
                self.pos_at = None;
                self.pause_listen();
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
                self.pause_listen();
                // librespot logs the actual reason (key / file / CDN error) just before this
                let track = self.queue.as_ref().and_then(|q| q.tracks.get(q.idx()));
                let session = match self.session() {
                    None => "not connected",
                    Some(s) if s.is_invalid() => "invalid",
                    Some(_) => "ok",
                };
                log::warn!(
                    "playback failed: {} ({} - {}), session {session}, retried {}, fails {}",
                    track_id.to_uri().unwrap_or_default(),
                    track.map_or("?", |t| &t.name),
                    track.map_or("?", |t| &t.artists),
                    self.retried,
                    self.fails,
                );
                // dead session after sleep/network change shows up as unavailable: retry once
                if !self.retried && self.session().is_some_and(|s| s.is_invalid()) {
                    log::info!("session is dead, reconnecting and retrying");
                    self.retried = true;
                    self.play_current();
                } else {
                    self.fails += 1;
                    if self.fails > self.queue.as_ref().map_or(0, |q| q.order.len()) {
                        log::warn!("every track in the queue failed, stopping");
                        self.status = "nothing in this list is playable".into();
                        self.player.stop();
                        self.want_play = false;
                    } else {
                        log::info!("skipping to the next track");
                        self.status = "can't play that track, skipping".into();
                        self.skip(1);
                    }
                }
            }
            PlayerEvent::Stopped { .. } => {
                self.playing = false;
                self.pos_at = None;
                self.pause_listen();
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
        if self.help {
            // any key closes the key help
            self.help = false;
            return;
        }
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
            KeyCode::Char('?') => self.help = !self.help,
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
            KeyCode::Char('a') => self.goto_album(),
            KeyCode::Char('A') => self.goto_artist(),
            KeyCode::Char('v') => self.toggle_artist_mode(),
            KeyCode::Char('f') => self.cycle_releases(),
            KeyCode::Esc | KeyCode::Backspace => self.back(),
            KeyCode::Char('n') => self.skip(1),
            KeyCode::Char('p') => self.prev(),
            KeyCode::Char('.') => self.seek_by(SeekDirection::Forward, Duration::from_secs(10)),
            KeyCode::Char(',') => self.seek_by(SeekDirection::Backward, Duration::from_secs(10)),
            KeyCode::Char('+') | KeyCode::Char('=') => self.set_volume(3277),
            KeyCode::Char('-') => self.set_volume(-3277),
            KeyCode::Char('o') => {
                // jump to what's playing
                if let Some(q) = &self.queue {
                    let (idx, key, name, tracks) = (q.idx(), q.key.clone(), q.name.clone(), q.tracks.clone());
                    let mut v = self.new_view(key, name, Kind::Tracks);
                    v.tracks = tracks;
                    v.loading = false;
                    self.show(v, true);
                    self.tr_state = TableState::default().with_selected(idx);
                    self.focus = Focus::Tracks;
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

/// Truncate to at most `max` terminal columns, ending in "…" when cut.
fn fit(s: &str, max: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if s.width() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw > max - 1 {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.trim_end().to_string() + "…"
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

/// Ask the terminal for its background (OSC 11): sets BG, and LIGHT unless `SPOT_THEME=light|dark`
/// forces it. Must run before the input thread starts reading stdin, or it eats the reply.
fn probe_terminal() {
    use std::sync::atomic::Ordering::Relaxed;
    use terminal_colorsaurus::{QueryOptions, ThemeMode, color_palette};
    let p = color_palette(QueryOptions::default()).ok();
    if let Some(p) = &p {
        let (r, g, b) = p.background.scale_to_8bit();
        BG.store(u32::from_be_bytes([0, r, g, b]), Relaxed);
    }
    let light = match std::env::var("SPOT_THEME").as_deref() {
        Ok("light") => true,
        Ok("dark") => false,
        _ => p.is_some_and(|p| p.theme_mode() == ThemeMode::Light),
    };
    LIGHT.store(light, Relaxed);
}

/// Terminal background as 0xRRGGBB; NO_BG until the terminal tells us.
static BG: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(NO_BG);
const NO_BG: u32 = u32::MAX;

/// `a` moved `t` of the way toward `b` (0xRRGGBB).
fn mix(a: u32, b: u32, t: f32) -> u32 {
    let ch = |c: u32, sh: u32| ((c >> sh) & 0xff) as f32;
    [16, 8, 0].iter().fold(0, |acc, &sh| acc | (((ch(a, sh) + (ch(b, sh) - ch(a, sh)) * t).round() as u32) << sh))
}

fn rgb_hex(c: Color) -> Option<u32> {
    match c {
        Color::Rgb(r, g, b) => Some(u32::from_be_bytes([0, r, g, b])),
        _ => None,
    }
}

fn bg_hex() -> Option<u32> {
    Some(BG.load(std::sync::atomic::Ordering::Relaxed)).filter(|&b| b != NO_BG)
}

fn hex_rgb(h: u32) -> Color {
    Color::Rgb((h >> 16) as u8, (h >> 8) as u8, h as u8)
}

/// Background for the sidebar and player bar: the terminal's own background nudged a few percent
/// toward the theme's text color. None without truecolor or a known background (no tint then).
fn panel(th: &Theme) -> Option<u32> {
    let light = LIGHT.load(std::sync::atomic::Ordering::Relaxed);
    Some(mix(bg_hex()?, rgb_hex(th.text)?, if light { 0.045 } else { 0.06 }))
}

/// OSC 11 reply body, `11;rgb:RRRR/GGGG/BBBB` (1-4 hex digits per channel), as 0xRRGGBB.
fn parse_bg_reply(body: &[u8]) -> Option<u32> {
    let rest = std::str::from_utf8(body).ok()?.strip_prefix("11;rgb:")?;
    let mut out = 0;
    for part in rest.split('/') {
        if !(1..=4).contains(&part.len()) {
            return None;
        }
        let max = (1u32 << (4 * part.len())) - 1;
        out = (out << 8) | (u32::from_str_radix(part, 16).ok()? * 255 / max);
    }
    Some(out)
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

/// 10435 -> "10,435"
fn num(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn plural(n: usize, what: &str) -> String {
    format!("{} {what}{}", num(n), if n == 1 { "" } else { "s" })
}

/// Total length of a list: "42 min", "3 h 5 min", "712 h".
fn hours(ms: u64) -> String {
    let m = (ms + 30_000) / 60_000;
    match (m / 60, m % 60) {
        (0, m) => format!("{m} min"),
        (h, _) if h >= 10 => format!("{} h", num(h as usize)),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// `w` cells of a thick filled rule followed by a thin empty one, optionally with a knob.
fn meter(w: usize, ratio: f64, knob: bool, th: &Theme) -> Vec<Span<'static>> {
    let filled = ((ratio.clamp(0.0, 1.0) * w as f64).round() as usize).min(w);
    let (filled, knob) = if knob && w > 0 { (filled.min(w - 1), 1) } else { (filled, 0) };
    vec![
        Span::styled("━".repeat(filled), Style::new().fg(th.accent)),
        Span::styled("●".repeat(knob), Style::new().fg(th.accent)),
        Span::styled("─".repeat(w - filled - knob), Style::new().fg(th.dim)),
    ]
}

/// Key hints for the status line, most useful first; trimmed from the end when space runs out.
fn hints(app: &App) -> Vec<(&'static str, &'static str)> {
    let mut h = vec![];
    match app.focus {
        Focus::Playlists => {
            h.push(("⏎", "open"));
            h.push(("/", "search"));
            h.push(("space", "pause"));
            h.push(("s", "shuffle"));
            h.push(("r", "repeat"));
        }
        Focus::Tracks => {
            h.push(("⏎", if app.view.kind == Kind::Tracks { "play" } else { "open" }));
            h.push(("/", "search"));
            h.push(("^f", "find"));
            if app.view.artist.is_some() {
                h.push(("v", if app.artist_songs { "releases" } else { "songs" }));
                if app.view.kind == Kind::Albums {
                    h.push(("f", ["all", "albums", "singles"][app.view.filter.next() as usize]));
                }
            }
            if !app.history.is_empty() {
                h.push(("esc", "back"));
            }
            h.push(("a", "album"));
            h.push(("A", "artist"));
            h.push(("s", "shuffle"));
        }
    }
    h.push(("?", "keys"));
    h
}

/// The `?` key block (nano-style, at the bottom): every key in as many columns as fit.
fn help_lines(width: usize, th: &Theme) -> Vec<Line<'static>> {
    const KEYS: &[(&str, &str)] = &[
        ("/", "search"),
        ("ctrl+f", "find in list"),
        ("enter", "play / open"),
        ("space", "pause"),
        ("n  p", "next / previous"),
        (",  .", "seek 10s"),
        ("+  -", "volume"),
        ("s", "shuffle"),
        ("r", "repeat"),
        ("P", "play whole list"),
        ("o", "go to playing"),
        ("a  A", "album / artist"),
        ("v", "artist: songs / releases"),
        ("f", "releases: all / albums / singles"),
        ("esc", "back"),
        ("tab", "switch pane"),
        ("ctrl+t", "theme"),
        ("g  G", "top / bottom"),
        ("q", "quit"),
    ];
    const COL: usize = 36;
    let cols = (width.saturating_sub(1) / COL).clamp(1, 4);
    let rows = KEYS.len().div_ceil(cols);
    let mut out = vec![Line::from(vec![
        Span::styled(" keys", Style::new().fg(th.text).bold()),
        Span::styled("   any key closes", Style::new().fg(th.dim)),
    ])];
    for r in 0..rows {
        let mut spans = vec![Span::raw(" ")];
        for c in 0..cols {
            if let Some((k, d)) = KEYS.get(c * rows + r) {
                spans.push(Span::styled(format!("{k:>7}  "), Style::new().fg(th.accent).bold()));
                spans.push(Span::styled(format!("{d:<COL$}", COL = COL - 9), Style::new().fg(th.sub)));
            }
        }
        out.push(Line::from(spans));
    }
    out
}

fn draw(f: &mut Frame, app: &mut App) {
    use ratatui::{layout::Rect, widgets::HighlightSpacing};
    let th = theme();
    let width = |spans: &[Span]| spans.iter().map(|s| s.width()).sum::<usize>();
    let dim = Style::new().fg(th.dim);
    let body = f.area();

    // A terminal-app frame: two panes split by a thin rule, the player under a rule across the
    // whole width, and a vim-style status line at the bottom carrying the search / find prompts,
    // notices and key hints. `?` swaps the status line for a nano-style key block.
    let help = app.help.then(|| help_lines(body.width as usize, th));
    let bottom_h = help.as_ref().map_or(1, |l| l.len() as u16);
    let [top, rule, player, bottom] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(1), Constraint::Length(3), Constraint::Length(bottom_h)]).areas(body);
    let side_w = (body.width * 26 / 100).clamp(18, 36).min(body.width / 2);
    let [left, sep, right] =
        Layout::horizontal([Constraint::Length(side_w), Constraint::Length(1), Constraint::Fill(1)]).areas(top);
    f.render_widget(Paragraph::new(vec![Line::from("│"); sep.height as usize]).style(dim), sep);
    let join = (sep.x - rule.x) as usize;
    f.render_widget(
        Paragraph::new(Line::from(format!("{}┴{}", "─".repeat(join), "─".repeat((rule.width as usize).saturating_sub(join + 1))))).style(dim),
        rule,
    );
    let line_at = |r: Rect, dy: u16| Rect { y: r.y + dy, height: 1.min(r.height.saturating_sub(dy)), ..r };
    let title_style =
        |focused: bool| if focused { Style::new().fg(th.accent).bold() } else { Style::new().fg(th.sub).bold() };
    // Selection: the theme's highlight softened into the terminal background, plus an accent bar
    // at the left edge while the pane has focus.
    let hl = |focused: bool| {
        let bg = match (bg_hex(), rgb_hex(th.hl)) {
            (Some(s), Some(h)) => hex_rgb(mix(s, h, if focused { 0.75 } else { 0.4 })),
            _ if focused => th.hl,
            _ => th.deep,
        };
        let st = Style::new().bg(bg);
        if focused { st.bold() } else { st }
    };
    let bar_sym = |focused: bool| Span::styled(if focused { "┃" } else { " " }, Style::new().fg(th.accent));
    let pl_focus = app.focus == Focus::Playlists;
    let mark = |playing: bool| if playing { "▶" } else { "‖" };

    // sidebar: title, blank, playlists (the playing one carries the play mark)
    f.render_widget(Paragraph::new(Span::styled("    playlists", title_style(pl_focus))), line_at(left, 0));
    let playing_key = app.queue.as_ref().map(|q| q.key.as_str());
    let name_w = (left.width as usize).saturating_sub(5);
    let items: Vec<ListItem> = app
        .playlists
        .iter()
        .map(|p| {
            let playing = Some(p.key().as_str()) == playing_key;
            let style = if playing {
                Style::new().fg(th.accent).bold()
            } else if p.id.is_none() {
                Style::new().fg(th.pink)
            } else {
                Style::new().fg(th.text)
            };
            let m = if playing { mark(app.playing) } else { " " };
            ListItem::new(format!(" {m} {}", fit(&p.name, name_w))).style(style)
        })
        .collect();
    let list_area = Rect { y: left.y + 2, height: left.height.saturating_sub(2), width: left.width.saturating_sub(1), ..left };
    let pl_list = List::new(items)
        .highlight_style(hl(pl_focus))
        .highlight_symbol(bar_sym(pl_focus))
        .highlight_spacing(HighlightSpacing::Always);
    f.render_stateful_widget(pl_list, list_area, &mut app.pl_state);

    // the open list: title · meta, column header, numbered rows, scrollbar
    let tracks = app.view.kind == Kind::Tracks;
    // by uri: a list refreshed from the cache can have shifted under the queue's copy
    let playing_uri = app.queue.as_ref().filter(|q| q.key == app.view.key).and_then(|q| q.tracks.get(q.idx())).map(|t| t.uri.as_str());
    let shown = app.shown();
    let total = shown.len();
    let meta = if app.view.loading {
        "loading…".into()
    } else if app.view.key.is_empty() {
        String::new()
    } else if tracks {
        let ms = app.view.tracks.iter().map(|t| t.ms).sum();
        format!("{} · {}", plural(app.view.len(), "song"), hours(ms))
    } else {
        let what = match app.view.kind {
            Kind::Artists => "artist",
            _ if app.view.artist.is_some() => match app.view.filter {
                Releases::All => "release",
                Releases::Albums => "album",
                Releases::Singles => "single",
            },
            _ => "album",
        };
        plural(app.view.len(), what)
    };
    let inner = Rect { x: right.x + 1, width: right.width.saturating_sub(3), ..right }; // pad, scrollbar, margin
    let title_row = line_at(inner, 0);
    let title_w = (title_row.width as usize).saturating_sub(Span::raw(&meta).width() + 4);
    f.render_widget(Paragraph::new(Span::styled(format!(" {}", fit(&app.view.title, title_w)), title_style(!pl_focus))), title_row);
    f.render_widget(Paragraph::new(Line::from(Span::styled(meta, dim)).right_aligned()), title_row);

    // Build only the rows that fit: ratatui would otherwise lay out every row of a 10k-song list each frame.
    let table_area = Rect { y: inner.y + 1, height: inner.height.saturating_sub(1), ..inner };
    let height = table_area.height.saturating_sub(1) as usize;
    let sel = app.tr_state.selected().map(|s| s.min(total.saturating_sub(1)));
    let mut off = app.tr_state.offset().min(total.saturating_sub(height));
    if let Some(s) = sel {
        if s < off {
            off = s;
        } else if s >= off + height {
            off = s + 1 - height;
        }
    }
    let num_w = app.view.len().max(1).to_string().len() as u16;
    let wide = table_area.width >= 90; // room for an album column
    let (cols, labels): (Vec<Constraint>, &[&str]) = match (tracks, wide) {
        (true, true) => (
            vec![Constraint::Length(num_w), Constraint::Fill(4), Constraint::Fill(3), Constraint::Fill(3), Constraint::Length(5)],
            &["", "title", "artist", "album", "time"],
        ),
        (true, false) => (
            vec![Constraint::Length(num_w), Constraint::Fill(3), Constraint::Fill(2), Constraint::Length(5)],
            &["", "title", "artist", "time"],
        ),
        (false, _) => (vec![Constraint::Length(num_w), Constraint::Fill(3), Constraint::Fill(2)], &["", "name", "details"]),
    };
    const GAP: u16 = 2;
    // the selection bar takes the first cell
    let cols_area = Rect { x: table_area.x + 1, width: table_area.width.saturating_sub(1), ..table_area };
    let w: Vec<usize> = Layout::horizontal(cols.clone()).spacing(GAP).split(cols_area).iter().map(|r| r.width as usize).collect();
    let cell = |s: &str, k: usize, style: Style| Cell::from(fit(s, w[k])).style(style);
    let numcell = |i: usize, playing: bool, style: Style| {
        let s = if playing { mark(app.playing).to_string() } else { num(i + 1) };
        Cell::from(Line::from(s).right_aligned()).style(style)
    };
    let rows: Vec<Row> = (off..total.min(off + height))
        .filter_map(|r| shown.get(r))
        .map(|i| {
            if !tracks {
                let l = &app.view.links[i];
                return Row::new([
                    numcell(i, false, dim),
                    cell(&l.name, 1, Style::new().fg(th.text)),
                    cell(&l.sub, 2, Style::new().fg(th.sub)),
                ]);
            }
            let t = &app.view.tracks[i];
            let playing = Some(t.uri.as_str()) == playing_uri;
            let [n, name, artist, album, time] = if playing {
                let a = Style::new().fg(th.accent);
                [a.bold(), a.bold(), a, a, a]
            } else {
                [th.dim, th.text, th.sub, th.dim, th.dim].map(|c| Style::new().fg(c))
            };
            let mut cells = vec![numcell(i, playing, n), cell(&t.name, 1, name), cell(&t.artists, 2, artist)];
            if wide {
                cells.push(cell(&t.album, 3, album));
            }
            cells.push(Cell::from(Line::from(mmss(t.ms)).right_aligned()).style(time));
            Row::new(cells)
        })
        .collect();
    let header = Row::new(labels.iter().map(|&l| {
        let line = Line::from(l);
        Cell::from(if l == "time" { line.right_aligned() } else { line })
    }))
    .style(dim);
    let table = Table::new(rows, cols)
        .header(header)
        .column_spacing(GAP)
        .row_highlight_style(hl(!pl_focus))
        .highlight_symbol(Line::from(bar_sym(!pl_focus)))
        .highlight_spacing(HighlightSpacing::Always);
    let mut window = TableState::default().with_selected(sel.map(|s| s - off));
    f.render_stateful_widget(table, table_area, &mut window);
    *app.tr_state.offset_mut() = off;
    if total > height {
        use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};
        let area = Rect { x: table_area.right() + 1, y: table_area.y + 1, width: 1, height: height as u16 };
        let mut st = ScrollbarState::new(total - height + 1).position(off).viewport_content_length(height);
        // thumb only: a track line would read as a divider
        let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(None)
            .thumb_symbol("┃")
            .thumb_style(dim);
        f.render_stateful_widget(sb, area, &mut st);
    }

    // player: mark + title with modes and where it's playing from, artist · album with what's next,
    // then the progress bar
    let pad = Rect { x: player.x + 1, width: player.width.saturating_sub(2), ..player };
    let [now, sub, bar] = Layout::vertical([Constraint::Length(1); 3]).areas(pad);
    let gold = Style::new().fg(th.gold).bold();
    let sep_span = || Span::styled("  ·  ", dim);
    let cur = app.queue.as_ref().and_then(|q| q.tracks.get(q.idx()).map(|t| (q, t)));
    let (pos, len) = cur.map_or((0, 0), |(_, t)| (app.position().min(t.ms), t.ms));
    let mut top_right: Vec<Span> = vec![];
    let add = |top_right: &mut Vec<Span<'static>>, span: Span<'static>| {
        if !top_right.is_empty() {
            top_right.push(sep_span());
        }
        top_right.push(span);
    };
    if app.shuffle {
        add(&mut top_right, Span::styled("shuffle", gold));
    }
    match app.repeat {
        Repeat::Off => {}
        Repeat::All => add(&mut top_right, Span::styled("repeat", gold)),
        Repeat::One => add(&mut top_right, Span::styled("repeat 1", gold)),
    }
    if let Some((q, _)) = cur {
        let ctx_w = Span::raw(&q.name).width();
        if (now.width as usize) >= ctx_w + width(&top_right) + 32 {
            add(&mut top_right, Span::styled(q.name.clone(), Style::new().fg(th.accent2)));
        }
    }
    let avail = (now.width as usize).saturating_sub(5 + width(&top_right));
    let top_left = match cur {
        Some((_, t)) => vec![
            Span::styled(format!("{} ", mark(app.playing)), Style::new().fg(th.accent).bold()),
            Span::styled(fit(&t.name, avail), Style::new().fg(th.text).bold()),
        ],
        None => vec![Span::styled(fit("nothing playing", avail + 3), dim)],
    };
    f.render_widget(Paragraph::new(Line::from(top_left)), now);
    f.render_widget(Paragraph::new(Line::from(top_right).right_aligned()), now);
    if let Some((q, t)) = cur {
        // up next (when the queue isn't on its last song; repeat one plays this again)
        let next = (app.repeat != Repeat::One).then(|| q.order.get(q.pos + 1).and_then(|&i| q.tracks.get(i))).flatten();
        let full = (sub.width as usize).saturating_sub(2);
        let next_spans = next.map(|n| {
            let room = (full / 2).saturating_sub(8);
            let name = fit(&n.name, room);
            let mut v = vec![Span::styled("next  ", dim), Span::styled(name.clone(), Style::new().fg(th.sub))];
            let rest = room.saturating_sub(Span::raw(&name).width() + 3);
            if rest >= 6 {
                v.push(Span::styled(" · ", dim));
                v.push(Span::styled(fit(&n.artists, rest), dim));
            }
            v
        });
        let show_next = next_spans.as_ref().is_some_and(|n| full >= width(n) + 28);
        let avail = if show_next { full - width(next_spans.as_ref().unwrap()) - 3 } else { full };
        let artist = fit(&t.artists, avail);
        let mut spans = vec![Span::raw("  "), Span::styled(artist.clone(), Style::new().fg(th.sub))];
        let rest = avail.saturating_sub(Span::raw(&artist).width() + 3);
        if !t.album.is_empty() && rest >= 6 {
            spans.push(Span::styled(" · ", dim));
            spans.push(Span::styled(fit(&t.album, rest), dim));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), sub);
        if show_next {
            f.render_widget(Paragraph::new(Line::from(next_spans.unwrap()).right_aligned()), sub);
        }
    }
    let (lt, rt) = if cur.is_some() { (mmss(pos), mmss(len)) } else { (String::new(), String::new()) };
    let (lt, rt) = (Span::styled(format!("{lt:>4} "), dim), Span::styled(format!(" {rt}"), dim));
    let track_w = (bar.width as usize).saturating_sub(lt.width() + rt.width());
    let ratio = if len > 0 { pos as f64 / len as f64 } else { 0.0 };
    let mut spans = vec![lt];
    spans.extend(meter(track_w, ratio, cur.is_some(), th));
    spans.push(rt);
    f.render_widget(Paragraph::new(Line::from(spans)), bar);

    // bottom: on a faint tint of the terminal background (needs truecolor)
    if let Some(p) = panel(th) {
        f.render_widget(Block::new().style(Style::new().bg(hex_rgb(p))), bottom);
    }
    if let Some(lines) = help {
        f.render_widget(Paragraph::new(lines), bottom);
        return;
    }
    // status line: prompt (search / find) > error > notice > key hints; position and volume at the right
    let bw = bottom.width as usize;
    let (left_side, right_side): (Vec<Span>, Vec<Span>) = if let Some(buf) = &app.input {
        let kinds = [Kind::Tracks, Kind::Albums, Kind::Artists].map(|k| {
            let st = if k == app.search_kind { gold } else { dim };
            Span::styled(k.label(), st)
        });
        let [a, b, c] = kinds;
        let right = vec![a, Span::styled(" · ", dim), b, Span::styled(" · ", dim), c, Span::styled("   tab ", dim)];
        (prompt(" /", th.accent, buf, bw.saturating_sub(width(&right) + 2), th), right)
    } else if let Some(buf) = &app.find {
        let right = vec![Span::styled(format!("{} of {} ", num(total), num(app.view.len())), dim)];
        (prompt(" find", th.gold, buf, bw.saturating_sub(width(&right) + 2), th), right)
    } else {
        let mut right = vec![];
        if app.sess.borrow().is_none() {
            right.push(Span::styled("connecting…   ", dim));
        }
        if app.focus == Focus::Tracks && total > 0 {
            right.push(Span::styled(format!("{}/{}   ", num(sel.map_or(0, |s| s + 1)), num(total)), dim));
        }
        let pct = (app.vol as u32 * 100 + 32767) / 65535;
        if app.vol_until.is_some_and(|u| Instant::now() < u) {
            right.push(Span::styled("vol ", dim));
            right.extend(meter(10, pct as f64 / 100.0, false, th));
            right.push(Span::styled(format!(" {pct:>3}% "), Style::new().fg(th.text)));
        } else {
            right.push(Span::styled(format!("vol {pct}% "), dim));
        }
        let avail = bw.saturating_sub(width(&right) + 2);
        let left = if !app.status.is_empty() {
            vec![Span::styled(format!(" {}", fit(&app.status, avail)), Style::new().fg(th.err).bold())]
        } else if let Some((n, _)) = &app.note {
            vec![Span::styled(format!(" {}", fit(n, avail)), Style::new().fg(th.accent))]
        } else {
            let mut v = vec![];
            let mut used = 0;
            for (k, d) in hints(app) {
                let need = k.chars().count() + d.len() + 3;
                if used + need > avail {
                    break;
                }
                v.push(Span::styled(format!(" {k}"), Style::new().fg(th.sub).bold()));
                v.push(Span::styled(format!(" {d} "), dim));
                used += need;
            }
            v
        };
        (left, right)
    };
    f.render_widget(Paragraph::new(Line::from(left_side)), bottom);
    f.render_widget(Paragraph::new(Line::from(right_side).right_aligned()), bottom);
}

/// A one-line text prompt: colored label, the end of the text (so the cursor stays in view), a caret.
fn prompt(label: &str, color: Color, buf: &str, avail: usize, th: &Theme) -> Vec<Span<'static>> {
    let avail = avail.saturating_sub(label.len() + 3);
    let text = buf.chars().rev().scan(0, |w, c| {
        *w += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        (*w <= avail).then_some(c)
    });
    let text: String = text.collect::<Vec<_>>().into_iter().rev().collect();
    vec![
        Span::styled(format!("{label} "), Style::new().fg(color).bold()),
        Span::styled(text, Style::new().fg(th.text)),
        Span::styled("▏", Style::new().fg(color)),
    ]
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
                // OSC: the background color we ask for when the scheme changes; skip anything else
                b']' => {
                    i += 2;
                    let start = i;
                    while i < b.len() && b[i] != 0x07 && !(b[i] == 0x1b && b.get(i + 1) == Some(&b'\\')) {
                        i += 1;
                    }
                    out.extend(parse_bg_reply(&b[start..i]).map(Msg::Background));
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

/// Always writes a detailed log (spot + librespot at debug) to ~/.cache/spot/log, since the TUI
/// owns the terminal. The previous run's log is kept as log.1.
fn init_log() {
    let spec = "info,spot=debug,librespot=debug";
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
    match std::env::args().nth(1).as_deref() {
        None => {}
        Some("clean") => return clean_cache(),
        Some(a) => {
            let ok = matches!(a, "-h" | "--help" | "help");
            let usage = "usage: spot          start\n       spot clean    clear cached songs, lists and releases (keeps login)";
            if ok { println!("{usage}") } else { eprintln!("spot: unknown argument '{a}'\n{usage}") }
            std::process::exit(if ok { 0 } else { 2 });
        }
    }
    init_log();
    let ui = std::thread::spawn(|| {
        if let Err(e) = run() {
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

/// `spot clean`: drop everything that's only a cache. Login, settings, the resume point and
/// unsent scrobbles stay.
fn clean_cache() {
    fn size(p: &std::path::Path) -> u64 {
        match fs::symlink_metadata(p) {
            Ok(m) if m.is_dir() => fs::read_dir(p).into_iter().flatten().flatten().map(|e| size(&e.path())).sum(),
            Ok(m) => m.len(),
            Err(_) => 0,
        }
    }
    let dir = cache_dir();
    let mut freed = 0;
    for name in ["audio", "lists", "releases", "playlists"] {
        let p = dir.join(name);
        let n = size(&p);
        let r = if p.is_dir() { fs::remove_dir_all(&p) } else { fs::remove_file(&p) };
        match r {
            Ok(()) => freed += n,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => eprintln!("spot: couldn't remove {}: {e}", p.display()),
        }
    }
    println!("Cleared {:.1} MB of cache from {}.", freed as f64 / 1e6, dir.display());
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
    let tok = match saved {
        Some(refresh) => Tok { access: String::new(), refresh, exp: Instant::now(), failed_login: None },
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
        println!("One more browser approval for the audio session…");
        Some(rt.block_on(approve_session(&cache))?)
    } else {
        None
    };
    let session = first_session.clone().unwrap_or_else(|| Session::new(SessionConfig::default(), Some(cache.clone())));

    let (tx, rx) = mpsc::channel::<Msg>();
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
    probe_terminal();
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
        cur: None,
        loaded: false,
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
        quit: false,
    };

    if let Some(last) = read_last() {
        app.restore(last);
    }
    app.open_playlist(0);

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
        // wake up in time to take the popup / volume meter down
        for until in [app.note.as_ref().map(|n| n.1), app.vol_until].into_iter().flatten() {
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
        for m in first.into_iter().chain(std::iter::from_fn(|| rx.try_recv().ok())) {
            match m {
                Msg::Key(k) => {
                    app.status.clear();
                    app.on_key(k);
                }
                Msg::Redraw => {}
                Msg::Scheme(light) => {
                    LIGHT.store(light, std::sync::atomic::Ordering::Relaxed);
                    // the background changed with it: ask for the new one (answered as Msg::Background)
                    use std::io::Write;
                    let _ = std::io::stdout().write_all(b"\x1b]11;?\x1b\\").and_then(|_| std::io::stdout().flush());
                }
                Msg::Background(bg) => BG.store(bg, std::sync::atomic::Ordering::Relaxed),
                Msg::Player(ev) => app.on_player(ev),
                Msg::Playlists(p) => {
                    let n = p.len();
                    app.playlists = p;
                    app.pl_state.select(Some(app.pl_state.selected().unwrap_or(0).min(n.saturating_sub(1))));
                }
                Msg::Replace { view, tracks } => {
                    // keep the cursor on the same song (unless it's at the top, where new likes land)
                    let keep = (view == app.view.id && app.find.is_none())
                        .then(|| app.selected_track().filter(|&i| i > 0).and_then(|i| app.view.tracks.get(i)).map(|t| t.uri.clone()))
                        .flatten();
                    let Some(v) = app.view_mut(view) else { continue };
                    v.tracks = tracks;
                    if view != app.view.id {
                        continue;
                    }
                    app.refilter();
                    let sel = keep
                        .and_then(|u| app.view.tracks.iter().position(|t| t.uri == u))
                        .or(app.tr_state.selected())
                        .map(|s| s.min(app.shown().len().saturating_sub(1)));
                    app.tr_state.select(sel);
                }
                Msg::Connected(Ok(s)) => app.on_connected(s),
                Msg::Connected(Err(e)) => {
                    log::error!("audio session: {e}");
                    app.status = e;
                }
                Msg::Status(e) => {
                    log::info!("status: {e}");
                    app.status = e;
                }
                Msg::Tracks { view, tracks, done } => {
                    let Some(v) = app.view_mut(view) else { continue };
                    if !tracks.is_empty() {
                        Arc::make_mut(&mut v.tracks).extend(tracks);
                    }
                    v.loading = !done;
                    if done {
                        trim_heap();
                    }
                    if view != app.view.id {
                        continue;
                    }
                    app.refilter();
                    if app.tr_state.selected().is_none() && !app.view.tracks.is_empty() {
                        app.tr_state.select(Some(0));
                    }
                    // goto album: land on the song we came from
                    if app.find.is_none()
                        && let Some(w) = &app.view.want
                        && let Some(i) = app.view.tracks.iter().position(|t| &t.uri == w)
                    {
                        app.tr_state.select(Some(i));
                        app.view.want = None;
                    }
                }
                Msg::Links { view, links, done } => {
                    let Some(v) = app.view_mut(view) else { continue };
                    if v.artist.is_some() {
                        let f = v.filter;
                        v.links.extend(links.iter().filter(|l| f.keeps(l)).cloned());
                        v.releases.extend(links);
                    } else {
                        v.links.extend(links);
                    }
                    v.loading = !done;
                    if view != app.view.id {
                        continue;
                    }
                    app.refilter();
                    // ratatui drops the selection while a list renders empty
                    if app.tr_state.selected().is_none() && !app.view.links.is_empty() {
                        app.tr_state.select(Some(0));
                    }
                }
                Msg::ReplaceLinks { view, links } => {
                    let keep = (view == app.view.id)
                        .then(|| app.selected_track().and_then(|i| app.view.links.get(i)).map(|l| l.uri.clone()))
                        .flatten();
                    let Some(v) = app.view_mut(view) else { continue };
                    let f = v.filter;
                    v.links = links.iter().filter(|l| f.keeps(l)).cloned().collect();
                    v.releases = links;
                    if view != app.view.id {
                        continue;
                    }
                    app.refilter();
                    let sel = keep
                        .and_then(|u| app.view.links.iter().position(|l| l.uri == u))
                        .or(app.tr_state.selected())
                        .map(|s| s.min(app.shown().len().saturating_sub(1)));
                    app.tr_state.select(sel);
                }
                Msg::Error(e) => {
                    log::error!("{e}");
                    app.status = e;
                }
                Msg::Media(ev) => {
                    log::debug!("media control: {ev:?}");
                    app.on_media(ev);
                }
                Msg::Relogin(true) => {
                    log::warn!("Web API refresh token rejected, logging in through the browser");
                    app.status = "Spotify login expired, approve it in your browser…".into();
                }
                Msg::Relogin(false) => {
                    if app.status.starts_with("Spotify login expired") {
                        app.status.clear();
                    }
                }
            }
        }
    }
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
