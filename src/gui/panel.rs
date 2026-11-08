// The quick search panel: ⌥Space from any app opens a Spotlight-style bar that searches Spotify
// and plays what you pick. Empty, it lists what was played lately; typed in, your playlists that
// match come first, then Spotify's top result, songs, albums and artists. ↑↓ move, tab jumps to
// the next section, → opens an album, playlist or artist's songs (← or esc goes back), ⏎ plays,
// ⌘⏎ queues, ⌘C copies a link (with no results, to what's playing), ⌃S shuffle, ⌃R repeat, ⌘O
// opens spot's window, esc closes. It asks the player the way `spot search` and `spot play` do.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use rune::{AppHandle, Color, Cursor, Cx, El, Image, ImageCache, KeyEvent, SurfaceFit, SystemMaterial, TextEdit, Tx, Waker, col, el, icon, row, text, text_input};

use super::view::ts;
use super::{Now, Ui, icons};
use crate::*;

const RECENT: usize = 7;
const MINE: usize = 1; // of your playlists matching
pub const WIDTH: f32 = 640.0;
const MARGIN: f32 = 24.0; // around the card, for its shadow
const BAR: f32 = 54.0;
const HIT: f32 = 38.0;
const FOOT: f32 = 50.0;
const HEAD: f32 = 22.0; // a section's title
const THUMB: f32 = 28.0;
/// The window's height, always, so it never runs into the bottom of the screen and gets pushed
/// up. Spotify's results (the top one, three songs, an album, an artist: four sections) fit in
/// its list; past what fits, the rows scroll with the selection.
pub const HEIGHT: f32 = 486.0;
/// What the list has room for, under the bar and over what's playing.
const ROOM: f32 = HEIGHT - 2.0 * MARGIN - BAR - 12.0 - FOOT;

#[derive(Clone)]
struct Hit {
    uri: String,
    name: String,
    sub: String,
    cover: Option<String>,
    ms: u64,
    explicit: bool,
    section: String,
    number: u32, // inside an album: the song's track number
}

impl Hit {
    /// A `spot search --tsv` line: uri, name, subtitle, cover url, ms, explicit, and (from
    /// `search --mixed`) the section; `songs` of an album adds the track number after it.
    fn parse(line: &str) -> Option<Hit> {
        let f: Vec<&str> = line.split('\t').collect();
        (f.len() >= 6).then(|| Hit {
            uri: f[0].into(),
            name: f[1].into(),
            sub: f[2].into(),
            cover: Some(f[3]).filter(|c| !c.is_empty()).map(String::from),
            ms: f[4].parse().unwrap_or(0),
            explicit: f[5] == "1",
            section: f.get(6).unwrap_or(&"").to_string(),
            number: f.get(7).and_then(|n| n.parse().ok()).unwrap_or(0),
        })
    }

    fn of(l: &Link, section: &str) -> Hit {
        Hit { uri: l.uri.clone(), name: l.name.clone(), sub: l.sub.clone(), cover: l.image.clone(), ms: 0, explicit: false, section: section.into(), number: 0 }
    }

    /// An album, playlist or artist: → lists its songs.
    fn opens(&self) -> bool {
        self.uri == "liked" || ["spotify:album:", "spotify:playlist:", "spotify:artist:"].iter().any(|p| self.uri.starts_with(p))
    }
}

/// Inside an album, playlist or artist (→): what it was opened from, to go back to.
struct Inside {
    parent: Hit,
    back: Vec<Hit>,
    picked: usize,
    scroll: usize,
}

pub struct Panel {
    query: TextEdit,
    results: Vec<Hit>,
    picked: usize,
    scroll: usize,          // the first row shown
    recent: Vec<Hit>,       // the empty bar's
    playlists: Vec<Hit>,    // all of yours, matched as you type
    remote: Vec<Hit>,       // Spotify's answer to the last search
    inside: Option<Inside>, // → on an album, playlist or artist
    note: Option<String>,
    busy: bool,
    generation: Arc<AtomicU64>,
    tx: mpsc::Sender<Msg>,
    rt: tokio::runtime::Handle,
    images: Option<ImageCache>,
    thumbs: HashMap<String, Option<Image>>,
    pub now: Option<Now>,
    copied: bool,
    pub me: Option<AppHandle<Panel>>,
    pub main: Option<AppHandle<Ui>>,
}

