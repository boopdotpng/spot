// The settings page (⌘,), in place of the open list: what config.toml holds,
// plus the Spotify and Last.fm logins. Each change is written to config.toml as it's made,
// keeping the rest of the file. Most apply when spot restarts, which the page offers to do.

use rune::{Color, Cursor, El, ScrollSpec, TextEdit, col, el, row, text, text_input};

use super::view::{BODY, BODY_MED, LABEL, SMALL, TITLE, ts};
use super::{Ui, icons};
use crate::settings::{Appearance, Settings};
use crate::*;

#[derive(Default)]
pub struct Page {
    pub open: bool,
    saved: Settings, // config.toml as of the last change on this page
    client_id: TextEdit,
    hotkey: TextEdit,
    fm_key: TextEdit,
    fm_secret: TextEdit,
    client_changed: bool, // saved a new client id, used from the next start
    bad: Option<&'static str>, // which field holds something that can't be saved
    fm: Fm,
    logout: bool, // asked to log out: waiting for the second press
    note: Option<String>, // a save that failed
    cached: u64, // bytes of songs and lists on disk, as of opening the page
}

#[derive(Default, PartialEq)]
enum Fm {
    #[default]
    Idle,
    Waiting, // on the browser
    Failed(String),
}

impl Page {
    /// Fill the page from the files, which may have been edited by hand since.
    pub fn show(&mut self) {
        let (saved, _) = settings::load();
        let id = saved.client_id.clone().or_else(old_client_id).unwrap_or_default();
        let (key, secret) = lastfm_creds().unwrap_or_default();
        self.client_id = TextEdit::new(&id);
        self.hotkey = TextEdit::new(&saved.hotkey);
        self.fm_key = TextEdit::new(&key);
        self.fm_secret = TextEdit::new(&secret);
        self.saved = saved;
        self.bad = None;
        self.logout = false;
        self.note = None;
        self.cached = cache_size();
        if self.fm != Fm::Waiting {
            self.fm = Fm::Idle;
        }
        self.open = true;
    }

    /// Whether what's saved differs from what this run started with.
    fn needs_restart(&self, now: &Settings) -> bool {
        let s = &self.saved;
        self.client_changed || s.quality != now.quality || s.normalize != now.normalize || s.audio_cache_gb != now.audio_cache_gb || s.hotkey != now.hotkey
    }
}

fn save(ui: &mut Ui, key: &str, value: Option<String>) {
    ui.prefs.note = settings::set(key, value.as_deref()).err().map(|e| format!("couldn't save config.toml: {e}"));
}

// ---------- pieces ----------

fn section(key: &str, title: &str, rows: Vec<El>) -> El {
    let th = super::view::theme_now();
    col().key(key.to_string()).w_full().gap(2.0)
        .child(text(title.to_uppercase()).font(LABEL).color(th.dim).mb(6.0))
        .children(rows)
}

/// A setting: what it is on the left, a line on what it does under that, its control on the right.
fn line(key: &str, label: &str, sub: &str, control: El) -> El {
    let th = super::view::theme_now();
    row().key(key.to_string()).w_full().py(9.0).gap(16.0).center_y()
        .child(col().key("words").grow(1.0).shrink().min_w(0.0).gap(2.0)
            .child(text(label.to_string()).font(BODY_MED).color(th.text))
            .child(text(sub.to_string()).font(SMALL).color(th.sub).wrap()))
        .child(control)
}

/// One of a few: pills side by side, the chosen one filled.
fn choice(key: &str, options: Vec<(String, bool)>, pick: impl Fn(&mut Ui, usize) + Clone + 'static) -> El {
    let th = super::view::theme_now();
    let mut r = row().key(key.to_string()).h(28.0).padding(2.0).gap(2.0).radius(9.0).fill(th.wash).center_y();
    for (i, (label, on)) in options.into_iter().enumerate() {
        let pick = pick.clone();
        r = r.child(row().key(format!("o{i}")).h_full().px(10.0).center().radius(7.0)
            .fill(if on { th.raised } else { Color::TRANSPARENT })
            .shadow(1.0, if on { 3.0 } else { 0.0 }, Color::BLACK.alpha(if on { 0.18 } else { 0.0 }))
            .hover(if on { th.raised } else { th.text.alpha(0.05) })
            .cursor(Cursor::Pointer)
            .on_press_tx(move |ui: &mut Ui, tx| {
                pick(ui, i);
                ui.after(tx);
            })
            .child(text(label).font(ts(12.0, if on { 620.0 } else { 520.0 })).color(if on { th.text } else { th.sub })));
    }
    r
}

