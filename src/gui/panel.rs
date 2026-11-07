// The quick search panel: ⌥Space from any app opens a Spotlight-style bar that searches Spotify
// and plays what you pick. ↑↓ move, ⏎ plays, ⌘⏎ queues, ⌘C copies a link (with no results, to
// what's playing), tab cycles songs / albums / artists, ⌃S shuffle, ⌃R repeat, ⌘O
// opens spot's window, esc closes. It asks the player the way `spot search` and `spot play` do.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use rune::{AppHandle, Color, Cursor, Cx, El, Image, ImageCache, KeyEvent, SurfaceFit, SystemMaterial, TextEdit, Tx, Waker, col, el, icon, row, text, text_input};

use super::view::ts;
use super::{Now, Ui, icons};
use crate::*;

const KINDS: [(&str, Option<&str>); 3] = [("songs", None), ("albums", Some("--albums")), ("artists", Some("--artists"))];
const MAX: usize = 8;
pub const WIDTH: f32 = 640.0;
const MARGIN: f32 = 24.0; // around the card, for its shadow
const BAR: f32 = 60.0;
const HIT: f32 = 50.0;
const FOOT: f32 = 58.0;

#[derive(Clone)]
struct Hit {
    uri: String,
    name: String,
    sub: String,
    cover: Option<String>,
    ms: u64,
    explicit: bool,
}

impl Hit {
    /// A `spot search --tsv` line: uri, name, subtitle, cover url, ms, explicit.
    fn parse(line: &str) -> Option<Hit> {
        let f: Vec<&str> = line.split('\t').collect();
        (f.len() >= 6).then(|| Hit {
            uri: f[0].into(),
            name: f[1].into(),
            sub: f[2].into(),
            cover: Some(f[3]).filter(|c| !c.is_empty()).map(String::from),
            ms: f[4].parse().unwrap_or(0),
            explicit: f[5] == "1",
        })
    }
}

pub struct Panel {
    query: TextEdit,
    kind: usize,
    results: Vec<Hit>,
    picked: usize,
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

/// The window's height as it opens: the bar, and what's playing under it if anything is.
pub fn opened_height(now: bool) -> f32 {
    2.0 * MARGIN + BAR + if now { FOOT } else { 0.0 }
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
            kind: 0,
            results: vec![],
            picked: 0,
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

    /// Shown again: a fresh, empty bar with the keyboard.
    pub fn opened(&mut self, now: Option<Now>) {
        self.query.clear();
        self.query.focused = true;
        self.results.clear();
        self.note = None;
        self.busy = false;
        self.picked = 0;
        self.copied = false;
        self.now = now;
        self.generation.fetch_add(1, Relaxed);
    }

    /// Closing: back to an empty bar now, while it's hidden, so it opens on one. Cleared only as
    /// it opened, it showed the last search for a frame and then shrank to the bar.
    pub fn closed(&mut self, tx: &mut Tx) {
        self.generation.fetch_add(1, Relaxed); // answers on their way are moot
        self.query.clear();
        self.results.clear();
        self.note = None;
        self.busy = false;
        self.picked = 0;
        self.copied = false;
        self.fit(tx);
    }

    fn height(&self) -> f32 {
        if self.results.is_empty() && self.note.is_none() {
            return opened_height(self.now.is_some());
        }
        let list = if self.results.is_empty() { 0.0 } else { 12.0 + self.results.len() as f32 * HIT };
        let note = if self.note.is_some() { 44.0 } else { 0.0 };
        let foot = if self.now.is_some() { FOOT } else { 0.0 };
        2.0 * MARGIN + BAR + list + note + foot
    }

    fn fit(&self, tx: &mut Tx) {
        tx.resize_window((WIDTH, self.height()));
    }

    /// Search as you type: waits a moment for typing to stop, and drops answers to older queries.
    fn search(&mut self) {
        let g = self.generation.fetch_add(1, Relaxed) + 1;
        let q = self.query.text.trim().to_string();
        if q.is_empty() {
            self.results.clear();
            self.note = None;
            self.busy = false;
            return;
        }
        self.busy = true;
        let (gens, tx, me, flag) = (self.generation.clone(), self.tx.clone(), self.me.clone(), KINDS[self.kind].1);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(140));
            if gens.load(Relaxed) != g {
                return;
            }
            let n = MAX.to_string();
            let mut args = vec!["search", "--tsv", "-n", &n];
            args.extend(flag);
            args.push(&q);
            let answer = ask(&tx, &args);
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
        if code == 0 {
            self.results = text.lines().filter_map(Hit::parse).take(MAX).collect();
            self.note = self.results.is_empty().then(|| "no results".into());
            self.picked = 0;
        } else {
            self.results.clear();
            self.note = Some(text);
        }
        let wanted: Vec<String> = self.results.iter().filter_map(|h| h.cover.as_deref().map(art::small)).collect();
        for url in wanted {
            self.want_thumb(&url);
        }
        self.fit(tx);
    }

