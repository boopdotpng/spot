// The main window: a quiet frame (sidebar, list, player) where the covers are the only color.
// The surfaces lean toward the playing cover's hue, one accent taken from it marks what's
// playing, and a list's header fades down from its own cover's color.

use rune::{Color, Cursor, Cx, El, Phase, ScrollSpec, SurfaceFit, TextStyle, Transition, col, el, icon, row, spacer, text, text_input};

use super::menu::{self, Menu};
use super::{Ui, artist, icons, motion, player, rows, search};
use crate::*;

pub const ROW: f32 = 44.0; // a song's row (with its cover)
pub const LINK_ROW: f32 = 46.0;
const HERO_ART: f32 = 128.0;
const HERO_WASH: f32 = 240.0; // how far down the header's color fades into the window

pub const fn ts(size: f32, weight: f32) -> TextStyle {
    TextStyle::new(size, weight)
}
pub const TITLE: TextStyle = ts(24.0, 720.0).lh(30.0).tracking(-0.4);
/// A list's name over it.
const HERO_TITLE: TextStyle = ts(36.0, 760.0).lh(42.0).tracking(-0.9);
/// `style`, made smaller (down to `min` points) until `s` fits on one line `width` points wide,
/// so a long title or name shrinks before it's cut short. The line height stays, so the header
/// doesn't move as it does; past `min` the text still truncates.
pub fn fit(s: &str, style: TextStyle, width: f32, min: f32) -> TextStyle {
    let wide = |st: TextStyle| rune::text::measure(s, &[], st, None).0;
    if width <= 0.0 || wide(style) <= width {
        return style;
    }
    let at = |size: f32| TextStyle { size, tracking: style.tracking * size / style.size, ..style };
    // width goes nearly as the size does: guess from that, then step down by half points
    let mut size = (style.size * width / wide(style)).floor().clamp(min, style.size);
    while size > min && wide(at(size)) > width {
        size = (size - 0.5).max(min);
    }
    at(size)
}
pub const BODY: TextStyle = ts(13.0, 450.0).lh(17.0);
pub const BODY_MED: TextStyle = ts(13.0, 560.0).lh(17.0);
pub const SMALL: TextStyle = ts(12.0, 450.0).lh(16.0);
pub const LABEL: TextStyle = ts(9.5, 600.0).lh(13.0).tracking(1.4);
/// Numbers, times and dates: the window's own face, with tabular figures so columns of them line up.
pub const NUM: TextStyle = ts(12.0, 450.0).lh(16.0).features("'tnum' 1");

/// A button that's just an icon: quiet (a little faded) until hovered, with a tooltip saying what
/// it does.
pub fn icon_button(key: &str, i: &'static rune::Icon, size: f32, color: Color, th: &Theme, help: &str, f: impl Fn(&mut Ui) + 'static) -> El {
    row().key(key.to_string()).size(size + 14.0, size + 14.0).center().radius((size + 14.0) / 2.0)
        .hover(th.text.alpha(0.06)).hover_group().cursor(Cursor::Pointer).press_scale(0.9)
        .help(help.to_string()).label(help.to_string())
        .on_press_tx(move |ui: &mut Ui, tx| {
            f(ui);
            ui.after(tx);
        })
        .child(icon(i, size).color(color).group_opacity(0.75, 1.0))
}

/// `names`, with each artist in `links` (uri, name: the first of `names`, in order) a way to their
/// page; past them, plain. `pre` runs before going (to pick a row); `help` is the first's.
pub fn artist_line(key: impl Into<String>, names: &str, links: &[(String, String)], font: TextStyle, c: Color, hover: Color, help: &str, pre: impl Fn(&mut Ui) + Clone + 'static) -> El {
    let mut line = row().key(key.into()).min_w(0.0).shrink();
    let mut at = 0;
    for (i, (uri, name)) in links.iter().enumerate() {
        if !names[at..].starts_with(name.as_str()) {
            break;
        }
        if i > 0 {
            line = line.child(text(",\u{a0}").key(format!("sep-{i}")).font(font).color(c));
        }
        let (uri, n, pre) = (uri.clone(), name.clone(), pre.clone());
        line = line.child(row().key(format!("a-{i}")).min_w(0.0).shrink().hover_group().cursor(Cursor::Pointer)
            .help(if i == 0 { help.to_string() } else { format!("Go to {name}") })
            .on_press_tx(move |ui: &mut Ui, tx| {
                pre(ui);
                ui.app.open_artist(uri.clone(), n.clone());
                ui.after(tx);
            })
            .child(text(name.clone()).font(font).color(c).group_color(hover).truncate()));
        at += name.len();
        match names[at..].strip_prefix(", ") {
            Some(_) if i + 1 < links.len() => at += 2,
            _ => break,
        }
    }
    if at < names.len() {
        line = line.child(text(names[at..].to_string()).key("rest").font(font).color(c).truncate().shrink().help_when_truncated(names.to_string()));
    }
    line
}

