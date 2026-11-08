// Colors: one near-monochrome palette, light and dark, and a single accent taken from the
// playing song's cover. The covers are the color in the window: the surfaces lean a hair toward
// the playing cover's hue, so the window warms or cools with each song, and the accent marks
// what's playing (its row, its playlist, the play button) and how far through it is.

use rune::Color;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

/// The window's colors, light or dark.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Theme {
    pub accent: Color, // what's playing, and the bar's fill: the cover's color, quietened
    pub text: Color,   // a warm off-white (or near-black)
    pub sub: Color,
    pub dim: Color, // numbers, durations, column heads
    pub err: Color,
    pub bg: Color,     // the window
    pub side: Color,   // the sidebar and the player: a hair off the window
    pub raised: Color, // fields, cards, menus
    pub line: Color,   // hairlines between regions
    pub wash: Color,   // quiet fills: empty covers, tracks of bars, the keyboard's cursor
    pub sel: Color,    // the open list in the sidebar, the selected row: a lighter surface
    pub dark: bool,
}

/// The playing cover's color as 0x01RRGGBB (sRGB), or 0 with nothing playing.
static ART: AtomicU32 = AtomicU32::new(0);

/// The playing cover's color (sRGB 0..1), or None: the accent follows it.
pub fn set_art(tint: Option<[f32; 3]>) {
    let v = tint.map_or(0, |[r, g, b]| {
        let c = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u32;
        0x0100_0000 | c(r) << 16 | c(g) << 8 | c(b)
    });
    ART.store(v, Relaxed);
}

/// Changes when the accent does, for rows' memo versions.
pub fn art_key() -> u32 {
    ART.load(Relaxed)
}

/// 0xRRGGBB (sRGB) as a color.
pub fn rgb(h: u32) -> Color {
    let ch = |sh: u32| rune::color::srgb_decode(((h >> sh) & 0xff) as f32 / 255.0);
    Color { r: ch(16), g: ch(8), b: ch(0), a: 1.0 }
}

/// A warm grey: the window's text is all on one hue, barely tinted.
fn grey(l: f32, c: f32) -> Color {
    rune::color::oklch(l, c, 75.0)
}

/// A cover's color (0xRRGGBB) as oklab lightness, chroma and hue (radians).
fn lch(hex: u32) -> (f32, f32, f32) {
    let [l, a, b, _] = rgb(hex).to_oklab();
    (l, (a * a + b * b).sqrt(), b.atan2(a))
}

/// The playing cover's hue in degrees, if it has a real color; else the warm grey's.
fn surface_hue() -> Option<f32> {
    match ART.load(Relaxed) {
        0 => None,
        v => Some(lch(v & 0xff_ffff)).filter(|&(_, c, _)| c >= 0.02).map(|(_, _, h)| h.to_degrees()),
    }
}

pub fn theme(dark: bool) -> Theme {
    // the surfaces take the playing cover's hue, just enough to feel: the window least, the
    // sidebar and the player a little more
    let hue = surface_hue();
    let surface = |l: f32, quiet: f32, tinted: f32| match hue {
        Some(h) => rune::color::oklch(l, tinted, h),
        None => grey(l, quiet),
    };
    let (bg, side, raised, text, sub, dim, err) = if dark {
        (surface(0.175, 0.003, 0.006), surface(0.158, 0.003, 0.010), surface(0.225, 0.004, 0.010), grey(0.935, 0.012), grey(0.70, 0.006), grey(0.52, 0.005), rgb(0xe0705e))
    } else {
        (surface(0.987, 0.002, 0.006), surface(0.968, 0.003, 0.010), Color::WHITE, grey(0.24, 0.008), grey(0.47, 0.006), grey(0.64, 0.005), rgb(0xc2402f))
    };
    let (line, wash, sel) = if dark {
        (text.alpha(0.07), text.alpha(0.05), text.alpha(0.08))
    } else {
        (text.alpha(0.09), text.alpha(0.045), text.alpha(0.07))
    };
    let accent = match ART.load(Relaxed) {
        0 => sub,
        v => quiet(v & 0xff_ffff, dark),
    };
    Theme { accent, text, sub, dim, err, bg, side, raised, line, wash, sel, dark }
}

/// A cover's color as the accent: its hue, held to a quiet chroma and a lightness that reads on
/// the window, so a neon sleeve doesn't shout and a grey one still shows as a color.
fn quiet(hex: u32, dark: bool) -> Color {
    let (l, chroma, hue) = lch(hex);
    if chroma < 0.02 {
        // no real color in it: a plain light grey
        return grey(if dark { 0.80 } else { 0.42 }, 0.0);
    }
    let (lo, hi) = if dark { (0.70, 0.80) } else { (0.45, 0.56) };
    let c = chroma.clamp(0.05, 0.14);
    Color::from_oklab([l.clamp(lo, hi), c * hue.cos(), c * hue.sin(), 1.0])
}

/// A cover's color (sRGB 0..1) as the wash at the top of a list's header: deep and quiet on
/// dark, pale on light, fading down into the window.
pub fn backdrop([r, g, b]: [f32; 3], dark: bool) -> Color {
    let c = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u32;
    let (_, chroma, hue) = lch(c(r) << 16 | c(g) << 8 | c(b));
    let (l, cap) = if dark { (0.30, 0.07) } else { (0.92, 0.045) };
    let c = if chroma < 0.02 { 0.0 } else { chroma.min(cap) };
    Color::from_oklab([l, c * hue.cos(), c * hue.sin(), 1.0])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contrast(x: Color, y: Color) -> f32 {
        let (a, b) = (x.luminance() + 0.05, y.luminance() + 0.05);
        a.max(b) / a.min(b)
    }

    #[test]
    fn text_reads_on_the_window() {
        for dark in [true, false] {
            let th = theme(dark);
            assert!(contrast(th.text, th.bg) > 7.0);
            assert!(contrast(th.sub, th.bg) > 4.5);
        }
    }

    #[test]
    fn any_cover_gives_an_accent_that_reads() {
        for hex in [0xff0000, 0x00ff00, 0x0000ff, 0x101010, 0xfafafa, 0x7a3fd0, 0xffe800] {
            for dark in [true, false] {
                let c = quiet(hex, dark);
                assert!(contrast(c, theme(dark).bg) > 3.0, "{hex:06x} dark={dark}");
            }
        }
    }

    #[test]
    fn text_reads_on_any_tinted_window() {
        for hex in [0xff0000, 0x00ff00, 0x0000ff, 0x7a3fd0, 0xffe800] {
            set_art(Some([((hex >> 16) & 0xff) as f32 / 255.0, ((hex >> 8) & 0xff) as f32 / 255.0, (hex & 0xff) as f32 / 255.0]));
            for dark in [true, false] {
                let th = theme(dark);
                assert!(contrast(th.text, th.bg) > 7.0 && contrast(th.text, th.side) > 7.0, "{hex:06x} dark={dark}");
                assert!(contrast(th.accent, th.bg) > 3.0, "{hex:06x} dark={dark}");
            }
        }
        set_art(None);
    }

    #[test]
    fn hex_is_srgb() {
        assert_eq!(rgb(0xffffff), Color::WHITE);
        assert!((rgb(0x808080).r - 0.2158).abs() < 0.001);
    }
}
