// The player along the bottom: the song, the transport and a hairline bar that takes clicks and
// drags (its fill is the one accent, the cover's color), and the volume, which also takes the
// scroll wheel. Everything but the song is white or grey and a little faded until hovered.

use rune::{Color, Cursor, El, Phase, col, el, icon, row, spacer, text};

use super::view::{NUM, artist_line, SMALL, cover, icon_button, ts};
use super::{Ui, icons, motion};
use crate::*;

pub const PLAYER: f32 = 84.0;
const VOL_W: f32 = 92.0;

pub fn player(ui: &Ui, th: &Theme) -> El {
    let a = &ui.app;
    let cur = a.now();
    // the song, and where it's playing from. While the next song's cover is on its way the last
    // one stays, so the new one comes into focus over it rather than over a blank square
    let now_img = ui.cover.as_ref().filter(|_| cur.is_some_and(|t| t.cover.is_some())).map(|(_, i)| i);
    let art = cover("now-cover".into(), now_img, 52.0, 6.0, th, &icons::NOTE)
        .cursor(Cursor::Pointer).help("Go to playing  o")
        .on_press_tx(|ui: &mut Ui, tx| {
            ui.app.show_queue();
            ui.after(tx);
        });
    let mut left = row().key("now").w(300.0).gap(14.0).center_y().child(art);
    if let Some(t) = cur {
        let ctx = if a.queued.is_some() { Some("Queue".to_string()) } else { a.queue.as_ref().map(|q| q.name.trim_start_matches(['♥', ' ']).to_string()) };
        let link = |key: String, s: String, font: rune::TextStyle, c: Color, hover: Color, help: &'static str, f: fn(&mut Ui)| {
            row().key(key).min_w(0.0).shrink().hover_group().cursor(Cursor::Pointer).help(help)
                .on_press_tx(move |ui: &mut Ui, tx| {
                    f(ui);
                    ui.after(tx);
                })
                .child(text(s).font(font).color(c).group_color(hover).truncate())
        };
        // a new song's name and artists roll up into place as the last one's roll away
        let mut words = col().key(format!("now-words-{}", t.uri)).transition(motion::roll(10.0)).grow(1.0).shrink().min_w(0.0).gap(2.0)
            .child(link("now-name".into(), t.name.clone(), ts(13.5, 620.0), th.text, th.text.alpha(0.8), "Go to album  a", |ui| {
                ui.app.focus = Focus::Playlists; // goto acts on what's playing, not the cursor
                ui.app.goto_album();
            }))
            .child(artist_line("now-by", &t.artists, &artist_links(t), SMALL, th.sub, th.text, "Go to artist  A", |_| {}));
        if let Some(c) = ctx {
            words = words.child(link("now-ctx".into(), c, ts(11.0, 500.0), th.dim, th.sub, "Show where it's playing from  o", |ui| ui.app.show_queue()));
        }
        left = left.child(words);
    } else {
        left = left.child(text("nothing playing").font(SMALL).color(th.dim));
    }

    // transport: modes go white when on, with a dot popping up under them
    let mode = |key: &str, i: &'static rune::Icon, on: bool, help: &str, f: fn(&mut Ui)| {
        let b = icon_button(key, i, 16.0, if on { th.text } else { th.dim }, th, help, f);
        let mut r = row().key(format!("{key}-wrap")).child(b);
        if on {
            r = r.child(el().key("lit").pin(Some(13.0), Some(27.0), None, None).size(4.0, 4.0).radius(2.0).fill(th.text).transition(motion::swap()));
        }
        r
    };
    let play_icon = if a.want_play { &icons::PAUSE } else { &icons::PLAY };
    let play = row().key("play").size(34.0, 34.0).radius(17.0).center().fill(th.text.alpha(0.88)).hover(th.text).cursor(Cursor::Pointer).press_scale(0.88)
        .help(if a.want_play { "Pause  space" } else { "Play  space" }).label(if a.want_play { "Pause" } else { "Play" })
        .on_press_tx(|ui: &mut Ui, tx| {
            ui.app.toggle();
            ui.after(tx);
        })
        .child(icon(play_icon, 16.0).key(if a.want_play { "pause" } else { "play" }).color(th.bg).transition(motion::swap()));
    let repeat_help = match a.repeat {
        Repeat::Off => "Repeat  r",
        Repeat::All => "Repeating the list  r",
        Repeat::One => "Repeating this song  r",
    };
    let mut repeat = mode("repeat", &icons::REPEAT, a.repeat != Repeat::Off, repeat_help, |ui| ui.app.cycle_repeat());
    if a.repeat == Repeat::One {
        repeat = repeat.child(row().key("one").pin(Some(19.0), Some(4.0), None, None).size(11.0, 11.0).radius(5.5).fill(th.text).center().transition(motion::swap())
            .child(text("1").font(ts(8.0, 800.0)).color(th.bg)));
    }
    let controls = row().key("controls").gap(10.0).center_y()
        .child(mode("shuffle", &icons::SHUFFLE, a.shuffle, if a.shuffle { "Shuffling  s" } else { "Shuffle  s" }, |ui| ui.app.toggle_shuffle()))
        .child(icon_button("prev", &icons::PREV, 17.0, th.text, th, "Previous  p", |ui| ui.app.prev()))
        .child(play)
        .child(icon_button("next", &icons::NEXT, 17.0, th.text, th, "Next  n", |ui| ui.app.skip_soon(1)))
        .child(repeat);

    // the bar: a hairline, filled in the accent; under the pointer a knob shows at the end, and a
    // drag moves it (and the clock) before letting go seeks
    let len = cur.map_or(0, |t| t.ms);
    let fill = el().key("bar-fill").absolute(0.0, 0.0).h(2.0).w(0.0).radius(1.0).fill(th.accent)
        .live_rect(&ui.bar);
    let track = el().key("bar-track").w_full().h(2.0).radius(1.0).fill(th.text.alpha(if th.dark { 0.12 } else { 0.1 })).clip_rounded(1.0)
        .child(fill);
    let knob = el().key("bar-knob").absolute(0.0, -4.0).size(10.0, 10.0).radius(5.0).fill(th.text)
        .shadow(1.0, 4.0, Color::BLACK.alpha(0.3)).group_opacity(0.0, 1.0)
        .live_rect(&ui.knob);
    let bar = el().key("bar").grow(1.0).h(2.0)
        .on_layout(|ui: &mut Ui, r: [f32; 4]| {
            if (ui.bar_w - r[2]).abs() > 0.5 {
                ui.bar_w = r[2];
                ui.sync_bar();
            }
        })
        .child(track).child(knob);
    let seek = row().key("seek").grow(1.0).h(18.0).center_y().cursor(Cursor::Pointer).hover_group()
        .on_drag(move |ui: &mut Ui, d, tx| {
            if d.w <= 0.0 || len == 0 {
                return;
            }
            let ratio = (d.x / d.w).clamp(0.0, 1.0);
            if d.phase == Phase::Up {
                ui.scrub = None;
                if !d.cancelled() {
                    ui.app.seek_to((ratio as f64 * len as f64) as u64);
                }
                ui.after(tx);
            } else {
                ui.scrub = Some(ratio);
                ui.sync_bar();
                ui.sync_clock();
                tx.live_only();
            }
        })
        .child(bar);
    let times = row().key("times").w_full().max_w(620.0).gap(10.0).center_y()
        .child(row().key("pos").w(40.0).justify_end().child(text("0:00").live_text(&ui.clock).font(NUM).color(th.dim)))
        .child(seek)
        .child(row().key("len").w(40.0).child(text(if cur.is_some() { mmss(len) } else { String::new() }).font(NUM).color(th.dim)));
    let center = col().key("center").grow(1.0).shrink().center().gap(4.0).child(controls).child(times);

    // up next, and the volume
    let pct = a.vol_pct();
    let muted = ui.unmute.is_some();
    let loud = a.vol_until.is_some();
    let vol_w = VOL_W * pct as f32 / 100.0;
    let vol_bar = el().key("vol-bar").w(VOL_W).h(2.0)
        .child(el().key("vol-track").w(VOL_W).h(2.0).radius(1.0).fill(th.text.alpha(0.12)).clip_rounded(1.0)
            .child(el().key("vol-fill").h(2.0).w(vol_w).radius(1.0).fill(if loud { th.text } else { th.sub }).motion(rune::tok::motion::SNAPPY)))
        .child(el().key("vol-knob").absolute(vol_w - 5.0, -4.0).size(10.0, 10.0).radius(5.0).fill(th.text)
            .shadow(1.0, 5.0, Color::BLACK.alpha(0.3)).group_opacity(0.0, 1.0).motion(rune::tok::motion::SNAPPY));
    let vol_slider = row().key("vol").h(20.0).center_y().cursor(Cursor::Pointer).hover_group()
        .help(format!("Volume {pct}%  + -"))
        .on_drag(|ui: &mut Ui, d, tx| {
            if d.w > 0.0 {
                let ratio = (d.x / d.w).clamp(0.0, 1.0);
                let want = (ratio * 65535.0) as i32;
                ui.unmute = None;
                ui.app.set_volume(want - ui.app.vol as i32);
                ui.after(tx);
            }
        })
        .child(vol_bar);
    let speaker = icon_button("mute", if muted || pct == 0 { &icons::MUTED } else { &icons::VOLUME }, 15.0, th.sub, th, if muted { "Unmute" } else { "Mute" }, |ui| {
        match ui.unmute.take() {
            Some(v) => ui.app.set_volume(v as i32 - ui.app.vol as i32),
            None => {
                ui.unmute = Some(ui.app.vol);
                ui.app.set_volume(-(ui.app.vol as i32));
            }
        }
    });
    let vol = row().key("vol-row").gap(6.0).center_y()
        .on_wheel(|ui: &mut Ui, w, tx| {
            let step = (-w.dy * 220.0) as i32;
            if step != 0 {
                ui.unmute = None;
                ui.app.set_volume(step);
                ui.after(tx);
            }
            true
        })
        .child(speaker).child(vol_slider)
        .child(row().key("pct").w(40.0).justify_end().child(text(format!("{pct}%")).font(NUM).color(if loud { th.text } else { th.dim })));
    let next_help = match cur.filter(|_| a.repeat != Repeat::One).and_then(|_| a.up_next()) {
        Some(n) => format!("Up next: {} · {}  Q", n.name, n.artists),
        None => "Up next  Q".to_string(),
    };
    let queue = icon_button("upnext", &icons::QUEUE, 16.0, if a.view.key == QUEUE_KEY { th.text } else { th.sub }, th, &next_help, |ui| ui.app.open_queue());
    // up next and the volume all but vanish until the pointer comes near (or the volume changes)
    let tools = row().key("tools").gap(6.0).center_y().group_opacity(if loud { 1.0 } else { 0.35 }, 1.0).child(queue).child(vol);
    let right = row().key("right").w(300.0).h_full().gap(6.0).center_y().hover_group().child(spacer()).child(tools);

    let rule = el().key("player-rule").pin(Some(0.0), Some(0.0), Some(0.0), None).h(1.0).fill(th.line);
    row().key("player").w_full().h(PLAYER).px(18.0).gap(16.0).center_y().fill(th.side)
        .child(rule).child(left).child(center).child(right)
}
