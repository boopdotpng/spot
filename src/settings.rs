// ~/.config/spot/config.toml, all of it optional. Read once, when the player starts. Anything
// wrong with it is reported and that setting keeps its default: a typo shouldn't stop the music.
// The first run writes it with every setting commented out, so the defaults stay spot's to change.
// The launcher gets these from the player (the socket's `launcher` command), so one parser.

use crate::*;
use toml::{Table, Value};

const TEMPLATE: &str = "\
# spot's settings; uncomment to change one. These are the defaults.
# Restart the player after a change: spot quit, then start it again.
# Every key: https://github.com/boopdotpng/spot/blob/master/docs/config.md

# quality = 320         # 96, 160 or 320 kbps
# audio_cache_gb = 1    # played songs kept on disk; 0 turns it off
# popup = true          # macOS launcher: show what's playing on media keys
# popup_seconds = 3     # how long it stays up

# Your own colors for the TUI: change a built-in theme, or add one to ctrl+t.
# [themes.moss.dark]
# accent = \"#88cc55\"
";

pub struct Settings {
    pub client_id: Option<String>, // your Spotify app's; the first run saves it here
    pub quality: u16, // kbps: 96, 160 or 320
    pub popup: bool,  // the launcher's popup on media keys
    pub popup_seconds: f64,
    pub audio_cache_gb: f64, // played songs kept on disk; 0 = off
    pub themes: Vec<Family>, // added to the built-in ones, or replacing them by name
}

impl Default for Settings {
    fn default() -> Self {
        Settings { client_id: None, quality: 320, popup: true, popup_seconds: 3.0, audio_cache_gb: 1.0, themes: vec![] }
    }
}

/// The settings, and what was wrong with the file.
pub fn load() -> (Settings, Vec<String>) {
    match fs::read_to_string(config_dir().join("config.toml")) {
        Ok(text) => parse(&text),
        Err(_) => Default::default(),
    }
}

fn path() -> PathBuf {
    config_dir().join("config.toml")
}

/// The commented-out file, if there isn't one. Never touches one that's there.
pub fn write_template() {
    let _ = fs::create_dir_all(config_dir());
    let new = fs::OpenOptions::new().write(true).create_new(true).open(path());
    if let Ok(mut f) = new {
        use std::io::Write;
        let _ = f.write_all(TEMPLATE.as_bytes());
    }
}

/// Add the client id, keeping what's in the file. At the top: below a [section] it'd belong to that.
pub fn save_client_id(id: &str) -> Result<PathBuf, String> {
    let text = fs::read_to_string(path()).unwrap_or_else(|_| TEMPLATE.into());
    fs::create_dir_all(config_dir()).map_err(|e| e.to_string())?;
    fs::write(path(), with_client_id(&text, id)).map_err(|e| e.to_string())?;
    Ok(path())
}

fn with_client_id(text: &str, id: &str) -> String {
    format!("client_id = \"{id}\"\n\n{text}")
}

fn parse(text: &str) -> (Settings, Vec<String>) {
    let mut s = Settings::default();
    let mut bad = vec![];
    let table = match text.parse::<Table>() {
        Ok(t) => t,
        Err(e) => return (s, vec![format!("config.toml: {}", e.message())]),
    };
    for (key, v) in &table {
        match (key.as_str(), v) {
            ("client_id", Value::String(id)) if !id.trim().is_empty() => s.client_id = Some(id.trim().into()),
            ("client_id", _) => bad.push("client_id: your Spotify app's client id, in quotes".into()),
            ("quality", Value::Integer(q @ (96 | 160 | 320))) => s.quality = *q as u16,
            ("quality", _) => bad.push("quality: 96, 160 or 320".into()),
            ("popup", Value::Boolean(b)) => s.popup = *b,
            ("popup", _) => bad.push("popup: true or false".into()),
            ("popup_seconds", v) => match v.as_float().or(v.as_integer().map(|i| i as f64)) {
                Some(x) if x > 0.0 && x.is_finite() => s.popup_seconds = x,
                _ => bad.push("popup_seconds: a number above 0".into()),
            },
            ("audio_cache_gb", v) => match v.as_float().or(v.as_integer().map(|i| i as f64)) {
                Some(x) if x >= 0.0 && x.is_finite() => s.audio_cache_gb = x,
                _ => bad.push("audio_cache_gb: a number, 0 or above".into()),
            },
            ("themes", Value::Table(themes)) => {
                for (name, t) in themes {
                    match t {
                        Value::Table(t) => s.themes.push(custom_theme(name, t, &mut bad)),
                        _ => bad.push(format!("themes.{name}: not a table")),
                    }
                }
            }
            _ => bad.push(format!("unknown setting {key}")),
        }
    }
    bad.iter_mut().filter(|b| !b.starts_with("config.toml")).for_each(|b| *b = format!("config.toml: {b}"));
    (s, bad)
}

/// A theme's colors over the built-in of that name, or desert's for a new one. A new theme with
/// no `light` uses its dark colors for both.
fn custom_theme(name: &str, t: &Table, bad: &mut Vec<String>) -> Family {
    let builtin = FAMILIES.iter().find(|f| f.0 == name);
    let base = builtin.unwrap_or(&FAMILIES[0]);
    let (mut dark, mut light) = (base.1, base.2);
    for (key, v) in t {
        match (key.as_str(), v) {
            ("dark", Value::Table(c)) => colors(name, key, c, &mut dark, bad),
            ("light", Value::Table(c)) => colors(name, key, c, &mut light, bad),
            _ => bad.push(format!("themes.{name}: expected [themes.{name}.dark] or .light, not {key}")),
        }
    }
    if builtin.is_none() && !t.contains_key("light") {
        light = dark;
    }
    (name.to_string(), dark, light)
}