/// Ask the player, as a `spot` command would; waits for the answer.
fn ask(tx: &mpsc::Sender<Msg>, args: &[&str]) -> (i32, String) {
    let (rtx, rrx) = mpsc::channel();
    if tx.send(Msg::Ctl(args.iter().map(|a| a.to_string()).collect(), rtx)).is_err() {
        return (1, "spot isn't answering".into());
    }
    rrx.recv_timeout(Duration::from_secs(60)).unwrap_or((1, "spot didn't answer".into()))
}

impl Panel {
    pub fn new(tx: mpsc::Sender<Msg>, rt: tokio::runtime::Handle) -> Panel {
        Panel {
            query: TextEdit::new(""),
            results: vec![],
            picked: 0,
            scroll: 0,
            recent: vec![],
            playlists: vec![],
            remote: vec![],
            inside: None,
            note: None,
            busy: false,
            generation: Arc::default(),
            tx,
            rt,
            images: None,
            thumbs: HashMap::new(),
            now: None,
            copied: false,
            me: None,
            main: None,
        }
    }

    /// The recently played and your playlists, kept up to date while hidden: closed, it already
    /// shows the recently played, so it opens on them rather than growing to them after.
    pub fn set_lists(&mut self, recent: &[Link], playlists: &[Link], tx: &mut Tx) {
        self.recent = recent.iter().take(RECENT).map(|l| Hit::of(l, "Recently played")).collect();
        self.playlists = playlists.iter().map(|l| Hit::of(l, "Your playlists")).collect();
        if self.query.text.is_empty() && self.inside.is_none() {
            self.results = self.recent.clone();
            self.picked = self.picked.min(self.results.len().saturating_sub(1));
            self.want_thumbs();
            self.fit(tx);
        }
    }

    /// Shown again: a fresh, empty bar with the keyboard, over what was played lately (it
    /// already shows them, from closing).
    pub fn opened(&mut self, now: Option<Now>) {
        self.query.clear();
        self.query.focused = true;
        self.remote.clear();
        self.inside = None;
        self.results = self.recent.clone();
        self.note = None;
        self.busy = false;
        self.picked = 0;
        self.scroll = 0;
        self.copied = false;
        self.now = now;
        self.generation.fetch_add(1, Relaxed);
        self.want_thumbs();
    }

    /// Closing: back to an empty bar now, while it's hidden, so it opens on one. Cleared only as
    /// it opened, it showed the last search for a frame and then shrank to the bar.
    pub fn closed(&mut self, tx: &mut Tx) {
        self.generation.fetch_add(1, Relaxed); // answers on their way are moot
        self.query.clear();
        self.results = self.recent.clone();
        self.remote.clear();
        self.inside = None;
        self.note = None;
        self.busy = false;
        self.picked = 0;
        self.scroll = 0;
        self.copied = false;
        self.fit(tx);
    }

    /// The rows on screen from row `scroll`, as many as the list has room for: their indices,
    /// and whether each starts a section (has a title).
    fn shown_from(&self, scroll: usize) -> Vec<(usize, bool)> {
        let mut used = if self.inside.is_some() { HEAD } else { 0.0 };
        let mut out = vec![];
        for i in scroll..self.results.len() {
            let s = &self.results[i].section;
            let titled = !s.is_empty() && (i == scroll || self.results[i - 1].section != *s);
            used += HIT + if titled { HEAD } else { 0.0 };
            if used > ROOM + 0.5 && !out.is_empty() {
                break;
            }
            out.push((i, titled));
        }
        out
    }

    fn shown(&self) -> Vec<(usize, bool)> {
        self.shown_from(self.scroll)
    }

    /// Move the selection to `i`, scrolling it into view.
    fn pick(&mut self, i: usize) {
        self.picked = i;
        if i < self.scroll {
            self.scroll = i;
        }
        while self.scroll < i && self.shown_from(self.scroll).last().is_none_or(|&(last, _)| last < i) {
            self.scroll += 1;
        }
        self.want_thumbs();
    }

    /// Your playlists the words match, the best first.
    fn matching(&self, q: &str) -> Vec<Hit> {
        let mut found: Vec<(u8, &Hit)> = self.playlists.iter().map(|h| (match_score(q, &h.name), h)).filter(|(s, _)| *s > 0).collect();
        found.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
        found.into_iter().take(MINE).map(|(_, h)| h.clone()).collect()
    }

