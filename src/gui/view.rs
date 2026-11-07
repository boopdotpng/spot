// The main window: a quiet frame (sidebar, list, player) on near-neutral surfaces, with the color
// coming from the music: each list's header takes its covers' colors, the player its song's.

use rune::{Color, Cursor, Cx, El, Phase, ScrollSpec, SurfaceFit, TextStyle, Transition, col, el, icon, row, spacer, text, text_input};

use super::{Ui, icons, player, rows};
use crate::*;

pub const ROW: f32 = 50.0; // a song's row (with its cover)
pub const LINK_ROW: f32 = 46.0;
const HERO_ART: f32 = 112.0;

pub const fn ts(size: f32, weight: f32) -> TextStyle {
    TextStyle::new(size, weight)
}
pub const TITLE: TextStyle = ts(30.0, 760.0).lh(36.0).tracking(-0.6);
pub const BODY: TextStyle = ts(13.0, 450.0).lh(17.0);
pub const BODY_MED: TextStyle = ts(13.0, 560.0).lh(17.0);
pub const SMALL: TextStyle = ts(12.0, 450.0).lh(16.0);
pub const LABEL: TextStyle = ts(10.5, 650.0).lh(13.0).tracking(0.9);
pub const NUM: TextStyle = ts(12.0, 450.0).lh(16.0).features("'tnum' 1");

/// Right angle for `linear_gradient`: CSS degrees as radians.
pub fn deg(d: f32) -> f32 {
    d.to_radians()
}

/// A button that's just an icon: quiet until hovered, with a tooltip saying what it does.
pub fn icon_button(key: &str, i: &'static rune::Icon, size: f32, color: Color, th: &Theme, help: &str, f: impl Fn(&mut Ui) + 'static) -> El {
    row().key(key.to_string()).size(size + 14.0, size + 14.0).center().radius((size + 14.0) / 2.0)
        .hover(th.text.alpha(0.08)).cursor(Cursor::Pointer).press_scale(0.9)
        .help(help.to_string()).label(help.to_string())
        .on_press_tx(move |ui: &mut Ui, tx| {
            f(ui);
            ui.after(tx);
        })
        .child(icon(i, size).color(color))
}

/// A cover, or its stand-in while there's none.
pub fn cover(key: String, image: Option<&rune::Image>, size: f32, radius: f32, th: &Theme, fallback: &'static rune::Icon) -> El {
    let base = el().key(key).size(size, size).radius(radius).clip_rounded(radius);
    match image {
        Some(i) => base.fill(th.wash).pixels_fit(i.pixels(), SurfaceFit::Fill),
        None => base.fill(th.wash).center().child(icon(fallback, size * 0.4).color(th.dim)),
    }
}

/// A tile in two colors for what has no art: a playlist, a show, an artist.
pub fn swatch_tile(key: String, (a, b): (Color, Color), size: f32, radius: f32, glyph: Option<&'static rune::Icon>) -> El {
    let mut t = row().key(key).size(size, size).radius(radius).center()
        .fill(a).linear_gradient(deg(135.0), [(0.0, a), (1.0, b)]);
    if let Some(g) = glyph {
        t = t.child(icon(g, size * 0.5).color(Color::WHITE.alpha(0.92)));
    }
    t
}

/// Three bars, the mark for what's playing: the accent while it plays, faded while paused. They
/// hold still: a looping animation would redraw the window many times a second for as long as
/// the music plays, which is most of the battery a music player's window can cost.
pub fn eq(color: Color, playing: bool, key: &str) -> El {
    let c = if playing { color } else { color.alpha(0.45) };
    let bar = |i: usize, h: f32| el().key(format!("{key}-{i}")).w(2.5).h(h).radius(1.25).fill(c);
    row().key(key.to_string()).h(12.0).gap(2.0).items_end()
        .child(bar(0, 7.0)).child(bar(1, 12.0)).child(bar(2, 9.0))
}

