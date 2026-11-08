// The open list's rows. Under the pointer a row shows what a click can do: its number turns into
// a play button, and queue and "more" buttons come up by the time; a right click (or "more")
// opens the system's menu for it. Artist and album names go to the artist and album. Playlists
// also list when each song was added, and sort by it from that column's head.

use std::borrow::Cow;

use rune::{Color, Cursor, Cx, El, GrowthSpec, PopupAnchor, PopupItem, PopupMenu, ScrollSpec, Tx, col, el, icon, memo, row, text};

use super::view::{BODY, BODY_MED, LABEL, LINK_ROW, NUM, ROW, SMALL, cover, eq, theme_now, tile, ts};
use super::{Ui, icons};
use crate::*;

/// Rows are wide enough for an album column, and for a date added column.
fn widths(cx: &mut Cx<Ui>) -> (bool, bool) {
    let w = cx.layout_size("main").map_or(f32::MAX, |s| s[0]);
    (w >= 720.0, w >= 600.0)
}

const DATE_W: f32 = 104.0;

/// Point the cursor at row `r` of the list, as a click on it does.
pub(super) fn pick(ui: &mut Ui, r: usize) {
    ui.app.focus = Focus::Tracks;
    ui.app.tr_state.select(Some(r));
}

/// Row `r` of the list: lit while it's the cursor's, and a click puts the cursor there (a double
/// click plays or opens it). `under` is its fill otherwise.
fn pickable(e: El, r: usize, selected: bool, focused: bool, under: Color, th: &Theme) -> El {
    let fill = match (selected, focused) {
        (true, true) => th.sel,
        (true, false) => th.wash,
        (false, _) => under,
    };
    e.fill(fill)
        .on_press_tx(move |ui: &mut Ui, tx| {
            pick(ui, r);
            ui.after(tx);
        })
        .on_double_press_tx(move |ui: &mut Ui, tx| {
            pick(ui, r);
            ui.app.enter();
            ui.after(tx);
        })
}

/// Sort the playlist the next way: as listed, newest added, oldest added.
pub fn next_sort(ui: &mut Ui, tx: &mut Tx) {
    let next = ui.app.sort.next();
    ui.app.set_sort(next);
    ui.after(tx);
}

fn column_head(ui: &Ui, th: &Theme, cols: &[(&'static str, Option<f32>)]) -> El {
    let mut r = row().key("cols").w_full().h(28.0).px(26.0).gap(14.0).center_y();
    for (i, (label, w)) in cols.iter().enumerate() {
        if *label == "date added" {
            r = r.child(date_head(ui, th, w.unwrap_or(DATE_W)));
            continue;
        }
        let t = text(label.to_uppercase()).font(LABEL).color(th.dim.alpha(0.8));
        let cell = match w {
            Some(w) => row().key(format!("col-{i}")).w(*w).child(match *label {
                "time" => row().w_full().justify_end().child(t),
                "#" => row().w_full().justify_center().child(t),
                _ => t,
            }),
            None => row().key(format!("col-{i}")).grow(1.0).shrink().min_w(0.0).child(t),
        };
        r = r.child(cell);
    }
    // a hairline under the heads that shows once rows slide beneath them
    let rule = el().key("cols-rule").h(1.0).w_full().fill(th.line).live_opacity(&ui.scrolled);
    col().key("cols-wrap").w_full().child(r).child(rule)
}

/// The date added column's head: click to sort by it, newest first, then oldest, then as listed.
fn date_head(ui: &Ui, th: &Theme, w: f32) -> El {
    let s = ui.app.sort;
    let on = s != Sort::List;
    let mut r = row().key("col-date").w(w).h(20.0).gap(4.0).center_y().cursor(Cursor::Pointer).hover_group()
        .help("Sort by date added  d")
        .on_press_tx(next_sort)
        .child(text("DATE ADDED").font(LABEL).color(if on { th.sub } else { th.dim.alpha(0.8) }).group_color(th.text));
    if on {
        r = r.child(icon(if s == Sort::Newest { &icons::DOWN } else { &icons::UP }, 9.0).color(th.sub));
    }
    r
}

pub(super) fn scroller(ui: &Ui, key: String, rows: El) -> El {
    let scrolled = ui.scrolled.clone();
    col().key(key).w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false })
        .scroll_edge_fade(16.0)
        .on_scroll_offset(move |at| {
            scrolled.set((at.offset[1] / 24.0).clamp(0.0, 1.0));
        })
        .child(rows)
}

