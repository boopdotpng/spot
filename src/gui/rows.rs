// The open list's rows. Under the pointer a row shows what a click can do: its number turns into
// a play button, and queue and "more" buttons come up by the time; a right click (or "more")
// opens the system's menu for it. Artist and album names go to the artist and album.

use std::borrow::Cow;

use rune::{Color, Cursor, Cx, El, GrowthSpec, PopupAnchor, PopupItem, PopupMenu, ScrollSpec, Tx, col, el, icon, memo, row, text};

use super::view::{BODY, BODY_MED, LABEL, LINK_ROW, NUM, ROW, SMALL, cover, eq, swatch_tile, theme_now, ts};
use super::{Ui, icons};
use crate::*;

/// Rows are wide enough for an album column.
fn wide(cx: &mut Cx<Ui>) -> bool {
    cx.layout_size("main").is_none_or(|s| s[0] >= 720.0)
}

/// Point the cursor at row `r` of the list, as a click on it does.
fn pick(ui: &mut Ui, r: usize) {
    ui.app.focus = Focus::Tracks;
    ui.app.tr_state.select(Some(r));
}

fn column_head(ui: &Ui, th: &Theme, cols: &[(&'static str, Option<f32>)]) -> El {
    let mut r = row().key("cols").w_full().h(28.0).px(26.0).gap(14.0).center_y();
    for (i, (label, w)) in cols.iter().enumerate() {
        let t = text(label.to_uppercase()).font(LABEL).color(th.dim);
        let cell = match w {
            Some(w) => row().key(format!("col-{i}")).w(*w).child(if *label == "time" { row().w_full().justify_end().child(t) } else { t }),
            None => row().key(format!("col-{i}")).grow(1.0).shrink().min_w(0.0).child(t),
        };
        r = r.child(cell);
    }
    // a hairline under the heads that shows once rows slide beneath them
    let rule = el().key("cols-rule").h(1.0).w_full().fill(th.line).live_opacity(&ui.scrolled);
    col().key("cols-wrap").w_full().child(r).child(rule)
}

fn scroller(ui: &Ui, key: String, rows: El) -> El {
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
    let wide = wide(cx);
    let mut cols: Vec<(&'static str, Option<f32>)> = vec![("#", Some(28.0)), ("", Some(34.0))];
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
        cols.extend([("", Some(56.0)), ("time", Some(44.0))]);
    }
    let n = a.shown().len();
    let rows = col().key("rows").w_full().px(14.0).pt(4.0).pb(24.0).growth(GrowthSpec::default())
        .virtual_rows(n, ROW, |_: &Ui, r| Cow::Owned(format!("row-{r}")), move |ui: &Ui, r| track_row(ui, r, &theme_now(), wide))
        .virtual_rows_version((v.id, a.find.is_some(), a.matches.len(), v.tracks.len()));
    col().key("list").w_full().grow(1.0).min_h(0.0).child(column_head(ui, th, &cols)).child(scroller(ui, format!("list-{}", v.id), rows))
}

/// A row's little buttons: quiet, brighter under the pointer, a tooltip each.
fn row_button(key: &str, i: &'static rune::Icon, th: &Theme, help: &str, f: impl Fn(&mut Ui, &mut Tx) + 'static) -> El {
    row().key(key.to_string()).size(26.0, 26.0).radius(13.0).center().cursor(Cursor::Pointer)
        .hover(th.text.alpha(0.1)).press_scale(0.88).help(help.to_string()).label(help.to_string())
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

fn track_row(ui: &Ui, r: usize, th: &Theme, wide: bool) -> El {
    let a = &ui.app;
    let v = &a.view;
    let Some(i) = a.shown().get(r) else { return el().key(format!("row-{r}")).h(ROW) };
    let t = &v.tracks[i];
    let episodes = is_episodes(&v.key);
    let feed = episodes && !is_show(&v.key);
    let up_next = v.key == QUEUE_KEY;
    let playing_uri = a.queue.as_ref().filter(|q| q.key == v.key && a.queued.is_none()).and_then(|q| q.tracks.get(q.idx())).map(|t| t.uri.as_str());
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
    let version = ((v.id, r, i, playing, playing && a.playing, selected, focused), (thumb.as_ref().map(rune::Image::id), progress, wide, th.dark, header.clone(), FAMILY.load(std::sync::atomic::Ordering::Relaxed)));
    memo(format!("row-{r}"), version, || {
        let fill = if selected {
            if focused { th.sel } else { th.wash }
        } else {
            Color::TRANSPARENT
        };
        let name_c = if playing { th.accent } else { th.text };
        // the number, or the eq while it plays; under the pointer, a play button
        let mark = if playing { eq(th.accent, a.playing, "row-eq") } else { text(num(i + 1)).font(NUM).color(th.dim) };
        let num_cell = row().key("n").w(28.0).h(28.0).justify_end().center_y()
            .child(row().key("n-mark").group_opacity(1.0, 0.0).child(mark))
            .child(row().key("n-play").pin(Some(4.0), Some(2.0), None, None).size(24.0, 24.0).radius(12.0).center()
                .group_opacity(0.0, 1.0).cursor(Cursor::Pointer).hover(th.text.alpha(0.1)).press_scale(0.85).help("Play")
                .on_press_tx(move |ui: &mut Ui, tx| {
                    pick(ui, r);
                    ui.app.enter();
                    ui.after(tx);
                })
                .child(icon(&icons::PLAY, 15.0).color(th.text)));
        let mut title_line = row().key("tl").gap(6.0).center_y().min_w(0.0)
            .child(text(t.name.clone()).key("name").font(BODY_MED).color(name_c).truncate().shrink().help_when_truncated(t.name.clone()));
        if t.explicit {
            title_line = title_line.child(row().key("e").size(15.0, 15.0).radius(3.0).fill(th.dim.alpha(0.4)).center()
                .child(text("E").font(ts(9.0, 750.0)).color(th.text.alpha(0.8))));
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
            .child(num_cell)
            .child(cover("c".into(), thumb.as_ref(), 34.0, 5.0, th, if episodes { &icons::MIC } else { &icons::NOTE }))
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
                line = line.child(row().key("album").grow(1.0).shrink().min_w(0.0).child(link_text("al", t.album.clone(), BODY, th.sub, th.text, move |ui| {
                    pick(ui, r);
                    ui.app.goto_album();
                })));
            }
            line = line.child(actions).child(row().key("time").w(44.0).justify_end().child(text(mmss(t.ms)).font(NUM).color(th.dim)));
        }
        let line = line.radius(9.0).fill(fill).hover(if selected { fill } else { th.wash })
            .on_press_tx(move |ui: &mut Ui, tx| {
                pick(ui, r);
                ui.after(tx);
            })
            .on_double_press_tx(move |ui: &mut Ui, tx| {
                pick(ui, r);
                ui.app.enter();
                ui.after(tx);
            })
            .on_secondary_press(move |ui: &mut Ui, at, tx| {
                pick(ui, r);
                ui.after(tx);
                track_menu(ui, at.into(), tx);
            });
        let mut cell = col().key(format!("row-{r}")).w_full();
        if let Some(h) = header {
            cell = cell.child(row().key("disc").h(34.0).px(14.0).pt(if r == 0 { 0.0 } else { 10.0 }).items_end()
                .child(text(h.to_uppercase()).font(LABEL).color(th.accent2)));
        }
        cell.child(line)
    })
}

/// The song under the cursor, as a link to send someone.
fn web_link(uri: &str) -> Option<String> {
    let mut p = uri.strip_prefix("spotify:")?.splitn(2, ':');
    Some(format!("https://open.spotify.com/{}/{}", p.next()?, p.next()?))
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
                    a.note = Some((format!("{} plays next", t.name), Instant::now() + Duration::from_millis(1500)));
                    a.enqueue_at(0, [t]);
                }
            }
            Some("queue") => a.enqueue_selected(),
            Some("album") => a.goto_album(),
            Some("artist") => a.goto_artist(),
            Some("remove") => a.dequeue_selected(),
            Some("copy") => {
                if let Some(url) = a.selected_track().and_then(|i| a.view.tracks.get(i)).and_then(|t| web_link(&t.uri)) {
                    tx.copy(url);
                    a.note = Some(("link copied".into(), Instant::now() + Duration::from_millis(1500)));
                }
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
        .child(column_head(ui, th, &[("#", Some(28.0)), ("", Some(32.0)), ("name", None), ("details", None)]))
        .child(scroller(ui, format!("list-{}", v.id), rows))
}

fn link_row(ui: &Ui, r: usize, th: &Theme) -> El {
    let a = &ui.app;
    let Some(i) = a.shown().get(r) else { return el().key(format!("row-{r}")).h(LINK_ROW) };
    let l = &a.view.links[i];
    let selected = a.tr_state.selected() == Some(r);
    let focused = a.focus == Focus::Tracks;
    let kind = a.view.kind;
    let version = (a.view.id, r, i, selected, focused, th.dark, FAMILY.load(std::sync::atomic::Ordering::Relaxed));
    memo(format!("row-{r}"), version, || {
        let fill = if selected {
            if focused { th.sel } else { th.wash }
        } else {
            Color::TRANSPARENT
        };
        let round = kind == Kind::Artists;
        let glyph = match kind {
            Kind::Artists => &icons::PERSON,
            Kind::Shows => &icons::MIC,
            _ => &icons::ALBUM,
        };
        let tile = swatch_tile("ic".into(), swatch(&l.name, th), 32.0, if round { 16.0 } else { 7.0 }, Some(glyph));
        let uri = l.uri.clone();
        row().key(format!("row-{r}")).w_full().h(LINK_ROW).px(12.0).gap(14.0).center_y().radius(9.0)
            .fill(fill).hover(if selected { fill } else { th.wash }).cursor(Cursor::Pointer)
            .on_press_tx(move |ui: &mut Ui, tx| {
                pick(ui, r);
                ui.after(tx);
            })
            .on_double_press_tx(move |ui: &mut Ui, tx| {
                pick(ui, r);
                ui.app.enter();
                ui.after(tx);
            })
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
                        Some("copy") => {
                            if let Some(url) = web_link(&uri) {
                                tx.copy(url);
                                ui.app.note = Some(("link copied".into(), Instant::now() + Duration::from_millis(1500)));
                            }
                        }
                        _ => {}
                    }
                    ui.after(tx);
                });
            })
            .child(row().key("n").w(28.0).justify_end().child(text(num(i + 1)).font(NUM).color(th.dim)))
            .child(tile)
            .child(row().key("name").grow(1.0).shrink().min_w(0.0).child(text(l.name.clone()).font(BODY_MED).color(th.text).truncate().help_when_truncated(l.name.clone())))
            .child(row().key("sub").grow(1.0).shrink().min_w(0.0).child(text(l.sub.clone()).font(BODY).color(th.sub).truncate()))
    })
}
