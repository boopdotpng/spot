// Everything on disk: where it goes, and the cached lists, the resume point and `spot clean`.
// Track lists are plain text, one tab-separated track per line, so even a big library loads in
// a few ms at startup. Writes go through a temp file so a crash can't leave half a list.

use crate::*;

/// spot's directory under $`var`, else under ~/`home`.
fn xdg_dir(var: &str, home: &str) -> PathBuf {
    let base = std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(home));
    base.join("spot")
}

pub fn cache_dir() -> PathBuf {
    xdg_dir("XDG_CACHE_HOME", ".cache")
}

pub fn config_dir() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config")
}

pub fn tracks_path(key: &str) -> PathBuf {
    cache_dir().join("lists").join(key)
}

/// Cache file name for an album / artist uri (`spotify:album:…` → `album_…`).
pub fn uri_key(uri: &str) -> String {
    uri.trim_start_matches("spotify:").replace(':', "_")
}

/// An artist's releases (the albums & singles page), one `uri, single, name, sub, image` per line.
pub fn releases_path(uri: &str) -> PathBuf {
    cache_dir().join("releases").join(uri_key(uri))
}

/// An artist's overview: the portrait, the bio, then one tagged line per song or card.
pub fn artist_path(uri: &str) -> PathBuf {
    cache_dir().join("artists").join(uri_key(uri))
}

fn link_line(l: &Link) -> String {
    format!("{}\t{}\t{}\t{}\t{}", l.uri, l.single as u8, clean(&l.name), clean(&l.sub), l.image.as_deref().unwrap_or(""))
}

fn parse_link_line(l: &str) -> Option<Link> {
    let mut f = l.split('\t');
    let (uri, single, name, sub) = (f.next()?, f.next()?, f.next()?, f.next()?);
    let image = f.next().filter(|s| !s.is_empty()).map(String::from); // missing from older caches
    Some(Link { uri: uri.into(), single: single == "1", name: name.into(), sub: sub.into(), image })
}

pub fn read_artist(uri: &str) -> Option<ArtistInfo> {
    let s = fs::read_to_string(artist_path(uri)).ok()?;
    let mut lines = s.lines();
    let portrait = lines.next()?;
    let mut a = ArtistInfo {
        portrait: (!portrait.is_empty()).then(|| portrait.into()),
        bio: lines.next()?.replace("\\n", "\n"),
        ..Default::default()
    };
    for l in lines {
        let Some((tag, rest)) = l.split_once('\t') else { continue };
        match tag {
            "T" => a.top.extend(parse_line(rest)),
            "R" => a.releases.extend(parse_link_line(rest)),
            "A" => a.appears.extend(parse_link_line(rest)),
            "F" => a.related.extend(parse_link_line(rest)),
            _ => {}
        }
    }
    Some(a)
}

pub fn save_artist(uri: &str, a: &ArtistInfo) {
    use std::fmt::Write;
    let mut s = format!("{}\n{}\n", a.portrait.as_deref().unwrap_or(""), clean(&a.bio.replace('\n', "\\n")));
    let mut top = String::new();
    push_tracks(&mut top, &a.top);
    for l in top.lines() {
        let _ = writeln!(s, "T\t{l}");
    }
    for (tag, links) in [("R", &a.releases), ("A", &a.appears), ("F", &a.related)] {
        for l in links {
            let _ = writeln!(s, "{tag}\t{}", link_line(l));
        }
    }
    write_atomic(&artist_path(uri), &s);
}

/// The podcasts the user follows, kept like an artist's releases.
pub fn shows_path() -> PathBuf {
    cache_dir().join("shows")
}

/// How long a song plays before it's recently played, and how many of each are kept.
pub const HEARD_MS: u64 = 30_000;
pub const RECENT_LISTS: usize = 12;
pub const RECENT_SONGS: usize = 30;

/// A recently played song or episode (the rest are lists).
pub fn is_song(uri: &str) -> bool {
    uri.starts_with("spotify:track:") || uri.starts_with("spotify:episode:")
}

