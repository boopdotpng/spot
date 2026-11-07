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
    pub disc: u32, // disc number on its album; 0 = unknown
    pub explicit: bool,
}

// A podcast episode is a Track too, so the queue, the player and the CLI work on it unchanged:
// `artists` is the show's name, `album` its release date and `album_id` the show.
impl Track {
    pub fn is_episode(&self) -> bool {
        self.uri.starts_with("spotify:episode:")
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
    let link = |uri: &str, name: &str| Link { uri: uri.into(), name: name.into(), sub: String::new(), single: false };
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

#[derive(Clone, Copy, PartialEq, Default)]
pub enum Repeat {
    #[default]
    Off,
    All, // loop the list (reshuffled each pass when shuffling)
    One, // replay the current song
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

pub enum Msg {
    Player(PlayerEvent),
    Playlists(Vec<Playlist>),
    Shows(Vec<Link>), // the podcasts the user follows
    Tracks { view: u64, tracks: Vec<Track>, done: bool },
    Replace { view: u64, tracks: Arc<Vec<Track>> }, // fresh copy of a list that was shown from cache
    Links { view: u64, links: Vec<Link>, done: bool },
    ReplaceLinks { view: u64, links: Vec<Link> }, // fresh copy of an artist's releases shown from cache
    Error(String),
    Media(MediaControlEvent),
    Relogin(bool), // true: waiting on browser login; false: it finished
    Connected(Result<Session, String>),
    Status(String),
    Ctl(Vec<String>, ctl::Reply), // a `spot <command>` request
    Loaded(ctl::Reply, ctl::Then, ctl::List), // what a `spot play` / `queue add` named, fetched
    Watch(ctl::Watcher),
    Art(String, Option<art::Cover>), // a cover's url, and the image (None: couldn't get it)
    Show, // `spot` / `spot show`: open the window
    Tick, // nothing new; a timer is due (or one was just set)
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
            self.order = (0..n).collect();
            self.pos = first.unwrap_or(0);
        }
    }
}
