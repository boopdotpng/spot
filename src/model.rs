// The data everything passes around: tracks, lists, the queue, and the messages the main loop runs on.

use crate::*;

#[derive(Clone)]
pub struct Track {
    pub uri: String,
    pub name: String,
    pub artists: String,
    pub lead: usize, // byte length of the first artist's name in `artists` (names can contain ", ")
    pub album: String,
    pub cover: Option<String>,
    pub ms: u64,
    pub album_id: u128, // SpotifyIds for goto album / artist (first artist); 0 = unknown
    pub artist_id: u128,
    pub more: Vec<(usize, u128)>, // the other artists: each one's name's byte length and id
    pub disc: u32, // disc number on its album; 0 = unknown
    pub number: u32, // track number on its disc; 0 = unknown
    pub explicit: bool,
    pub added: u64, // when it went into the playlist (or Liked Songs), unix seconds; 0 = unknown
}

// A podcast episode is a Track too, so the queue, the player and the CLI work on it unchanged:
// `artists` is the show's name, `album` its release date and `album_id` the show.
impl Track {
    pub fn is_episode(&self) -> bool {
        self.uri.starts_with("spotify:episode:")
    }

    /// Each artist's name and id (0 = unknown), cut from `artists` by `lead` and `more`. Ends
    /// early if those don't fit it (lists cached before `more` was kept have only the first).
    pub fn credits(&self) -> Vec<(&str, u128)> {
        let Some(first) = self.artists.get(..self.lead).filter(|n| !n.is_empty()) else { return Vec::new() };
        let mut out = vec![(first, self.artist_id)];
        let mut at = self.lead;
        for &(len, id) in &self.more {
            let Some(name) = self.artists.get(at..).and_then(|r| r.strip_prefix(", ")).and_then(|r| r.get(..len)) else { break };
            out.push((name, id));
            at += 2 + len;
        }
        out
    }
}

/// A list of a show's episodes (a view's or queue's key is its uri).
pub fn is_show(key: &str) -> bool {
    key.starts_with("spotify:show:")
}

/// The podcast sidebar's lists of episodes from several shows.
pub const CONTINUE_KEY: &str = "podcasts:continue";
pub const NEW_KEY: &str = "podcasts:new";

/// Above the followed shows in the podcast sidebar.
pub fn podcast_home() -> [Link; 2] {
    let link = |uri: &str, name: &str| Link { uri: uri.into(), name: name.into(), sub: String::new(), single: false, image: None };
    [link(CONTINUE_KEY, "Continue listening"), link(NEW_KEY, "New episodes")]
}

/// A list of episodes: those are never shuffled, and show dates and progress instead of albums.
pub fn is_episodes(key: &str) -> bool {
    is_show(key) || key.starts_with("podcasts:")
}

/// An album or artist search result.
#[derive(Clone)]
pub struct Link {
    pub uri: String,
    pub name: String,
    pub sub: String,
    pub single: bool, // a single release (EPs and compilations count as albums)
    pub image: Option<String>, // its cover or portrait, largest size
}

/// What an artist page shows, cycled with `v`: the overview (popular songs and cards), every
/// song, or the discography as a grid.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ArtistMode {
    Overview,
    Songs,
    Releases,
}

impl ArtistMode {
    pub fn next(self) -> Self {
        match self {
            ArtistMode::Overview => ArtistMode::Songs,
            ArtistMode::Songs => ArtistMode::Releases,
            ArtistMode::Releases => ArtistMode::Overview,
        }
    }
}

/// An artist's overview, all from the one metadata lookup that also lists their releases.
#[derive(Clone, Default)]
pub struct ArtistInfo {
    pub portrait: Option<String>,
    pub top: Vec<Track>,     // popular songs, most played first
    pub releases: Vec<Link>, // albums, singles and compilations, newest first
    pub appears: Vec<Link>,  // others' releases they're on; `sub` names whose
    pub related: Vec<Link>,  // fans also like (Spotify often leaves this empty)
    pub bio: String,
}

