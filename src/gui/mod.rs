// The window, drawn with Rune: the player lives in its state (`Ui::app`), on the main thread. A
// driver thread hands it the player's messages (Web API pages, player events, `spot` commands,
// media keys) and wakes it for its timers. Closing the window keeps everything running; ⌥Space
// opens the quick search panel (panel.rs) from any app, and a song you didn't see start can
// pop up at the top of the screen (toast.rs).

use std::{
    cell::RefCell,
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
};

use rune::{
    Align, AppHandle, AppSpec, Chrome, DisplayTarget, Image, ImageCache, KeyEvent, Live, LiveText, Mods,
    OverlayHandle, OverlayPlacement, OverlaySpec, ScreenEdge, TextEdit, Tx, Waker, WindowOptions,
    WindowSpec,
};

use crate::*;

mod icons;
mod panel;
mod player;
mod rows;
mod prefs;
mod toast;
mod view;

use panel::Panel;
use toast::Toast;

/// What's playing, as the panel and the popup show it.
#[derive(Clone)]
pub struct Now {
    pub uri: String,
    pub name: String,
    pub artists: String,
    pub playing: bool,
    pub shuffle: bool,
    pub repeat: Repeat,
    pub cover: Option<Image>,
}

impl Now {
    /// Whether the panel or popup would draw something else.
    fn same(&self, o: &Now) -> bool {
        (&self.uri, self.playing, self.shuffle, self.repeat, self.cover.as_ref().map(Image::id))
            == (&o.uri, o.playing, o.shuffle, o.repeat, o.cover.as_ref().map(Image::id))
    }
}

/// When the driver should next wake the player for its timers, and when it's waiting until.
#[derive(Default)]
struct Wake {
    next: Option<Instant>,
    waiting: Option<Instant>,
}

/// The small covers on a list's rows, by url, with the color each is mostly made of: None while
/// one is on its way (or failed).
#[derive(Default)]
pub struct Thumbs {
    map: HashMap<String, Option<(Image, [f32; 3])>>,
    order: VecDeque<String>,
}

/// Rows' covers kept at once; past this the oldest go (each is 64 px, 16 KB decoded).
const THUMBS: usize = 600;

pub struct Ui {
    pub app: App,
    pub search: TextEdit,
    pub prefs: prefs::Page, // the settings page
    pub find: TextEdit,
    images: Option<ImageCache>,
    pub cover: Option<(String, Image)>, // the playing song's cover, by url
    pub tint: Option<[f32; 3]>,          // and its color
    pub thumbs: RefCell<Thumbs>,
    pub clock: LiveText, // where the song is, "1:23"
    pub bar: Live<[f32; 4]>, // the progress bar's fill: set each second, so the view doesn't run
    pub knob: Live<[f32; 4]>, // and the knob at its end, shown under the pointer
    pub bar_w: f32,
    pub scrub: Option<f32>, // where a drag along the bar is, 0 to 1, until it lets go
    pub scrolled: Live<f32>, // how far the list has scrolled under the header, 0 to 1
    pub side_w: f32,         // the sidebar, dragged wider or narrower by its edge
    pub unmute: Option<u16>, // the volume to go back to, while muted
    pub focused: bool, // the window has the keyboard
    pub me: Option<AppHandle<Ui>>,
    panel: Option<AppHandle<Panel>>,
    toast: Option<OverlayHandle<Toast>>,
    toast_gen: Arc<AtomicU64>,
    wake: Arc<Mutex<Wake>>,
    told: Option<Now>, // what the panel and popup last heard
    seen: (u64, bool, bool, Option<usize>), // view, focus, sidebar, cursor: scrolled to when it moves
}

impl Ui {
    fn new(app: App, wake: Arc<Mutex<Wake>>) -> Ui {
        Ui {
            app,
            search: TextEdit::new(""),
            prefs: prefs::Page::default(),
            find: TextEdit::new(""),
            images: None,
            cover: None,
            tint: None,
            thumbs: RefCell::default(),
            clock: LiveText::new("0:00"),
            bar: Live::new([0.0; 4]),
            knob: Live::new([0.0; 4]),
            bar_w: 0.0,
            scrub: None,
            scrolled: Live::new(0.0),
            side_w: 232.0,
            unmute: None,
            focused: false,
            me: None,
            panel: None,
            toast: None,
            toast_gen: Arc::default(),
            wake,
            told: None,
            seen: (0, false, false, None),
        }
    }