pub fn view(ui: &Ui, cx: &mut Cx<Ui>) -> El {
    DARK.store(cx.system_dark(), std::sync::atomic::Ordering::Relaxed);
    let th = theme(cx.system_dark());
    let body = row().key("body").w_full().grow(1.0).min_h(0.0)
        .child(sidebar(ui, &th))
        .child(main_pane(ui, &th, cx));
    let mut root = col().key("root").w_full().h_full().fill(th.bg)
        .child(body)
        .child(player::player(ui, &th));
    if let Some(p) = message(ui, &th) {
        root = root.child(p);
    }
    if ui.app.help {
        root = root.child(help(&th));
    }
    root
}

/// Whether the window last drew dark (rows are built where the view's `Cx` isn't at hand).
pub static DARK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// The theme a row builder draws in: rows are built from state alone, so they read the light
/// or dark the window last drew in.
pub fn theme_now() -> Theme {
    theme(DARK.load(std::sync::atomic::Ordering::Relaxed))
}

// ---------- sidebar ----------

fn sidebar(ui: &Ui, th: &Theme) -> El {
    let a = &ui.app;
    let focused = a.focus == Focus::Playlists;
    let tab = |label: &'static str, on: bool, podcasts: bool| {
        col().key(format!("tab-{label}")).gap(5.0).cursor(Cursor::Pointer)
            .on_press_tx(move |ui: &mut Ui, tx| {
                if ui.app.podcasts != podcasts {
                    ui.app.toggle_podcasts();
                }
                ui.after(tx);
            })
            .child(text(label).font(ts(13.0, if on { 650.0 } else { 550.0 })).color(if on { th.text } else { th.dim }))
            .child(el().key("under").h(2.0).w_full().radius(1.0).fill(if on { th.accent } else { Color::TRANSPARENT }))
    };
    let tabs = row().key("side-tabs").px(18.0).gap(18.0)
        .child(tab("Playlists", !a.podcasts, false))
        .child(tab("Podcasts", a.podcasts, true));
    let playing_key = a.queue.as_ref().map(|q| q.key.as_str());
    let row_for = |i: usize, name: &str, key: &str, tile: El, selected: bool| {
        let open = a.view.key == key;
        let playing = Some(key) == playing_key;
        let fill = if selected && focused { th.sel } else if open { th.wash } else { Color::TRANSPARENT };
        let mut r = row().key(format!("side-{i}")).h(32.0).w_full().px(8.0).gap(10.0).center_y().radius(8.0)
            .fill(fill).hover(th.text.alpha(0.05)).cursor(Cursor::Pointer)
            .on_press_tx(move |ui: &mut Ui, tx| {
                ui.app.focus = Focus::Playlists;
                if ui.app.podcasts { ui.app.sh_state.select(Some(i)) } else { ui.app.pl_state.select(Some(i)) }
                ui.app.enter();
                ui.after(tx);
            })
            .child(tile)
            .child(text(name.to_string()).font(if open || playing { BODY_MED } else { BODY })
                .color(if playing { th.accent } else if open { th.text } else { th.sub }).truncate().grow(1.0).shrink()
                .help_when_truncated(name.to_string()));
        if playing {
            r = r.child(eq(th.accent, a.playing, &format!("side-eq-{i}")));
        }
        r
    };
    let mut list = col().key("side-rows").w_full().px(8.0).pt(4.0).pb(16.0).gap(1.0);
    if a.podcasts {
        for (i, s) in a.shows.iter().enumerate() {
            let pinned = !is_show(&s.uri);
            let tile = if pinned {
                let ic = if s.uri == NEW_KEY { &icons::SPARK } else { &icons::QUEUE };
                swatch_tile("t".into(), (th.accent, th.accent2), 20.0, 6.0, Some(ic))
            } else {
                swatch_tile("t".into(), swatch(&s.name, th), 20.0, 6.0, Some(&icons::MIC))
            };
            list = list.child(row_for(i, &s.name, &s.uri, tile, a.sh_state.selected() == Some(i)));
        }
    } else {
        for (i, p) in a.playlists.iter().enumerate() {
            let liked = p.id.is_none();
            let name = if liked { "Liked Songs" } else { p.name.as_str() };
            let tile = if liked {
                swatch_tile("t".into(), (th.pink, th.accent), 20.0, 6.0, Some(&icons::HEART))
            } else {
                swatch_tile("t".into(), swatch(&p.name, th), 20.0, 6.0, None)
            };
            list = list.child(row_for(i, name, &p.key(), tile, a.pl_state.selected() == Some(i)));
        }
    }
    let scroller = col().key("side-scroll").w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).scroll_edge_fade(18.0).child(list);

    // the themes, as dots: click one to wear it
    let current = FAMILY.load(std::sync::atomic::Ordering::Relaxed).min(families().len() - 1);
    let mut dots = row().key("dots").gap(7.0).center_y();
    for i in 0..families().len() {
        let on = i == current;
        let c = family_accent(i, th.dark);
        dots = dots.child(row().key(format!("dot-{i}")).size(16.0, 16.0).radius(8.0).center().cursor(Cursor::Pointer)
            .border(1.5, if on { c } else { Color::TRANSPARENT }).press_scale(0.8)
            .help(families()[i].0.clone())
            .on_press_tx(move |ui: &mut Ui, tx| {
                set_family(i);
                ui.after(tx);
            })
            .child(el().key("d").size(8.0, 8.0).radius(4.0).fill(c)));
    }
    let footer = row().key("side-foot").w_full().h(44.0).px(14.0).gap(8.0).center_y()
        .child(dots).child(spacer())
        .child(icon_button("keys", &icons::KEYS, 16.0, th.dim, th, "Keyboard shortcuts  ?", |ui| ui.app.help = true));

    let rule = el().key("side-rule").pin(None, Some(0.0), Some(0.0), Some(0.0)).w(1.0).fill(th.line);
    // the edge drags the sidebar wider or narrower
    let grip = el().key("side-grip").pin(None, Some(0.0), Some(-4.0), Some(0.0)).w(8.0).cursor(Cursor::ResizeColumn)
        .on_drag(|ui: &mut Ui, d, tx| {
            if d.phase == Phase::Down {
                d.data = ui.side_w;
            }
            ui.side_w = (d.data + d.dx).clamp(180.0, 420.0).round();
            tx.instant();
        });
    col().key("sidebar").w(ui.side_w).h_full().fill(th.side)
        .child(row().key("side-drag").w_full().h(52.0).window_drag())
        .child(tabs)
        .child(el().h(12.0))
        .child(scroller)
        .child(el().key("foot-rule").h(1.0).mx(14.0).fill(th.line))
        .child(footer)
        .child(rule)
        .child(grip)
}