fn switch(key: &str, on: bool, flip: impl Fn(&mut Ui) + 'static) -> El {
    let th = super::view::theme_now();
    row().key(key.to_string()).size(38.0, 22.0).padding(2.0).radius(11.0)
        .fill(if on { th.text } else { th.text.alpha(0.16) })
        .cursor(Cursor::Pointer).label(key.to_string())
        .on_press_tx(move |ui: &mut Ui, tx| {
            flip(ui);
            ui.after(tx);
        })
        .child(el().key("gap").w(if on { 16.0 } else { 0.0 }).h(1.0))
        .child(el().key("knob").size(18.0, 18.0).radius(9.0).fill(if on { th.bg } else { Color::WHITE }).shadow(1.0, 3.0, Color::BLACK.alpha(0.25)))
}

pub fn button(key: &str, label: &str, kind: Kind, f: impl Fn(&mut Ui) + 'static) -> El {
    let th = super::view::theme_now();
    let (fill, color) = match kind {
        Kind::Primary => (th.text, th.bg),
        Kind::Plain => (th.wash, th.text),
        Kind::Danger => (th.err.alpha(0.16), th.err),
    };
    row().key(key.to_string()).h(28.0).px(12.0).center().radius(8.0).fill(fill)
        .hover(fill.alpha(0.85)).cursor(Cursor::Pointer).press_scale(0.96)
        .on_press_tx(move |ui: &mut Ui, tx| {
            f(ui);
            ui.after(tx);
        })
        .child(text(label.to_string()).font(ts(12.0, 600.0)).color(color))
}

#[derive(Clone, Copy)]
pub enum Kind {
    Primary,
    Plain,
    Danger,
}

/// A text field that saves on return (and says so if what's in it can't be saved).
fn input(key: &'static str, edit: &TextEdit, w: f32, placeholder: &str, bad: bool, secure: bool, get: fn(&mut Ui) -> &mut TextEdit, submit: fn(&mut Ui)) -> El {
    let th = super::view::theme_now();
    let focused = edit.focused;
    let mut field = text_input(edit, BODY).key(key.to_string()).single_line().grow(1.0).color(th.text).caret(th.text, th.text.alpha(0.2))
        .placeholder(placeholder.to_string(), th.sub)
        .on_edit(get)
        .on_submit(move |ui: &mut Ui, tx| {
            submit(ui);
            tx.blur();
            ui.after(tx);
        });
    if secure {
        field = field.secure();
    }
    let border = if bad { th.err } else if focused { th.text.alpha(0.3) } else { th.line };
    row().key(format!("{key}-box")).w(w).h(30.0).px(10.0).center_y().radius(8.0)
        .fill(th.raised).border(1.0, border)
        .child(field)
}

// ---------- the page ----------