    /// After anything changed the player: follow it everywhere it shows.
    pub fn after(&mut self, tx: &mut Tx) {
        if self.app.quit {
            self.quit();
        }
        if std::mem::take(&mut self.app.show) {
            self.open_window();
        }
        self.app.tick_watchers();
        self.app.sync_art();
        self.sync_cover();
        self.sync_bar();
        self.sync_clock();
        self.tell_now();
        self.popup();
        self.scroll(tx);
        self.rearm();
    }

    fn quit(&mut self) -> ! {
        self.hide_all();
        self.app.shutdown()
    }

    /// Quit and come back, for settings that apply at start.
    pub fn restart(&mut self) -> ! {
        self.hide_all();
        self.app.restart()
    }

    fn hide_all(&self) {
        if let Some(me) = &self.me {
            me.hide();
        }
        if let Some(p) = &self.panel {
            p.hide();
        }
    }

    pub fn open_window(&mut self) {
        if let Some(me) = &self.me {
            me.set_dock_icon(true);
            me.open();
        }
    }

    /// The playing song's cover as an image, once it's here.
    fn sync_cover(&mut self) {
        let Some(c) = &self.app.art.cover else {
            self.cover = None;
            self.tint = None;
            return;
        };
        if self.cover.as_ref().is_some_and(|(u, _)| *u == c.url) {
            return;
        }
        if let Some(images) = &self.images {
            self.cover = Some((c.url.clone(), images.load(c.bytes.as_slice().to_vec())));
            self.tint = Some(c.tint);
        }
    }

    /// The progress bar's fill, where the song is. It moves once a second (the timers tick
    /// then while the window shows), a step too small to see, so playing costs one small frame a
    /// second rather than a frame every refresh.
    pub fn sync_bar(&self) {
        let a = &self.app;
        let ms = a.now().map_or(0, |t| t.ms);
        let ratio = match self.scrub {
            Some(r) => r,
            None if ms > 0 => a.position().min(ms) as f32 / ms as f32,
            None => 0.0,
        };
        let w = (self.bar_w * ratio).round();
        self.bar.set([0.0, 0.0, w, 4.0]);
        self.knob.set([w - 6.0, -4.0, 12.0, 12.0]);
    }

    pub fn sync_clock(&mut self) {
        let pos = self.app.now().map_or(0, |t| match self.scrub {
            Some(r) => (r as f64 * t.ms as f64) as u64,
            None => self.app.position().min(t.ms),
        });
        self.clock.set(mmss(pos));
    }

    fn now_info(&self) -> Option<Now> {
        let t = self.app.now()?;
        Some(Now {
            uri: t.uri.clone(),
            name: t.name.clone(),
            artists: t.artists.clone(),
            playing: self.app.want_play,
            shuffle: self.app.shuffle,
            repeat: self.app.repeat,
            cover: self.cover.as_ref().filter(|(u, _)| t.cover.as_ref() == Some(u)).map(|(_, i)| i.clone()),
        })
    }