// ---------- the open list ----------

/// What a list is, over its title.
fn kicker(key: &str) -> &'static str {
    match key {
        "liked" => "your library",
        k if k == QUEUE_KEY => "queue",
        k if k.starts_with("spotify:album:") => "album",
        k if k.starts_with("spotify:artist:") => "artist",
        k if k.starts_with("spotify:show:") => "podcast",
        k if k.starts_with("podcasts:") => "podcasts",
        k if k.starts_with("search:") => "search",
        "" => "",
        _ => "playlist",
    }
}

/// The colors and art over a list: the covers it opens with, or what stands in for them.
fn hero_art(ui: &Ui, th: &Theme) -> (El, Color, Color) {
    let a = &ui.app;
    let v = &a.view;
    let size = HERO_ART;
    let r = 12.0;
    let covers: Vec<&str> = {
        let mut seen: Vec<&str> = vec![];
        for t in v.tracks.iter() {
            if let Some(c) = t.cover.as_deref()
                && !seen.contains(&c)
            {
                seen.push(c);
                if seen.len() == 4 {
                    break;
                }
            }
        }
        seen
    };
    let tints: Vec<Color> = covers.iter().filter_map(|c| ui.thumb_and_tint(&art::small(c)).map(|(_, t)| cover_color(t, th))).collect();
    let c1 = tints.first().copied();
    let c2 = tints.get(1).copied().or(c1);
    if v.key == "liked" {
        return (swatch_tile("hero-art".into(), (th.pink, th.accent), size, r, Some(&icons::HEART)), th.pink, th.accent);
    }
    if v.key == QUEUE_KEY {
        let now = ui.cover.as_ref().map(|(_, i)| i);
        let tint = ui.tint.map(|t| cover_color(t, th)).unwrap_or(th.accent);
        let art = match now {
            Some(i) => cover("hero-art".into(), Some(i), size, r, th, &icons::QUEUE),
            None => swatch_tile("hero-art".into(), (th.accent, th.accent2), size, r, Some(&icons::QUEUE)),
        };
        return (art, tint, th.accent2);
    }
    if v.kind != Kind::Tracks || covers.is_empty() {
        let pair = swatch(&v.title, th);
        let glyph = match v.kind {
            Kind::Artists => &icons::PERSON,
            Kind::Shows => &icons::MIC,
            Kind::Albums if v.artist.is_some() => &icons::PERSON,
            Kind::Albums => &icons::ALBUM,
            Kind::Tracks if is_episodes(&v.key) => &icons::MIC,
            Kind::Tracks => &icons::NOTE,
        };
        return (swatch_tile("hero-art".into(), pair, size, r, Some(glyph)), pair.0, pair.1);
    }
    let one = covers.len() < 4 || v.key.starts_with("spotify:album:") || is_episodes(&v.key);
    let art = if one {
        let img = ui.thumb(&art::medium(covers[0])).or_else(|| ui.thumb(&art::small(covers[0])));
        cover("hero-art".into(), img.as_ref(), size, r, th, &icons::NOTE)
    } else {
        // the first four albums in it, as a square
        let q = size / 2.0;
        let cell = |i: usize| cover(format!("m{i}"), ui.thumb(&art::small(covers[i])).as_ref(), q, 0.0, th, &icons::NOTE);
        col().key("hero-art").size(size, size).radius(r).clip_rounded(r)
            .child(row().key("m-top").child(cell(0)).child(cell(1)))
            .child(row().key("m-bot").child(cell(2)).child(cell(3)))
    };
    (art, c1.unwrap_or(th.accent), c2.unwrap_or(th.accent2))
}