pub fn track_list(ui: &Ui, th: &Theme, cx: &mut Cx<Ui>) -> El {
    let a = &ui.app;
    let v = &a.view;
    let episodes = is_episodes(&v.key);
    let feed = episodes && !is_show(&v.key);
    let (wide, roomy) = widths(cx);
    // an album's own songs are all on it: no album column, but track numbers
    let album = v.key.starts_with("spotify:album:");
    let wide = wide && !album;
    let dates = a.dated() && roomy;
    let mut cols: Vec<(&'static str, Option<f32>)> = vec![(if album { "#" } else { "" }, Some(34.0))];
    if episodes {
        cols.push(("episode", None));
        if feed {
            cols.push(("podcast", None));
        }
        cols.extend([("released", Some(96.0)), ("", Some(56.0)), ("time", Some(72.0))]);
    } else {
        cols.push(("title", None));
        if wide {
            cols.push(("album", None));
        }
        if dates {
            cols.push(("date added", Some(DATE_W)));
        }
        cols.extend([("", Some(56.0)), ("time", Some(44.0))]);
    }
    let n = a.shown().len();
    // an album ends on when it came out, its label and its copyrights (not while finding in it)
    let foot = album && !v.about.is_empty() && a.find.is_none() && !v.loading;
    let rows = col().key("rows").w_full().px(14.0).pt(4.0).pb(24.0).growth(GrowthSpec::default())
        .virtual_rows(n + usize::from(foot), ROW, move |_: &Ui, r| Cow::Owned(if r == n { "foot".to_string() } else { format!("row-{r}") }), move |ui: &Ui, r| if r == n { album_foot(ui, &theme_now()) } else { track_row(ui, r, &theme_now(), wide, dates) })
        .virtual_rows_version((v.id, a.find.is_some(), a.matches.len(), v.tracks.len(), a.sort, foot));
    col().key("list").w_full().grow(1.0).min_h(0.0).child(column_head(ui, th, &cols)).child(scroller(ui, format!("list-{}", v.id), rows))
}

/// Under an album's songs: "April 23, 2021 · Mom+Pop", then its copyrights, small and quiet,
/// lined up with the titles.
fn album_foot(ui: &Ui, th: &Theme) -> El {
    let about = &ui.app.view.about;
    let mut c = col().key("foot").w_full().pt(26.0).pb(4.0).pl(12.0 + 34.0 + 14.0).pr(12.0).gap(3.0);
    for (i, l) in about.iter().enumerate() {
        let (font, color) = if i == 0 { (SMALL, th.sub) } else { (ts(11.0, 450.0).lh(15.0), th.dim) };
        c = c.child(text(l.clone()).key(format!("l{i}")).font(font).color(color).truncate().help_when_truncated(l.clone()));
    }
    c
}

/// A row's little buttons: quiet, brighter under the pointer, a tooltip each.
fn row_button(key: &str, i: &'static rune::Icon, th: &Theme, help: &str, f: impl Fn(&mut Ui, &mut Tx) + 'static) -> El {
    row().key(key.to_string()).size(26.0, 26.0).radius(13.0).center().cursor(Cursor::Pointer)
        .hover(th.text.alpha(0.08)).press_scale(0.88).help(help.to_string()).label(help.to_string())
        .on_press_tx(move |ui: &mut Ui, tx| {
            f(ui, tx);
            ui.after(tx);
        })
        .child(icon(i, 15.0).color(th.sub))
}

/// Text that goes somewhere: brightens and takes the pointer under it.
fn link_text(key: &str, s: String, font: rune::TextStyle, c: Color, hover: Color, f: impl Fn(&mut Ui) + 'static) -> El {
    row().key(key.to_string()).min_w(0.0).shrink().hover_group().cursor(Cursor::Pointer)
        .on_press_tx(move |ui: &mut Ui, tx| {
            f(ui);
            ui.after(tx);
        })
        .child(text(s.clone()).font(font).color(c).group_color(hover).truncate().help_when_truncated(s))
}

/// A row's play button, under the pointer over its cover or number: `dark` over a cover.
fn play_button(r: usize, dark: bool, th: &Theme) -> El {
    row().key("art-play").pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).radius(5.0).center()
        .fill(if dark { Color::BLACK.alpha(0.5) } else { Color::TRANSPARENT }).group_opacity(0.0, 1.0)
        .cursor(Cursor::Pointer).press_scale(0.9).help("Play")
        .on_press_tx(move |ui: &mut Ui, tx| {
            pick(ui, r);
            ui.app.enter();
            ui.after(tx);
        })
        .child(icon(&icons::PLAY, 15.0).color(if dark { Color::WHITE } else { th.text }))
}