    /// Tell the panel and the popup what's playing, when that's changed.
    fn tell_now(&mut self) {
        let now = self.now_info();
        let same = match (&now, &self.told) {
            (Some(a), Some(b)) => a.same(b),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.told = now.clone();
        if let Some(p) = &self.panel {
            let now = now.clone();
            p.update(move |p, _| p.now = now);
        }
        if let Some(t) = &self.toast
            && let Some(now) = now
        {
            t.update(move |t, _| t.now = Some(now));
        }
    }

    /// A media key skipped, or (with popup = "track") a song started: say what's playing, at
    /// the top of the screen, unless spot's window is in front already.
    fn popup(&mut self) {
        let m = std::mem::take(&mut self.app.moved);
        let want = match self.app.settings.popup {
            settings::Popup::Off => false,
            settings::Popup::Skip => m.skipped,
            settings::Popup::Track => m.skipped || m.track,
        };
        let panel_up = self.panel.as_ref().is_some_and(|p| p.is_visible());
        if !want || self.focused || panel_up || self.app.now().is_none() {
            return;
        }
        let Some(toast) = self.toast.clone() else { return };
        toast.update(|t, _| t.shows += 1);
        toast.show();
        toast.fade_in(Duration::from_millis(180));
        let generation = self.toast_gen.fetch_add(1, Relaxed) + 1;
        let (gens, secs) = (self.toast_gen.clone(), self.app.settings.popup_seconds);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs_f64(secs));
            if gens.load(Relaxed) == generation {
                toast.fade_out(Duration::from_millis(320));
            }
        });
    }

    /// Keep the cursor in view when the keyboard moves it.
    fn scroll(&mut self, tx: &mut Tx) {
        let a = &self.app;
        let side = a.focus == Focus::Playlists;
        let sel = if side { if a.podcasts { a.sh_state.selected() } else { a.pl_state.selected() } } else { a.tr_state.selected() };
        let seen = (a.view.id, side, a.podcasts, sel);
        if seen == self.seen {
            return;
        }
        self.seen = seen;
        if let Some(i) = sel {
            tx.scroll_into_view(if side { format!("side-{i}") } else { format!("row-{i}") });
        }
    }

    /// Tell the driver when the timers next need it; wake it if that's sooner than it thinks.
    fn rearm(&mut self) {
        let visible = self.me.as_ref().is_some_and(|h| h.is_visible());
        let next = self.app.next_wake(visible);
        let mut w = self.wake.lock().unwrap_or_else(|e| e.into_inner());
        w.next = next;
        let poke = next.is_some_and(|n| w.waiting.is_none_or(|waiting| n < waiting));
        drop(w);
        if poke {
            let _ = self.app.tx.send(Msg::Tick);
        }
    }

    /// A row's small cover: fetched the first time it's asked for, None until it's here.
    pub fn thumb(&self, url: &str) -> Option<Image> {
        self.thumb_and_tint(url).map(|(i, _)| i)
    }

    /// A small cover and its color.
    pub fn thumb_and_tint(&self, url: &str) -> Option<(Image, [f32; 3])> {
        let mut t = self.thumbs.borrow_mut();
        if let Some(i) = t.map.get(url) {
            return i.clone();
        }
        if art::headless() {
            let got = art::fetch_blocking(self.app.rt.handle(), url).zip(self.images.as_ref()).map(|(bytes, images)| {
                let tint = image::load_from_memory(&bytes).map_or([0.5; 3], |i| art::tint(&i));
                let image = images.load(bytes);
                let _ = image.wait();
                (image, tint)
            });
            t.map.insert(url.to_string(), got.clone());
            return got;
        }
        t.map.insert(url.to_string(), None);
        t.order.push_back(url.to_string());
        while t.order.len() > THUMBS {
            if let Some(old) = t.order.pop_front() {
                t.map.remove(&old);
            }
        }
        let (url, me) = (url.to_string(), self.me.clone());
        self.app.rt.spawn(async move {
            let bytes = art::fetch_bytes(&url).await;
            if let (Some(me), Some(bytes)) = (me, bytes) {
                let tint = image::load_from_memory(&bytes).map_or([0.5; 3], |i| art::tint(&i));
                me.update(move |ui, _| {
                    let Some(images) = &ui.images else { return };
                    let image = images.load(bytes);
                    if let Some(slot) = ui.thumbs.get_mut().map.get_mut(&url) {
                        *slot = Some((image, tint));
                    }
                });
            }
        });
        None
    }

    pub fn focus_search(&mut self) {
        self.search.focused = true;
        self.app.help = false;
    }

    pub fn open_find(&mut self) {
        self.app.focus = Focus::Tracks;
        if self.app.find.is_none() {
            self.find.clear();
            self.app.set_find("");
        }
        self.find.focused = true;
    }

    /// ⌥Space: the quick search panel, on the screen the pointer's on; again, it goes away.
    pub fn toggle_panel(&mut self) {
        let Some(p) = &self.panel else { return };
        if p.is_visible() {
            p.hide();
            return;
        }
        let now = self.now_info();
        p.update(move |p, _| p.opened(now));
        p.place(DisplayTarget::UnderPointer, OverlayPlacement::Edge { edge: ScreenEdge::Top, margin: 180.0, align: Align::Center, inset: 0.0 });
        p.show();
    }
}

