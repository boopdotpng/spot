// Colors: the theme families, light / dark, and the terminal's own background.

use crate::*;

/// One palette per theme family and light/dark; ANSI fallback without truecolor.
pub struct Theme {
    pub accent: Color,  // focus, playing
    pub accent2: Color, // context name
    pub deep: Color,    // unfocused selection, progress track
    pub hl: Color,      // focused selection background
    pub text: Color,
    pub sub: Color,
    pub dim: Color, // borders, durations
    pub gold: Color, // shuffle / repeat / prompt labels
    pub pink: Color, // Liked Songs
    pub err: Color,
}

/// Ask the terminal for its background (OSC 11): sets BG, and LIGHT unless `SPOT_THEME=light|dark`
/// forces it. Must run before the input thread starts reading stdin, or it eats the reply.
pub fn probe_terminal() {
    use std::sync::atomic::Ordering::Relaxed;
    use terminal_colorsaurus::{QueryOptions, ThemeMode, color_palette};
    let p = color_palette(QueryOptions::default()).ok();
    if let Some(p) = &p {
        let (r, g, b) = p.background.scale_to_8bit();
        BG.store(u32::from_be_bytes([0, r, g, b]), Relaxed);
    }
    let light = match std::env::var("SPOT_THEME").as_deref() {
        Ok("light") => true,
        Ok("dark") => false,
        _ => p.is_some_and(|p| p.theme_mode() == ThemeMode::Light),
    };
    LIGHT.store(light, Relaxed);
}

/// Terminal background as 0xRRGGBB; NO_BG until the terminal tells us.
pub static BG: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(NO_BG);
pub const NO_BG: u32 = u32::MAX;

/// `a` moved `t` of the way toward `b` (0xRRGGBB).
pub fn mix(a: u32, b: u32, t: f32) -> u32 {
    let ch = |c: u32, sh: u32| ((c >> sh) & 0xff) as f32;
    [16, 8, 0].iter().fold(0, |acc, &sh| acc | (((ch(a, sh) + (ch(b, sh) - ch(a, sh)) * t).round() as u32) << sh))
}

pub fn rgb_hex(c: Color) -> Option<u32> {
    match c {
        Color::Rgb(r, g, b) => Some(u32::from_be_bytes([0, r, g, b])),
        _ => None,
    }
}

pub fn bg_hex() -> Option<u32> {
    Some(BG.load(std::sync::atomic::Ordering::Relaxed)).filter(|&b| b != NO_BG)
}

pub fn hex_rgb(h: u32) -> Color {
    Color::Rgb((h >> 16) as u8, (h >> 8) as u8, h as u8)
}

/// Background for the sidebar and player bar: the terminal's own background nudged a few percent
/// toward the theme's text color. None without truecolor or a known background (no tint then).
pub fn panel(th: &Theme) -> Option<u32> {
    let light = LIGHT.load(std::sync::atomic::Ordering::Relaxed);
    Some(mix(bg_hex()?, rgb_hex(th.text)?, if light { 0.045 } else { 0.06 }))
}

/// OSC 11 reply body, `11;rgb:RRRR/GGGG/BBBB` (1-4 hex digits per channel), as 0xRRGGBB.
pub fn parse_bg_reply(body: &[u8]) -> Option<u32> {
    let rest = std::str::from_utf8(body).ok()?.strip_prefix("11;rgb:")?;
    let mut out = 0;
    for part in rest.split('/') {
        if !(1..=4).contains(&part.len()) {
            return None;
        }
        let max = (1u32 << (4 * part.len())) - 1;
        out = (out << 8) | (u32::from_str_radix(part, 16).ok()? * 255 / max);
    }
    Some(out)
}

/// Current light/dark mode; flipped live when the terminal reports a color scheme change.
pub static LIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Index into families(), cycled with ctrl+t and remembered in the cache dir.
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

pub fn theme() -> &'static Theme {
    use std::sync::atomic::Ordering::Relaxed;
    static T: std::sync::OnceLock<Vec<Theme>> = std::sync::OnceLock::new();
    let t = T.get_or_init(|| {
        let truecolor = std::env::var("COLORTERM").is_ok_and(|v| v.contains("truecolor") || v.contains("24bit"));
        families().iter().flat_map(|(_, dark, light)| [palette(truecolor.then_some(dark), false), palette(truecolor.then_some(light), true)]).collect()
    });
    &t[FAMILY.load(Relaxed) * 2 + LIGHT.load(Relaxed) as usize]
}

pub fn palette(hex: Option<&[u32; 10]>, light: bool) -> Theme {
    let rgb = |h: u32| Color::Rgb((h >> 16) as u8, (h >> 8) as u8, h as u8);
    match (hex, light) {
        (Some(h), _) => Theme {
            accent: rgb(h[0]),
            accent2: rgb(h[1]),
            deep: rgb(h[2]),
            hl: rgb(h[3]),
            text: rgb(h[4]),
            sub: rgb(h[5]),
            dim: rgb(h[6]),
            gold: rgb(h[7]),
            pink: rgb(h[8]),
            err: rgb(h[9]),
        },
        (None, false) => Theme {
            accent: Color::Yellow,
            accent2: Color::Yellow,
            deep: Color::DarkGray,
            hl: Color::DarkGray,
            text: Color::White,
            sub: Color::Gray,
            dim: Color::DarkGray,
            gold: Color::LightYellow,
            pink: Color::Magenta,
            err: Color::Red,
        },
        (None, true) => Theme {
            accent: Color::Yellow,
            accent2: Color::Yellow,
            deep: Color::Gray,
            hl: Color::Gray,
            text: Color::Black,
            sub: Color::DarkGray,
            dim: Color::Gray,
            gold: Color::Red,
            pink: Color::Magenta,
            err: Color::Red,
        },
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