/// What was played lately, for the launcher (`spot launcher lists`).
pub fn recent_path() -> PathBuf {
    cache_dir().join("recent")
}

/// Where each episode was left (ms, or HEARD), one `uri, ms` per line. Not a cache: `spot clean` keeps it.
fn episodes_path() -> PathBuf {
    cache_dir().join("episodes")
}

/// An episode played to the end.
pub const HEARD: u64 = u64::MAX;

pub fn read_progress() -> HashMap<String, u64> {
    let s = fs::read_to_string(episodes_path()).unwrap_or_default();
    s.lines().filter_map(|l| l.split_once('\t')).filter_map(|(u, ms)| Some((u.to_string(), ms.parse().ok()?))).collect()
}

pub fn save_progress(progress: &HashMap<String, u64>) {
    let s: String = progress.iter().map(|(u, ms)| format!("{u}\t{ms}\n")).collect();
    write_atomic(&episodes_path(), &s);
}

/// Episodes part way through, last played first. Not a cache either.
pub fn continuing_path() -> PathBuf {
    cache_dir().join("continuing")
}

pub fn same_uris<'a>(a: impl ExactSizeIterator<Item = &'a String>, b: impl ExactSizeIterator<Item = &'a String>) -> bool {
    a.len() == b.len() && a.zip(b).all(|(x, y)| x == y)
}

pub fn read_links(path: &std::path::Path) -> Vec<Link> {
    let s = fs::read_to_string(path).unwrap_or_default();
    s.lines().filter_map(parse_link_line).collect()
}

pub fn save_links(path: &std::path::Path, links: &[Link]) {
    use std::fmt::Write;
    let mut s = String::new();
    for l in links {
        let _ = writeln!(s, "{}", link_line(l));
    }
    write_atomic(path, &s);
}

pub fn last_path() -> PathBuf {
    cache_dir().join("last")
}

pub fn write_atomic(path: &std::path::Path, data: &str) {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    // Background refreshes can write the same list concurrently. Each needs its own temp file.
    let tmp = path.with_extension(format!("{}.{:016x}.tmp", std::process::id(), rand::random::<u64>()));
    let Ok(mut file) = fs::OpenOptions::new().write(true).create_new(true).open(&tmp) else { return };
    if file.write_all(data.as_bytes()).and_then(|_| fs::rename(&tmp, path)).is_err() {
        let _ = fs::remove_file(&tmp);
    }
}

/// Tabs / newlines would break the line format; same byte length, so `lead` stays valid.
pub fn clean(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains(['\t', '\n', '\r']) { s.replace(['\t', '\n', '\r'], " ").into() } else { s.into() }
}

pub fn push_tracks<'a>(out: &mut String, tracks: impl IntoIterator<Item = &'a Track>) {
    use std::fmt::Write;
    for t in tracks {
        let _ = writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:x}\t{:x}\t{}\t{}\t{}\t{}\t{}",
            t.uri,
            clean(&t.name),
            clean(&t.artists),
            t.lead,
            clean(&t.album),
            t.cover.as_deref().unwrap_or(""),
            t.ms,
            t.album_id,
            t.artist_id,
            t.disc,
            t.explicit as u8,
            t.added,
            t.number,
            t.more.iter().map(|(len, id)| format!("{len}:{id:x}")).collect::<Vec<_>>().join(",")
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
        explicit: f.next() == Some("1"),
        added: f.next().and_then(|x| x.parse().ok()).unwrap_or(0),
        number: f.next().and_then(|x| x.parse().ok()).unwrap_or(0),
        more: f.next().map_or(Vec::new(), |x| {
            x.split(',').filter_map(|m| {
                let (len, id) = m.split_once(':')?;
                Some((len.parse().ok()?, u128::from_str_radix(id, 16).ok()?))
            }).collect()
        }),
    })
}