fn colors(name: &str, mode: &str, t: &Table, palette: &mut [u32; 10], bad: &mut Vec<String>) {
    for (key, v) in t {
        let Some(i) = COLOR_NAMES.iter().position(|c| c == key) else {
            bad.push(format!("themes.{name}.{mode}: unknown color {key}"));
            continue;
        };
        match v.as_str().and_then(hex) {
            Some(c) => palette[i] = c,
            None => bad.push(format!("themes.{name}.{mode}.{key}: a color like \"#d0924a\"")),
        }
    }
}

fn hex(s: &str) -> Option<u32> {
    let h = s.strip_prefix('#')?;
    if h.len() == 6 { u32::from_str_radix(h, 16).ok() } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_is_the_defaults() {
        let (s, bad) = parse(TEMPLATE);
        assert!(bad.is_empty(), "{bad:?}");
        assert_eq!((s.quality, s.popup), (320, true));
        // uncommented, it says what the defaults really are, and every key is a real one
        let on: String = TEMPLATE.lines().map(|l| format!("{}\n", l.strip_prefix("# ").filter(|l| l.contains(" = ") || l.starts_with('[')).unwrap_or(""))).collect();
        let (s, bad) = parse(&on);
        let d = Settings::default();
        assert!(bad.is_empty(), "{bad:?}");
        assert_eq!((s.quality, s.popup, s.popup_seconds, s.audio_cache_gb), (d.quality, d.popup, d.popup_seconds, d.audio_cache_gb));
        assert_eq!(s.themes.len(), 1);
    }

    #[test]
    fn client_id_lands_above_any_section() {
        let (s, bad) = parse(&with_client_id("popup = false\n[themes.mine.dark]\nerr = \"#ff0000\"\n", "abc123"));
        assert!(bad.is_empty(), "{bad:?}");
        assert_eq!(s.client_id.as_deref(), Some("abc123"));
        assert!(!s.popup);
        assert_eq!(s.themes.len(), 1);
    }

    #[test]
    fn empty_is_the_defaults() {
        let (s, bad) = parse("");
        assert!(bad.is_empty());
        assert_eq!((s.quality, s.popup, s.popup_seconds), (320, true, 3.0));
    }

    #[test]
    fn reads_settings() {
        let (s, bad) = parse("quality = 160\npopup = false\npopup_seconds = 5\n");
        assert!(bad.is_empty(), "{bad:?}");
        assert_eq!((s.quality, s.popup, s.popup_seconds), (160, false, 5.0));
        assert_eq!(parse("popup_seconds = 1.5").0.popup_seconds, 1.5);
        assert_eq!(parse("audio_cache_gb = 0").0.audio_cache_gb, 0.0);
        assert_eq!(parse("audio_cache_gb = 2.5").0.audio_cache_gb, 2.5);
        assert_eq!(parse("audio_cache_gb = -1").1.len(), 1);
    }

    #[test]
    fn bad_values_keep_defaults_and_say_so() {
        let (s, bad) = parse("quality = 128\npopup = 1\npopup_seconds = 0\nnope = 1\n");
        assert_eq!((s.quality, s.popup, s.popup_seconds), (320, true, 3.0));
        assert_eq!(bad.len(), 4, "{bad:?}");
        let (s, bad) = parse("this isn't toml");
        assert_eq!(s.quality, 320);
        assert_eq!(bad.len(), 1);
    }

    #[test]
    fn a_theme_overrides_only_what_it_names() {
        let (s, bad) = parse("[themes.moss.dark]\naccent = \"#112233\"\n");
        assert!(bad.is_empty(), "{bad:?}");
        let moss = FAMILIES.iter().find(|f| f.0 == "moss").unwrap();
        let (name, dark, light) = &s.themes[0];
        assert_eq!(name, "moss");
        assert_eq!(dark[0], 0x112233);
        assert_eq!(dark[1..], moss.1[1..]);
        assert_eq!(*light, moss.2);
    }

    #[test]
    fn a_new_theme_without_light_uses_dark() {
        let (s, _) = parse("[themes.mine.dark]\nerr = \"#ff0000\"\n");
        let (_, dark, light) = &s.themes[0];
        assert_eq!(dark[9], 0xff0000);
        assert_eq!(dark, light);
    }

    #[test]
    fn bad_colors_are_reported() {
        let (s, bad) = parse("[themes.mine.dark]\naccent = \"red\"\nfoo = \"#000000\"\n");
        assert_eq!(bad.len(), 2, "{bad:?}");
        assert_eq!(s.themes[0].1[0], FAMILIES[0].1[0]);
    }

    #[test]
    fn config_themes_join_the_built_in_ones() {
        let (s, _) = parse("[themes.mine.dark]\n[themes.moss.dark]\naccent = \"#112233\"\n");
        let all = merged(s.themes);
        assert_eq!(all.len(), FAMILIES.len() + 1);
        assert_eq!(all.iter().find(|f| f.0 == "moss").unwrap().1[0], 0x112233);
        assert_eq!(all.last().unwrap().0, "mine");
    }
}
