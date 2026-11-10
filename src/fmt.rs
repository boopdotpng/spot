// Numbers and times as spot writes them, in the TUI and on the command line.

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

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's days_from_civil).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// The date of a day since 1970-01-01, as (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { yoe + era * 400 + 1 } else { yoe + era * 400 }, m, d)
}

/// "2024-03-04T05:06:07Z" (the web API's added_at) as unix seconds; 0 if it isn't one.
pub fn iso_secs(s: &str) -> u64 {
    let n = |r: std::ops::Range<usize>| s.get(r).and_then(|x| x.parse::<i64>().ok());
    let (Some(y), Some(mo), Some(d)) = (n(0..4), n(5..7), n(8..10)) else { return 0 };
    let (h, mi, se) = (n(11..13).unwrap_or(0), n(14..16).unwrap_or(0), n(17..19).unwrap_or(0));
    let t = days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + se;
    t.max(0) as u64
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// When a song was added, as a list shows it: "today", "3 days ago", then "Mar 4, 2024".
pub fn added(secs: u64, now: u64) -> String {
    if secs == 0 {
        return String::new();
    }
    let days = (now / 86400) as i64 - (secs / 86400) as i64;
    match days {
        ..=0 => "today".into(),
        1 => "yesterday".into(),
        2..=6 => format!("{days} days ago"),
        _ => {
            const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
            let (y, m, d) = civil_from_days((secs / 86400) as i64);
            format!("{} {d}, {y}", MONTHS[m as usize - 1])
        }
    }
}

/// Words as a search compares them: lowercase letters and digits, single spaces.
pub fn plain(s: &str) -> String {
    let s: String = s.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect();
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// How well `name` matches the words typed: 3 exactly, 2 starting with them, 1 containing them.
pub fn match_score(q: &str, name: &str) -> u8 {
    let (q, name) = (plain(q), plain(name));
    if q.is_empty() {
        0
    } else if name == q {
        3
    } else if name.starts_with(&q) || name.strip_prefix("the ").is_some_and(|n| n.starts_with(&q)) {
        2
    } else if name.contains(&q) {
        1
    } else {
        0
    }
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

    #[test]
    fn dates() {
        assert_eq!(iso_secs("1970-01-01T00:00:00Z"), 0);
        assert_eq!(iso_secs("2024-03-04T05:06:07Z"), 1709528767);
        assert_eq!(iso_secs("nope"), 0);
        let t = iso_secs("2024-03-04T05:06:07Z");
        assert_eq!(added(t, t + 3600), "today");
        assert_eq!(added(t, t + 86400), "yesterday");
        assert_eq!(added(t, t + 3 * 86400), "3 days ago");
        assert_eq!(added(t, t + 30 * 86400), "Mar 4, 2024");
        assert_eq!(added(iso_secs("2000-02-29T00:00:00Z"), 2_000_000_000), "Feb 29, 2000");
        assert_eq!(added(0, 5), "");
    }
}