/// The view's key events, as the player's keys. Typing in a field never gets here.
fn on_key(ui: &mut Ui, ev: &KeyEvent, tx: &mut Tx) -> bool {
    if ui.prefs.open {
        // the page takes the keys, but for esc and the window's own (⌘Q, ⌘W)
        match &ev.key {
            rune::Key::Escape => ui.prefs.open = false,
            rune::Key::Command('q' | 'w') => {}
            _ => return !matches!(ev.key, rune::Key::Command(_)),
        }
        if matches!(ev.key, rune::Key::Escape) {
            return true;
        }
    }
    let took = match &ev.key {
        rune::Key::Command(',') => {
            ui.app.help = false;
            ui.prefs.show();
            true
        }
        rune::Key::Command(c) => match c {
            'f' => {
                ui.open_find();
                true
            }
            'k' | 'l' => {
                ui.focus_search();
                true
            }
            'q' => {
                ui.app.quit = true;
                true
            }
            'w' => {
                tx.close_window();
                true
            }
            'o' => {
                ui.toggle_panel();
                true
            }
            c => ui.app.on_key(Key::Char(*c), true),
        },
        rune::Key::Text(s) if ev.mods.ctrl => match s.as_str() {
            "f" | "\u{6}" => {
                ui.open_find();
                true
            }
            "d" | "\u{4}" => ui.app.on_key(Key::PageDown, false),
            "u" | "\u{15}" => ui.app.on_key(Key::PageUp, false),
            _ => false,
        },
        rune::Key::Text(s) if s == "/" && !ui.app.help => {
            ui.focus_search();
            true
        }
        k => key_of(k, ev.mods).is_some_and(|k| ui.app.on_key(k, false)),
    };
    if took {
        ui.after(tx);
    }
    took
}

/// A Rune key as the player's, for what isn't typing.
pub fn key_of(k: &rune::Key, mods: Mods) -> Option<Key> {
    Some(match k {
        rune::Key::Text(s) => {
            let mut c = s.chars();
            match (c.next(), c.next()) {
                (Some(ch), None) => Key::Char(ch),
                _ => return None,
            }
        }
        rune::Key::Enter => Key::Enter,
        rune::Key::Escape => Key::Esc,
        rune::Key::Tab if mods.shift => Key::BackTab,
        rune::Key::Tab => Key::Tab,
        rune::Key::Up => Key::Up,
        rune::Key::Down => Key::Down,
        rune::Key::Left => Key::Left,
        rune::Key::Right => Key::Right,
        rune::Key::PageUp => Key::PageUp,
        rune::Key::PageDown => Key::PageDown,
        rune::Key::Home => Key::Home,
        rune::Key::End => Key::End,
        rune::Key::Backspace | rune::Key::Delete => Key::Backspace,
        _ => return None,
    })
}

fn setup(ui: &mut Ui, waker: Waker) {
    ui.images = Some(ImageCache::new(waker));
}