/// A cover, or its stand-in while there's none.
pub fn cover(key: String, image: Option<&rune::Image>, size: f32, radius: f32, th: &Theme, fallback: &'static rune::Icon) -> El {
    // the picture is a child of its own, keyed by the picture, so one that arrives after the list
    // shows comes into focus over the placeholder, and a new one over the one before it, instead
    // of snapping in
    let base = el().key(key).size(size, size).radius(radius).clip_rounded(radius).fill(th.wash).center();
    match image {
        Some(i) => {
            let id = u64::from_le_bytes(i.id()[..8].try_into().unwrap_or_default());
            base.child(el().key(format!("pic-{id:x}")).size(size, size).pixels_fit(i.pixels(), SurfaceFit::Fill).transition(motion::focus()))
        }
        None => base.child(icon(fallback, size * 0.4).key("glyph").color(th.dim)),
    }
}

/// A plain tile for what has no art (a playlist, a show, an artist): a flat square
/// with a quiet glyph, so the covers stay the only color on screen.
pub fn tile(key: String, size: f32, radius: f32, glyph: Option<&'static rune::Icon>, th: &Theme) -> El {
    let mut t = row().key(key).size(size, size).radius(radius).center().fill(th.wash).border(1.0, th.line);
    if let Some(g) = glyph {
        t = t.child(icon(g, (size * 0.48).max(11.0)).color(th.sub));
    }
    t
}

/// Liked Songs keeps its recognizable cover in both the sidebar and the list header.
pub(super) fn liked_cover(key: &str, size: f32, radius: f32) -> El {
    row().key(key.to_string()).size(size, size).radius(radius).clip_rounded(radius).center()
        .linear_gradient(135.0_f32.to_radians(), [(0.0, crate::theme::rgb(0x450af5)), (1.0, crate::theme::rgb(0xc4efd9))])
        .child(icon(&icons::HEART, size * 0.72).color(Color::WHITE))
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
    DARK.store(crate::theme::dark(cx.system_dark()), std::sync::atomic::Ordering::Relaxed);
    let th = theme(crate::theme::dark(cx.system_dark()));
    let pane = if ui.prefs.open { super::prefs::page(ui, &th) } else { main_pane(ui, &th, cx) };
    let body = row().key("body").w_full().grow(1.0).min_h(0.0)
        .child(sidebar(ui, &th))
        .child(pane);
    let mut root = col().key("root").w_full().h_full().fill(th.bg)
        .child(body)
        .child(player::player(ui, &th));
    if let Some(p) = message(ui, &th) {
        root = root.child(p);
    }
    if ui.app.help {
        root = root.child(help(&th));
    }
    super::menu::view(ui, &th, root)
}

/// Whether the window last drew dark (rows are built where the view's `Cx` isn't at hand).
pub static DARK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// The theme a row builder draws in: rows are built from state alone, so they read the light
/// or dark the window last drew in.
pub fn theme_now() -> Theme {
    theme(DARK.load(std::sync::atomic::Ordering::Relaxed))
}

// ---------- sidebar ----------

