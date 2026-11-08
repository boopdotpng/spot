// Back and forward with two fingers across the trackpad, as in a browser: an arrow comes in from
// the side the fingers move away from, following them, and once they've gone far enough it turns
// solid (inverted, as the play button is) with a little pop; letting go there goes back (or
// forward). A swipe that's
// mostly up and down is left to scroll the list, and a mouse wheel never swipes.

use rune::{Color, El, GesturePhase, Live, Spring, Tx, WheelEvent, col, el, icon, row};

use super::{Ui, icons};
use crate::Theme;

/// How far the fingers go to go back or forward.
const ARM: f32 = 120.0;
const CHIP: f32 = 38.0;

#[derive(Default, PartialEq, Clone, Copy)]
enum Lock {
    #[default]
    Open, // nothing's moved yet
    Across, // a swipe: ours
    Along,  // a scroll: the list's
    Done,   // let go: what coasts after it is ours too, and does nothing
}

#[derive(Default)]
pub struct Swipe {
    dx: f32,
    dy: f32,
    lock: Lock,
}

/// One side's arrow, bound to live values so following the fingers redraws only it.
pub struct Mark {
    shown: Live<f32>,
    at: Live<[f32; 2]>,
    size: Live<f32>,
    armed: Live<f32>, // 1 once letting go would go: the solid face shows
    calm: Live<f32>,  // and the plain one doesn't
}

impl Mark {
    pub fn new() -> Mark {
        Mark { shown: Live::new(0.0), at: Live::new([0.0, 0.0]), size: Live::new(0.85), armed: Live::new(0.0), calm: Live::new(1.0) }
    }

    /// Show it `p` of the way to going (1 and over: it goes on letting go), `dir` -1 coming in
    /// from the left, 1 from the right.
    fn show(&self, p: f32, dir: f32) {
        let armed = p >= 1.0;
        self.shown.set((p * 1.6).min(1.0));
        self.at.set([-dir * (CHIP + 12.0) * (1.0 - p.min(1.0)), 0.0]);
        self.size.set(if armed { 1.12 } else { 0.85 + 0.15 * p });
        self.armed.set(if armed { 1.0 } else { 0.0 });
        self.calm.set(if armed { 0.0 } else { 1.0 });
    }

    fn hide(&self) {
        self.shown.set(0.0);
        self.size.set(0.85);
    }
}

/// A wheel event over the list, or over the header: true when it's part of a swipe (taken), false
/// to let it scroll.
pub fn wheel(ui: &mut Ui, w: WheelEvent, tx: &mut Tx) -> bool {
    if !w.precise || ui.menu.is_some() {
        return false;
    }
    if w.momentum {
        // the coast after a swipe goes nowhere; after a scroll, it scrolls
        return ui.swipe.lock == Lock::Done;
    }
    if w.phase == GesturePhase::Began || ui.swipe.lock == Lock::Done {
        ui.swipe = Swipe::default();
    }
    let s = &mut ui.swipe;
    s.dx += w.dx;
    s.dy += w.dy;
    // Rune offers a gesture to handlers only until it's taken or has scrolled: one declined
    // delta locks it to the list. So the first delta that moves at all decides
    if s.lock == Lock::Open && (s.dx != 0.0 || s.dy != 0.0) {
        s.lock = if s.dx.abs() > s.dy.abs() { Lock::Across } else { Lock::Along };
    }
    let ended = w.phase == GesturePhase::Ended;
    match s.lock {
        Lock::Across => {}
        _ => {
            if ended {
                ui.swipe = Swipe::default();
            }
            return false;
        }
    }
    // the fingers' way: right goes back, left goes forward (with nowhere to go, no arrow)
    let back = s.dx > 0.0;
    let can = if back { !ui.app.history.is_empty() } else { !ui.app.ahead.is_empty() };
    let p = if can { s.dx.abs() / ARM } else { 0.0 };
    let (on, off) = if back { (&ui.swipe_back, &ui.swipe_fwd) } else { (&ui.swipe_fwd, &ui.swipe_back) };
    off.hide();
    if ended {
        on.hide();
        ui.swipe.lock = Lock::Done;
        log::debug!("swipe: {:.0} pt {}", ui.swipe.dx, if back { "back" } else { "forward" });
        if p >= 1.0 {
            if back { ui.app.back() } else { ui.app.forward() }
            ui.after(tx);
            return true;
        }
    } else if p > 0.0 {
        on.show(p, if back { -1.0 } else { 1.0 });
    }
    tx.live_only();
    true
}

/// The two arrows, at the pane's left and right edges, unseen until a swipe brings one in.
pub fn marks(ui: &Ui, th: &Theme) -> [El; 2] {
    let follow = Spring::new(0.16, 0.0);
    let mark = |m: &Mark, back: bool| {
        let edge = if back { (Some(0.0), None) } else { (None, Some(0.0)) };
        let glyph = if back { &icons::ARROW_LEFT } else { &icons::ARROW_RIGHT };
        let fade = Spring::new(0.16, 0.0);
        // two faces, one over the other: plain (the window's raised surface, a text-colored
        // arrow) and solid (text-colored, the arrow cut out in the window's color)
        let face = |key: &'static str, fill: Color, ink: Color, on: &Live<f32>| {
            row().key(key).pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).radius(CHIP / 2.0).center().fill(fill)
                .live_opacity(on.smooth(fade))
                .child(icon(glyph, 18.0).color(ink))
        };
        let chip = el().key("chip").size(CHIP, CHIP).radius(CHIP / 2.0)
            .border(1.0, th.line).shadow(4.0, 18.0, Color::BLACK.alpha(if th.dark { 0.45 } else { 0.14 }))
            .live_opacity(m.shown.smooth(follow))
            .live_offset(m.at.smooth(follow))
            .live_scale(m.size.smooth(Spring::new(0.24, 0.32)))
            .child(face("plain", th.raised, th.text, &m.calm))
            .child(face("solid", th.text, th.bg, &m.armed));
        col().key(if back { "swipe-back" } else { "swipe-fwd" }).pin(edge.0, Some(0.0), edge.1, Some(0.0)).w(CHIP + 28.0).center()
            .inert().child(chip)
    };
    [mark(&ui.swipe_back, true), mark(&ui.swipe_fwd, false)]
}
