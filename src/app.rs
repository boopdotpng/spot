// The app's state: what's open, what's playing, and the main loop's message handling.

use crate::*;

#[derive(PartialEq)]
pub enum Focus {
    Playlists,
    Tracks,
}

pub struct View {
    pub id: u64,
    pub key: String,
    pub title: String,
    pub kind: Kind, // Tracks, or a list of album/artist links
    pub tracks: Arc<Vec<Track>>, // shared with the queue, so playing a big list doesn't copy it
    pub links: Vec<Link>,
    pub loading: bool,
    pub artist: Option<String>, // artist page (key = artist uri): name, for switching songs/releases
    pub releases: Vec<Link>,    // artist page: every release, of which `links` is the `filter`ed part
    pub filter: Releases,
    pub want: Option<String>,   // track uri to select once it loads (goto album)
}

impl View {
    pub fn new(id: u64, key: String, title: String, kind: Kind) -> Self {
        View { id, key, title, kind, tracks: Arc::default(), links: vec![], loading: true, artist: None, releases: vec![], filter: Releases::All, want: None }
    }
    pub fn len(&self) -> usize {
        if self.kind == Kind::Tracks { self.tracks.len() } else { self.links.len() }
    }
    pub fn haystack(&self, i: usize) -> String {
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
pub enum Shown<'a> {
    All(usize),
    Matches(&'a [usize]),
}

impl Shown<'_> {
    pub fn len(&self) -> usize {
        match self {
            Shown::All(n) => *n,
            Shown::Matches(m) => m.len(),
        }
    }
    /// Original index of the r-th listed row.
    pub fn get(&self, r: usize) -> Option<usize> {
        match self {
            Shown::All(n) => (r < *n).then_some(r),
            Shown::Matches(m) => m.get(r).copied(),
        }
    }
}

pub struct App {
    pub rt: tokio::runtime::Runtime,
    pub api: Arc<Api>,
    pub tx: mpsc::Sender<Msg>,
    pub cache: Cache,
    pub sess: tokio::sync::watch::Sender<Option<Session>>, // None while connecting
    pub player: Arc<Player>,
    pub mixer: Arc<dyn Mixer>,
    pub media: Option<MediaControls>,

