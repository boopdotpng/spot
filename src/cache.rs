// Everything on disk: where it goes, and the cached lists, the resume point and `spot clean`.
// Track lists are plain text, one tab-separated track per line, so even a big library loads in
// a few ms at startup. Writes go through a temp file so a crash can't leave half a list.

use crate::*;

pub fn cache_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache"));
    base.join("spot")
}

pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("spot")
}

pub fn tracks_path(key: &str) -> PathBuf {
    cache_dir().join("lists").join(key)
}

/// Cache file name for an album / artist uri (`spotify:album:…` → `album_…`).
pub fn uri_key(uri: &str) -> String {
    uri.trim_start_matches("spotify:").replace(':', "_")
}

/// An artist's releases (the albums & singles page), one `uri, single, name, sub` per line.
pub fn releases_path(uri: &str) -> PathBuf {
    cache_dir().join("releases").join(uri_key(uri))
}

pub fn same_uris<'a>(a: impl ExactSizeIterator<Item = &'a String>, b: impl ExactSizeIterator<Item = &'a String>) -> bool {
    a.len() == b.len() && a.zip(b).all(|(x, y)| x == y)
}

pub fn read_links(path: &std::path::Path) -> Vec<Link> {
    let s = fs::read_to_string(path).unwrap_or_default();
    s.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let (uri, single, name, sub) = (f.next()?, f.next()?, f.next()?, f.next()?);
            Some(Link { uri: uri.into(), single: single == "1", name: name.into(), sub: sub.into() })
        })
        .collect()
}

pub fn save_links(path: &std::path::Path, links: &[Link]) {
    use std::fmt::Write;
    let mut s = String::new();
    for l in links {
        let _ = writeln!(s, "{}\t{}\t{}\t{}", l.uri, l.single as u8, clean(&l.name), clean(&l.sub));
    }
    write_atomic(path, &s);
}

pub fn last_path() -> PathBuf {
    cache_dir().join("last")
}

pub fn write_atomic(path: &std::path::Path, data: &str) {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("tmp");
    if fs::write(&tmp, data).is_ok() {
        let _ = fs::rename(&tmp, path);
    }
}

/// Tabs / newlines would break the line format; same byte length, so `lead` stays valid.
pub fn clean(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains(['\t', '\n', '\r']) { s.replace(['\t', '\n', '\r'], " ").into() } else { s.into() }
}

pub fn push_tracks(out: &mut String, tracks: &[Track]) {
    use std::fmt::Write;
    for t in tracks {
        let _ = writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:x}\t{:x}\t{}",
            t.uri,
            clean(&t.name),
            clean(&t.artists),
            t.lead,
            clean(&t.album),
            t.cover.as_deref().unwrap_or(""),
            t.ms,
            t.album_id,
            t.artist_id,
            t.disc
        );
    }
}

pub fn parse_line(l: &str) -> Option<Track> {
    let mut f = l.split('\t');
    let (uri, name, artists) = (f.next()?, f.next()?, f.next()?);
    let lead = f.next()?.parse().ok().filter(|&n| artists.is_char_boundary(n)).unwrap_or(0);
    let (album, cover, ms) = (f.next()?, f.next()?, f.next()?.parse().ok()?);
    let mut id = || f.next().and_then(|x| u128::from_str_radix(x, 16).ok()).unwrap_or(0);
    Some(Track {
        uri: uri.into(),
        name: name.into(),
        artists: artists.into(),
        lead,
        album: album.into(),
        cover: (!cover.is_empty()).then(|| cover.into()),
        ms,
        album_id: id(),
        artist_id: id(),
        disc: f.next().and_then(|x| x.parse().ok()).unwrap_or(0),
    })
}

pub fn read_tracks(path: &std::path::Path) -> Vec<Track> {
    fs::read_to_string(path).map(|s| s.lines().filter_map(parse_line).collect()).unwrap_or_default()
}

pub fn save_tracks(path: &std::path::Path, tracks: &[Track]) {
    let mut s = String::with_capacity(tracks.len() * 200);
    push_tracks(&mut s, tracks);
    write_atomic(path, &s);
}

