// Numbers and times as spot writes them, in the window and on the command line.

use crate::*;

pub fn mmss(ms: u64) -> String {
    format!("{}:{:02}", ms / 60000, ms / 1000 % 60)
}

/// 10435 -> "10,435"
pub fn num(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn plural(n: usize, what: &str) -> String {
    format!("{} {what}{}", num(n), if n == 1 { "" } else { "s" })
}

/// Total length of a list: "42 min", "3 h 5 min", "712 h".
pub fn hours(ms: u64) -> String {
    let m = (ms + 30_000) / 60_000;
    match (m / 60, m % 60) {
        (0, m) => format!("{m} min"),
        (h, _) if h >= 10 => format!("{} h", num(h as usize)),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// An episode's time column, and whether it's been heard: its length, what's left of it, or "played".
pub fn episode_time(ms: u64, progress: Option<u64>) -> (String, bool) {
    match progress {
        Some(HEARD) => ("played".into(), true),
        Some(at) => (format!("{} left", short(ms.saturating_sub(at))), false),
        None => (short(ms), false),
    }
}

/// A length for a narrow column: "42m", "1h 05m".
pub fn short(ms: u64) -> String {
    let m = (ms + 30_000) / 60_000;
    if m < 60 { format!("{m}m") } else { format!("{}h {:02}m", m / 60, m % 60) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn episodes_show_their_length_whats_left_or_played() {
        const MIN: u64 = 60_000;
        assert_eq!(episode_time(95 * MIN, None), ("1h 35m".to_string(), false));
        assert_eq!(episode_time(95 * MIN, Some(50 * MIN)), ("45m left".to_string(), false));
        assert_eq!(episode_time(179 * MIN, Some(MIN)), ("2h 58m left".to_string(), false));
        assert_eq!(episode_time(95 * MIN, Some(HEARD)), ("played".to_string(), true));
    }

    #[test]
    fn counts_and_lengths() {
        assert_eq!(num(10435), "10,435");
        assert_eq!(plural(1, "song"), "1 song");
        assert_eq!(hours(185 * 60_000), "3 h 5 min");
        assert_eq!(mmss(61_000), "1:01");
    }
}