/// The overview's sections of cards.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Section {
    Top, // a search's top result
    Releases,
    Appears,
    Related,
}

/// Which of an artist's releases their page lists, cycled with `f`.
#[derive(Clone, Copy, PartialEq)]
pub enum Releases {
    All,
    Albums,
    Singles,
}

impl Releases {
    pub fn next(self) -> Self {
        match self {
            Releases::All => Releases::Albums,
            Releases::Albums => Releases::Singles,
            Releases::Singles => Releases::All,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Releases::All => "albums & singles",
            Releases::Albums => "albums",
            Releases::Singles => "singles",
        }
    }
    pub fn keeps(self, l: &Link) -> bool {
        match self {
            Releases::All => true,
            Releases::Albums => !l.single,
            Releases::Singles => l.single,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Tracks,
    Albums,
    Artists,
    Shows,
}

impl Kind {
    pub fn next(self) -> Self {
        match self {
            Kind::Tracks => Kind::Albums,
            Kind::Albums => Kind::Artists,
            Kind::Artists => Kind::Shows,
            Kind::Shows => Kind::Tracks,
        }
    }
    /// What Spotify's search calls it.
    pub fn ty(self) -> &'static str {
        match self {
            Kind::Tracks => "track",
            Kind::Albums => "album",
            Kind::Artists => "artist",
            Kind::Shows => "show",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Kind::Tracks => "songs",
            Kind::Albums => "albums",
            Kind::Artists => "artists",
            Kind::Shows => "podcasts",
        }
    }
}

/// What a search is for, as its chip says it: everything, or songs, albums, ….
pub fn search_label(kind: Option<Kind>) -> &'static str {
    kind.map_or("all", Kind::label)
}

#[derive(Clone, Copy, PartialEq, Default)]
pub enum Repeat {
    #[default]
    Off,
    All, // loop the list (reshuffled each pass when shuffling)
    One, // replay the current song
}

impl Repeat {
    pub fn name(self) -> &'static str {
        match self {
            Repeat::Off => "off",
            Repeat::All => "all",
            Repeat::One => "one",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        }
    }
}

#[derive(Clone)]
pub struct Playlist {
    pub id: Option<String>, // None = Liked Songs
    pub name: String,
    pub image: Option<String>, // its cover, the smallest Spotify has
    pub rank: usize,           // where Spotify lists it, to go back to when it's unpinned
}

impl Playlist {
    pub fn key(&self) -> String {
        self.id.clone().unwrap_or_else(|| "liked".into())
    }
    /// `liked`, or its spotify:playlist: uri.
    pub fn uri(&self) -> String {
        self.id.as_ref().map_or_else(|| "liked".into(), |id| format!("spotify:playlist:{id}"))
    }
}

/// A search's top result: a song, an album or an artist, as a card; a song keeps its track to
/// play.
#[derive(Clone)]
pub struct Hit {
    pub link: Link,
    pub song: Option<Track>,
}

/// A song (or an episode) as a card: a search's top result.
pub fn song_link(t: &Track) -> Link {
    let kind = if t.uri.starts_with("spotify:episode:") { "Episode" } else { "Song" };
    Link { uri: t.uri.clone(), name: t.name.clone(), sub: format!("{kind} · {}", t.artists), single: false, image: t.cover.clone() }
}