    /// The covers of the rows on screen (Liked Songs opened has thousands).
    pub fn want_thumbs(&mut self) {
        let wanted: Vec<String> = self.shown().iter().filter_map(|&(i, _)| self.results[i].cover.as_deref().map(art::small)).collect();
        for url in wanted {
            self.want_thumb(&url);
        }
    }

    /// The window at its one size (it's only ever off it before the first open).
    pub fn fit(&self, tx: &mut Tx) {
        tx.resize_window((WIDTH, HEIGHT));
    }

    /// Search as you type: waits a moment for typing to stop, and drops answers to older queries.
    /// Your playlists show at once, and the last query's results go; Spotify's come after, with
    /// stand-ins for them meanwhile.
    fn search(&mut self) {
        let g = self.generation.fetch_add(1, Relaxed) + 1;
        let q = self.query.text.trim().to_string();
        self.inside = None;
        self.note = None;
        self.picked = 0;
        self.scroll = 0;
        if q.is_empty() {
            self.remote.clear();
            self.results = self.recent.clone();
            self.busy = false;
            return;
        }
        self.remote.clear();
        self.results = self.matching(&q);
        self.want_thumbs();
        self.busy = true;
        let (gens, tx, me) = (self.generation.clone(), self.tx.clone(), self.me.clone());
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(140));
            if gens.load(Relaxed) != g {
                return;
            }
            let answer = ask(&tx, &["search", "--mixed", "--tsv", &q]);
            if let Some(me) = me {
                me.update(move |p, tx| p.got(g, answer, tx));
            }
        });
    }

    fn got(&mut self, g: u64, (code, text): (i32, String), tx: &mut Tx) {
        if self.generation.load(Relaxed) != g {
            return;
        }
        self.busy = false;
        let mine = self.matching(self.query.text.trim());
        let picked = self.results.get(self.picked).map(|h| h.uri.clone());
        if code == 0 {
            self.remote = text.lines().filter_map(Hit::parse).collect();
            self.note = None;
        } else {
            self.remote.clear();
            self.note = Some(text);
        }
        self.results = mine.into_iter().chain(self.remote.iter().cloned()).collect();
        if self.results.is_empty() && self.note.is_none() {
            self.note = Some("no results".into());
        }
        // a playlist picked while Spotify's answer came stays picked
        let keep = picked.and_then(|u| self.results.iter().position(|h| h.uri == u)).unwrap_or(0);
        self.scroll = 0;
        self.pick(keep);
        self.want_thumbs();
        self.fit(tx);
    }

    /// →: the picked album, playlist or artist's songs (an artist's popular ones), in place.
    fn open_picked(&mut self, tx: &mut Tx) -> bool {
        let Some(h) = self.results.get(self.picked).filter(|h| h.opens() && self.inside.is_none()).cloned() else { return false };
        let g = self.generation.fetch_add(1, Relaxed) + 1;
        let back = std::mem::take(&mut self.results);
        self.inside = Some(Inside { parent: h.clone(), back, picked: self.picked, scroll: self.scroll });
        self.picked = 0;
        self.scroll = 0;
        self.note = None;
        self.busy = true;
        let (ptx, me) = (self.tx.clone(), self.me.clone());
        std::thread::spawn(move || {
            let answer = ask(&ptx, &["songs", &h.uri]);
            if let Some(me) = me {
                me.update(move |p, tx| p.got_songs(g, answer, tx));
            }
        });
        self.fit(tx);
        true
    }

    fn got_songs(&mut self, g: u64, (code, text): (i32, String), tx: &mut Tx) {
        if self.generation.load(Relaxed) != g || self.inside.is_none() {
            return;
        }
        self.busy = false;
        if code == 0 {
            self.results = text.lines().filter_map(Hit::parse).collect();
            self.note = self.results.is_empty().then(|| "no songs".into());
        } else {
            self.note = Some(text);
        }
        self.want_thumbs();
        self.fit(tx);
    }

    /// ← or esc inside: back to the results it was opened from.
    fn go_back(&mut self, tx: &mut Tx) -> bool {
        let Some(i) = self.inside.take() else { return false };
        self.generation.fetch_add(1, Relaxed); // its songs, still on their way, are moot
        self.results = i.back;
        self.picked = i.picked;
        self.scroll = i.scroll;
        self.note = None;
        self.busy = false;
        self.fit(tx);
        true
    }

    /// tab: the first row of the next section (⇧: of this one, or the one before).
    fn next_section(&mut self, back: bool) {
        let n = self.results.len();
        if n == 0 {
            return;
        }
        let starts: Vec<usize> = (0..n).filter(|&i| i == 0 || self.results[i - 1].section != self.results[i].section).collect();
        let to = if back {
            starts.iter().rev().find(|&&s| s < self.picked).copied().unwrap_or(*starts.last().unwrap_or(&0))
        } else {
            starts.iter().find(|&&s| s > self.picked).copied().unwrap_or(0)
        };
        self.pick(to);
    }

    fn want_thumb(&mut self, url: &str) {
        // not wired up yet (told the lists as spot starts): asked again when they show
        if self.thumbs.contains_key(url) || self.me.is_none() || self.images.is_none() {
            return;
        }
        if self.thumbs.len() > 200 {
            self.thumbs.clear();
        }
        self.thumbs.insert(url.to_string(), None);
        let (url, me) = (url.to_string(), self.me.clone());
        self.rt.spawn(async move {
            if let (Some(bytes), Some(me)) = (art::fetch_bytes(&url).await, me) {
                me.update(move |p, _| {
                    if let Some(images) = &p.images {
                        let image = images.load(bytes);
                        p.thumbs.insert(url, Some(image));
                    }
                });
            }
        });
    }

    fn thumb(&self, h: &Hit) -> Option<&Image> {
        self.thumbs.get(&art::small(h.cover.as_ref()?))?.as_ref()
    }

    /// Play (or queue) the picked result, then get out of the way. Typed faster than the results
    /// came: the bar closes at once and the first result plays once it's found.
    fn choose(&mut self, queue: bool, tx: &mut Tx) {
        let (ptx, me) = (self.tx.clone(), self.me.clone());
        let local = self.results.get(self.picked).is_some_and(|h| h.section == "Your playlists");
        if self.inside.is_none() && ((self.busy && !local) || self.results.is_empty()) {
            let q = self.query.text.trim().to_string();
            if q.is_empty() {
                return;
            }
            self.closed(tx);
            tx.close_window();
            std::thread::spawn(move || {
                let (code, text) = ask(&ptx, &["search", "--mixed", "--tsv", &q]);
                let Some(first) = (code == 0).then(|| text.lines().find_map(Hit::parse)).flatten() else { return };
                let _ = ask(&ptx, &if queue { vec!["queue", "add", first.uri.as_str()] } else { vec!["play", first.uri.as_str()] });
            });
            return;
        }
        let Some(h) = self.results.get(self.picked) else { return };
        let uri = h.uri.clone();
        // a song inside an album or playlist plays it from there on; an artist's, on its own
        let from = self.inside.as_ref().map(|i| i.parent.uri.clone()).filter(|p| !queue && !p.starts_with("spotify:artist:"));
        std::thread::spawn(move || {
            let args = match (&from, queue) {
                (_, true) => vec!["queue", "add", uri.as_str()],
                (Some(list), false) => vec!["play", list.as_str(), "--from", uri.as_str()],
                (None, false) => vec!["play", uri.as_str()],
            };
            let (code, text) = ask(&ptx, &args);
            if let Some(me) = me {
                me.update(move |p, tx| {
                    if code == 0 {
                        p.closed(tx);
                        tx.close_window();
                    } else {
                        p.note = Some(text);
                        p.fit(tx);
                    }
                });
            }
        });
    }

    /// ⌘C: the link to the picked result, or with no results what's playing.
    fn copy_link(&mut self, tx: &mut Tx) {
        let uri = self.results.get(self.picked).map(|h| h.uri.clone()).or_else(|| self.now.as_ref().map(|n| n.uri.clone()));
        let Some(url) = uri.as_deref().and_then(super::rows::web_link) else { return };
        tx.copy(url);
        self.copied = true;
        if let Some(me) = self.me.clone() {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(1200));
                me.update(|p, _| p.copied = false);
            });
        }
    }

    /// A control on what's playing: shown changed at once, then told to the player.
    fn control(&mut self, args: &'static [&'static str], change: impl FnOnce(&mut Now)) {
        if let Some(n) = &mut self.now {
            change(n);
        }
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = ask(&tx, args);
        });
    }

    fn open_main(&mut self, tx: &mut Tx) {
        if let Some(m) = &self.main {
            m.update(|ui, tx| {
                ui.open_window();
                ui.after(tx);
            });
        }
        self.closed(tx);
        tx.close_window();
    }
}