/// A row's cover, with the eq over it while it plays.
fn cover_art(thumb: Option<&rune::Image>, playing: bool, on: bool, episodes: bool, r: usize, th: &Theme) -> El {
    let mut art = row().key("art").size(34.0, 34.0)
        .child(cover("c".into(), thumb, 34.0, 5.0, th, if episodes { &icons::MIC } else { &icons::NOTE }));
    if playing {
        art = art.child(row().key("art-eq").pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).radius(5.0).center()
            .fill(Color::BLACK.alpha(0.55)).group_opacity(1.0, 0.0)
            .child(eq(th.accent, on, "row-eq")));
    }
    art.child(play_button(r, true, th))
}

/// An album's song: its track number in place of the cover (they'd all be the same), the eq
/// instead while it plays.
fn numbered(n: u32, playing: bool, on: bool, r: usize, th: &Theme) -> El {
    let shown = if playing {
        eq(th.accent, on, "row-eq")
    } else {
        text(if n == 0 { String::new() } else { n.to_string() }).font(NUM).color(th.dim)
    };
    row().key("art").size(34.0, 34.0)
        .child(row().key("num").pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).center().group_opacity(1.0, 0.0).child(shown))
        .child(play_button(r, false, th))
}

pub(super) fn track_row(ui: &Ui, r: usize, th: &Theme, wide: bool, dates: bool) -> El {
    let a = &ui.app;
    let v = &a.view;
    let Some(i) = a.shown().get(r) else { return el().key(format!("row-{r}")).h(ROW) };
    let t = &v.tracks[i];
    let episodes = is_episodes(&v.key);
    let feed = episodes && !is_show(&v.key);
    let up_next = v.key == QUEUE_KEY;
    let album = v.key.starts_with("spotify:album:");
    let playing_uri = a.queue.as_ref().filter(|q| q.key == v.queue_key() && a.queued.is_none()).and_then(|q| q.tracks.get(q.idx())).map(|t| t.uri.as_str());
    let playing = Some(t.uri.as_str()) == playing_uri || (up_next && a.queued.as_ref().is_some_and(|q| q.uri == t.uri) && r == 0);
    let selected = a.tr_state.selected() == Some(r);
    let focused = a.focus == Focus::Tracks;
    let thumb = t.cover.as_deref().map(art::small).and_then(|u| ui.thumb(&u));
    let progress = if episodes { a.progress.get(&t.uri).copied() } else { None };
    // a header above the first song of each disc (and the queue's two halves)
    let queued = a.upnext.len();
    let disc_of = |r: usize| a.shown().get(r).map_or(0, |i| if up_next { usize::from(i >= queued) } else { v.tracks[i].disc as usize });
    let discs = up_next || v.key.starts_with("spotify:album:") && v.tracks.iter().any(|t| t.disc > 1);
    let header = (discs && (r == 0 || disc_of(r) != disc_of(r - 1))).then(|| match (up_next, disc_of(r), &a.queue) {
        (false, d, _) => format!("disc {d}"),
        (true, 0, _) => "queue".to_string(),
        (true, _, Some(q)) => format!("then {}", q.name),
        (true, _, None) => "then".to_string(),
    });
    let version = ((v.id, r, i, playing, playing && a.playing, selected, focused), (thumb.as_ref().map(rune::Image::id), progress, wide, dates, t.number, th.dark, header.clone(), art_key()));
    memo(format!("row-{r}"), version, || {
        // the cover (an album's songs: their number); while it plays, the eq (in the accent) over
        // it, and under the pointer a play button
        let art = if album { numbered(t.number, playing, a.playing, r, th) } else { cover_art(thumb.as_ref(), playing, a.playing, episodes, r, th) };
        let mut title_line = row().key("tl").gap(6.0).center_y().min_w(0.0)
            .child(text(t.name.clone()).key("name").font(BODY_MED).color(if playing { th.accent } else { th.text }).truncate().shrink().help_when_truncated(t.name.clone()));
        if t.explicit {
            title_line = title_line.child(explicit(th));
        }
        let heard = progress == Some(HEARD);
        let by = if episodes && !feed { t.album.clone() } else { t.artists.clone() };
        let by_el = if episodes {
            text(by).key("by").font(SMALL).color(th.sub).truncate()
        } else {
            link_text("by", by, SMALL, th.sub, th.text, move |ui| {
                pick(ui, r);
                ui.app.goto_artist();
            })
        };
        let title = col().key("t").grow(1.0).shrink().min_w(0.0).gap(1.0).child(title_line).child(by_el);
        let mut line = row().key("cells").w_full().h(ROW).px(12.0).gap(14.0).center_y().hover_group()
            .child(art)
            .child(title.opacity(if heard && !playing { 0.55 } else { 1.0 }));
        let actions = row().key("acts").w(56.0).gap(4.0).justify_end().center_y().group_opacity(0.0, 1.0)
            .child(row_button("queue-add", &icons::PLUS, th, "Add to queue  e", move |ui, _| {
                pick(ui, r);
                ui.app.enqueue_selected();
            }))
            .child(row_button("more", &icons::MORE, th, "More", move |ui, tx| {
                pick(ui, r);
                track_menu(ui, PopupAnchor::from(format!("row-{r}/more")), tx);
            }));
        if episodes {
            if feed {
                line = line.child(row().key("show").grow(1.0).shrink().min_w(0.0).child(text(t.artists.clone()).font(BODY).color(th.sub).truncate()));
            }
            let (left, _) = episode_time(t.ms, progress);
            line = line
                .child(row().key("rel").w(96.0).child(text(t.album.clone()).font(NUM).color(th.dim)))
                .child(actions)
                .child(row().key("time").w(72.0).justify_end().child(text(left).font(NUM).color(th.dim)));
        } else {
            if wide {
                line = line.child(row().key("album").grow(1.0).shrink().min_w(0.0).child(link_text("al", t.album.clone(), BODY, th.dim, th.text, move |ui| {
                    pick(ui, r);
                    ui.app.goto_album();
                })));
            }
            if dates {
                line = line.child(row().key("added").w(DATE_W).child(text(added(t.added, now_secs())).font(NUM).color(th.dim)));
            }
            line = line.child(actions).child(row().key("time").w(44.0).justify_end().child(text(mmss(t.ms)).font(NUM).color(th.dim)));
        }
        // the playing row: a faint wash of the accent
        let under = if playing { th.accent.alpha(if th.dark { 0.09 } else { 0.08 }) } else { Color::TRANSPARENT };
        let line = pickable(line.radius(8.0), r, selected, focused, under, th)
            .on_secondary_press(move |ui: &mut Ui, at, tx| {
                pick(ui, r);
                ui.after(tx);
                track_menu(ui, at.into(), tx);
            });
        let mut cell = col().key(format!("row-{r}")).w_full();
        if let Some(h) = header {
            cell = cell.child(row().key("disc").h(34.0).px(14.0).pt(if r == 0 { 0.0 } else { 10.0 }).items_end()
                .child(text(h.to_uppercase()).font(LABEL).color(th.dim)));
        }
        cell.child(line)
    })
}