pub enum Msg {
    Key(Press),
    Attach(terminal::Attachment),
    Terminal { id: u64, event: terminal::Event },
    Scheme(bool), // terminal switched color scheme; true = light
    Background(u32), // terminal's background color (OSC 11 reply)
    Player(PlayerEvent),
    Playlists(Vec<Playlist>),
    Shows(Vec<Link>), // the podcasts the user follows
    Tracks { view: u64, tracks: Vec<Track>, done: bool },
    Replace { view: u64, tracks: Arc<Vec<Track>> }, // fresh copy of a list that was shown from cache
    Links { view: u64, links: Vec<Link>, done: bool },
    ReplaceLinks { view: u64, links: Vec<Link> }, // fresh copy of an artist's releases shown from cache
    Artist { view: u64, info: Arc<ArtistInfo> },
    Found { view: u64, found: Box<(Option<Hit>, ArtistInfo)> }, // a search of everything: songs, albums (releases) and artists (related)
    About { view: u64, about: Vec<String> }, // an album's release date and label, then its copyrights  // an artist's overview, fresh (or None came: see lists)
    Error(String),
    Media(MediaControlEvent),
    Relogin(bool), // true: waiting on browser login; false: it finished
    Connected(Result<Session, String>),
    Status(String),
    Ctl(Vec<String>, ctl::Reply), // a `spot <command>` request
    Loaded(ctl::Reply, ctl::Then, ctl::List), // what a `spot play` / `spot queue` named, fetched
    Watch(ctl::Watcher),
    Art(String, Option<ratatui_image::protocol::Protocol>), // a cover's url, and it drawn for the terminal
    Quit, // SIGTERM / SIGINT, or `spot quit`
}

/// The audio session, None until it has connected (in the background, so startup doesn't wait).
pub type Sess = tokio::sync::watch::Receiver<Option<Session>>;

pub async fn ready(mut s: Sess) -> Result<Session, String> {
    let s = s.wait_for(Option::is_some).await.map_err(|_| "not connected")?;
    Ok(s.clone().expect("checked by wait_for"))
}

pub struct Queue {
    pub key: String,
    pub name: String,
    pub tracks: Arc<Vec<Track>>,
    pub order: Vec<usize>, // play order (indices into tracks); shuffled when shuffle is on
    pub pos: usize,
    pub sorted: Vec<usize>, // the list's order as it was sorted when played; empty = as listed
}

/// How a playlist is listed: as it is, or by when its songs were added.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Sort {
    #[default]
    List,
    Newest,
    Oldest,
}

impl Sort {
    pub fn next(self) -> Sort {
        match self {
            Sort::List => Sort::Newest,
            Sort::Newest => Sort::Oldest,
            Sort::Oldest => Sort::List,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Sort::List => "list",
            Sort::Newest => "newest",
            Sort::Oldest => "oldest",
        }
    }
    pub fn from_name(s: &str) -> Sort {
        match s.trim() {
            "newest" => Sort::Newest,
            "oldest" => Sort::Oldest,
            _ => Sort::List,
        }
    }
    /// `tracks` in this order, as indices; empty for `List`. Songs with no date keep their
    /// place relative to each other, after the dated ones.
    pub fn order(self, tracks: &[Track]) -> Vec<usize> {
        let mut o: Vec<usize> = (0..tracks.len()).collect();
        match self {
            Sort::List => return vec![],
            Sort::Newest => o.sort_by_key(|&i| (tracks[i].added == 0, std::cmp::Reverse(tracks[i].added))),
            Sort::Oldest => o.sort_by_key(|&i| (tracks[i].added == 0, tracks[i].added)),
        }
        o
    }
}

impl Queue {
    pub fn idx(&self) -> usize {
        self.order[self.pos]
    }

    /// The songs after the one it's on, in play order.
    pub fn upcoming(&self) -> impl Iterator<Item = &Track> {
        self.order.iter().skip(self.pos + 1).map(|&i| &self.tracks[i])
    }

    /// Uniform Fisher-Yates over every track (OS-seeded ChaCha RNG), no "smart" weighting.
    /// `first` stays at the front so the current/chosen song keeps playing.
    /// Episodes always play in list order.
    pub fn reorder(&mut self, shuffle: bool, first: Option<usize>) {
        use rand::seq::SliceRandom;
        let n = self.tracks.len();
        if shuffle && !is_episodes(&self.key) {
            let mut rest: Vec<usize> = (0..n).filter(|&i| Some(i) != first).collect();
            rest.shuffle(&mut rand::rng());
            self.order = first.into_iter().chain(rest).collect();
            self.pos = 0;
        } else {
            self.order = if self.sorted.len() == n { self.sorted.clone() } else { (0..n).collect() };
            self.pos = first.and_then(|f| self.order.iter().position(|&i| i == f)).unwrap_or(0);
        }
    }
}