/// The player's messages and timers, handed to the window's thread.
fn drive(rx: mpsc::Receiver<Msg>, ui: AppHandle<Ui>, wake: Arc<Mutex<Wake>>) {
    loop {
        let timeout = {
            let mut w = wake.lock().unwrap_or_else(|e| e.into_inner());
            w.waiting = w.next;
            w.waiting.map_or(Duration::from_secs(3600), |t| t.saturating_duration_since(Instant::now()))
        };
        match rx.recv_timeout(timeout) {
            Ok(m) => {
                let batch: Vec<Msg> = std::iter::once(m).chain(rx.try_iter()).filter(|m| !matches!(m, Msg::Tick)).collect();
                if batch.is_empty() {
                    continue; // only re-arming the timer
                }
                ui.update(move |ui, tx| {
                    for m in batch {
                        ui.app.on_msg(m);
                    }
                    ui.app.tick();
                    ui.after(tx);
                });
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                wake.lock().unwrap_or_else(|e| e.into_inner()).next = None;
                ui.update_if(|ui, tx| {
                    let changed = ui.app.tick();
                    ui.after(tx);
                    changed
                });
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Run spot: the window (open now if `show`), the panel, the popup. Returns only to quit.
pub fn run(app: App, rx: mpsc::Receiver<Msg>, show: bool, lock: Option<fs::File>) {
    let hotkey = app.settings.hotkey.clone();
    let tx = app.tx.clone();
    let rt = app.rt.handle().clone();
    let wake = Arc::new(Mutex::new(Wake::default()));
    let spec = AppSpec {
        min_size: Some((760.0, 480.0)),
        setup: Some(setup),
        on_key: Some(on_key),
        ..AppSpec::new("spot", (1120.0, 740.0), || unreachable!("built by init"), view::view)
    };
    let w = wake.clone();
    let mut main = rune::App::new(spec)
        .init(move || Ui::new(app, w))
        .keep_running()
        .accessory()
        .standard_menus()
        .chrome(Chrome { full_size_content: true, traffic_lights: Some((20.0, 22.0)), drag_regions: true })
        .on_focus(|ui, focused, _| ui.focused = focused)
        .on_close(|ui, _| {
            if let Some(me) = &ui.me {
                me.set_dock_icon(false);
            }
        })
        .on_reopen(|ui, _, _| {
            ui.open_window();
            true
        })
        .global_shortcut(&hotkey, |ui, _| ui.toggle_panel());
    if !show {
        main = main.windowless();
    }
    // Rune's own flags (`--snap out.png`, `--tree`, `--script`: rendering without a window, for
    // checking the UI) come from SPOT_RUNE, never from spot's command line
    let rune_args = std::env::var("SPOT_RUNE").unwrap_or_default();
    main = main.args(rune_args.split_whitespace().map(String::from).collect::<Vec<_>>());
    let me = main.handle();

    let panel_spec = AppSpec { on_key: Some(panel::on_key), setup: Some(panel::setup), ..AppSpec::new("spot search", (640.0, 460.0), || unreachable!("given its state"), panel::view) };
    let panel_app = rune::App::new(panel_spec).windowless().on_focus(|p, focused, tx| {
        if !focused {
            p.closed();
            tx.close_window();
        }
    });
    let (panel, _) = main.window(
        WindowSpec::from_app("panel", panel_app)
            .state(Panel::new(tx.clone(), rt.clone()))
            .configured_options(WindowOptions::launcher_panel().macos(|n| n.corner_radius(20.0).shadow(false))),
    );
    let toast = main.overlay(
        OverlaySpec { name: "spot now playing", size: (460.0, 76.0), edge: ScreenEdge::Top, margin: 6.0, align: Align::Center, inset: 0.0, interactive: false, view: toast::view },
        Toast::default(),
    );

    let (h, p, t) = (me.clone(), panel.clone(), toast.clone());
    me.update(move |ui, tx| {
        ui.me = Some(h.clone());
        ui.panel = Some(p.clone());
        ui.toast = Some(t);
        if show {
            h.set_dock_icon(true);
        }
        ui.after(tx);
    });
    let (pm, mm) = (panel.clone(), me.clone());
    panel.update(move |p, _| {
        p.me = Some(pm);
        p.main = Some(mm);
    });
    std::thread::spawn(move || drive(rx, me, wake));
    main.run();
    drop(lock);
}

/// spot can't start (it isn't set up yet, say), and there's no terminal to say so: a window that does.
pub fn problem(e: String) {
    static WHY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let _ = WHY.set(e);
    fn view(_: &(), cx: &mut rune::Cx<()>) -> rune::El {
        use rune::{col, text, TextStyle};
        let th = theme(cx.system_dark());
        let why = WHY.get().map_or("", String::as_str);
        col().w_full().h_full().fill(th.bg).padding(36.0).pt(56.0).gap(14.0).window_drag()
            .child(text("spot can't start").font(TextStyle::new(22.0, 650.0)).color(th.text))
            .child(text(why.to_string()).font(TextStyle::new(14.0, 400.0).lh(21.0)).color(th.sub).wrap())
            .child(text("Open Terminal and run `spot` to set it up.").font(TextStyle::new(14.0, 500.0)).color(th.accent))
    }
    rune::App::new(AppSpec::new("spot", (520.0, 240.0), || (), view))
        .chrome(Chrome { full_size_content: true, traffic_lights: None, drag_regions: true })
        .run();
}