pub fn setup(p: &mut Panel, waker: Waker) {
    p.images = Some(ImageCache::new(waker));
}

/// The bar's keys, before the field types them.
fn field_key(p: &mut Panel, ev: &KeyEvent, tx: &mut Tx) -> bool {
    let n = p.results.len();
    let ctrl = |c: &str, code: &str| matches!(&ev.key, rune::Key::Text(s) if ev.mods.ctrl && (s == c || s == code));
    match &ev.key {
        rune::Key::Down if n > 0 => p.pick((p.picked + 1) % n),
        rune::Key::Up if n > 0 => p.pick((p.picked + n - 1) % n),
        rune::Key::Enter => p.choose(ev.mods.cmd, tx),
        rune::Key::Tab => p.next_section(ev.mods.shift),
        // → at the end of the words opens the picked album, playlist or artist; ← comes back
        rune::Key::Right if p.query.anchor == p.query.focus && p.query.focus == p.query.text.len() => return p.open_picked(tx),
        rune::Key::Left => return p.go_back(tx),
        rune::Key::Escape => {
            if !p.go_back(tx) {
                p.closed(tx);
                tx.close_window();
            }
        }
        rune::Key::Command('c') if p.query.selected().is_empty() => p.copy_link(tx),
        rune::Key::Command('o') => p.open_main(tx),
        _ if ctrl("s", "\u{13}") => p.control(&["shuffle"], |n| n.shuffle = !n.shuffle),
        _ if ctrl("r", "\u{12}") => p.control(&["repeat"], |n| n.repeat = n.repeat.next()),
        _ => return false,
    }
    true
}

