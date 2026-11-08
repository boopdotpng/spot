// Artist pages: the overview (a header over the portrait, popular songs, then the discography
// and the rest as cards) and the discography as a grid of covers. Cards are as wide as a line
// of them fits the pane, and the cursor walks them by line and by card (see artist.rs).

use std::borrow::Cow;

use rune::{Color, Cursor, Cx, El, GrowthSpec, ScrollSpec, SurfaceFit, col, el, icon, row, spacer, text};

use super::rows::{pick, scroller, track_row};
use super::view::{BODY, BODY_MED, LABEL, SMALL, cover, theme_now, ts};
use super::{Ui, icons, motion};
use crate::artist::top_cols;
use crate::*;

const PAD: f32 = 20.0; // the pane's edges
const GAP: f32 = 10.0; // between cards
const CARD_MIN: f32 = 150.0;
const CARD_PAD: f32 = 8.0;
const CARD_TEXT: f32 = 8.0 + 17.0 + 2.0 + 16.0; // under the cover: a gap, the name, the line under it
const PORTRAIT: f32 = 168.0;
/// The top bar (back, search), which floats over the overview's header.
pub const TOP_BAR: f32 = 52.0;
const NAME: rune::TextStyle = ts(46.0, 780.0).lh(54.0).tracking(-1.4);
const HEADING: rune::TextStyle = ts(19.0, 700.0).lh(24.0).tracking(-0.3);

fn pane_w(cx: &mut Cx<Ui>) -> f32 {
    cx.layout_size("main").map_or(900.0, |s| s[0])
}

/// Cards to a line, and how wide each is, for the pane as it last laid out.
fn grid_layout(cx: &mut Cx<Ui>) -> (usize, f32) {
    let w = pane_w(cx);
    let avail = (w - 2.0 * PAD).max(CARD_MIN);
    let cols = ((avail + GAP) / (CARD_MIN + GAP)).floor().clamp(2.0, 10.0) as usize;
    let card = ((avail - GAP * (cols - 1) as f32) / cols as f32).floor();
    (cols, card)
}

/// A release or an artist: the cover (round for an artist), its name and what it is. A click
/// opens it, as enter does on the cursor's.
fn card(ui: &Ui, r: usize, l: &Link, w: f32, th: &Theme) -> El {
    let a = &ui.app;
    let selected = a.tr_state.selected() == Some(r);
    let focused = a.focus == Focus::Tracks;
    let artist = l.uri.starts_with("spotify:artist:");
    let side = w - 2.0 * CARD_PAD;
    let img = l.image.as_deref().and_then(|u| ui.thumb(&art::medium(u)));
    let glyph = if artist { &icons::PERSON } else { &icons::ALBUM };
    let fill = match (selected, focused) {
        (true, true) => th.sel,
        (true, false) => th.wash,
        _ => Color::TRANSPARENT,
    };
    let uri = l.uri.clone();
    col().key(format!("row-{r}")).w(w).padding(CARD_PAD).gap(8.0).radius(10.0).fill(fill)
        .hover(if selected { fill } else { th.text.alpha(0.04) })
        .cursor(Cursor::Pointer).press_scale(0.98).label(l.name.clone())
        .on_press_tx(move |ui: &mut Ui, tx| {
            pick(ui, r);
            ui.app.enter();
            ui.after(tx);
        })
        .on_secondary_press(move |ui: &mut Ui, at, tx| {
            pick(ui, r);
            super::rows::link_menu(ui, at.into(), uri.clone());
            ui.after(tx);
        })
        .child(cover("c".into(), img.as_ref(), side, if artist { side / 2.0 } else { 6.0 }, th, glyph))
        .child(col().key("words").w_full().min_w(0.0).gap(2.0)
            .child(text(l.name.clone()).key("name").font(BODY_MED).color(th.text).truncate().help_when_truncated(l.name.clone()))
            .child(text(l.sub.clone()).key("sub").font(SMALL).color(th.sub).truncate()))
}

/// A section's cards, `cols` to a line; the first is stop `start`.
fn card_lines(ui: &Ui, key: &str, start: usize, cards: &[&Link], cols: usize, w: f32, th: &Theme) -> El {
    let mut lines = col().key(key.to_string()).w_full().gap(GAP);
    for (n, chunk) in cards.chunks(cols).enumerate() {
        let mut line = row().key(format!("{key}-{n}")).w_full().gap(GAP);
        for (k, l) in chunk.iter().enumerate() {
            line = line.child(card(ui, start + n * cols + k, l, w, th));
        }
        lines = lines.child(line);
    }
    lines
}

