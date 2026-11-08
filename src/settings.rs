// ~/.config/spot/config.toml, all of it optional. Read once, when the player starts. Anything
// wrong with it is reported and that setting keeps its default: a typo shouldn't stop the music.
// The first run writes it with every setting commented out, so the defaults stay spot's to change.

use crate::*;
use toml::{Table, Value};

const TEMPLATE: &str = "\
# spot's settings; uncomment to change one. These are the defaults.
# Restart the player after a change: spot quit, then start it again.
# Every key: https://github.com/boopdotpng/spot/blob/master/docs/config.md

# quality = 320         # 96, 160 or 320 kbps
# audio_cache_gb = 1    # played songs kept on disk; 0 turns it off
# normalize = true      # play every song at about the same loudness
# hotkey = \"option+space\"  # what opens the quick search, from any app
# appearance = \"system\"  # \"system\", \"light\" or \"dark\"
";

pub struct Settings {
    pub client_id: Option<String>, // your Spotify app's; the first run saves it here
    pub quality: u16, // kbps: 96, 160 or 320
    pub audio_cache_gb: f64, // played songs kept on disk; 0 = off
    pub normalize: bool, // Spotify's loudness data evens out songs
    pub hotkey: String, // the launcher's, as hotkey() writes it
    pub appearance: Appearance, // light, dark, or the system's
}

/// Light or dark: the system's (and following it when it changes), or one of them always.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub const ALL: [Appearance; 3] = [Appearance::System, Appearance::Light, Appearance::Dark];

    /// As config.toml writes it.
    pub fn name(self) -> &'static str {
        match self {
            Appearance::System => "system",
            Appearance::Light => "light",
            Appearance::Dark => "dark",
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings { client_id: None, quality: 320, audio_cache_gb: 1.0, normalize: true, hotkey: "option+space".into(), appearance: Appearance::System }
    }
}

/// The settings, and what was wrong with the file.
pub fn load() -> (Settings, Vec<String>) {
    match fs::read_to_string(path()) {
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

/// Save one setting from the settings page, keeping the rest of the file: `value` is TOML (a
/// string in quotes), or None to drop the line and go back to the default.
pub fn set(key: &str, value: Option<&str>) -> Result<(), String> {
    let text = fs::read_to_string(path()).unwrap_or_else(|_| TEMPLATE.into());
    fs::create_dir_all(config_dir()).map_err(|e| e.to_string())?;
    fs::write(path(), with_key(&text, key, value)).map_err(|e| e.to_string())
}

pub fn quoted(s: &str) -> String {
    Value::String(s.into()).to_string()
}

/// The file with `key` set: its line replaced (or its commented-out one from the template), else
/// a new one at the top, above any [section] it would otherwise belong to.
fn with_key(text: &str, key: &str, value: Option<&str>) -> String {
    let names = |l: &str| l.strip_prefix(key).is_some_and(|r| r.trim_start().starts_with('='));
    let mut section = false;
    let mut lines: Vec<(bool, bool, &str)> = vec![]; // (top level, commented out, line)
    for l in text.lines() {
        let t = l.trim_start();
        section |= t.starts_with('[');
        let off = t.strip_prefix('#').map(str::trim_start);
        lines.push((!section && names(off.unwrap_or(t)), off.is_some(), l));
    }
    let set = |out: &mut Vec<String>| value.map(|v| out.push(format!("{key} = {v}")));
    let active = lines.iter().any(|(k, off, _)| *k && !off);
    let mut done = value.is_none();
    let mut out = vec![];
    for (k, off, l) in lines {
        // the live line, or else the template's commented-out one, takes the value; others stay
        if k && (!off || !active) && !done {
            set(&mut out);
            done = true;
        } else if !(k && !off) {
            out.push(l.to_string());
        }
    }
    if !done {
        let mut top = vec![];
        set(&mut top);
        out.splice(0..0, top);
    }
    out.join("\n") + "\n"
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
            ("audio_cache_gb", v) => match v.as_float().or(v.as_integer().map(|i| i as f64)) {
                Some(x) if x >= 0.0 && x.is_finite() => s.audio_cache_gb = x,
                _ => bad.push("audio_cache_gb: a number, 0 or above".into()),
            },
            ("normalize", Value::Boolean(b)) => s.normalize = *b,
            ("normalize", _) => bad.push("normalize: true or false".into()),
            ("hotkey", v) => match v.as_str().and_then(hotkey) {
                Some(h) => s.hotkey = h,
                None => bad.push("hotkey: modifiers and a key, like \"ctrl+option+space\"".into()),
            },
            ("appearance", v) => match v.as_str().and_then(|a| Appearance::ALL.into_iter().find(|x| x.name() == a)) {
                Some(a) => s.appearance = a,
                None => bad.push("appearance: \"system\", \"light\" or \"dark\"".into()),
            },
            // themes are gone (the accent comes from the playing cover), and so is the now
            // playing popup; old ones are let be
            ("themes" | "popup" | "popup_seconds", _) => {}
            _ => bad.push(format!("unknown setting {key}")),
        }
    }
    bad.iter_mut().filter(|b| !b.starts_with("config.toml")).for_each(|b| *b = format!("config.toml: {b}"));
    (s, bad)
}