    pub playlists: Vec<Playlist>,
    pub pl_state: ListState,
    pub view: View,
    pub view_seq: u64,                  // last view id handed out
    pub history: Vec<(View, usize)>,    // views to go back to (esc), with their selected row
    pub artist_songs: bool,             // artist pages list every song (else albums & singles)
    pub releases: Releases,             // which releases new artist pages list (remembered)
    pub tr_state: TableState,
    pub focus: Focus,
    pub input: Option<String>,
    pub find: Option<String>, // ctrl+f filter over the open track list
    pub matches: Vec<usize>,  // view indices matching `find`, kept current by refilter()
    pub search_kind: Kind,
    pub shuffle: bool,
    pub repeat: Repeat,
    pub fails: usize, // consecutive unplayable tracks, so repeat-all can't spin forever
    pub queue: Option<Queue>, // the list playing
    pub upnext: Vec<Track>,   // the queue (`e`, `spot queue add`): plays before the list carries on
    pub queued: Option<Track>, // the queued song playing now, taken off `upnext` (the list waits)
    pub cur: Option<SpotifyUri>,
    pub loaded: bool, // `cur` is loaded into the player (it waits for the session)
    pub load_at: Option<Instant>, // manual skip in progress: hold off loading `cur` until then
    pub playing: bool,
    pub pos_ms: u64,
    pub pos_at: Option<Instant>,
    pub status: String,
    pub note: Option<(String, Instant)>, // brief popup and when it disappears
    pub retried: bool,
    pub settings: settings::Settings,
    pub vol: u16,               // user volume; the mixer dips below it during fades
    pub want_play: bool,        // intent, flips immediately (playing lags until the player confirms)
    pub fade_gen: Arc<std::sync::atomic::AtomicU64>, // bumps cancel an in-flight fade
    pub vol_until: Option<Instant>, // volume meter shows until then, after a change
    pub help: bool,                 // key help on the bottom line
    pub lastfm: Option<Arc<LastFm>>,
    pub listen: Option<Listen>, // current play, for scrobbling (only with lastfm)
    pub plays: u64,             // bumped per song started, so `spot watch` can tell plays apart
    pub finished: bool,         // the queue ran out
    pub ctl: ctl::State,
    pub quit: bool,
    pub terminal: Option<terminal::Display>,
}

impl App {
    /// Act on one message from the main loop's channel.
    pub fn on_msg(&mut self, m: Msg) {
        match m {
            Msg::Key(k) => {
                self.status.clear();
                self.on_key(k);
            }
            Msg::Attach(a) => self.attach_terminal(a),
            Msg::Terminal { id, event } => self.terminal_event(id, event),
            Msg::Scheme(light) => {
                LIGHT.store(light, std::sync::atomic::Ordering::Relaxed);
                // the background changed with it: ask for the new one (answered as Msg::Background)
                self.query_background();
            }
            Msg::Background(bg) => BG.store(bg, std::sync::atomic::Ordering::Relaxed),
            Msg::Player(ev) => self.on_player(ev),
            Msg::Playlists(p) => {
                let n = p.len();
                self.playlists = p;
                self.pl_state.select(Some(self.pl_state.selected().unwrap_or(0).min(n.saturating_sub(1))));
            }
            Msg::Replace { view, tracks } => {
                // keep the cursor on the same song (unless it's at the top, where new likes land)
                let keep = (view == self.view.id && self.find.is_none())
                    .then(|| self.selected_track().filter(|&i| i > 0).and_then(|i| self.view.tracks.get(i)).map(|t| t.uri.clone()))
                    .flatten();
                let Some(v) = self.view_mut(view) else { return };
                v.tracks = tracks;
                if view != self.view.id {
                    return;
                }
                self.refilter();
                let sel = keep
                    .and_then(|u| self.view.tracks.iter().position(|t| t.uri == u))
                    .or(self.tr_state.selected())
                    .map(|s| s.min(self.shown().len().saturating_sub(1)));
                self.tr_state.select(sel);
            }
            Msg::Connected(Ok(s)) => self.on_connected(s),
            Msg::Connected(Err(e)) => {
                log::error!("audio session: {e}");
                self.status = e;
            }
            Msg::Status(e) => {
                log::info!("status: {e}");
                self.status = e;
            }
            Msg::Tracks { view, tracks, done } => {
                let Some(v) = self.view_mut(view) else { return };
                if !tracks.is_empty() {
                    Arc::make_mut(&mut v.tracks).extend(tracks);
                }
                v.loading = !done;
                if done {
                    trim_heap();
                }
                if view != self.view.id {
                    return;
                }
                self.refilter();
                if self.tr_state.selected().is_none() && !self.view.tracks.is_empty() {
                    self.tr_state.select(Some(0));
                }
                // goto album: land on the song we came from
                if self.find.is_none()
                    && let Some(w) = &self.view.want
                    && let Some(i) = self.view.tracks.iter().position(|t| &t.uri == w)
                {
                    self.tr_state.select(Some(i));
                    self.view.want = None;
                }
            }
            Msg::Links { view, links, done } => {
                let Some(v) = self.view_mut(view) else { return };
                if v.artist.is_some() {
                    let f = v.filter;
                    v.links.extend(links.iter().filter(|l| f.keeps(l)).cloned());
                    v.releases.extend(links);
                } else {
                    v.links.extend(links);
                }
                v.loading = !done;
                if view != self.view.id {
                    return;
                }
                self.refilter();
                // ratatui drops the selection while a list renders empty
                if self.tr_state.selected().is_none() && !self.view.links.is_empty() {
                    self.tr_state.select(Some(0));
                }
            }
            Msg::ReplaceLinks { view, links } => {
                let keep = (view == self.view.id)
                    .then(|| self.selected_track().and_then(|i| self.view.links.get(i)).map(|l| l.uri.clone()))
                    .flatten();
                let Some(v) = self.view_mut(view) else { return };
                let f = v.filter;
                v.links = links.iter().filter(|l| f.keeps(l)).cloned().collect();
                v.releases = links;
                if view != self.view.id {
                    return;
                }
                self.refilter();
                let sel = keep
                    .and_then(|u| self.view.links.iter().position(|l| l.uri == u))
                    .or(self.tr_state.selected())
                    .map(|s| s.min(self.shown().len().saturating_sub(1)));
                self.tr_state.select(sel);
            }
            Msg::Error(e) => {
                log::error!("{e}");
                self.status = e;
            }
            Msg::Media(ev) => {
                log::debug!("media control: {ev:?}");
                self.on_media(ev);
            }
            Msg::Relogin(true) => {
                log::warn!("Web API refresh token rejected, logging in through the browser");
                self.status = "Spotify login expired, approve it in your browser…".into();
            }
            Msg::Relogin(false) => {
                if self.status.starts_with("Spotify login expired") {
                    self.status.clear();
                }
            }
            Msg::Ctl(req, reply) => self.on_ctl(req, reply),
            Msg::Loaded(reply, then, list) => self.on_loaded(reply, then, list),
            Msg::Watch(w) => self.watch(w),
            Msg::Quit => self.quit = true,
        }
    }
}
