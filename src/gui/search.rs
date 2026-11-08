// The search page: no header, only chips for what to search for (everything, songs, albums,
// artists, podcasts), then what was found. Everything is one scrolling page: the top result
// beside the first songs (the card as tall as they are), then a line each of artists and
// albums as cards. One kind is a list of songs, or a grid of cards.

use rune::{Color, Cursor, Cx, El, ScrollSpec, col, el, icon, row, spacer, text};

use super::artist::{GAP, PAD, card_lines, grid_layout, heading, pane_w};
use super::rows::{link_menu, pick, track_list, track_row};
use super::view::{BODY, ROW, SMALL, cover, fit, kinds, ts};
use super::{Ui, icons, motion};
use crate::*;

const HIT_ART: f32 = 92.0;
const HIT_NAME: rune::TextStyle = ts(30.0, 760.0).lh(36.0).tracking(-0.7);

pub fn page(ui: &Ui, th: &Theme, cx: &mut Cx<Ui>) -> El {
    let v = &ui.app.view;
    let chips = row().key("search-kinds").w_full().px(28.0).pt(4.0).pb(16.0).child(kinds(ui, th));
    let found = if v.is_search_all() {
        everything(ui, th, cx)
    } else if v.len() == 0 {
        nothing(v.loading, th)
    } else if v.kind == Kind::Tracks {
        track_list(ui, th, cx)
    } else {
        super::artist::grid(ui, cx)
    };
    col().key("search").w_full().grow(1.0).min_h(0.0).child(chips).child(found)
}

/// Where the results go while there are none: quiet while they come, then a word that none did.
fn nothing(loading: bool, th: &Theme) -> El {
    let mut e = col().key("none").w_full().grow(1.0).center().gap(10.0);
    if !loading {
        e = e.child(icon(&icons::SEARCH, 26.0).color(th.dim.alpha(0.6)))
            .child(text("nothing found").font(BODY).color(th.dim));
    }
    e
}

/// Everything at once: the top result and the songs beside it, then artists, then albums.
fn everything(ui: &Ui, th: &Theme, cx: &mut Cx<Ui>) -> El {
    let a = &ui.app;
    let v = &a.view;
    let Some(info) = v.overview() else { return el() };
    let (cols, w) = grid_layout(cx);
    a.grid_cols.set(cols);
    if v.hit.is_none() && v.tracks.is_empty() && info.releases.is_empty() && info.related.is_empty() {
        return nothing(v.loading, th);
    }
    let blocks = a.blocks();
    let start = |s: Section| blocks.iter().find(|b| b.section == Some(s)).map_or(0, |b| b.start);
    let mut page = col().key("all").w_full().pb(40.0).gap(30.0);

    // the top result, and the songs beside it (under it, in a narrow pane)
    let wide = cols >= 4;
    let pane = pane_w(cx);
    let mut top = if wide { row().items_start() } else { col() }.key("all-top").w_full().px(PAD).gap(2.0 * GAP);
    if let Some(hit) = &v.hit {
        let hit_w = if wide { ((pane - 2.0 * PAD) * 0.42).floor() } else { pane - 2.0 * PAD };
        top = top.child(col().key("hit").w(hit_w).gap(12.0)
            .child(row().key("hit-pad").px(8.0).child(heading("hit", "Top result", th)))
            // as tall as the songs beside it, so the two end together
            .child(hit_card(ui, start(Section::Top), &hit.link, hit_w, th).min_h(if wide { v.tracks.len() as f32 * ROW } else { 0.0 })));
    }
    if !v.tracks.is_empty() {
        let mut songs = col().key("songs").grow(1.0).shrink().min_w(0.0).gap(12.0)
            .child(row().key("songs-pad").px(8.0).child(heading("songs", "Songs", th)));
        let mut list = col().key("songs-rows").w_full().min_w(0.0);
        for k in 0..v.tracks.len() {
            list = list.child(track_row(ui, k, th, false, false));
        }
        songs = songs.child(list);
        top = top.child(songs);
    }
    page = page.child(top.transition(motion::rise(12.0)).enter_delay(0.04));

    for (s, key, title) in [(Section::Related, "artists", "Artists"), (Section::Releases, "albums", "Albums")] {
        let cards = v.cards(s, cols);
        if cards.is_empty() {
            continue;
        }
        page = page.child(col().key(key).w_full().px(PAD).gap(12.0).transition(motion::rise(12.0)).enter_delay(0.1)
            .child(row().key(format!("{key}-pad")).px(8.0).child(heading(key, title, th)))
            .child(card_lines(ui, &format!("{key}-cards"), start(s), &cards, cols, w, th)));
    }
    let scrolled = ui.scrolled.clone();
    col().key(format!("list-{}", v.id)).w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false })
        .scroll_edge_fade(16.0)
        .on_wheel(super::swipe::wheel)
        .on_scroll_offset(move |at| {
            scrolled.set((at.offset[1] / 24.0).clamp(0.0, 1.0));
        })
        .child(page)
}