fn main_pane(ui: &Ui, th: &Theme, cx: &mut Cx<Ui>) -> El {
    let a = &ui.app;
    let v = &a.view;
    let back = !a.history.is_empty();
    let mut top = row().key("top").w_full().h(52.0).px(16.0).gap(8.0).center_y().window_drag();
    if back {
        top = top.child(icon_button("back", &icons::BACK, 18.0, th.sub, th, "Back  esc", |ui| ui.app.back()));
    }
    top = top.child(spacer());
    if a.find.is_some() {
        top = top.child(find_field(ui, th));
    }
    top = top.child(search_field(ui, th));

    // title, and what's in it
    let tracks = v.kind == Kind::Tracks;
    let episodes = is_episodes(&v.key);
    let meta = if v.loading && v.len() == 0 {
        "loading…".to_string()
    } else if v.key.is_empty() {
        String::new()
    } else if tracks {
        let ms = v.tracks.iter().map(|t| t.ms).sum();
        format!("{}  ·  {}", plural(v.len(), if episodes { "episode" } else { "song" }), hours(ms))
    } else {
        let what = match v.kind {
            Kind::Artists => "artist",
            Kind::Shows => "podcast",
            _ if v.artist.is_some() => match v.filter {
                Releases::All => "release",
                Releases::Albums => "album",
                Releases::Singles => "single",
            },
            _ => "album",
        };
        plural(v.len(), what)
    };
    let meta = if v.loading && v.len() > 0 { format!("{meta}  ·  loading…") } else { meta };
    let (art, c1, c2) = hero_art(ui, th);
    let title = v.title.trim_start_matches(['♥', ' ']).to_string();
    let mut words = col().key("hero-words").grow(1.0).shrink().min_w(0.0).gap(4.0)
        .child(text(kicker(&v.key).to_uppercase()).key("kicker").font(LABEL).color(c1.mix(th.text, 0.35)))
        .child(text(title.clone()).key("title").font(TITLE).color(th.text).truncate().help_when_truncated(title))
        .child(text(meta).key("meta").font(SMALL).color(th.sub));
    if a.input.is_some() || a.live_search {
        words = words.child(kinds(ui, th));
    }
    let mut hero_row = row().key("hero-row").w_full().px(28.0).pb(20.0).gap(22.0).items_end()
        .child(art.shadow(10.0, 34.0, c1.alpha(if th.dark { 0.45 } else { 0.35 })).transition(Transition::blur_replace()))
        .child(words);
    if tracks && v.len() > 0 {
        hero_row = hero_row.child(hero_actions(ui, th, c1, c2));
    }
    // the covers' colors, washed down from the top of the list and fading out before the rows
    let (s1, s2) = if th.dark { (0.34, 0.22) } else { (0.30, 0.20) };
    let wash = el().key("hero-wash").pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
        .linear_gradient(deg(180.0), [(0.0, c1.alpha(s1)), (0.7, c1.alpha(s1 * 0.25)), (1.0, c1.alpha(0.0))])
        .motion(rune::Spring::new(0.8, 0.0));
    let glint = el().key("hero-glint").pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
        .radial_gradient([0.92, 0.0], [520.0, 260.0], [(0.0, c2.alpha(s2)), (1.0, c2.alpha(0.0))])
        .motion(rune::Spring::new(0.8, 0.0));
    let hero = col().key("hero").w_full().child(wash).child(glint).child(top).child(hero_row);

    let list: El = if v.len() == 0 && !v.loading && !v.key.is_empty() {
        col().key("empty").w_full().grow(1.0).center().gap(10.0)
            .child(icon(&icons::SPARK, 28.0).color(th.dim.alpha(0.6)))
            .child(text(if a.find.is_some() { "nothing matches" } else { "nothing here yet" }).font(BODY).color(th.dim))
    } else if tracks {
        rows::track_list(ui, th, cx)
    } else {
        rows::link_list(ui, th)
    };
    col().key("main").grow(1.0).shrink().h_full().min_w(0.0)
        .child(hero).child(list)
}