/// What can be done with a playlist in the sidebar, as a menu.
fn playlist_menu(ui: &mut Ui, i: usize, at: menu::At) {
    let Some(p) = ui.app.playlists.get(i) else { return };
    let pin = if ui.app.is_pinned(p) { "Unpin" } else { "Pin to top" };
    let m = Menu::new()
        .item("open", "Open", Some(&icons::OPEN))
        .item("pin", pin, Some(&icons::PIN))
        .separator()
        .item("copy", "Copy link", Some(&icons::LINK));
    menu::open(ui, m, at, move |ui: &mut Ui, picked, tx| match picked {
        "open" => {
            ui.app.focus = Focus::Playlists;
            ui.app.pl_state.select(Some(i));
            ui.app.enter();
        }
        "pin" => ui.app.toggle_pin(i),
        "copy" => {
            let uri = ui.app.playlists.get(i).map(Playlist::uri);
            rows::copy_link(&mut ui.app, tx, uri);
        }
        _ => {}
    });
}

const SIDE_ROW: f32 = 32.0;
const SIDE_GAP: f32 = 1.0;
const SIDE_RULE: f32 = 9.0; // the hairline under the pinned lists, and the space around it

fn sidebar(ui: &Ui, th: &Theme) -> El {
    let a = &ui.app;
    let focused = a.focus == Focus::Playlists;
    // the tabs: the open one in full weight and white, the other grey
    let tab = |label: &'static str, on: bool, podcasts: bool| {
        row().key(format!("tab-{label}")).cursor(Cursor::Pointer).hover_group()
            .on_press_tx(move |ui: &mut Ui, tx| {
                ui.prefs.open = false;
                if ui.app.podcasts != podcasts {
                    ui.app.toggle_podcasts();
                }
                ui.after(tx);
            })
            .child(text(label).font(ts(13.0, if on { 650.0 } else { 500.0 })).color(if on { th.text } else { th.dim }).group_color(th.text))
    };
    let tabs = row().key("side-tabs").px(18.0).gap(16.0)
        .child(tab("Playlists", !a.podcasts, false))
        .child(tab("Podcasts", a.podcasts, true));
    let playing_key = a.queue.as_ref().map(|q| q.key.as_str());
    let row_for = |i: usize, name: &str, key: &str, tile: El, selected: bool, pinned: bool| {
        let open = a.view.key == key;
        let playing = Some(key) == playing_key;
        let key_owned = key.to_string();
        let key_press = key_owned.clone();
        // the keyboard's cursor is lit as the song list's cursor is; the open list sits on the
        // highlight that slides from list to list (under the rows, below)
        let fill = if !open && selected && focused { th.sel } else { Color::TRANSPARENT };
        let mut r = row().key(format!("side-{i}")).h(SIDE_ROW).w_full().px(8.0).gap(10.0).center_y().radius(7.0)
            .fill(fill).hover(if open { Color::TRANSPARENT } else { th.wash }).hover_group().cursor(Cursor::Pointer)
            .on_press_tx(move |ui: &mut Ui, tx| {
                ui.prefs.open = false;
                ui.app.focus = Focus::Playlists;
                if ui.app.podcasts { ui.app.sh_state.select(Some(i)) } else { ui.app.pl_state.select(Some(i)) }
                // already open (as on a double click's second press): keep it, rather than open it
                // again, which would reset its scroll and cursor and refetch it
                if ui.app.view.key == key_press {
                    ui.app.focus = Focus::Tracks;
                } else {
                    ui.app.enter();
                }
                ui.after(tx);
            })
            // a double click shuffles it and plays (the first click has opened it)
            .on_double_press_tx(move |ui: &mut Ui, tx| {
                ui.prefs.open = false;
                ui.app.focus = Focus::Playlists;
                if ui.app.podcasts { ui.app.sh_state.select(Some(i)) } else { ui.app.pl_state.select(Some(i)) }
                if ui.app.view.key != key_owned {
                    ui.app.enter();
                }
                ui.app.shuffle_play_view();
                ui.after(tx);
            })
            // drawn as the song list's rows are: the highlight marks the open list, the bars the playing one
            .child(tile)
            .child(text(name.to_string()).font(BODY).color(if playing { th.accent } else { th.text }).truncate().grow(1.0).shrink()
                .help_when_truncated(name.to_string()));
        if playing {
            r = r.child(eq(th.accent, a.playing, &format!("side-eq-{i}")));
        } else if pinned {
            r = r.child(icon(&icons::PIN, 12.0).color(th.sub));
        }
        r
    };
    let mut list = col().key("side-rows").w_full().px(8.0).pt(4.0).pb(16.0).gap(SIDE_GAP);
    // the open list's highlight: one element, under the rows, that slides to whichever list
    // opens (its own for each tab, so changing tabs doesn't send it flying)
    let keys: Vec<String> = if a.podcasts { a.shows.iter().map(|s| s.uri.clone()).collect() } else { a.playlists.iter().map(Playlist::key).collect() };
    let split = (!a.podcasts && a.playlists.iter().any(|p| a.is_pinned(p)))
        .then(|| a.playlists.iter().rposition(|p| p.id.is_none() || a.is_pinned(p))).flatten()
        .filter(|&last| last + 1 < a.playlists.len());
    if let Some(i) = keys.iter().position(|k| *k == a.view.key) {
        let y = i as f32 * (SIDE_ROW + SIDE_GAP) + if split.is_some_and(|s| i > s) { SIDE_RULE + SIDE_GAP } else { 0.0 };
        list = list.child(el().key(if a.podcasts { "side-lit-shows" } else { "side-lit-lists" })
            .pin(Some(8.0), Some(4.0), Some(8.0), None).h(SIDE_ROW).radius(7.0).fill(th.sel)
            .offset(0.0, y).offset_motion(motion::SLIDE));
    }
    if a.podcasts {
        for (i, s) in a.shows.iter().enumerate() {
            let pinned = !is_show(&s.uri);
            let ic = if !pinned { &icons::MIC } else if s.uri == NEW_KEY { &icons::SPARK } else { &icons::QUEUE };
            let tile = match s.image.as_deref().map(art::small).and_then(|u| ui.thumb(&u)) {
                Some(img) => cover("t".into(), Some(&img), 20.0, 4.0, th, ic),
                None => tile("t".into(), 20.0, 5.0, Some(ic), th),
            };
            list = list.child(row_for(i, &s.name, &s.uri, tile, a.sh_state.selected() == Some(i), false));
        }
    } else {
        // Liked Songs and the pinned are one group, a hairline (`split`, above) over the rest
        for (i, p) in a.playlists.iter().enumerate() {
            let liked = p.id.is_none();
            let name = if liked { "Liked Songs" } else if p.name.trim().is_empty() { "Untitled playlist" } else { p.name.as_str() };
            let tile = if liked {
                liked_cover("t", 20.0, 5.0)
            } else {
                match p.image.as_deref().and_then(|u| ui.thumb(u)) {
                    Some(img) => cover("t".into(), Some(&img), 20.0, 4.0, th, &icons::NOTE),
                    None => tile("t".into(), 20.0, 5.0, Some(&icons::NOTE), th),
                }
            };
            let pinned = a.is_pinned(p);
            let mut r = row_for(i, name, &p.key(), tile, a.pl_state.selected() == Some(i), pinned);
            if !liked {
                r = r.on_secondary_press(move |ui: &mut Ui, at, tx| {
                    playlist_menu(ui, i, at.into());
                    ui.after(tx);
                });
            }
            list = list.child(r);
            if split == Some(i) {
                list = list.child(row().key("side-pins-rule").w_full().h(SIDE_RULE).px(8.0).center_y()
                    .child(el().w_full().h(1.0).fill(th.line)));
            }
        }
    }
    let scroller = col().key("side-scroll").w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).scroll_edge_fade(18.0).child(list);

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
        .child(el().h(10.0))
        .child(scroller)
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

