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
}

impl Kind {
    pub fn next(self) -> Self {
        match self {
            Kind::Tracks => Kind::Albums,
            Kind::Albums => Kind::Artists,
            Kind::Artists => Kind::Tracks,
        }
    }
    /// What Spotify's search calls it.
    pub fn ty(self) -> &'static str {
        match self {
            Kind::Tracks => "track",
            Kind::Albums => "album",
            Kind::Artists => "artist",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Kind::Tracks => "songs",
            Kind::Albums => "albums",
            Kind::Artists => "artists",
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Repeat {
    Off,
    All, // loop the list (reshuffled each pass when shuffling)
    One, // replay the current song
}

#[derive(Clone)]
pub struct Playlist {
    pub id: Option<String>, // None = Liked Songs
    pub name: String,
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
    Ctl(Vec<String>, ctl::Reply), // a `spot <command>` request
    Loaded(ctl::Reply, ctl::Then, ctl::List), // what a `spot play` / `queue add` named, fetched
    Watch(ctl::Watcher),
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

    /// Uniform Fisher-Yates over every track (OS-seeded ChaCha RNG), no "smart" weighting.
    /// `first` stays at the front so the current/chosen song keeps playing.
    pub fn reorder(&mut self, shuffle: bool, first: Option<usize>) {
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
