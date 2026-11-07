// Colors: the theme families, each light and dark, and what the window paints with them.

use crate::*;
use rune::Color;

/// One family's palette for light or dark, plus the surfaces the window derives from it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Theme {
    pub accent: Color,  // focus, playing
    pub accent2: Color, // where it's playing from
    pub deep: Color,    // progress track, quiet fills
    pub hl: Color,      // the selected row
    pub text: Color,
    pub sub: Color,
    pub dim: Color, // durations, rules
    pub gold: Color, // shuffle / repeat
    pub pink: Color, // Liked Songs
    pub err: Color,
    pub bg: Color,      // the window: a near-neutral, only a breath of the family's hue
    pub side: Color,    // the sidebar and the player: a hair off the window
    pub raised: Color,  // fields, cards, menus
    pub line: Color,    // hairlines between regions
    pub wash: Color,    // quiet fills: hover, empty covers, tracks of bars
    pub sel: Color,     // the selected row
    pub on_accent: Color, // text on an accent fill
    pub dark: bool,
}

/// Index into families(), cycled with ⌘T and remembered in the cache dir.
pub static FAMILY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Theme families as (name, dark, light). Each palette is
/// [accent, accent2, deep, hl, text, sub, dim, gold, pink, err] (see Theme).
pub const FAMILIES: [(&str, [u32; 10], [u32; 10]); 7] = [
    // muted desert sunset: amber sky, dusky mountains, dark scrub; at midday for light
    ("desert",
     [0xd0924a, 0xb08a66, 0x2e2722, 0x5c4128, 0xd6c8b4, 0x8f8679, 0x4f4a55, 0xd9b066, 0xa58b98, 0xc26a50],
     [0xa8621c, 0x8a6440, 0xe8dfd3, 0xf0d3a8, 0x3a3029, 0x6e655a, 0xa39a90, 0x9a7014, 0x8f5f78, 0xb0472f]),
    // shallow water: teal, slate blue, coral reef
    ("lagoon",
     [0x4fb3a9, 0x7fa8b8, 0x1d2b2e, 0x1f4a4c, 0xcfdcda, 0x7f9796, 0x3f5256, 0xe39a7b, 0xd98ba0, 0xe0685e],
     [0x16857c, 0x3f6f86, 0xdcebea, 0xbfe3df, 0x23393b, 0x56706f, 0x9db3b2, 0xc0623f, 0xb4506e, 0xc23b30]),
    // forest floor: sage, lichen, wild plum
    ("moss",
     [0x9cbf6b, 0xb5a77a, 0x262b22, 0x3d4a2c, 0xd8dccb, 0x8d9581, 0x4a5243, 0xe0b45e, 0xc08aa8, 0xd46a5a],
     [0x5b8a2a, 0x7d6e3f, 0xe5eadb, 0xd6e6bd, 0x2e3326, 0x626b55, 0xa7ae98, 0xa8781a, 0x96507a, 0xb5402f]),
    // twilight: violet, periwinkle, peach
    ("iris",
     [0xa98be0, 0x8fa0d8, 0x26223a, 0x3e3466, 0xdcd6ec, 0x928ba8, 0x4b4663, 0xf0b48a, 0xe58fb6, 0xe86f7a],
     [0x6c4cc4, 0x4a5fb0, 0xe8e3f4, 0xd9cff5, 0x2d2840, 0x625b78, 0xaaa3c0, 0xb8673a, 0xb03f7a, 0xc2364a]),
    // pastels; light variants are deepened so they stay readable on white
    // cherry blossom: rose, lavender, cream
    ("blossom",
     [0xf5a6c8, 0xc6a8f0, 0x2a2430, 0x4a3550, 0xeadff0, 0xa597b0, 0x564b60, 0xf6d3a1, 0xe8a0d8, 0xf28b9a],
     [0xc4467e, 0x7f5cc4, 0xf3e6ef, 0xf6d4e5, 0x3d2f3f, 0x7a6a80, 0xbfaec4, 0xb27a28, 0xa850a0, 0xcc3b55]),
    // mint and sky
    ("mint",
     [0x94e2c0, 0x89c4f4, 0x1f2a2a, 0x2f4a48, 0xdcefea, 0x92aaa8, 0x46585a, 0xf9e2af, 0xf5b8d0, 0xf38ba8],
     [0x1f9a73, 0x2f7fc4, 0xe0f1ec, 0xc6ebde, 0x26393a, 0x5d7473, 0xa5bab8, 0xb8861c, 0xc0507e, 0xd0304f]),
    // peach sorbet with lilac
    ("sorbet",
     [0xfab387, 0xcba6f7, 0x2c2626, 0x4d3a36, 0xf0e2da, 0xab9a92, 0x5a4e4c, 0xf9e2af, 0xf5c2e7, 0xf38ba8],
     [0xd0601c, 0x8839ef, 0xf6e8e0, 0xfad8c4, 0x40302a, 0x7d6a62, 0xc4b2aa, 0xb07a10, 0xc0479a, 0xd20f39]),
];

/// A theme: its name and palettes, dark and light.
pub type Family = (String, [u32; 10], [u32; 10]);
/// The palette's colors, as config.toml names them.
pub const COLOR_NAMES: [&str; 10] = ["accent", "accent2", "deep", "hl", "text", "sub", "dim", "gold", "pink", "err"];

static LIVE: std::sync::OnceLock<Vec<Family>> = std::sync::OnceLock::new();

/// The built-in themes with config.toml's: same name replaces, a new one goes at the end.
pub fn merged(custom: Vec<Family>) -> Vec<Family> {
    let mut all: Vec<Family> = FAMILIES.iter().map(|(n, d, l)| (n.to_string(), *d, *l)).collect();
    for f in custom {
        match all.iter_mut().find(|a| a.0 == f.0) {
            Some(a) => *a = f,
            None => all.push(f),
        }
    }
    all
}