/// Play the list, or play it shuffled; and find in it.
fn hero_actions(ui: &Ui, th: &Theme, c1: Color, c2: Color) -> El {
    let playing_here = ui.app.queue.as_ref().is_some_and(|q| q.key == ui.app.view.key) && ui.app.want_play;
    let play = row().key("hero-play").size(48.0, 48.0).radius(24.0).center().cursor(Cursor::Pointer).press_scale(0.9)
        .fill(th.accent).linear_gradient(deg(135.0), [(0.0, th.accent), (1.0, c2.mix(th.accent, 0.35))])
        .shadow(6.0, 22.0, c1.alpha(0.45))
        .help(if playing_here { "Pause  space" } else { "Play  P" }).label("Play")
        .on_press_tx(move |ui: &mut Ui, tx| {
            if playing_here { ui.app.toggle() } else { ui.app.play_from_view(None) }
            ui.after(tx);
        })
        .child(icon(if playing_here { &icons::PAUSE } else { &icons::PLAY }, 22.0).color(th.on_accent));
    let shuffled = icon_button("hero-shuffle", &icons::SHUFFLE, 18.0, th.sub, th, "Shuffle play", |ui| {
        if !ui.app.shuffle {
            ui.app.toggle_shuffle();
        }
        ui.app.play_from_view(None);
    });
    let find = icon_button("hero-find", &icons::FIND, 18.0, th.sub, th, "Find in list  ⌘F", |ui| ui.open_find());
    row().key("hero-actions").gap(6.0).center_y().child(find).child(shuffled).child(play)
}