/// The color the header fades down from: the list's own cover's (sRGB), once it's been looked at.
fn hero_tint(ui: &Ui) -> Option<[f32; 3]> {
    let v = &ui.app.view;
    if v.key == "liked" {
        let c = crate::theme::rgb(0x450af5);
        return Some([c.r, c.g, c.b].map(rune::color::srgb_encode));
    }
    if v.key == QUEUE_KEY {
        return ui.app.art.cover.as_ref().map(|c| c.tint);
    }
    if v.artist.is_some() {
        return v.portrait.as_deref().and_then(|u| ui.tint(&art::medium(u)));
    }
    if let Some(u) = own_cover(ui) {
        return ui.tint(&art::medium(u)).or_else(|| ui.tint(u));
    }
    if v.kind != Kind::Tracks {
        return None;
    }
    v.tracks.iter().find_map(|t| t.cover.as_deref()).and_then(|u| ui.tint(&art::small(u)))
}

/// The cover Spotify gives the open playlist (the sidebar's), when it's one of yours.
fn own_cover(ui: &Ui) -> Option<&str> {
    let key = &ui.app.view.key;
    ui.app.playlists.iter().find(|p| p.id.as_deref() == Some(key.as_str()))?.image.as_deref()
}

/// The art over a list: the playlist's own cover (as in the sidebar), else the first four albums
/// in it as a square, or a plain tile.
fn hero_art(ui: &Ui, th: &Theme) -> El {
    let a = &ui.app;
    let v = &a.view;
    let size = HERO_ART;
    let r = 8.0;
    if v.key == "liked" {
        return liked_cover("hero-art", size, r);
    }
    if v.artist.is_some() {
        let img = v.portrait.as_deref().and_then(|u| ui.thumb(&art::medium(u)));
        return cover("hero-art".into(), img.as_ref(), size, size / 2.0, th, &icons::PERSON);
    }
    if v.key == QUEUE_KEY {
        return match ui.cover.as_ref().map(|(_, i)| i) {
            Some(i) => cover("hero-art".into(), Some(i), size, r, th, &icons::QUEUE),
            None => tile("hero-art".into(), size, r, Some(&icons::QUEUE), th),
        };
    }
    if let Some(u) = own_cover(ui) {
        let img = ui.thumb(&art::medium(u)).or_else(|| ui.thumb(u));
        return cover("hero-art".into(), img.as_ref(), size, r, th, &icons::NOTE);
    }
    let mut covers: Vec<&str> = vec![];
    for c in v.tracks.iter().filter_map(|t| t.cover.as_deref()) {
        if !covers.contains(&c) {
            covers.push(c);
            if covers.len() == 4 {
                break;
            }
        }
    }
    if v.kind != Kind::Tracks || covers.is_empty() {
        let glyph = match v.kind {
            Kind::Artists => &icons::PERSON,
            Kind::Shows => &icons::MIC,
            Kind::Albums if v.artist.is_some() => &icons::PERSON,
            Kind::Albums => &icons::ALBUM,
            Kind::Tracks if is_episodes(&v.key) => &icons::MIC,
            Kind::Tracks => &icons::NOTE,
        };
        return tile("hero-art".into(), size, r, Some(glyph), th);
    }
    let one = covers.len() < 4 || v.key.starts_with("spotify:album:") || is_episodes(&v.key);
    if one {
        let img = ui.thumb(&art::medium(covers[0])).or_else(|| ui.thumb(&art::small(covers[0])));
        cover("hero-art".into(), img.as_ref(), size, r, th, &icons::NOTE)
    } else {
        // the first four albums in it, as a square
        let q = size / 2.0;
        let cell = |i: usize| cover(format!("m{i}"), ui.thumb(&art::medium(covers[i])).or_else(|| ui.thumb(&art::small(covers[i]))).as_ref(), q, 0.0, th, &icons::NOTE);
        col().key("hero-art").size(size, size).radius(r).clip_rounded(r)
            .child(row().key("m-top").child(cell(0)).child(cell(1)))
            .child(row().key("m-bot").child(cell(2)).child(cell(3)))
    }
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
    // one field: ⌘F turns it to finding in the open list, until esc or it's left empty. Idle,
    // it's a round button (or ⌘K) that opens into the field, so the bar is empty
    top = top.child(if ui.finding() {
        find_field(ui, th)
    } else if ui.search.focused || !ui.search.text.is_empty() {
        search_field(ui, th)
    } else {
        icon_button("search-open", &icons::SEARCH, 16.0, th.sub, th, "Search  ⌘K", |ui| ui.focus_search())
    });
    if v.search_query().is_some() {
        // a search's results: no header, only what to search for, then what it found
        let [back_mark, fwd_mark] = super::swipe::marks(ui, th);
        return col().key("main").grow(1.0).shrink().h_full().min_w(0.0).on_wheel(super::swipe::wheel)
            .child(top)
            .child(search::page(ui, th, cx))
            .child(back_mark).child(fwd_mark);
    }
    if v.info.is_some() {
        // the bar floats over the header's backdrop, and takes the window's color (and a
        // hairline) once the page scrolls under it
        let bar_bg = col().key("top-bg").pin(Some(0.0), Some(0.0), Some(0.0), None).live_opacity(&ui.scrolled)
            .child(el().key("top-fill").w_full().h(artist::TOP_BAR).fill(th.bg))
            .child(el().key("top-rule").w_full().h(1.0).fill(th.line));
        let [back_mark, fwd_mark] = super::swipe::marks(ui, th);
        return col().key("main").grow(1.0).shrink().h_full().min_w(0.0).on_wheel(super::swipe::wheel)
            .child(artist::overview(ui, th, cx))
            .child(bar_bg)
            .child(top.pin(Some(0.0), Some(0.0), Some(0.0), None))
            .child(back_mark).child(fwd_mark);
    }

    // title, and what's in it
    let tracks = v.kind == Kind::Tracks;
    let episodes = is_episodes(&v.key);
    let meta = if v.loading && v.len() == 0 {
        "loading…".to_string()
    } else if v.key.is_empty() {
        String::new()
    } else if tracks {
        let ms = v.tracks.iter().map(|t| t.ms).sum();
        format!("{} · {}", plural(v.len(), if episodes { "episode" } else { "song" }), hours(ms))
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
    let meta = if v.loading && v.len() > 0 { format!("{meta} · loading…") } else { meta };
    // a flat header: the art, then what it is, its title, and a line of what's in it with the
    // things to do with it
    let title = v.title.trim_start_matches(['♥', ' ']).to_string();
    // the words' column: the pane, less its edges, the art and the gap after it
    let words_w = cx.layout_size("main").map_or(0.0, |s| s[0]) - 2.0 * 28.0 - HERO_ART - 22.0;
    let title_font = fit(&title, HERO_TITLE, words_w, 24.0);
    let mut line = row().key("hero-line").gap(14.0).center_y().min_w(0.0);
    if let Some(by) = &v.by {
        // an album's artist, as Spotify puts it: first on the line, and a way to their page
        let el = artist_line("by", &by.names, &by.artists, ts(12.0, 650.0), th.text, th.text.alpha(0.75), "Go to artist", |_| {});
        line = line.child(row().key("by-meta").gap(6.0).center_y().min_w(0.0).shrink()
            .child(el)
            .child(text("·").font(SMALL).color(th.sub))
            .child(text(meta).key("meta").font(SMALL).color(th.sub).shrink().truncate()));
    } else {
        line = line.child(text(meta).key("meta").font(SMALL).color(th.sub).shrink().truncate());
    }
    if tracks && v.len() > 0 {
        line = line.child(hero_actions(ui, th));
    }
    let mut words = col().key("hero-words").grow(1.0).shrink().min_w(0.0).gap(2.0)
        .child(text(kicker(&v.key).to_uppercase()).key("kicker").font(LABEL).color(th.dim))
        .child(text(title.clone()).key("title").font(title_font).color(th.text).truncate().help_when_truncated(title))
        .child(line);
    if v.is_grid() {
        words = words.child(row().key("chips-pad").pt(8.0).child(artist::release_chips(ui, th)));
    }
    // a list opening (or coming back) makes an entrance: its art comes into focus and its words
    // rise in just after, while the last list's header slips away. A search stays put as you type
    let staged = v.id.to_string();
    let hero_row = row().key(format!("hero-row-{staged}")).w_full().px(28.0).pt(2.0).pb(18.0).gap(22.0).items_end()
        .transition(motion::stage())
        .child(hero_art(ui, th).transition(motion::focus()))
        .child(words.transition(motion::rise(10.0)).enter_delay(0.05));
    let hero = col().key("hero").w_full().child(top).child(hero_row);

    let list: El = if v.len() == 0 && !v.loading && !v.key.is_empty() {
        col().key("empty").w_full().grow(1.0).center().gap(10.0)
            .child(icon(&icons::SPARK, 28.0).color(th.dim.alpha(0.6)))
            .child(text(if a.find.is_some() { "nothing matches" } else { "nothing here yet" }).font(BODY).color(th.dim))
    } else if tracks {
        rows::track_list(ui, th, cx)
    } else if v.is_grid() {
        artist::grid(ui, cx)
    } else {
        rows::link_list(ui, th)
    };
    // two fingers across it go back and forward; over the list, its scroller hears them first
    let mut main = col().key("main").grow(1.0).shrink().h_full().min_w(0.0).on_wheel(super::swipe::wheel);
    if let Some(tint) = hero_tint(ui) {
        // the list's cover's color at the top, fading into the window behind the header
        main = main.child(el().key(format!("hero-wash-{staged}")).pin(Some(0.0), Some(0.0), Some(0.0), None).h(HERO_WASH)
            .gradient(crate::theme::backdrop(tint, th.dark), th.bg)
            .transition(Transition::opacity().springs(motion::BLOOM)));
    }
    let [back_mark, fwd_mark] = super::swipe::marks(ui, th);
    main.child(hero).child(list).child(back_mark).child(fwd_mark)
}

/// Play the list: filled in the accent, the playing cover's color. Sorting is the date column's
/// header; finding, the search field's ⌘F.
fn hero_actions(ui: &Ui, th: &Theme) -> El {
    let playing_here = ui.app.queue.as_ref().is_some_and(|q| q.key == ui.app.view.key) && ui.app.want_play;
    let play = row().key("hero-play").size(36.0, 36.0).radius(18.0).center().cursor(Cursor::Pointer).press_scale(0.9)
        .fill(th.accent).hover(th.accent.mix(th.text, 0.25))
        .help(if playing_here { "Pause  space" } else { "Play  P" }).label("Play")
        .on_press_tx(move |ui: &mut Ui, tx| {
            if playing_here { ui.app.toggle() } else { ui.app.play_from_view(None) }
            ui.after(tx);
        })
        .child(icon(if playing_here { &icons::PAUSE } else { &icons::PLAY }, 15.0).key(if playing_here { "pause" } else { "play" }).color(th.bg).transition(motion::swap()));
    row().key("hero-actions").center_y().child(play)
}

/// A rounded text field. `wire` binds the input inside it (its edit, change and key handlers
/// belong on the `text_input` itself, or the field stays read-only).
fn field(th: &Theme, key: &str, edit: &rune::TextEdit, w: f32, i: &'static rune::Icon, placeholder: &str, wire: impl FnOnce(El) -> El) -> El {
    let focused = edit.focused;
    row().key(format!("{key}-box")).w(w).h(30.0).px(10.0).gap(8.0).center_y().radius(15.0)
        .fill(if focused { th.raised } else { th.wash })
        .border(1.0, if focused { th.text.alpha(0.22) } else { th.line })
        .child(icon(i, 14.0).color(if focused { th.text } else { th.dim }))
        .child(wire(text_input(edit, BODY).key(key.to_string()).single_line().grow(1.0).color(th.text).caret(th.text, th.text.alpha(0.2)).placeholder(placeholder.to_string(), th.sub)))
}

fn search_field(ui: &Ui, th: &Theme) -> El {
    let label = match ui.app.search_kind {
        None => "Search".to_string(),
        Some(k) => format!("Search {}", k.label()),
    };
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
            }
            if matches!(k, Key::Esc | Key::Enter) {
                tx.blur();
            }
            ui.after(tx);
            took
        }))
}