/// The explicit mark: a quiet chip, read as metadata rather than a warning.
pub fn explicit(th: &Theme) -> El {
    row().key("e").size(14.0, 14.0).radius(3.0).fill(th.wash).center()
        .child(text("E").font(ts(8.5, 650.0)).color(th.dim))
}

/// A Spotify uri as a link to send someone.
pub fn web_link(uri: &str) -> Option<String> {
    let mut p = uri.strip_prefix("spotify:")?.splitn(2, ':');
    Some(format!("https://open.spotify.com/{}/{}", p.next()?, p.next()?))
}

/// Copy `uri`'s link, and say so.
pub fn copy_link(a: &mut App, tx: &mut Tx, uri: Option<String>) {
    if let Some(url) = uri.as_deref().and_then(web_link) {
        tx.copy(url);
        a.flash("link copied");
    }
}

/// What can be done with the song under the cursor, as the system's menu.
fn track_menu(ui: &Ui, at: PopupAnchor, tx: &mut Tx) {
    let a = &ui.app;
    let Some(t) = a.selected_track().and_then(|i| a.view.tracks.get(i)) else { return };
    let episode = t.is_episode();
    let up_next = a.view.key == QUEUE_KEY;
    let mut m = PopupMenu::new()
        .item(PopupItem::new("play", "Play").symbol("play.fill"))
        .item(PopupItem::new("next", "Play next").symbol("text.line.first.and.arrowtriangle.forward"))
        .item(PopupItem::new("queue", "Add to queue").symbol("text.badge.plus").enabled(!up_next))
        .separator();
    if episode {
        m = m.item(PopupItem::new("album", "Go to podcast").symbol("mic"));
    } else {
        m = m.item(PopupItem::new("album", "Go to album").symbol("square.stack"))
            .item(PopupItem::new("artist", "Go to artist").symbol("music.mic"));
    }
    if up_next {
        m = m.separator().item(PopupItem::new("remove", "Remove from queue").symbol("minus.circle"));
    }
    m = m.separator().item(PopupItem::new("copy", "Copy link").symbol("link"));
    tx.popup_menu(m, at, |ui: &mut Ui, picked, tx| {
        let a = &mut ui.app;
        match picked {
            Some("play") => a.enter(),
            Some("next") => {
                if let Some(t) = a.selected_track().and_then(|i| a.view.tracks.get(i)).cloned() {
                    a.flash(format!("{} plays next", t.name));
                    a.enqueue_at(0, [t]);
                }
            }
            Some("queue") => a.enqueue_selected(),
            Some("album") => a.goto_album(),
            Some("artist") => a.goto_artist(),
            Some("remove") => a.dequeue_selected(),
            Some("copy") => {
                let uri = a.selected_track().and_then(|i| a.view.tracks.get(i)).map(|t| t.uri.clone());
                copy_link(a, tx, uri);
            }
            _ => {}
        }
        ui.after(tx);
    });
}