/// A rounded text field. `wire` binds the input inside it (its edit, change and key handlers
/// belong on the `text_input` itself, or the field stays read-only).
fn field(th: &Theme, key: &str, edit: &rune::TextEdit, w: f32, i: &'static rune::Icon, placeholder: &str, wire: impl FnOnce(El) -> El) -> El {
    let focused = edit.focused;
    row().key(format!("{key}-box")).w(w).h(30.0).px(10.0).gap(8.0).center_y().radius(15.0)
        .fill(if focused { th.raised } else { th.text.alpha(if th.dark { 0.07 } else { 0.05 }) })
        .border(1.0, if focused { th.accent.alpha(0.65) } else { th.line })
        .shadow(1.0, if focused { 10.0 } else { 0.0 }, th.accent.alpha(if focused { 0.18 } else { 0.0 }))
        .child(icon(i, 14.0).color(if focused { th.accent } else { th.dim }))
        .child(wire(text_input(edit, BODY).key(key.to_string()).single_line().grow(1.0).color(th.text).placeholder(placeholder.to_string(), th.dim)))
}

fn search_field(ui: &Ui, th: &Theme) -> El {
    let label = format!("Search {}", ui.app.search_kind.label());
    field(th, "search", &ui.search, 260.0, &icons::SEARCH, &label, |f| f.on_edit(|ui: &mut Ui| &mut ui.search)
        .on_input_change(|ui: &mut Ui, tx| {
            let t = ui.search.text.clone();
            ui.app.set_search(&t);
            ui.after(tx);
        })
        .on_input_key(|ui: &mut Ui, ev, tx| {
            let Some(k) = super::key_of(&ev.key, ev.mods) else { return false };
            if !matches!(k, Key::Enter | Key::Esc | Key::Tab | Key::BackTab | Key::Up | Key::Down) {
                return false;
            }
            let took = ui.app.on_search_key(k);
            if matches!(k, Key::Esc) {
                ui.search.clear();
                tx.blur();
            }
            if matches!(k, Key::Enter) {
                tx.blur();
            }
            ui.after(tx);
            took
        }))
}

fn find_field(ui: &Ui, th: &Theme) -> El {
    let shown = ui.app.shown().len();
    let f = field(th, "find", &ui.find, 200.0, &icons::FIND, "Find in list", |f| {
        f.on_edit(|ui: &mut Ui| &mut ui.find)
            .on_input_change(|ui: &mut Ui, tx| {
                let t = ui.find.text.clone();
                ui.app.set_find(&t);
                ui.after(tx);
            })
            .on_input_key(|ui: &mut Ui, ev, tx| {
                let Some(k) = super::key_of(&ev.key, ev.mods) else { return false };
                if !matches!(k, Key::Enter | Key::Esc | Key::Up | Key::Down | Key::PageUp | Key::PageDown) {
                    return false;
                }
                let took = ui.app.on_find_key(k);
                if matches!(k, Key::Esc | Key::Enter) {
                    ui.find.clear();
                    tx.blur();
                }
                ui.after(tx);
                took
            })
    });
    let count = text(format!("{} of {}", num(shown), num(ui.app.view.len()))).font(SMALL).color(th.dim);
    row().key("find-row").gap(10.0).center_y().child(count).child(f)
}

/// While searching: what to search for, as chips (tab cycles them).
fn kinds(ui: &Ui, th: &Theme) -> El {
    let mut r = row().key("kinds").gap(6.0).pt(8.0);
    for k in [Kind::Tracks, Kind::Albums, Kind::Artists, Kind::Shows] {
        let on = ui.app.search_kind == k;
        r = r.child(row().key(format!("kind-{}", k.label())).h(26.0).px(12.0).center().radius(13.0)
            .fill(if on { th.accent } else { th.text.alpha(0.07) })
            .hover(if on { th.accent } else { th.text.alpha(0.11) })
            .cursor(Cursor::Pointer).press_scale(0.94)
            .on_press_tx(move |ui: &mut Ui, tx| {
                ui.app.search_kind = k;
                ui.app.search_at = Some(Instant::now());
                ui.after(tx);
            })
            .child(text(k.label()).font(ts(12.0, 600.0)).color(if on { th.on_accent } else { th.sub })));
    }
    r
}

// ---------- notes, errors, and the keys ----------