/// The top result, large: its cover (an artist's round), its name, and what it is. A click
/// plays the song or opens the album or artist, as enter does on it.
fn hit_card(ui: &Ui, r: usize, l: &Link, w: f32, th: &Theme) -> El {
    let a = &ui.app;
    let selected = a.tr_state.selected() == Some(r);
    let focused = a.focus == Focus::Tracks;
    let artist = l.uri.starts_with("spotify:artist:");
    let song = l.uri.starts_with("spotify:track:");
    let img = l.image.as_deref().and_then(|u| ui.thumb(&art::medium(u)));
    let (kind, glyph) = match () {
        _ if artist => ("Artist", &icons::PERSON),
        _ if song => ("Song", &icons::NOTE),
        _ => ("Album", &icons::ALBUM),
    };
    // whose it is: a song's line is "Song · artists", an album's "artists · year type"
    let by = if artist {
        String::new()
    } else if song {
        l.sub.split_once(" · ").map_or("", |(_, by)| by).to_string()
    } else {
        l.sub.split(" · ").next().unwrap_or_default().to_string()
    };
    let rest = th.text.alpha(0.04);
    let fill = match (selected, focused) {
        (true, true) => th.sel,
        (true, false) => th.wash,
        _ => rest,
    };
    let pill = row().key("kind").h(22.0).px(10.0).center().radius(11.0).fill(th.text.alpha(0.08))
        .child(text(kind).font(ts(11.5, 650.0)).color(th.text));
    let mut line = row().key("line").w_full().min_w(0.0).gap(8.0).center_y().child(pill);
    if !by.is_empty() {
        line = line.child(text(by).key("by").font(SMALL).color(th.sub).shrink().truncate());
    }
    let uri = l.uri.clone();
    col().key(format!("row-{r}")).w_full().padding(20.0).gap(18.0).radius(12.0).fill(fill).hover_group()
        .hover(if selected { fill } else { th.text.alpha(0.07) })
        .cursor(Cursor::Pointer).press_scale(0.99).label(l.name.clone())
        .help(if song { "Play" } else { "Open" })
        .on_press_tx(move |ui: &mut Ui, tx| {
            pick(ui, r);
            ui.app.enter();
            ui.after(tx);
        })
        .on_secondary_press(move |ui: &mut Ui, at, tx| {
            pick(ui, r);
            link_menu(ui, at.into(), uri.clone());
            ui.after(tx);
        })
        .child(cover("hit-art".into(), img.as_ref(), HIT_ART, if artist { HIT_ART / 2.0 } else { 6.0 }, th, glyph)
            .shadow(6.0, 22.0, Color::BLACK.alpha(if th.dark { 0.4 } else { 0.14 })))
        .child(spacer())
        .child(col().key("words").w_full().min_w(0.0).gap(8.0)
            .child(text(l.name.clone()).key("name").font(fit(&l.name, HIT_NAME, w - 40.0, 20.0)).color(th.text).truncate().help_when_truncated(l.name.clone()))
            .child(line))
        .child(hit_play(ui, r, l, song, selected && focused, th))
}

/// The top result's play, in the accent like a list's: in the card's corner under the pointer
/// (or while it's picked or playing). A song plays as a click on the card would; an album or
/// artist plays without opening. Pause while it's the one playing.
fn hit_play(ui: &Ui, r: usize, l: &Link, song: bool, picked: bool, th: &Theme) -> El {
    const S: f32 = 44.0;
    let a = &ui.app;
    let key = l.uri.strip_prefix("spotify:playlist:").unwrap_or(&l.uri);
    let here = if song { a.now().is_some_and(|t| t.uri == l.uri) } else { a.queue.as_ref().is_some_and(|q| q.key == key) };
    let playing = here && a.want_play;
    let uri = l.uri.clone();
    let rest = if here || picked { 1.0 } else { 0.0 };
    row().key("hit-play").pin(None, None, Some(20.0), Some(20.0)).size(S, S).radius(S / 2.0).center()
        .fill(th.accent).hover(th.accent.mix(th.text, 0.25)).group_opacity(rest, 1.0)
        .shadow(4.0, 14.0, Color::BLACK.alpha(if th.dark { 0.4 } else { 0.18 }))
        .cursor(Cursor::Pointer).press_scale(0.9)
        .help(if playing { "Pause" } else { "Play" }).label(if playing { "Pause" } else { "Play" })
        .on_press_tx(move |ui: &mut Ui, tx| {
            if here {
                ui.app.toggle();
            } else if song {
                pick(ui, r);
                ui.app.enter();
            } else {
                ui.app.play_uri(uri.clone());
            }
            ui.after(tx);
        })
        .child(icon(if playing { &icons::PAUSE } else { &icons::PLAY }, 18.0).key(if playing { "pause" } else { "play" })
            .color(th.bg).transition(motion::swap()))
}
