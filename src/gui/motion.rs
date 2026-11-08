// The window's motion, in one place so what moves moves alike: things rise into place, small
// glyphs pop, a choice's highlight slides to its new place, and a header's color blooms in.
// Everything plays once, on a change, and settles; nothing loops (a looping animation redraws
// the window for as long as it runs, which is most of what a music player's window can cost).

use rune::{Spring, SpringPair, Transition};

/// Settling into place: quick, no bounce, and quicker still on the way out.
pub const GLIDE: SpringPair = SpringPair::new(Spring::new(0.38, 0.0), Spring::new(0.2, 0.0));
/// Small things arriving: a hint of overshoot.
pub const POP: SpringPair = SpringPair::new(Spring::new(0.34, 0.26), Spring::new(0.16, 0.0));
/// A highlight sliding to what's now chosen, with a little give at the end.
pub const SLIDE: Spring = Spring::new(0.38, 0.16);
/// A header's color washing in behind it, slower than the rest.
pub const BLOOM: SpringPair = SpringPair::new(Spring::new(0.9, 0.0), Spring::new(0.3, 0.0));
/// Leaving fast and quietly, out of the way of what replaces it.
const OUT: Spring = Spring::new(0.14, 0.0);

/// In from `dy` below, fading up; out the same way.
pub fn rise(dy: f32) -> Transition {
    Transition::opacity().combined(Transition::offset(0.0, dy)).springs(GLIDE)
}

/// In from below and out through the top, as a ticker turns: the playing song's name when the
/// next one starts.
pub fn roll(dy: f32) -> Transition {
    Transition::asymmetric(rise(dy), rise(-dy))
}

/// Shown at once, and gone in a quick fade: a container whose children make the entrance.
pub fn stage() -> Transition {
    Transition::asymmetric(Transition::identity(), Transition::opacity()).springs(SpringPair::new(GLIDE.enter, OUT))
}

/// A glyph trading places with another (play for pause): in from small with a bounce.
pub fn swap() -> Transition {
    Transition::scale(0.5).allow_small_scale().springs(POP)
}

/// A card or a note coming up over the window: up and in from a little smaller.
pub fn pop(dy: f32) -> Transition {
    Transition::scale(0.95).combined(Transition::offset(0.0, dy)).springs(POP)
}

/// A cover arriving: from soft focus, a little small.
pub fn focus() -> Transition {
    Transition::blur_replace().combined(Transition::scale(0.97)).springs(GLIDE)
}

/// How long row `i` of a list that just opened waits to rise in: a quick ripple down the first
/// screenful (rows further down, out of sight, come at once).
pub fn ripple(i: usize) -> f32 {
    if i < 14 { 0.02 * i as f32 } else { 0.0 }
}