pub fn page(ui: &Ui, th: &Theme) -> El {
    let p = &ui.prefs;
    let s = &p.saved;
    let a = &ui.app;

    let quality = choice("quality", [96u16, 160, 320].iter().map(|q| (format!("{q}"), s.quality == *q)).collect(), |ui, i| {
        let q = [96u16, 160, 320][i];
        ui.prefs.saved.quality = q;
        save(ui, "quality", Some(q.to_string()));
    });
    let normalize = switch("normalize", s.normalize, |ui| {
        let on = !ui.prefs.saved.normalize;
        ui.prefs.saved.normalize = on;
        save(ui, "normalize", Some(on.to_string()));
    });
    let mut sizes = vec![0.0, 1.0, 2.0, 5.0, 10.0];
    if !sizes.contains(&s.audio_cache_gb) {
        sizes.push(s.audio_cache_gb); // set by hand to something else
    }
    let size_label = |g: f64| if g == 0.0 { "Off".to_string() } else { format!("{g} GB") };
    let cache = choice("cache", sizes.iter().map(|g| (size_label(*g), s.audio_cache_gb == *g)).collect(), move |ui, i| {
        let g = sizes[i];
        ui.prefs.saved.audio_cache_gb = g;
        save(ui, "audio_cache_gb", Some(g.to_string()));
    });

    let appearance = choice("appearance", Appearance::ALL.iter().map(|a| {
        let label = match a {
            Appearance::System => "Match system",
            Appearance::Light => "Light",
            Appearance::Dark => "Dark",
        };
        (label.to_string(), s.appearance == *a)
    }).collect(), |ui, i| {
        let a = Appearance::ALL[i];
        ui.prefs.saved.appearance = a;
        // at once, unlike most settings: the colors read it on every draw
        crate::theme::set_appearance(a);
        super::menus::set_appearance(a);
        ui.app.settings.appearance = a;
        save(ui, "appearance", (a != Appearance::System).then(|| settings::quoted(a.name())));
    });

    let hotkey = input("hotkey", &p.hotkey, 170.0, "option+space", p.bad == Some("hotkey"), false, |ui| &mut ui.prefs.hotkey, |ui| {
        match settings::hotkey(&ui.prefs.hotkey.text) {
            Some(h) => {
                ui.prefs.bad = None;
                ui.prefs.hotkey = TextEdit::new(&h);
                ui.prefs.saved.hotkey = h.clone();
                save(ui, "hotkey", Some(settings::quoted(&h)));
            }
            None => ui.prefs.bad = Some("hotkey"),
        }
    });

    // Spotify
    let who = a.sess.borrow().as_ref().map(|s| s.username());
    let account = match &who {
        Some(name) => format!("Logged in as {name}."),
        None => "Connecting…".to_string(),
    };
    let logout = if p.logout {
        row().key("logout-ask").gap(6.0).center_y()
            .child(button("logout-no", "Cancel", Kind::Plain, |ui| ui.prefs.logout = false))
            .child(button("logout-yes", "Log out", Kind::Danger, |ui| {
                forget_spotify();
                ui.restart();
            }))
    } else {
        button("logout", "Log out", Kind::Plain, |ui| ui.prefs.logout = true)
    };
    let logout_sub = if p.logout {
        format!("{account} spot restarts and opens your browser to log in again, maybe as someone else.")
    } else {
        account
    };
    let client = input("client-id", &p.client_id, 300.0, "32 letters and digits", p.bad == Some("client"), false, |ui| &mut ui.prefs.client_id, |ui| {
        let id = ui.prefs.client_id.text.trim().to_string();
        if id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit()) {
            ui.prefs.bad = None;
            if ui.prefs.saved.client_id.as_deref() != Some(id.as_str()) {
                ui.prefs.client_changed = true;
                ui.prefs.saved.client_id = Some(id.clone());
                save(ui, "client_id", Some(settings::quoted(&id)));
            }
        } else {
            ui.prefs.bad = Some("client");
        }
    });

    // Last.fm
    let connected = a.lastfm.is_some();
    let fm_status = match (&p.fm, connected) {
        (Fm::Waiting, _) => "Waiting for you to allow spot in your browser…".to_string(),
        (Fm::Failed(e), _) => format!("Couldn't connect: {e}"),
        (_, true) => "Connected: songs you listen to are scrobbled.".to_string(),
        (_, false) => "Not connected. Needs a Last.fm API account (last.fm/api/account/create).".to_string(),
    };
    let fm_button = if connected {
        button("fm-off", "Disconnect", Kind::Plain, |ui| {
            lastfm_disconnect();
            ui.app.lastfm = None;
            ui.app.listen = None;
        })
    } else if p.fm == Fm::Waiting {
        button("fm-wait", "Waiting…", Kind::Plain, |_| {})
    } else {
        button("fm-on", "Connect", Kind::Primary, connect_lastfm)
    };
    let fm_key = input("fm-key", &p.fm_key, 250.0, "API key", false, false, |ui| &mut ui.prefs.fm_key, |_| {});
    let fm_secret = input("fm-secret", &p.fm_secret, 250.0, "Shared secret", false, true, |ui| &mut ui.prefs.fm_secret, connect_lastfm);

    let body = col().key("prefs-body").w_full().px(28.0).pb(24.0).gap(22.0)
        .child(section("s-look", "Appearance", vec![
            line("l-appearance", "Theme", "Light, dark, or whichever macOS is using.", appearance),
        ]))
        .child(section("s-play", "Playback", vec![
            line("l-quality", "Quality", "kbps. 320 is the most Spotify streams to spot.", quality),
            line("l-normalize", "Even out loudness", "Plays every song at about the same volume.", normalize),
            line("l-cache", "Keep played songs", "On disk, so replays start at once and skip the network.", cache),
            line("l-clear", "Clear cache", &format!("{:.0} MB of songs and lists. Your login and settings stay; spot restarts.", p.cached as f64 / 1e6),
                button("clear", "Clear", Kind::Plain, |ui| {
                    clean_cache();
                    ui.restart();
                })),
        ]))
        .child(section("s-keys", "Quick search", vec![
            line("l-hotkey", "Hotkey", "Opens the search from any app. Modifiers and a key, like ctrl+option+s. Return saves.", hotkey),
        ]))
        .child(section("s-spotify", "Spotify", vec![
            line("l-account", "Account", &logout_sub, logout),
            line("l-client", "Client ID", "Your Spotify app's, from developer.spotify.com/dashboard. Return saves.", client),
        ]))
        .child(section("s-lastfm", "Last.fm", vec![
            line("l-fm", "Scrobbling", &fm_status, fm_button),
            line("l-fm-key", "API key", "From your Last.fm API account.", fm_key),
            line("l-fm-secret", "Shared secret", "Saved to ~/.config/spot/lastfm.", fm_secret),
        ]));

    // a page of its own, where the open list goes: the sidebar stays, so a click there leaves
    let top = row().key("prefs-top").w_full().h(52.0).px(16.0).gap(8.0).center_y().window_drag()
        .child(super::view::icon_button("prefs-back", &icons::BACK, 18.0, th.sub, th, "Back  esc", |ui| ui.prefs.open = false));
    let head = col().key("prefs-head").w_full().px(28.0).pt(4.0).pb(18.0)
        .child(text("Settings").font(TITLE).color(th.text));
    let column = col().key("prefs-col").w_full().max_w(720.0).child(head).child(body);
    let mut page = col().key("prefs-page").grow(1.0).h_full().min_w(0.0).fill(th.bg)
        .transition(super::motion::rise(10.0))
        .child(top)
        .child(col().key("prefs-scroll").w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).scroll_edge_fade(18.0)
            .child(row().key("prefs-center").w_full().justify_center().child(column)));
    let note = p.note.clone().or_else(|| p.needs_restart(&a.settings).then(|| "Some changes apply when spot restarts.".to_string()));
    if let Some(n) = note {
        let failed = p.note.is_some();
        let mut foot = row().key("prefs-foot").w_full().h(52.0).px(28.0).gap(12.0).center_y().fill(th.wash)
            .child(text(n).font(BODY).color(if failed { th.err } else { th.text }).grow(1.0).shrink().truncate());
        if !failed {
            foot = foot.child(button("restart", "Restart now", Kind::Primary, |ui| ui.restart()));
        }
        page = page.child(el().key("foot-rule").h(1.0).w_full().fill(th.line)).child(foot);
    }
    page
}

/// Save the keys, then log in to Last.fm in the browser, off the window's thread.
fn connect_lastfm(ui: &mut Ui) {
    let (key, secret) = (ui.prefs.fm_key.text.trim().to_string(), ui.prefs.fm_secret.text.trim().to_string());
    if key.is_empty() || secret.is_empty() {
        ui.prefs.fm = Fm::Failed("add your API key and shared secret first".into());
        return;
    }
    if let Err(e) = save_lastfm_creds(&key, &secret) {
        ui.prefs.fm = Fm::Failed(e);
        return;
    }
    ui.prefs.fm = Fm::Waiting;
    let (rt, redirect, tx, me) = (ui.app.rt.handle().clone(), ui.app.api.redirect.clone(), ui.app.tx.clone(), ui.me.clone());
    std::thread::spawn(move || {
        let r = lastfm_connect(&rt, &redirect, tx);
        if let Some(me) = me {
            me.update(move |ui, tx| {
                match r {
                    Ok(fm) => {
                        ui.app.lastfm = Some(fm);
                        ui.prefs.fm = Fm::Idle;
                    }
                    Err(e) => ui.prefs.fm = Fm::Failed(e),
                }
                ui.after(tx);
            });
        }
    });
}