/// Text that does something: quiet, brightening under the pointer.
fn text_button(key: &str, s: &str, th: &Theme, f: impl Fn(&mut Ui) + 'static) -> El {
    row().key(key.to_string()).hover_group().cursor(Cursor::Pointer)
        .on_press_tx(move |ui: &mut Ui, tx| {
            f(ui);
            ui.after(tx);
        })
        .child(text(s.to_string()).font(ts(12.5, 650.0)).color(th.sub).group_color(th.text))
}

/// The discography's filter, as chips: all, albums, singles (`f` cycles them).
pub fn release_chips(ui: &Ui, th: &Theme) -> El {
    let opts = [(Releases::All, "All"), (Releases::Albums, "Albums"), (Releases::Singles, "Singles")];
    let on = opts.iter().position(|&(f, _)| f == ui.app.view.filter).unwrap_or(0);
    super::view::segmented("chips", &opts, on, 70.0, "Releases  f", th, |ui, f| ui.app.set_releases(f))
}

fn heading(key: &str, s: &str, th: &Theme) -> El {
    text(s.to_string()).key(format!("{key}-h")).font(HEADING).color(th.text)
}

/// The portrait, blurred into the header's backdrop (up under the top bar), under the round portrait, the name, and
/// the things to do: play (every song, shuffled) and every song as a list.
fn header(ui: &Ui, info: &ArtistInfo, th: &Theme) -> El {
    let a = &ui.app;
    let v = &a.view;
    let img = info.portrait.as_deref().and_then(|u| ui.thumb(&art::medium(u)));
    let name = v.title.clone();
    let meta = if v.loading && info.releases.is_empty() {
        "loading…".to_string()
    } else {
        let mut parts = vec![plural(info.releases.len(), "release")];
        if !info.appears.is_empty() {
            parts.push(format!("on {}", plural(info.appears.len(), "other")));
        }
        parts.join(" · ")
    };
    let shuffling_here = a.want_play && a.queue.as_ref().is_some_and(|q| q.key == v.key);
    let play = row().key("ov-play").size(52.0, 52.0).radius(26.0).center().fill(th.text)
        .cursor(Cursor::Pointer).press_scale(0.92).hover(th.text.alpha(0.88))
        .help(if shuffling_here { "Pause  space" } else { "Shuffle every song  P" }).label("Play")
        .on_press_tx(move |ui: &mut Ui, tx| {
            if shuffling_here { ui.app.toggle() } else { ui.app.play_artist() }
            ui.after(tx);
        })
        .child(icon(if shuffling_here { &icons::PAUSE } else { &icons::PLAY }, 20.0).color(th.bg));
    let songs = row().key("ov-songs").h(32.0).px(14.0).gap(7.0).center_y().radius(16.0)
        .border(1.0, th.text.alpha(0.25)).hover(th.text.alpha(0.06)).cursor(Cursor::Pointer).press_scale(0.96)
        .help("Every song  v").on_press_tx(|ui: &mut Ui, tx| {
            ui.app.open_artist_mode(ArtistMode::Songs, true);
            ui.after(tx);
        })
        .child(icon(&icons::NOTE, 13.0).color(th.text))
        .child(text("All songs").font(ts(12.5, 650.0)).color(th.text));
    let actions = row().key("ov-actions").gap(14.0).center_y().pt(14.0).child(play).child(songs);
    let words = col().key("ov-words").grow(1.0).shrink().min_w(0.0).gap(2.0)
        .child(text("ARTIST").key("kicker").font(LABEL).color(th.dim))
        .child(text(name.clone()).key("name").font(NAME).color(th.text).truncate().help_when_truncated(name))
        .child(text(meta).key("meta").font(SMALL).color(th.sub))
        .child(actions);
    let portrait = cover("ov-portrait".into(), img.as_ref(), PORTRAIT, PORTRAIT / 2.0, th, &icons::PERSON)
        .shadow(10.0, 36.0, Color::BLACK.alpha(if th.dark { 0.45 } else { 0.18 }));
    let mut band = el().key("ov-band").w_full().h_full().pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).clip_rounded(0.0);
    if let Some(i) = &img {
        // the portrait, blurred to a wash of its colors, fading into the window
        band = band
            .child(el().key("ov-blur").pin(Some(-40.0), Some(-40.0), Some(-40.0), Some(-40.0)).pixels_fit(i.pixels(), SurfaceFit::Fill).blur(48.0).opacity(if th.dark { 0.62 } else { 0.35 })
                .transition(rune::Transition::opacity().springs(motion::BLOOM)))
            .child(el().key("ov-fade").pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
                .linear_gradient(std::f32::consts::PI, [(0.0, th.bg.alpha(0.0)), (1.0, th.bg)]));
    }
    col().key("ov-header").w_full()
        .child(band)
        .child(row().key("ov-hero").w_full().px(28.0).pt(TOP_BAR + 12.0).pb(8.0).gap(26.0).items_end()
            .child(portrait.transition(motion::focus()))
            .child(words.transition(motion::rise(10.0)).enter_delay(0.05)))
}