pub fn on_key(p: &mut Panel, ev: &KeyEvent, tx: &mut Tx) -> bool {
    if matches!(ev.key, rune::Key::Escape) {
        if !p.go_back(tx) {
            p.closed(tx);
            tx.close_window();
        }
        return true;
    }
    false
}

pub fn view(p: &Panel, cx: &mut Cx<Panel>) -> El {
    let th = theme(cx.system_dark());
    // most of the window's own background over the glass: one even surface, whatever desktop or
    // window is behind it, so text and the picked row read the same everywhere
    let tint = th.bg.alpha(if th.dark { 0.9 } else { 0.85 });
    let field = text_input(&p.query, ts(20.0, 450.0).lh(26.0)).key("query").single_line().grow(1.0)
        .color(th.text).placeholder("Songs, albums, artists, your playlists", th.sub)
        .on_edit(|p: &mut Panel| &mut p.query)
        .on_input_change(|p: &mut Panel, tx| {
            p.search();
            if !p.busy {
                p.fit(tx); // emptied: the recently played, at once
            }
        })
        .on_input_key(field_key);
    let bar = row().key("bar").w_full().h(BAR).px(20.0).gap(14.0).center_y()
        .child(icon(&icons::SEARCH, 20.0).color(if p.busy { th.text } else { th.text.alpha(0.6) }))
        .child(field);
    // dragging the card anywhere but a field or button moves it; it opens there next time
    let mut card = col().key("card").w_full().grow(1.0).min_h(0.0).on_drag(|_: &mut Panel, d: &mut rune::Drag, _: &mut Tx| super::launcher::drag(d.phase)).radius(20.0).continuous_corners().clip_rounded(20.0)
        .system_material(SystemMaterial::Glass)
        .fill(tint)
        .border(1.0, th.text.alpha(if th.dark { 0.12 } else { 0.1 }))
        .shadow(18.0, 48.0, Color::BLACK.alpha(if th.dark { 0.5 } else { 0.22 }))
        .child(bar);
    let head = |key: String, title: String| row().key(key).w_full().h(HEAD).px(12.0).pt(7.0).center_y()
        .child(text(title.to_uppercase()).font(ts(10.0, 650.0).tracking(0.6)).color(th.text.alpha(0.5)).truncate());
    // while a search is on its way the list stays, so the card keeps its shape and its foot stays put
    let searching = p.busy && p.inside.is_none();
    let listed = !p.results.is_empty() || p.inside.is_some() || searching;
    if listed {
        // it fills the card, which holds its height while a search is on its way: what doesn't
        // fit until then is cut off rather than pushing the card's foot out of it
        let mut list = col().key("results").w_full().grow(1.0).min_h(0.0).clip_rounded(0.0).px(8.0).pb(8.0).pt(4.0).gap(0.0);
        let numbered = p.inside.as_ref().is_some_and(|i| i.parent.uri.starts_with("spotify:album:"));
        if let Some(inside) = &p.inside {
            // what's open, clicked (or ←) to go back; an album's cover beside it, once
            let mut crumb = row().key("inside").w_full().h(HEAD).px(12.0).pt(5.0).gap(8.0).center_y().cursor(Cursor::Pointer)
                .on_press_tx(|p: &mut Panel, tx| {
                    p.go_back(tx);
                })
                .child(text("‹").font(ts(13.0, 650.0)).color(th.text.alpha(0.6)));
            if numbered && let Some(img) = p.thumb(&inside.parent) {
                crumb = crumb.child(el().key("crumb-art").size(16.0, 16.0).radius(4.0).clip_rounded(4.0).pixels_fit(img.pixels(), SurfaceFit::Fill));
            }
            list = list.child(crumb.child(text(inside.parent.name.clone()).font(ts(11.5, 650.0)).color(th.text.alpha(0.7)).truncate()));
            if p.results.is_empty() && p.note.is_none() {
                list = list.child(row().key("wait").w_full().h(HIT).px(14.0).center_y()
                    .child(text("…").font(ts(14.0, 500.0)).color(th.text.alpha(0.5))));
            }
        }
        for (i, titled) in p.shown() {
            let h = &p.results[i];
            if titled {
                list = list.child(head(format!("head-{i}"), h.section.clone()));
            }
            let on = i == p.picked;
            let round = h.uri.starts_with("spotify:artist:");
            let thumb = el().key("thumb").size(THUMB, THUMB).radius(if round { THUMB / 2.0 } else { 6.0 }).clip_rounded(if round { THUMB / 2.0 } else { 6.0 }).fill(th.text.alpha(0.08));
            let thumb = match p.thumb(h) {
                // an album's songs: their number where the cover would be (they'd all be the same)
                _ if numbered => row().key("thumb").size(THUMB, THUMB).center()
                    .child(text(if h.number > 0 { h.number.to_string() } else { String::new() }).font(super::view::NUM).color(th.text.alpha(0.5))),
                _ if h.uri == "liked" => super::view::liked_cover("thumb", THUMB, 6.0),
                Some(img) => thumb.pixels_fit(img.pixels(), SurfaceFit::Fill),
                None => thumb.center().child(icon(&icons::NOTE, 15.0).color(th.text.alpha(0.5))),
            };
            let mut name = row().key("nm").gap(6.0).center_y().min_w(0.0)
                .child(text(h.name.clone()).font(ts(13.5, 560.0).lh(17.0)).color(th.text).truncate().shrink());
            if h.explicit {
                name = name.child(super::rows::explicit(&th));
            }
            // the picked row: lit, with a bar of the accent at its left edge, and its words brighter
            let bar = el().key("pick-bar").pin(Some(0.0), Some(10.0), None, Some(10.0)).w(3.0).radius(1.5)
                .fill(if on { th.accent } else { Color::TRANSPARENT });
            let mut r = row().key(format!("hit-{i}")).w_full().h(HIT).px(12.0).gap(12.0).center_y().radius(12.0)
                .fill(if on { th.text.alpha(0.13) } else { Color::TRANSPARENT })
                .hover(if on { th.text.alpha(0.13) } else { th.text.alpha(0.05) })
                .cursor(Cursor::Pointer)
                .on_press_tx(move |p: &mut Panel, tx| {
                    p.picked = i;
                    p.choose(false, tx);
                })
                .child(bar)
                .child(thumb)
                .child(col().key("words").grow(1.0).shrink().min_w(0.0).gap(0.0)
                    .child(name)
                    .child(text(h.sub.clone()).font(ts(11.5, 450.0).lh(15.0)).color(th.text.alpha(if on { 0.8 } else { 0.62 })).truncate()));
            if h.ms > 0 {
                r = r.child(text(mmss(h.ms)).font(super::view::NUM).color(th.text.alpha(0.5)));
            }
            if on && h.opens() && p.inside.is_none() {
                // → opens it
                r = r.child(text("›").font(ts(17.0, 500.0)).color(th.text.alpha(0.45)));
            }
            list = list.child(r);
        }
        if searching {
            // stand-ins for Spotify's results, filling what the card has room for (the rest is cut off)
            for i in 0..((ROOM / HIT) as usize).saturating_sub(p.results.len()) {
                let bar = |key: &'static str, w: f32, h: f32| el().key(key).w(w).h(h).radius(h / 2.0).fill(th.text.alpha(0.07));
                list = list.child(row().key(format!("wait-{i}")).w_full().h(HIT).px(12.0).gap(12.0).center_y()
                    .child(el().key("thumb").size(THUMB, THUMB).radius(6.0).fill(th.text.alpha(0.06)))
                    .child(col().key("words").gap(7.0)
                        .child(bar("w1", [180.0, 140.0, 210.0, 160.0][i % 4], 9.0))
                        .child(bar("w2", [110.0, 90.0, 130.0, 100.0][i % 4], 7.0))));
            }
        }
        card = card.child(el().key("sep").mx(20.0).h(1.0).fill(th.text.alpha(0.08))).child(list);
    }
    if let Some(n) = &p.note {
        card = card.child(row().key("note").w_full().h(44.0).px(24.0).center_y().child(text(n.clone()).font(ts(13.0, 500.0)).color(th.text.alpha(0.75)).truncate()));
    }
    if !listed {
        // the card is always the window's height: what's playing stays at its foot
        card = card.child(el().key("room").w_full().grow(1.0));
    }
    if let Some(n) = &p.now {
        let mode = |on: bool| if on { th.text } else { th.text.alpha(0.4) };
        let small_btn = |key: &'static str, i: &'static rune::Icon, c: Color, f: fn(&mut Panel)| {
            row().key(key).size(30.0, 30.0).center().radius(15.0).hover(th.text.alpha(0.08)).cursor(Cursor::Pointer)
                .on_press(move |p: &mut Panel| f(p))
                .child(icon(i, 15.0).color(c))
        };
        let art = el().key("now-art").size(THUMB, THUMB).radius(6.0).clip_rounded(6.0).fill(th.text.alpha(0.08));
        let art = match &n.cover {
            Some(img) => art.pixels_fit(img.pixels(), SurfaceFit::Fill),
            None => art,
        };
        let mut foot = row().key("foot").w_full().h(FOOT).px(20.0).gap(12.0).center_y().fill(th.text.alpha(0.035))
            .child(art)
            .child(col().key("now").grow(1.0).shrink().min_w(0.0).gap(1.0)
                .child(text(n.name.clone()).font(ts(13.0, 600.0)).color(th.text).truncate())
                .child(text(n.artists.clone()).font(ts(11.5, 450.0)).color(th.text.alpha(0.7)).truncate()));
        if p.copied {
            foot = foot.child(text("link copied").font(ts(11.0, 500.0)).color(th.text));
        }
        // how far through the song it is, along the foot's bottom edge
        let progress = el().key("now-progress").pin(Some(0.0), None, None, Some(0.0)).h(2.0)
            .w(((WIDTH - 2.0 * MARGIN) * n.ratio()).round()).fill(th.accent);
        let foot = foot.child(progress).child(el().key("gap").w(4.0))
            .child(small_btn("p-shuffle", &icons::SHUFFLE, mode(n.shuffle), |p| p.control(&["shuffle"], |n| n.shuffle = !n.shuffle)))
            .child(small_btn("p-play", if n.playing { &icons::PAUSE } else { &icons::PLAY }, th.text, |p| p.control(&["toggle"], |n| n.playing = !n.playing)))
            .child(small_btn("p-repeat", &icons::REPEAT, mode(n.repeat != Repeat::Off), |p| p.control(&["repeat"], |n| n.repeat = n.repeat.next())));
        card = card.child(el().key("sep2").h(1.0).w_full().fill(th.text.alpha(0.07))).child(foot);
    }
    col().key("panel").w_full().h_full().padding(MARGIN).child(card)
}