fn find_field(ui: &Ui, th: &Theme) -> El {
    let shown = ui.app.shown().len();
    let f = field(th, "find", &ui.find, 260.0, &icons::FIND, "Find in list", |f| {
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

/// On a search's results: what to search for, as chips (tab in the field cycles them).
pub fn kinds(ui: &Ui, th: &Theme) -> El {
    let opts = [None, Some(Kind::Tracks), Some(Kind::Albums), Some(Kind::Artists), Some(Kind::Shows)].map(|k| (k, search_label(k)));
    let on = opts.iter().position(|&(k, _)| k == ui.app.search_kind).unwrap_or(0);
    let pick = |ui: &mut Ui, k: Option<Kind>| {
        ui.app.search_kind = k;
        ui.app.search_at = Some(Instant::now());
    };
    row().key("kinds").child(segmented("kind", &opts, on, 78.0, "", th, pick))
}

/// One choice of a few, as chips all one width: the chosen one sits on a solid pill, and when
/// the choice changes the pill slides over to it (with a little give at the end) while the
/// labels' colors cross. One width means where the pill goes is known without measuring.
pub fn segmented<T: Copy + 'static>(key: &str, opts: &[(T, &'static str)], on: usize, w: f32, help: &'static str, th: &Theme, pick: fn(&mut Ui, T)) -> El {
    const GAP: f32 = 6.0;
    const H: f32 = 26.0;
    let pill = el().key("pill").absolute(0.0, 0.0).size(w, H).radius(H / 2.0).fill(th.text)
        .offset(on as f32 * (w + GAP), 0.0).offset_motion(motion::SLIDE);
    let mut r = row().key(key.to_string()).gap(GAP).child(pill);
    for (i, &(v, label)) in opts.iter().enumerate() {
        let chosen = i == on;
        let mut chip = row().key(format!("chip-{label}")).size(w, H).center().radius(H / 2.0)
            .fill(if chosen { Color::TRANSPARENT } else { th.text.alpha(0.06) })
            .cursor(Cursor::Pointer).press_scale(0.94)
            .on_press_tx(move |ui: &mut Ui, tx| {
                pick(ui, v);
                ui.after(tx);
            })
            .child(text(label).font(ts(12.0, 600.0)).color(if chosen { th.bg } else { th.sub }));
        if !chosen {
            chip = chip.hover(th.text.alpha(0.1));
        }
        if !help.is_empty() {
            chip = chip.help(help);
        }
        r = r.child(chip);
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
        (n.clone(), th.sub)
    } else if a.sess.borrow().is_none() {
        ("connecting…".to_string(), th.sub)
    } else {
        return None;
    };
    Some(row().key("message").pin(None, None, None, Some(player::PLAYER + 16.0)).w_full().justify_center()
        .child(row().key(format!("message-{s}")).max_w(620.0).h(34.0).px(16.0).gap(8.0).center_y().radius(17.0)
            .fill(th.raised).border(1.0, c.alpha(0.3))
            .shadow(6.0, 24.0, Color::BLACK.alpha(if th.dark { 0.4 } else { 0.12 }))
            .transition(motion::pop(12.0))
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
        ("v", "artist: overview / songs / releases"),
        ("f", "releases: all / albums / singles"),
        ("esc  ⌘[", "back (or swipe right)"),
        ("⌘]", "forward (or swipe left)"),
        ("tab", "switch pane"),
        ("g  M  G", "top / middle / bottom"),
        ("d", "playlist: sort by date added"),
        ("⌘,", "settings"),
        ("⌘?  ?", "these keys"),
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
        .transition(motion::pop(14.0))
        .child(row().key("help-head").gap(10.0).center_y()
            .child(icon(&icons::KEYS, 18.0).color(th.sub))
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

#[cfg(test)]
mod fit_tests {
    use super::*;

    #[test]
    fn shrinks_to_fit() {
        let t = "Wonder Woman (Original Motion Picture Soundtrack)";
        let full = rune::text::measure(t, &[], HERO_TITLE, None).0;
        let f = fit(t, HERO_TITLE, 700.0, 24.0);
        eprintln!("full {full}, fitted {} at {}", rune::text::measure(t, &[], f, None).0, f.size);
        assert!(f.size < HERO_TITLE.size && f.size >= 24.0);
        assert!(rune::text::measure(t, &[], f, None).0 <= 700.0);
        assert_eq!(fit("Short", HERO_TITLE, 700.0, 24.0).size, HERO_TITLE.size);
        assert_eq!(fit(t, HERO_TITLE, 200.0, 24.0).size, 24.0);
    }
}