pub fn read_tracks(path: &std::path::Path) -> Vec<Track> {
    let s = fs::read_to_string(path).unwrap_or_default();
    // lists cached before the explicit flag or every artist's id were kept load fresh, or they'd
    // never get them (the date added, kept since, is filled in by the next refresh: see
    // liked_tracks, playlist_tracks)
    if s.lines().next().is_some_and(|l| l.split('\t').count() < 14) {
        return vec![];
    }
    s.lines().filter_map(parse_line).collect()
}

/// An album's footer (see `Msg::About`), kept next to its songs.
fn about_path(uri: &str) -> PathBuf {
    tracks_path(&format!("{}.about", uri_key(uri)))
}

pub fn read_about(uri: &str) -> Vec<String> {
    fs::read_to_string(about_path(uri)).unwrap_or_default().lines().map(String::from).collect()
}

pub fn save_about(uri: &str, about: &[String]) {
    write_atomic(&about_path(uri), &about.iter().map(|l| format!("{}\n", clean(l))).collect::<String>());
}

/// An album's own artists (uri, name), as its metadata credits them, kept next to its songs:
/// a song on it can credit more (a feature) or fewer.
fn album_by_path(uri: &str) -> PathBuf {
    tracks_path(&format!("{}.by", uri_key(uri)))
}

pub fn read_album_by(uri: &str) -> Vec<(String, String)> {
    let s = fs::read_to_string(album_by_path(uri)).unwrap_or_default();
    s.lines().filter_map(|l| l.split_once('\t')).map(|(u, n)| (u.to_string(), n.to_string())).collect()
}

pub fn save_album_by(uri: &str, artists: &[(String, String)]) {
    write_atomic(&album_by_path(uri), &artists.iter().map(|(u, n)| format!("{u}\t{}\n", clean(n))).collect::<String>());
}

pub fn save_tracks(path: &std::path::Path, tracks: &[Track]) {
    let mut s = String::with_capacity(tracks.len() * 200);
    push_tracks(&mut s, tracks);
    write_atomic(path, &s);
}

/// Saved playlist list (Liked Songs first, which is never stored).
pub fn read_playlists() -> Vec<Playlist> {
    let s = fs::read_to_string(cache_dir().join("playlists")).unwrap_or_default();
    let saved = s.lines().enumerate().filter_map(|(rank, l)| {
        let mut f = l.split('\t');
        let (id, name) = (f.next()?, f.next()?);
        let image = f.next().filter(|u| !u.is_empty()).map(String::from);
        Some(Playlist { id: Some(id.into()), name: name.into(), image, rank: rank + 1 })
    });
    let mut all: Vec<Playlist> = std::iter::once(liked()).chain(saved).collect();
    pin_order(&mut all, &read_pinned());
    all
}

/// The playlists pinned to the top of the sidebar, by id, in the order they were pinned.
pub fn read_pinned() -> Vec<String> {
    let s = fs::read_to_string(cache_dir().join("pinned")).unwrap_or_default();
    s.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()
}

pub fn save_pinned(ids: &[String]) {
    write_atomic(&cache_dir().join("pinned"), &ids.iter().map(|i| format!("{i}\n")).collect::<String>());
}

/// Liked Songs, then the pinned playlists in the order they were pinned, then the rest as Spotify
/// lists them.
pub fn pin_order(pls: &mut [Playlist], pinned: &[String]) {
    pls.sort_by_key(|p| match &p.id {
        None => (0, 0),
        Some(id) => pinned.iter().position(|x| x == id).map_or((2, p.rank), |i| (1, i)),
    });
}