/// Before anything draws: the first call to families() settles them.
pub fn use_themes(custom: Vec<Family>) {
    let _ = LIVE.set(merged(custom));
}

pub fn families() -> &'static [Family] {
    LIVE.get_or_init(|| merged(vec![]))
}

/// 0xRRGGBB (sRGB) as a color.
pub fn rgb(h: u32) -> Color {
    let ch = |sh: u32| rune::color::srgb_decode(((h >> sh) & 0xff) as f32 / 255.0);
    Color { r: ch(16), g: ch(8), b: ch(0), a: 1.0 }
}

/// The current family, light or dark.
pub fn theme(dark: bool) -> Theme {
    use std::sync::atomic::Ordering::Relaxed;
    let all = families();
    let f = &all[FAMILY.load(Relaxed).min(all.len() - 1)];
    palette(if dark { &f.1 } else { &f.2 }, dark)
}

pub fn palette(h: &[u32; 10], dark: bool) -> Theme {
    let c = h.map(rgb);
    let deep = c[2];
    // The surfaces are near-neutral greys that keep only a trace of the family's hue, so the
    // color in the window comes from the music (covers, the glow) and the accents, not from
    // one tint over everything.
    let [_, da, db, _] = deep.to_oklab();
    let hue = db.atan2(da).to_degrees();
    let grey = |l: f32, c: f32| rune::color::oklch(l, c, hue);
    let (bg, side, raised) = if dark {
        (grey(0.175, 0.007), grey(0.155, 0.007), grey(0.235, 0.009))
    } else {
        (grey(0.985, 0.003), grey(0.965, 0.005), Color::WHITE)
    };
    let text = c[4];
    let (line, wash) = if dark { (text.alpha(0.08), text.alpha(0.06)) } else { (text.alpha(0.1), text.alpha(0.055)) };
    let sel = c[0].alpha(if dark { 0.16 } else { 0.13 });
    let on_accent = if c[0].luminance() > 0.35 { bg.mix(Color::BLACK, 0.4) } else { Color::WHITE };
    Theme {
        accent: c[0],
        accent2: c[1],
        deep,
        hl: c[3],
        text: c[4],
        sub: c[5],
        dim: c[6],
        gold: c[7],
        pink: c[8],
        err: c[9],
        bg,
        side,
        raised,
        line,
        wash,
        sel,
        on_accent,
        dark,
    }
}

/// Switch to the next theme family and remember it.
pub fn cycle_family() -> &'static str {
    let all = families();
    use std::sync::atomic::Ordering::Relaxed;
    let i = (FAMILY.load(Relaxed) + 1) % all.len();
    FAMILY.store(i, Relaxed);
    let _ = fs::write(cache_dir().join("theme"), &all[i].0);
    &all[i].0
}

/// Switch to family `i` (a dot under the sidebar) and remember it.
pub fn set_family(i: usize) {
    let all = families();
    let i = i.min(all.len() - 1);
    FAMILY.store(i, std::sync::atomic::Ordering::Relaxed);
    let _ = fs::write(cache_dir().join("theme"), &all[i].0);
}

/// A family's accent, light or dark, for its dot.
pub fn family_accent(i: usize, dark: bool) -> Color {
    let f = &families()[i];
    rgb(if dark { f.1[0] } else { f.2[0] })
}

/// Two colors for a thing with no art of its own (a playlist, an artist), picked from its name
/// so it keeps them: a hue, and its neighbor a third of the way round, at the theme's lightness.
pub fn swatch(name: &str, th: &Theme) -> (Color, Color) {
    let h = name.bytes().fold(0x811c9dc5u32, |h, b| (h ^ b as u32).wrapping_mul(0x01000193));
    let hue = (h % 360) as f32;
    let step = 28.0 + (h >> 9) as f32 % 50.0;
    let (l, c) = if th.dark { (0.68, 0.13) } else { (0.72, 0.12) };
    (rune::color::oklch(l, c, hue), rune::color::oklch(l - 0.1, c, hue + step))
}

/// A cover's color (sRGB 0..1) as a Color, vivid enough to read as the cover's.
pub fn cover_color(tint: [f32; 3], th: &Theme) -> Color {
    let c = Color { r: rune::color::srgb_decode(tint[0]), g: rune::color::srgb_decode(tint[1]), b: rune::color::srgb_decode(tint[2]), a: 1.0 };
    let [l, a, b, _] = c.to_oklab();
    let chroma = (a * a + b * b).sqrt();
    let hue = b.atan2(a);
    let (lo, hi) = if th.dark { (0.5, 0.75) } else { (0.55, 0.78) };
    let ch = chroma.clamp(0.0, 0.16);
    Color::from_oklab([l.clamp(lo, hi), ch * hue.cos(), ch * hue.sin(), 1.0])
}

/// The remembered family, at startup.
pub fn load_family() {
    let name = fs::read_to_string(cache_dir().join("theme")).unwrap_or_default();
    if let Some(i) = families().iter().position(|(n, ..)| n == name.trim()) {
        FAMILY.store(i, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_family_reads_on_its_own_background() {
        for (_, dark, light) in families() {
            for (p, is_dark) in [(dark, true), (light, false)] {
                let th = palette(p, is_dark);
                let (t, b) = (th.text.luminance() + 0.05, th.bg.luminance() + 0.05);
                assert!(t.max(b) / t.min(b) > 7.0, "contrast {}", t.max(b) / t.min(b));
            }
        }
    }

    #[test]
    fn hex_is_srgb() {
        assert_eq!(rgb(0xffffff), Color::WHITE);
        assert!((rgb(0x808080).r - 0.2158).abs() < 0.001);
    }
}