    fn want_thumb(&mut self, url: &str) {
        if self.thumbs.contains_key(url) {
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
        if self.busy || self.results.is_empty() {
            let q = self.query.text.trim().to_string();
            if q.is_empty() {
                return;
            }
            let flag = KINDS[self.kind].1;
            self.closed(tx);
            tx.close_window();
            std::thread::spawn(move || {
                let mut args = vec!["search", "--tsv", "-n", "1"];
                args.extend(flag);
                args.push(&q);
                let (code, text) = ask(&ptx, &args);
                let Some(first) = (code == 0).then(|| text.lines().find_map(Hit::parse)).flatten() else { return };
                let _ = ask(&ptx, &if queue { vec!["queue", "add", first.uri.as_str()] } else { vec!["play", first.uri.as_str()] });
            });
            return;
        }
        let Some(h) = self.results.get(self.picked) else { return };
        let uri = h.uri.clone();
        std::thread::spawn(move || {
            let (code, text) = ask(&ptx, &if queue { vec!["queue", "add", uri.as_str()] } else { vec!["play", uri.as_str()] });
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

    fn cycle_kind(&mut self, back: bool) {
        self.kind = (self.kind + if back { KINDS.len() - 1 } else { 1 }) % KINDS.len();
        self.search();
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
        rune::Key::Down if n > 0 => p.picked = (p.picked + 1) % n,
        rune::Key::Up if n > 0 => p.picked = (p.picked + n - 1) % n,
        rune::Key::Enter => p.choose(ev.mods.cmd, tx),
        rune::Key::Tab => p.cycle_kind(ev.mods.shift),
        rune::Key::Escape => {
            p.closed(tx);
            tx.close_window();
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
        p.closed(tx);
        tx.close_window();
        return true;
    }
    false
}

pub fn view(p: &Panel, cx: &mut Cx<Panel>) -> El {
    let th = theme(cx.system_dark());
    // most of the window's own background over the glass: one even surface, whatever desktop or
    // window is behind it, so text and the picked row read the same everywhere
    let tint = th.bg.alpha(if th.dark { 0.9 } else { 0.85 });
    let field = text_input(&p.query, ts(21.0, 450.0).lh(28.0)).key("query").single_line().grow(1.0)
        .color(th.text).placeholder(format!("Search {}", KINDS[p.kind].0), th.sub)
        .on_edit(|p: &mut Panel| &mut p.query)
        .on_input_change(|p: &mut Panel, tx| {
            p.search();
            p.fit(tx);
        })
        .on_input_key(field_key);
    let kinds = row().key("kinds").gap(4.0).center_y().children(KINDS.iter().enumerate().map(|(i, (k, _))| {
        let on = i == p.kind;
        row().key(format!("k-{k}")).h(24.0).px(9.0).center().radius(12.0)
            .fill(if on { th.text.alpha(0.12) } else { Color::TRANSPARENT })
            .cursor(Cursor::Pointer)
            .on_press(move |p: &mut Panel| {
                p.kind = i;
                p.search();
            })
            .child(text(*k).font(ts(11.5, 600.0)).color(if on { th.text } else { th.text.alpha(0.55) }))
    }));
    let bar = row().key("bar").w_full().h(BAR).px(20.0).gap(14.0).center_y()
        .child(icon(&icons::SEARCH, 20.0).color(if p.busy { th.text } else { th.text.alpha(0.6) }))
        .child(field)
        .child(kinds);
    // dragging the card anywhere but a field or button moves it; it opens there next time
    let mut card = col().key("card").w_full().on_drag(|_: &mut Panel, d: &mut rune::Drag, _: &mut Tx| super::launcher::drag(d.phase)).radius(20.0).continuous_corners().clip_rounded(20.0)
        .system_material(SystemMaterial::Glass)
        .fill(tint)
        .border(1.0, th.text.alpha(if th.dark { 0.12 } else { 0.1 }))
        .shadow(18.0, 48.0, Color::BLACK.alpha(if th.dark { 0.5 } else { 0.22 }))
        .child(bar);
    if !p.results.is_empty() {
        let mut list = col().key("results").w_full().px(8.0).pb(8.0).pt(4.0).gap(0.0);
        for (i, h) in p.results.iter().enumerate() {
            let on = i == p.picked;
            let thumb = el().key("thumb").size(34.0, 34.0).radius(if KINDS[p.kind].0 == "artists" { 17.0 } else { 6.0 }).clip_rounded(6.0).fill(th.text.alpha(0.08));
            let thumb = match p.thumb(h) {
                Some(img) => thumb.pixels_fit(img.pixels(), SurfaceFit::Fill),
                None => thumb.center().child(icon(&icons::NOTE, 15.0).color(th.text.alpha(0.5))),
            };
            let mut name = row().key("nm").gap(6.0).center_y().min_w(0.0)
                .child(text(h.name.clone()).font(ts(14.0, 560.0)).color(th.text).truncate().shrink());
            if h.explicit {
                name = name.child(row().key("e").size(14.0, 14.0).radius(3.0).border(1.0, th.dim.alpha(0.6)).center()
                    .child(text("E").font(ts(8.5, 650.0)).color(th.dim)));
            }
            // the picked row: lit, with a bar of the accent at its left edge, and its words brighter
            let bar = el().key("pick-bar").pin(Some(0.0), Some(13.0), None, Some(13.0)).w(3.0).radius(1.5)
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
                .child(col().key("words").grow(1.0).shrink().min_w(0.0).gap(1.0)
                    .child(name)
                    .child(text(h.sub.clone()).font(ts(12.0, 450.0)).color(th.text.alpha(if on { 0.8 } else { 0.62 })).truncate()));
            if h.ms > 0 {
                r = r.child(text(mmss(h.ms)).font(super::view::NUM).color(th.text.alpha(0.5)));
            }
            list = list.child(r);
        }
        card = card.child(el().key("sep").mx(20.0).h(1.0).fill(th.text.alpha(0.08))).child(list);
    }
    if let Some(n) = &p.note {
        card = card.child(row().key("note").w_full().h(44.0).px(24.0).center_y().child(text(n.clone()).font(ts(13.0, 500.0)).color(th.text.alpha(0.75)).truncate()));
    }
    if let Some(n) = &p.now {
        let mode = |on: bool| if on { th.text } else { th.text.alpha(0.4) };
        let small_btn = |key: &'static str, i: &'static rune::Icon, c: Color, f: fn(&mut Panel)| {
            row().key(key).size(30.0, 30.0).center().radius(15.0).hover(th.text.alpha(0.08)).cursor(Cursor::Pointer)
                .on_press(move |p: &mut Panel| f(p))
                .child(icon(i, 15.0).color(c))
        };
        let art = el().key("now-art").size(34.0, 34.0).radius(6.0).clip_rounded(6.0).fill(th.text.alpha(0.08));
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
        let foot = foot.child(el().key("gap").w(4.0))
            .child(small_btn("p-shuffle", &icons::SHUFFLE, mode(n.shuffle), |p| p.control(&["shuffle"], |n| n.shuffle = !n.shuffle)))
            .child(small_btn("p-play", if n.playing { &icons::PAUSE } else { &icons::PLAY }, th.text, |p| p.control(&["toggle"], |n| n.playing = !n.playing)))
            .child(small_btn("p-repeat", &icons::REPEAT, mode(n.repeat != Repeat::Off), |p| p.control(&["repeat"], |n| n.repeat = n.repeat.next())));
        card = card.child(el().key("sep2").h(1.0).w_full().fill(th.text.alpha(0.07))).child(foot);
    }
    col().key("panel").w_full().h_full().padding(MARGIN).child(card)
}