pub fn save_playlists(pls: &[Playlist]) {
    use std::fmt::Write;
    let mut s = String::new();
    for p in pls {
        if let Some(id) = &p.id {
            let _ = writeln!(s, "{id}\t{}\t{}", clean(&p.name), p.image.as_deref().unwrap_or(""));
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
    let mut queue = Queue { key, name, tracks: Arc::new(tracks), order, pos: pos as usize, sorted: vec![] };
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
    push_tracks(&mut s, q.tracks.iter());
    write_atomic(&last_path(), &s);
}

/// What the cache keeps that can go (songs, lists and covers; never the login, the resume point
/// or unsent scrobbles).
const CLEARABLE: [&str; 7] = ["audio", "lists", "releases", "artists", "playlists", "shows", "art"];

/// Bytes a file or directory takes on disk.
pub fn disk_size(p: &std::path::Path) -> u64 {
    match fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => fs::read_dir(p).into_iter().flatten().flatten().map(|e| disk_size(&e.path())).sum(),
        Ok(m) => m.len(),
        Err(_) => 0,
    }
}

/// Bytes the clearable cache takes on disk.
fn cache_size() -> u64 {
    CLEARABLE.iter().map(|n| disk_size(&cache_dir().join(n))).sum()
}

/// Bytes the covers in `art` may take: a share of the whole cache's limit, set at startup.
static ART_BUDGET: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(u64::MAX);

pub fn set_art_budget(bytes: u64) {
    ART_BUDGET.store(bytes, std::sync::atomic::Ordering::Relaxed);
}

/// Drop covers until they fit their budget, least recently shown first (showing one touches
/// its file's modified time).
pub fn prune_art() {
    let budget = ART_BUDGET.load(std::sync::atomic::Ordering::Relaxed);
    let mut files: Vec<_> = fs::read_dir(cache_dir().join("art"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((m.modified().ok()?, m.len(), e.path()))
        })
        .collect();
    let mut used: u64 = files.iter().map(|f| f.1).sum();
    if used <= budget {
        return;
    }
    files.sort_by_key(|f| f.0);
    for (_, len, path) in files {
        if used <= budget {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            used -= len;
        }
    }
}

/// `spot clean`: drop everything that's only a cache, with the player stopped so nothing holds
/// on to what's gone.
pub fn clean_cache() -> i32 {
    if std::os::unix::net::UnixStream::connect(sock_path()).is_ok() {
        eprintln!("spot: the player is running; `spot quit` first");
        return 1;
    }
    let size = cache_size();
    for name in CLEARABLE {
        let p = cache_dir().join(name);
        let r = if p.is_dir() { fs::remove_dir_all(&p) } else { fs::remove_file(&p) };
        if let Err(e) = r
            && e.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("spot: couldn't remove {}: {e}", p.display());
        }
    }
    println!("Cleared {:.1} MB of cache from {}.", (size - cache_size()) as f64 / 1e6, cache_dir().display());
    0
}

/// The running player's control socket (ctl.rs).
pub fn sock_path() -> PathBuf {
    cache_dir().join("sock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_artist_survives_the_cache_and_names_with_commas_stay_whole() {
        let line = "spotify:track:x\tt\tTyler, the Creator, Kali Uchis, Bob\t18\tal\t\t1000\t1\ta\t1\t0\t0\t3\t10:b,3:c";
        let t = parse_line(line).unwrap();
        assert_eq!(t.credits(), vec![("Tyler, the Creator", 0xa), ("Kali Uchis", 0xb), ("Bob", 0xc)]);
        let mut out = String::new();
        push_tracks(&mut out, [&t]);
        assert_eq!(out.trim_end(), line);
        // cached before every artist was kept: only the first
        let old = parse_line("spotify:track:x\tt\tA, B\t1\tal\t\t1000\t1\ta\t1\t0\t0\t3").unwrap();
        assert_eq!(old.credits(), vec![("A", 0xa)]);
    }

    #[test]
    fn concurrent_cache_writes_publish_complete_files() {
        let dir = std::env::temp_dir().join(format!("spot-cache-test-{:016x}", rand::random::<u64>()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("list");
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            for i in 0..8 {
                let (path, barrier) = (&path, &barrier);
                scope.spawn(move || {
                    let data = i.to_string().repeat(64 * 1024);
                    barrier.wait();
                    for _ in 0..10 {
                        write_atomic(path, &data);
                        let saved = fs::read(path).unwrap();
                        assert_eq!(saved.len(), data.len());
                        assert!(saved.iter().all(|&b| b == saved[0]));
                    }
                });
            }
        });
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
    }
}