/// Saved playlist list (Liked Songs first, which is never stored).
pub fn read_playlists() -> Vec<Playlist> {
    let s = fs::read_to_string(cache_dir().join("playlists")).unwrap_or_default();
    let saved = s.lines().filter_map(|l| {
        let (id, name) = l.split_once('\t')?;
        Some(Playlist { id: Some(id.into()), name: name.into() })
    });
    std::iter::once(liked()).chain(saved).collect()
}

pub fn save_playlists(pls: &[Playlist]) {
    use std::fmt::Write;
    let mut s = String::new();
    for p in pls {
        if let Some(id) = &p.id {
            let _ = writeln!(s, "{id}\t{}", clean(&p.name));
        }
    }
    write_atomic(&cache_dir().join("playlists"), &s);
}

/// The queue as it was at quit, with the playing song's position.
pub struct Last {
    pub queue: Queue,
    pub pos_ms: u64,
    pub shuffle: bool,
    pub repeat: Repeat,
}

pub fn read_last() -> Option<Last> {
    let s = fs::read_to_string(last_path()).ok()?;
    let mut lines = s.lines();
    let (key, name) = (lines.next()?.to_string(), lines.next()?.to_string());
    let mut nums = lines.next()?.split(' ').map(|n| n.parse::<u64>().ok());
    let (pos, pos_ms, shuffle, repeat) = (nums.next()??, nums.next()??, nums.next()??, nums.next()??);
    let order: Vec<usize> = lines.next()?.split(',').filter_map(|n| n.parse().ok()).collect();
    let tracks: Vec<Track> = lines.filter_map(parse_line).collect();
    let mut queue = Queue { key, name, tracks: Arc::new(tracks), order, pos: pos as usize };
    let n = queue.tracks.len();
    if n == 0 {
        return None;
    }
    if queue.order.len() != n || queue.order.iter().any(|&i| i >= n) || queue.pos >= n {
        queue.order = (0..n).collect();
        queue.pos = 0;
    }
    let repeat = match repeat {
        1 => Repeat::All,
        2 => Repeat::One,
        _ => Repeat::Off,
    };
    Some(Last { queue, pos_ms, shuffle: shuffle == 1, repeat })
}

pub fn save_last(q: &Queue, pos_ms: u64, shuffle: bool, repeat: Repeat) {
    let order: Vec<String> = q.order.iter().map(|i| i.to_string()).collect();
    let mut s = String::with_capacity(q.tracks.len() * 200);
    s += &format!(
        "{}\n{}\n{} {pos_ms} {} {}\n{}\n",
        clean(&q.key),
        clean(&q.name),
        q.pos,
        shuffle as u8,
        repeat as u8,
        order.join(",")
    );
    push_tracks(&mut s, &q.tracks);
    write_atomic(&last_path(), &s);
}

/// `spot clean`: drop everything that's only a cache. Login, settings, the resume point and
/// unsent scrobbles stay.
pub fn clean_cache() {
    fn size(p: &std::path::Path) -> u64 {
        match fs::symlink_metadata(p) {
            Ok(m) if m.is_dir() => fs::read_dir(p).into_iter().flatten().flatten().map(|e| size(&e.path())).sum(),
            Ok(m) => m.len(),
            Err(_) => 0,
        }
    }
    let dir = cache_dir();
    let mut freed = 0;
    for name in ["audio", "lists", "releases", "playlists"] {
        let p = dir.join(name);
        let n = size(&p);
        let r = if p.is_dir() { fs::remove_dir_all(&p) } else { fs::remove_file(&p) };
        match r {
            Ok(()) => freed += n,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => eprintln!("spot: couldn't remove {}: {e}", p.display()),
        }
    }
    println!("Cleared {:.1} MB of cache from {}.", freed as f64 / 1e6, dir.display());
}

/// The running player's control socket (ctl.rs).
pub fn sock_path() -> PathBuf {
    cache_dir().join("sock")
}