/// A hotkey spot can register, written one way: its modifiers in a fixed order, then the
/// key (a letter, digit, space, return, tab or f1-f12). Only f-keys work without a modifier.
pub fn hotkey(s: &str) -> Option<String> {
    let s = s.to_lowercase();
    let mut parts: Vec<&str> = s.split('+').map(str::trim).collect();
    let key = parts.pop()?;
    let f_key = key.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()).is_some_and(|n| (1..=12).contains(&n));
    let single = key.len() == 1 && key.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    if !(f_key || single || matches!(key, "space" | "return" | "tab")) {
        return None;
    }
    let mut mods = [false; 4]; // ctrl, option, cmd, shift
    for m in parts {
        let i = match m {
            "ctrl" | "control" => 0,
            "option" | "opt" | "alt" => 1,
            "cmd" | "command" => 2,
            "shift" => 3,
            _ => return None,
        };
        mods[i] = true;
    }
    if !f_key && !mods.contains(&true) {
        return None;
    }
    let names = ["ctrl", "option", "cmd", "shift"].iter().zip(mods).filter(|(_, on)| *on).map(|(n, _)| *n);
    Some(names.chain([key]).collect::<Vec<_>>().join("+"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_is_the_defaults() {
        let (s, bad) = parse(TEMPLATE);
        assert!(bad.is_empty(), "{bad:?}");
        assert_eq!(s.quality, 320);
        // uncommented, it says what the defaults really are, and every key is a real one
        let on: String = TEMPLATE.lines().map(|l| format!("{}\n", l.strip_prefix("# ").filter(|l| l.contains(" = ") || l.starts_with('[')).unwrap_or(""))).collect();
        let (s, bad) = parse(&on);
        let d = Settings::default();
        assert!(bad.is_empty(), "{bad:?}");
        assert_eq!((s.quality, s.audio_cache_gb), (d.quality, d.audio_cache_gb));
        assert_eq!((s.normalize, &s.hotkey, s.appearance), (d.normalize, &d.hotkey, d.appearance));
    }

    #[test]
    fn client_id_lands_above_any_section() {
        let (s, bad) = parse(&with_client_id("quality = 160\n[themes.mine.dark]\nerr = \"#ff0000\"\n", "abc123"));
        assert!(bad.is_empty(), "{bad:?}");
        assert_eq!(s.client_id.as_deref(), Some("abc123"));
        assert_eq!(s.quality, 160);
    }

    #[test]
    fn settings_page_edits_keep_the_rest() {
        let t = with_key(TEMPLATE, "quality", Some("160"));
        assert!(t.contains("\nquality = 160\n") && !t.contains("# quality"), "{t}");
        assert_eq!(parse(&t).0.quality, 160);
        let t = with_key(&t, "quality", Some("96"));
        assert_eq!(t.matches("quality =").count(), 1);
        let t = with_key(&t, "audio_cache_gb", Some("2"));
        assert_eq!(parse(&t).0.audio_cache_gb, 2.0);
        let t = with_key(&t, "quality", None);
        assert_eq!(parse(&t).0.quality, 320);
        // no line for it at all: it goes on top, above any section
        let t = with_key("[themes.mine.dark]\naccent = \"#112233\"\n", "normalize", Some("false"));
        let (s, bad) = parse(&t);
        assert!(bad.is_empty(), "{bad:?}");
        assert!(!s.normalize);
    }

    #[test]
    fn empty_is_the_defaults() {
        let (s, bad) = parse("");
        assert!(bad.is_empty());
        assert_eq!((s.quality, s.audio_cache_gb), (320, 1.0));
    }

    #[test]
    fn reads_settings() {
        let (s, bad) = parse("quality = 160\nnormalize = false\n");
        assert!(bad.is_empty(), "{bad:?}");
        assert_eq!((s.quality, s.normalize), (160, false));
        assert_eq!(parse("audio_cache_gb = 0").0.audio_cache_gb, 0.0);
        assert_eq!(parse("audio_cache_gb = 2.5").0.audio_cache_gb, 2.5);
        assert_eq!(parse("audio_cache_gb = -1").1.len(), 1);
    }

    #[test]
    fn bad_values_keep_defaults_and_say_so() {
        let (s, bad) = parse("quality = 128\nnormalize = 1\naudio_cache_gb = -1\nnope = 1\n");
        assert_eq!((s.quality, s.normalize, s.audio_cache_gb), (320, true, 1.0));
        assert_eq!(bad.len(), 4, "{bad:?}");
        let (s, bad) = parse("this isn't toml");
        assert_eq!(s.quality, 320);
        assert_eq!(bad.len(), 1);
    }

    #[test]
    fn normalize_and_hotkey() {
        let d = Settings::default();
        assert!(d.normalize);
        assert!(!parse("normalize = false").0.normalize);
        assert_eq!(parse("normalize = 1").1.len(), 1);
        assert_eq!(parse("hotkey = \"Space + ALT + Cmd\"").1.len(), 1); // the key goes last
        assert_eq!(parse("hotkey = \"Cmd + ALT + Space\"").0.hotkey, "option+cmd+space");
        assert_eq!(parse("hotkey = \"control+shift+m\"").0.hotkey, "ctrl+shift+m");
        assert_eq!(parse("hotkey = \"f13\"").1.len(), 1);
        assert_eq!(parse("hotkey = \"f5\"").0.hotkey, "f5");
        for bad in ["\"space\"", "\"m\"", "\"hyper+space\"", "\"option+\"", "\"option+pgup\"", "3"] {
            let (s, errs) = parse(&format!("hotkey = {bad}"));
            assert_eq!((s.hotkey.as_str(), errs.len()), (d.hotkey.as_str(), 1), "{bad}");
        }
    }

    #[test]
    fn appearance_is_one_of_three() {
        let (s, bad) = parse("appearance = \"dark\"\n");
        assert!(bad.is_empty(), "{bad:?}");
        assert_eq!(s.appearance, Appearance::Dark);
        let (s, bad) = parse("appearance = \"dim\"\n");
        assert_eq!((s.appearance, bad.len()), (Appearance::System, 1));
    }

    #[test]
    fn old_themes_are_let_be() {
        let (_, bad) = parse("[themes.moss.dark]\naccent = \"#112233\"\n");
        assert!(bad.is_empty(), "{bad:?}");
        let (_, bad) = parse("popup = \"off\"\npopup_seconds = 5\n"); // the popup's gone too
        assert!(bad.is_empty(), "{bad:?}");
    }
}