pub fn link_list(ui: &Ui, th: &Theme) -> El {
    let a = &ui.app;
    let v = &a.view;
    let n = a.shown().len();
    let rows = col().key("rows").w_full().px(14.0).pt(4.0).pb(24.0).growth(GrowthSpec::default())
        .virtual_rows(n, LINK_ROW, |_: &Ui, r| Cow::Owned(format!("row-{r}")), move |ui: &Ui, r| link_row(ui, r, &theme_now()))
        .virtual_rows_version((v.id, a.find.is_some(), a.matches.len(), v.links.len()));
    col().key("list").w_full().grow(1.0).min_h(0.0)
        .child(column_head(ui, th, &[("", Some(32.0)), ("name", None), ("details", None)]))
        .child(scroller(ui, format!("list-{}", v.id), rows))
}

fn link_row(ui: &Ui, r: usize, th: &Theme) -> El {
    let a = &ui.app;
    let Some(i) = a.shown().get(r) else { return el().key(format!("row-{r}")).h(LINK_ROW) };
    let l = &a.view.links[i];
    let selected = a.tr_state.selected() == Some(r);
    let focused = a.focus == Focus::Tracks;
    let kind = a.view.kind;
    let thumb = l.image.as_deref().map(art::small).and_then(|u| ui.thumb(&u));
    let version = (a.view.id, r, i, selected, focused, th.dark, thumb.as_ref().map(rune::Image::id));
    memo(format!("row-{r}"), version, || {
        let round = kind == Kind::Artists;
        let glyph = match kind {
            Kind::Artists => &icons::PERSON,
            Kind::Shows => &icons::MIC,
            _ => &icons::ALBUM,
        };
        let radius = if round { 16.0 } else { 6.0 };
        let tile = match &thumb {
            Some(img) => cover("ic".into(), Some(img), 32.0, radius, th, glyph),
            None => tile("ic".into(), 32.0, radius, Some(glyph), th),
        };
        let uri = l.uri.clone();
        pickable(row().key(format!("row-{r}")).w_full().h(LINK_ROW).px(12.0).gap(14.0).center_y().radius(8.0), r, selected, focused, Color::TRANSPARENT, th)
            .cursor(Cursor::Pointer)
            .on_secondary_press(move |ui: &mut Ui, at, tx| {
                pick(ui, r);
                ui.after(tx);
                let m = PopupMenu::new()
                    .item(PopupItem::new("open", "Open").symbol("arrow.up.right"))
                    .separator()
                    .item(PopupItem::new("copy", "Copy link").symbol("link"));
                let uri = uri.clone();
                tx.popup_menu(m, at, move |ui: &mut Ui, picked, tx| {
                    match picked {
                        Some("open") => ui.app.enter(),
                        Some("copy") => copy_link(&mut ui.app, tx, Some(uri.clone())),
                        _ => {}
                    }
                    ui.after(tx);
                });
            })
            .child(tile)
            .child(row().key("name").grow(1.0).shrink().min_w(0.0).child(text(l.name.clone()).font(BODY_MED).color(th.text).truncate().help_when_truncated(l.name.clone())))
            .child(row().key("sub").grow(1.0).shrink().min_w(0.0).child(text(l.sub.clone()).font(BODY).color(th.sub).truncate()))
    })
}