/// What the old status line said: an error, a passing note, or that it's connecting.
fn message(ui: &Ui, th: &Theme) -> Option<El> {
    let a = &ui.app;
    let (s, c) = if !a.status.is_empty() {
        (a.status.clone(), th.err)
    } else if let Some((n, _)) = &a.note {
        (n.clone(), th.accent)
    } else if a.sess.borrow().is_none() {
        ("connecting…".to_string(), th.sub)
    } else {
        return None;
    };
    Some(row().key("message").pin(None, None, None, Some(player::PLAYER + 16.0)).w_full().justify_center()
        .child(row().key(format!("message-{s}")).max_w(620.0).h(34.0).px(16.0).gap(8.0).center_y().radius(17.0)
            .fill(th.raised).border(1.0, c.alpha(0.3))
            .shadow(6.0, 24.0, Color::BLACK.alpha(if th.dark { 0.4 } else { 0.12 }))
            .transition(Transition::opacity().combined(Transition::offset(0.0, 8.0)))
            .child(el().key("dot").size(7.0, 7.0).radius(3.5).fill(c))
            .child(text(s).font(BODY_MED).color(th.text).truncate())))
}

fn help(th: &Theme) -> El {
    const KEYS: &[(&str, &str)] = &[
        ("/  ⌘K", "search"),
        ("⌘F", "find in list"),
        ("⌥Space", "quick search, from any app"),
        ("↩", "play / open"),
        ("space", "pause"),
        ("n  p", "next / previous"),
        (",  .", "seek 10s"),
        ("+  -", "volume"),
        ("s", "shuffle"),
        ("r", "repeat"),
        ("P", "play whole list"),
        ("o", "go to playing"),
        ("e", "add to queue"),
        ("Q", "up next"),
        ("x", "queue: remove song"),
        ("m", "playlists / podcasts"),
        ("a  A", "album / artist"),
        ("v", "artist: songs / releases"),
        ("f", "releases: all / albums / singles"),
        ("esc", "back"),
        ("tab", "switch pane"),
        ("g  M  G", "top / middle / bottom"),
        ("⌘T", "next theme"),
        ("⌘W", "close (keeps playing)"),
        ("⌘Q", "quit"),
    ];
    let half = KEYS.len().div_ceil(2);
    let cap = |k: &str| row().key(format!("cap-{k}")).h(20.0).px(7.0).center().radius(5.0)
        .fill(th.wash).border(1.0, th.line)
        .child(text(k.to_string()).font(ts(11.5, 600.0)).color(th.text));
    let column = |keys: &[(&str, &str)], k: &str| {
        let mut c = col().key(k.to_string()).gap(6.0).w(300.0);
        for (i, (key, what)) in keys.iter().enumerate() {
            let mut caps = row().key("caps").w(96.0).gap(4.0).justify_end();
            for part in key.split("  ") {
                caps = caps.child(cap(part));
            }
            c = c.child(row().key(format!("{k}-{i}")).gap(12.0).center_y()
                .child(caps)
                .child(text(*what).font(BODY).color(th.sub)));
        }
        c
    };
    let card = col().key("help-card").padding(28.0).gap(18.0).radius(18.0)
        .fill(th.raised).border(1.0, th.line)
        .shadow(18.0, 60.0, Color::BLACK.alpha(if th.dark { 0.5 } else { 0.18 }))
        .transition(Transition::opacity().combined(Transition::offset(0.0, 10.0)))
        .child(row().key("help-head").gap(10.0).center_y()
            .child(icon(&icons::KEYS, 18.0).color(th.accent))
            .child(text("Keys").font(ts(18.0, 700.0)).color(th.text))
            .child(spacer())
            .child(text("any key closes").font(SMALL).color(th.dim)))
        .child(row().key("help-cols").gap(24.0).child(column(&KEYS[..half], "hc0")).child(column(&KEYS[half..], "hc1")));
    row().key("help").pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).center().fill(Color::BLACK.alpha(0.35))
        .transition(Transition::opacity())
        .on_press_tx(|ui: &mut Ui, tx| {
            ui.app.help = false;
            ui.after(tx);
        })
        .child(card)
}