/// The artist's overview, all one scrolling page.
pub fn overview(ui: &Ui, th: &Theme, cx: &mut Cx<Ui>) -> El {
    let a = &ui.app;
    let v = &a.view;
    let Some(info) = v.overview() else { return el() };
    let (cols, w) = grid_layout(cx);
    a.grid_cols.set(cols);
    let blocks = a.blocks();
    let start = |s: Section| blocks.iter().find(|b| b.section == Some(s)).map_or(0, |b| b.start);
    let mut page = col().key("ov").w_full().pb(40.0).gap(30.0).child(header(ui, info, th));

    if !v.tracks.is_empty() {
        // two columns of songs in rank order, down the left one and then the right
        let tc = top_cols(cols);
        let half = ((pane_w(cx) - 28.0 - GAP * (tc - 1) as f32) / tc as f32).floor();
        let lines = v.tracks.len().div_ceil(tc);
        let mut columns = row().key("popular-cols").w_full().gap(GAP).items_start();
        for c in 0..tc {
            let mut column = col().key(format!("popular-{c}")).w(half).min_w(0.0);
            for k in c * lines..((c + 1) * lines).min(v.tracks.len()) {
                column = column.child(track_row(ui, k, th, false, false));
            }
            columns = columns.child(column);
        }
        let sec = col().key("popular").w_full().px(14.0).transition(motion::rise(12.0)).enter_delay(0.08)
            .child(row().key("popular-head").px(14.0).pb(8.0).child(heading("popular", "Popular", th)))
            .child(columns);
        page = page.child(sec);
    }

    let releases = v.cards(Section::Releases, cols);
    if !info.releases.is_empty() {
        let head = row().key("disc-head").w_full().gap(16.0).center_y()
            .child(heading("disc", "Discography", th))
            .child(release_chips(ui, th))
            .child(spacer())
            .child(text_button("disc-all", "Show all", th, |ui| ui.app.open_artist_mode(ArtistMode::Releases, true)));
        let mut sec = col().key("disc").w_full().px(PAD).gap(12.0).transition(motion::rise(12.0)).enter_delay(0.14).child(row().key("disc-pad").w_full().px(8.0).child(head));
        sec = sec.child(if releases.is_empty() {
            text("none of these").key("disc-none").font(BODY).color(th.dim)
        } else {
            card_lines(ui, "disc-cards", start(Section::Releases), &releases, cols, w, th)
        });
        page = page.child(sec);
    }

    for (s, key, title) in [(Section::Appears, "appears", "Appears on"), (Section::Related, "related", "Fans also like")] {
        let cards = v.cards(s, cols);
        if cards.is_empty() {
            continue;
        }
        page = page.child(col().key(key).w_full().px(PAD).gap(12.0).transition(motion::rise(12.0)).enter_delay(0.2)
            .child(row().key(format!("{key}-pad")).px(8.0).child(heading(key, title, th)))
            .child(card_lines(ui, &format!("{key}-cards"), start(s), &cards, cols, w, th)));
    }
    // no edge fade: the header's backdrop runs up to the window's top, under the top bar
    let scrolled = ui.scrolled.clone();
    col().key(format!("list-{}", v.id)).w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false })
        .on_wheel(super::swipe::wheel)
        .on_scroll_offset(move |at| {
            scrolled.set((at.offset[1] / 24.0).clamp(0.0, 1.0));
        })
        .child(page)
}

/// The discography: every release as a card, a line of them to a row of the list.
pub fn grid(ui: &Ui, cx: &mut Cx<Ui>) -> El {
    let a = &ui.app;
    let v = &a.view;
    let (cols, w) = grid_layout(cx);
    a.grid_cols.set(cols);
    let lines = a.shown().len().div_ceil(cols);
    let rows = col().key("rows").w_full().px(PAD).pt(4.0).pb(24.0).growth(GrowthSpec::default())
        .virtual_rows(lines, w + CARD_TEXT + GAP, |_: &Ui, l| Cow::Owned(format!("line-{l}")), move |ui: &Ui, l| grid_line(ui, l, cols, w))
        .virtual_rows_version((v.id, a.find.is_some(), a.matches.len(), v.links.len(), cols, v.filter as u8));
    scroller(ui, format!("list-{}", v.id), rows)
}

fn grid_line(ui: &Ui, l: usize, cols: usize, w: f32) -> El {
    let a = &ui.app;
    let th = theme_now();
    let shown = a.shown();
    let mut line = row().key(format!("line-{l}")).w_full().gap(GAP).pb(GAP).transition(motion::rise(12.0)).enter_delay(motion::ripple(l * 3));
    for r in l * cols..(l + 1) * cols {
        if let Some(i) = shown.get(r) {
            line = line.child(card(ui, r, &a.view.links[i], w, &th));
        }
    }
    line
}
