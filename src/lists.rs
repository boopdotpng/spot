// Track lists: Liked Songs, playlists, albums, artists (every song) and shows (every episode),
// fetched fresh and compared with the cached copy; an artist's releases; and the songs a uri
// names, for playing it (resolve).

use crate::*;

/// A list's cache file name: `liked`, a playlist id, or `album_…` / `artist_…`.
pub fn list_key(uri: &str) -> String {
    uri.strip_prefix("spotify:playlist:").map_or_else(|| uri_key(uri), String::from)
}

/// The songs of `uri` (`liked`, or a playlist, album, artist or show uri) fresh from Spotify, or None
/// when `cached` is still current; saved to the cache when they changed. With nothing cached,
/// they also stream to `view` as they load.
pub async fn fetch_list(api: Arc<Api>, sess: Sess, uri: &str, cached: &[Track], tx: &mpsc::Sender<Msg>, view: u64) -> Result<Option<Vec<Track>>, String> {
    let fresh = if uri == "liked" {
        liked_tracks(api, cached, tx, view).await?
    } else if let Some(id) = uri.strip_prefix("spotify:playlist:") {
        playlist_tracks(sess, id, cached, tx, view).await?
    } else if is_show(uri) {
        show_episodes(&api, uri, cached, tx, view).await?
    } else {
        let session = ready(sess).await?;
        let artist = uri.starts_with("spotify:artist:");
        let uris = if artist {
            album_track_uris(&session, &artist_albums(&session, uri).await?).await?
        } else {
            let found = batch_meta::<PbAlbum>(&session, &[uri.to_string()], ExtensionKind::ALBUM_V4).await?;
            let a = found.get(uri).ok_or("album not found")?;
            let about = about(a);
            if about != read_about(uri) {
                save_about(uri, &about);
            }
            let artists: Vec<(String, String)> = a.artist.iter().filter_map(|x| Some((gid_uri("artist", x.gid())?, x.name().to_string()))).collect();
            if !artists.is_empty() && artists != read_album_by(uri) {
                save_album_by(uri, &artists);
            }
            let _ = tx.send(Msg::About { view, about, artists });
            a.disc.iter().flat_map(|d| d.track.iter()).filter_map(|t| gid_uri("track", t.gid())).collect()
        };
        let tracks = send_tracks(&session, &uris, artist, cached.is_empty(), tx, view).await?;
        // disc and track numbers too: caches from before they were kept have none
        let same = same_uris(tracks.iter().map(|t| &t.uri), cached.iter().map(|t| &t.uri))
            && tracks.iter().zip(cached).all(|(a, b)| (a.disc, a.number) == (b.disc, b.number));
        (!same).then_some(tracks)
    };
    if let Some(t) = &fresh {
        save_tracks(&tracks_path(&list_key(uri)), t);
    }
    Ok(fresh)
}

/// Refresh the list open in `view`. Shown from the cache, it's replaced only if it changed.
pub async fn load_list(api: Arc<Api>, sess: Sess, tx: mpsc::Sender<Msg>, view: u64, uri: String, cached: Arc<Vec<Track>>) {
    match fetch_list(api, sess, &uri, &cached, &tx, view).await {
        Ok(Some(tracks)) if !cached.is_empty() => {
            let _ = tx.send(Msg::Replace { view, tracks: Arc::new(tracks) });
        }
        Ok(_) => {}
        Err(e) => {
            let _ = tx.send(Msg::Error(e));
        }
    }
    let _ = tx.send(Msg::Tracks { view, tracks: vec![], done: true });
}

/// Albums as openable links, newest first: their covers, and "2024 album · 12 songs" (or, `by`,
/// whose they are, for releases an artist only appears on).
/// "album", "single", "EP" or "compilation".
fn album_kind(a: &PbAlbum) -> &'static str {
    use librespot_protocol::metadata::album::Type;
    match a.type_() {
        Type::SINGLE => "single",
        Type::EP => "EP",
        Type::COMPILATION => "compilation",
        _ => "album",
    }
}

async fn release_links(session: &Session, albums: &[String], by: bool) -> Result<Vec<Link>, String> {
    use librespot_protocol::metadata::album::Type;
    let found = batch_meta::<PbAlbum>(session, albums, ExtensionKind::ALBUM_V4).await?;
    let mut links: Vec<_> = albums
        .iter()
        .filter_map(|u| {
            let a = found.get(u)?;
            let kind = album_kind(a);
            let year = if a.date.year() > 0 { format!("{} ", a.date.year()) } else { String::new() };
            let sub = if by {
                let names: Vec<&str> = a.artist.iter().map(|x| x.name()).collect();
                format!("{year}· {}", names.join(", "))
            } else {
                let n: usize = a.disc.iter().map(|d| d.track.len()).sum();
                format!("{year}{kind} · {n} song{}", if n == 1 { "" } else { "s" })
            };
            let d = &a.date;
            let single = a.type_() == Type::SINGLE;
            let image = image_url(&a.cover_group.image);
            Some(((d.year(), d.month(), d.day()), Link { uri: u.clone(), name: a.name().into(), sub, single, image }))
        })
        .collect();
    // Newest release first, mixing albums, EPs and singles.
    links.sort_by_key(|(date, _)| std::cmp::Reverse(*date));
    Ok(links.into_iter().map(|(_, l)| l).collect())
}

/// An artist's releases as openable album links.
pub async fn artist_releases(sess: Sess, tx: mpsc::Sender<Msg>, view: u64, uri: String, cached: Vec<Link>) {
    let res = async {
        let session = ready(sess).await?;
        let albums = artist_albums(&session, &uri).await?;
        let links = release_links(&session, &albums, false).await?;
        save_links(&releases_path(&uri), &links);
        if cached.is_empty() {
            let _ = tx.send(Msg::Links { view, links, done: true });
        } else {
            if changed(&links, &cached) {
                let _ = tx.send(Msg::ReplaceLinks { view, links });
            }
            let _ = tx.send(Msg::Links { view, links: vec![], done: true });
        }
        Ok::<_, String>(())
    }
    .await;
    if let Err(e) = res {
        let _ = tx.send(Msg::Error(e));
        let _ = tx.send(Msg::Links { view, links: vec![], done: true });
    }
}

/// A search's top result's artist: their releases for its "more by" line (and the cache their
/// page reads). Quiet when it fails: the line just isn't there.
pub async fn more_by(sess: Sess, tx: mpsc::Sender<Msg>, view: u64, uri: String) {
    let res = async {
        let session = ready(sess).await?;
        let albums = artist_albums(&session, &uri).await?;
        let links = release_links(&session, &albums, false).await?;
        save_links(&releases_path(&uri), &links);
        let _ = tx.send(Msg::More { view, links });
        Ok::<_, String>(())
    }
    .await;
    if let Err(e) = res {
        log::warn!("more by {uri}: {e}");
    }
}

fn changed(a: &[Link], b: &[Link]) -> bool {
    !same_uris(a.iter().map(|l| &l.uri), b.iter().map(|l| &l.uri)) || a.iter().zip(b).any(|(a, b)| a.name != b.name || a.sub != b.sub || a.image != b.image)
}

/// The first (canonical) album of each group: each lists regional/edition variants of one release.
fn group_uris(groups: &[librespot_protocol::metadata::AlbumGroup]) -> impl Iterator<Item = String> + '_ {
    groups.iter().filter_map(|g| g.album.first()).filter_map(|al| gid_uri("album", al.gid()))
}

/// Spotify's bios are a little HTML (links to other artists, line breaks): plain text.
fn plain_bio(html: &str) -> String {
    let s = html.replace("<br/>", "\n").replace("<br>", "\n").replace("<br />", "\n");
    let mut out = String::with_capacity(s.len());
    let mut tag = false;
    for c in s.chars() {
        match c {
            '<' => tag = true,
            '>' if tag => tag = false,
            _ if !tag => out.push(c),
            _ => {}
        }
    }
    let out = out.replace("&amp;", "&").replace("&quot;", "\"").replace("&#39;", "'").replace("&lt;", "<").replace("&gt;", ">");
    out.lines().map(str::trim).collect::<Vec<_>>().join("\n").trim().to_string()
}

/// How many of the releases an artist appears on the overview looks up.
const APPEARS: usize = 12;

/// An artist's overview, fresh: sent to `view` (and saved) if it differs from what it showed.
pub async fn artist_overview(sess: Sess, tx: mpsc::Sender<Msg>, view: u64, uri: String, cached: Option<Arc<ArtistInfo>>) {
    let res = async {
        let session = ready(sess).await?;
        let a = batch_meta::<PbArtist>(&session, std::slice::from_ref(&uri), ExtensionKind::ARTIST_V4)
            .await?
            .remove(&uri)
            .ok_or("artist not found")?;
        // popular where the account is, else wherever Spotify lists first
        let country = session.country();
        let top = a.top_track.iter().find(|t| t.country() == country).or_else(|| a.top_track.first());
        let top_uris: Vec<String> = top.into_iter().flat_map(|t| t.track.iter()).filter_map(|t| gid_uri("track", t.gid())).take(10).collect();
        let found = batch_meta::<PbTrack>(&session, &top_uris, ExtensionKind::TRACK_V4).await?;
        let top: Vec<Track> = tracks_in_order(&top_uris, &found).collect();
        let albums: Vec<String> = group_uris(&a.album_group).chain(group_uris(&a.single_group)).chain(group_uris(&a.compilation_group)).collect();
        let releases = release_links(&session, &albums, false).await?;
        let appears_uris: Vec<String> = group_uris(&a.appears_on_group).take(APPEARS).collect();
        let appears = release_links(&session, &appears_uris, true).await?;
        let related = a
            .related
            .iter()
            .filter_map(|r| {
                let image = image_url(&r.portrait_group.image).or_else(|| image_url(&r.portrait));
                Some(Link { uri: gid_uri("artist", r.gid())?, name: r.name().into(), sub: "Artist".into(), single: false, image })
            })
            .collect();
        let portrait = image_url(&a.portrait_group.image).or_else(|| image_url(&a.portrait));
        let bio = a.biography.first().map(|b| plain_bio(b.text())).unwrap_or_default();
        let info = ArtistInfo { portrait, top, releases, appears, related, bio, ..Default::default() };
        // the discography page reads the same releases
        save_links(&releases_path(&uri), &info.releases);
        save_artist(&uri, &info);
        let same = cached.as_deref().is_some_and(|c| {
            c.portrait == info.portrait
                && c.bio == info.bio
                && same_uris(c.top.iter().map(|t| &t.uri), info.top.iter().map(|t| &t.uri))
                && !changed(&c.releases, &info.releases)
                && !changed(&c.appears, &info.appears)
                && !changed(&c.related, &info.related)
        });
        if !same {
            let _ = tx.send(Msg::Artist { view, info: Arc::new(info) });
        }
        Ok::<_, String>(())
    }
    .await;
    if let Err(e) = res {
        let _ = tx.send(Msg::Error(e));
    }
    let _ = tx.send(Msg::Links { view, links: vec![], done: true });
}

pub async fn liked_page(api: Arc<Api>, offset: usize) -> Result<(Vec<Track>, usize), String> {
    let v = api.get(&format!("{API}/me/tracks?limit=50&offset={offset}")).await?;
    let page = v["items"].as_array().into_iter().flatten().filter_map(|it| {
        let mut t = parse_track(&it["track"])?;
        t.added = it["added_at"].as_str().map_or(0, iso_secs);
        Some(t)
    }).collect();
    Ok((page, v["total"].as_u64().unwrap_or(0) as usize))
}

/// The full Liked Songs list, or None if `cached` is still current.
pub async fn liked_tracks(api: Arc<Api>, cached: &[Track], tx: &mpsc::Sender<Msg>, view: u64) -> Result<Option<Vec<Track>>, String> {
    let (mut got, total) = liked_page(api.clone(), 0).await?;
    // cached before dates added were kept: fetched in full once, to fill them in
    let undated = cached.iter().any(|t| t.added == 0);
    if cached.is_empty() {
        let _ = tx.send(Msg::Tracks { view, tracks: got.clone(), done: false });
    } else if !undated
        && let Some(j) = (cached.len() + got.len()).checked_sub(total)
        && j > 0
        && j <= cached.len()
        && got.last().is_some_and(|t| t.uri == cached[j - 1].uri)
    {
        // Likes come newest first: new ones push the cache down, unlikes pull what's after them
        // up. If the first page's last song sits where the cache has it, shifted by exactly the
        // change in count, everything after it is unchanged.
        if j == got.len() && got.iter().zip(cached).all(|(a, b)| a.uri == b.uri) {
            return Ok(None);
        }
        got.extend_from_slice(&cached[j..]);
        return Ok(Some(got));
    }
    // Each page takes about a second, so fetch the rest a few at a time, keeping their order.
    const PARALLEL: usize = 16;
    let mut offsets = (got.len().max(50)..total).step_by(50);
    let mut pending = std::collections::VecDeque::new();
    loop {
        while pending.len() < PARALLEL
            && let Some(o) = offsets.next()
        {
            pending.push_back(tokio::spawn(liked_page(api.clone(), o)));
        }
        let Some(next) = pending.pop_front() else { break };
        let (page, _) = next.await.map_err(|e| e.to_string())??;
        if cached.is_empty() {
            let _ = tx.send(Msg::Tracks { view, tracks: page.clone(), done: false });
        }
        got.extend(page);
    }
    Ok(Some(got))
}

/// New episodes land at the top: if the first pages' last one is in `cached`, what's after it
/// there is the rest of the show, provided that adds up to `total`.
fn merge_newest(got: &[Track], cached: &[Track], total: usize) -> Option<Vec<Track>> {
    let last = &got.last()?.uri;
    let j = cached.iter().position(|t| &t.uri == last)?;
    let merged: Vec<Track> = got.iter().chain(&cached[j + 1..]).cloned().collect();
    (merged.len() == total).then_some(merged)
}

/// A show's episodes, newest first, or None if `cached` is still current. A web API call, unlike
/// albums': the internal metadata doesn't list a show's episodes in the same order or with dates.
pub async fn show_episodes(api: &Api, uri: &str, cached: &[Track], tx: &mpsc::Sender<Msg>, view: u64) -> Result<Option<Vec<Track>>, String> {
    let id = uri.strip_prefix("spotify:show:").unwrap_or(uri);
    let show = api.get(&format!("{API}/shows/{id}")).await.map_err(|e| format!("podcast: {e}"))?;
    let name = show["name"].as_str().unwrap_or("?").to_string();
    let show_id = librespot_core::SpotifyId::from_base62(id).map_or(0, |i| i.id);
    let mut page = show["episodes"].clone();
    let mut got: Vec<Track> = vec![];
    loop {
        let eps: Vec<Track> = page["items"].as_array().into_iter().flatten().filter_map(|e| parse_episode(e, Some((&name, show_id)))).collect();
        if cached.is_empty() {
            let _ = tx.send(Msg::Tracks { view, tracks: eps.clone(), done: false });
        }
        got.extend(eps);
        if let Some(all) = merge_newest(&got, cached, page["total"].as_u64().unwrap_or(0) as usize) {
            let same = same_uris(all.iter().map(|t| &t.uri), cached.iter().map(|t| &t.uri));
            return Ok((!same).then_some(all));
        }
        let Some(next) = page["next"].as_str() else { break };
        page = api.get(next).await.map_err(|e| format!("podcast: {e}"))?;
    }
    Ok(Some(got))
}

/// A show's newest episodes: the first page only, the one the show itself comes with.
pub async fn show_newest(api: &Api, uri: &str) -> Result<Vec<Track>, String> {
    let id = uri.strip_prefix("spotify:show:").unwrap_or(uri);
    let show = api.get(&format!("{API}/shows/{id}")).await.map_err(|e| format!("podcast: {e}"))?;
    let name = show["name"].as_str().unwrap_or("?");
    let show_id = librespot_core::SpotifyId::from_base62(id).map_or(0, |i| i.id);
    let items = show["episodes"]["items"].as_array().into_iter().flatten();
    Ok(items.filter_map(|e| parse_episode(e, Some((name, show_id)))).collect())
}

/// `New episodes`: the latest few of each followed show that haven't been heard, newest first.
/// One request per show, so it's fetched when opened, a few at a time.
pub async fn load_new_episodes(api: Arc<Api>, shows: Vec<Link>, heard: HashSet<String>, tx: mpsc::Sender<Msg>, view: u64, cached: Arc<Vec<Track>>) {
    const PARALLEL: usize = 8;
    const EACH: usize = 5;
    const SHOWN: usize = 50;
    let latest = |s: Link| {
        let api = api.clone();
        tokio::spawn(async move {
            let id = s.uri.strip_prefix("spotify:show:").unwrap_or(&s.uri).to_string();
            let v = api.get(&format!("{API}/shows/{id}/episodes?limit={EACH}")).await?;
            let show_id = librespot_core::SpotifyId::from_base62(&id).map_or(0, |i| i.id);
            let items = v["items"].as_array().into_iter().flatten();
            Ok::<_, String>(items.filter_map(|e| parse_episode(e, Some((&s.name, show_id)))).collect::<Vec<_>>())
        })
    };
    let mut shows = shows.into_iter();
    let mut pending = std::collections::VecDeque::new();
    let mut found = vec![];
    loop {
        while pending.len() < PARALLEL
            && let Some(s) = shows.next()
        {
            pending.push_back(latest(s));
        }
        let Some(next) = pending.pop_front() else { break };
        match next.await.map_err(|e| e.to_string()).and_then(|r| r) {
            Ok(eps) => found.extend(eps),
            // one show failing shouldn't cost the rest
            Err(e) => drop(tx.send(Msg::Error(format!("new episodes: {e}")))),
        }
    }
    let fresh = newest_unheard(found, &heard, SHOWN);
    save_tracks(&tracks_path("new_episodes"), &fresh);
    let _ = tx.send(if cached.is_empty() {
        Msg::Tracks { view, tracks: fresh, done: false }
    } else {
        Msg::Replace { view, tracks: Arc::new(fresh) }
    });
    let _ = tx.send(Msg::Tracks { view, tracks: vec![], done: true });
}

/// Release dates are `YYYY-MM-DD`, so they sort as text.
fn newest_unheard(mut eps: Vec<Track>, heard: &HashSet<String>, n: usize) -> Vec<Track> {
    eps.retain(|t| !heard.contains(&t.uri));
    eps.sort_by(|a, b| b.album.cmp(&a.album));
    eps.truncate(n);
    eps
}

/// Resolve track uris to names in batches, streaming them to the view in order (unless it's
/// showing a cached copy, `stream` false); returns them all.
/// `dedupe` drops repeats of the same song (single + album versions in a discography).
pub async fn send_tracks(
    session: &Session,
    uris: &[String],
    dedupe: bool,
    stream: bool,
    tx: &mpsc::Sender<Msg>,
    view: u64,
) -> Result<Vec<Track>, String> {
    let mut seen = std::collections::HashSet::new();
    let mut all = Vec::with_capacity(uris.len());
    for chunk in uris.chunks(200) {
        let found = batch_meta::<PbTrack>(session, chunk, ExtensionKind::TRACK_V4).await?;
        let tracks: Vec<Track> = tracks_in_order(chunk, &found)
            .filter(|t| !dedupe || seen.insert((t.name.to_lowercase(), t.artists.to_lowercase())))
            .collect();
        if stream {
            let _ = tx.send(Msg::Tracks { view, tracks: tracks.clone(), done: false });
        }
        all.extend(tracks);
    }
    Ok(all)
}

/// The playlist's tracks, or None if `cached` is still current. Only songs the cache
/// doesn't have get looked up.
pub async fn playlist_tracks(sess: Sess, id: &str, cached: &[Track], tx: &mpsc::Sender<Msg>, view: u64) -> Result<Option<Vec<Track>>, String> {
    use librespot_metadata::Metadata;
    let session = ready(sess).await?;
    let uri = SpotifyUri::from_uri(&format!("spotify:playlist:{id}")).map_err(|e| e.to_string())?;
    let pl = librespot_metadata::Playlist::get(&session, &uri).await.map_err(|e| format!("playlist: {e}"))?;
    let items: Vec<(String, u64)> = pl
        .contents
        .items
        .iter()
        .filter(|it| matches!(it.id, SpotifyUri::Track { .. }))
        .filter_map(|it| Some((it.id.to_uri().ok()?, (it.attributes.timestamp.as_timestamp_ms() / 1000).max(0) as u64)))
        .collect();
    let uris: Vec<String> = items.iter().map(|(u, _)| u.clone()).collect();
    let when: HashMap<&str, u64> = items.iter().map(|(u, t)| (u.as_str(), *t)).collect();
    let dated = |mut t: Track| {
        t.added = when.get(t.uri.as_str()).copied().unwrap_or(0);
        t
    };
    if cached.is_empty() {
        let tracks = send_tracks(&session, &uris, false, true, tx, view).await?;
        let tracks: Vec<Track> = tracks.into_iter().map(dated).collect();
        // the rows streamed in undated: hand over the dated list
        let _ = tx.send(Msg::Replace { view, tracks: Arc::new(tracks.clone()) });
        return Ok(Some(tracks));
    }
    if uris.len() == cached.len() && uris.iter().zip(cached).all(|(u, t)| *u == t.uri && t.added == when[u.as_str()]) {
        return Ok(None);
    }
    let known: HashMap<&str, &Track> = cached.iter().map(|t| (t.uri.as_str(), t)).collect();
    let missing: Vec<String> = uris.iter().filter(|u| !known.contains_key(u.as_str())).cloned().collect();
    let found = batch_meta::<PbTrack>(&session, &missing, ExtensionKind::TRACK_V4).await?;
    Ok(Some(
        uris.iter()
            .filter_map(|u| known.get(u.as_str()).map(|t| (*t).clone()).or_else(|| found.get(u).map(|t| pb_track(u, t))))
            .map(dated)
            .collect(),
    ))
}

/// An album's year and kind ("2022 album") for its header, when it came out and on what label,
/// then its copyrights ("© 2021 …").
fn about(a: &PbAlbum) -> Vec<String> {
    use librespot_protocol::metadata::copyright::Type;
    const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    let d = &a.date;
    let date = match (d.year(), d.month(), d.day()) {
        (y, m @ 1..=12, day) if day > 0 => format!("{} {day}, {y}", MONTHS[m as usize - 1]),
        (y, m @ 1..=12, _) => format!("{} {y}", MONTHS[m as usize - 1]),
        (y, ..) if y > 0 => y.to_string(),
        _ => String::new(),
    };
    let first: Vec<&str> = [date.as_str(), a.label().trim()].into_iter().filter(|s| !s.is_empty()).collect();
    // the copyrights, each text once: the © and ℗ ones are often the same, and then share a line
    let mut rights: Vec<(String, String)> = vec![];
    for c in &a.copyright {
        // the text sometimes carries its own mark: one is enough
        let text = c.text().trim();
        let text = ["(C)", "(c)", "(P)", "(p)", "©", "℗"].iter().find_map(|m| text.strip_prefix(m)).unwrap_or(text).trim();
        let mark = if c.type_() == Type::P { "℗" } else { "©" };
        if text.is_empty() {
            continue;
        }
        match rights.iter_mut().find(|(_, t)| t == text) {
            Some((marks, _)) if marks != mark => *marks = "© ℗".to_string(),
            Some(_) => {}
            None => rights.push((mark.to_string(), text.to_string())),
        }
    }
    // first what it is, for the header: "2022 album"
    let what = if d.year() > 0 { format!("{} {}", d.year(), album_kind(a)) } else { album_kind(a).to_string() };
    [what, first.join(" · ")].into_iter().chain(rights.into_iter().map(|(m, t)| format!("{m} {t}"))).filter(|l| !l.is_empty()).collect()
}

pub async fn album_track_uris(session: &Session, albums: &[String]) -> Result<Vec<String>, String> {
    let found = batch_meta::<PbAlbum>(session, albums, ExtensionKind::ALBUM_V4).await?;
    Ok(albums
        .iter()
        .filter_map(|a| found.get(a))
        .flat_map(|a| a.disc.iter().flat_map(|d| d.track.iter()))
        .filter_map(|t| gid_uri("track", t.gid()))
        .collect())
}

/// An artist's albums, then singles/EPs, each newest first.
pub async fn artist_albums(session: &Session, uri: &str) -> Result<Vec<String>, String> {
    let a = batch_meta::<PbArtist>(session, &[uri.to_string()], ExtensionKind::ARTIST_V4)
        .await?
        .remove(uri)
        .ok_or("artist not found")?;
    Ok(group_uris(&a.album_group).chain(group_uris(&a.single_group)).collect())
}

/// The songs behind a uri, and the list's name. Albums and artists come from the cache when
/// they're in it; Liked Songs and playlists change, so they're refreshed.
pub async fn resolve(api: Arc<Api>, sess: Sess, uri: &str, name: Option<String>) -> Result<(String, Vec<Track>), String> {
    if uri.starts_with("spotify:track:") {
        let session = ready(sess).await?;
        let t = batch_meta::<PbTrack>(&session, &[uri.to_string()], ExtensionKind::TRACK_V4).await?.remove(uri).ok_or("song not found")?;
        let t = pb_track(uri, &t);
        return Ok((format!("{} — {}", t.name, t.artists), vec![t]));
    }
    if let Some(id) = uri.strip_prefix("spotify:episode:") {
        let v = api.get(&format!("{API}/episodes/{id}")).await?;
        let t = parse_episode(&v, None).ok_or("episode not found")?;
        return Ok((format!("{} — {}", t.name, t.artists), vec![t]));
    }
    let cached = read_tracks(&tracks_path(&list_key(uri)));
    if is_show(uri) && cached.is_empty() {
        // a show never opened: its newest page is enough to play, where all of a long-running
        // show's episodes would take a request per 50 of them
        let tracks = show_newest(&api, uri).await?;
        let name = name.unwrap_or_else(|| tracks.first().map_or("podcast".into(), |t| t.artists.clone()));
        return if tracks.is_empty() { Err(format!("{name} has no episodes")) } else { Ok((name, tracks)) };
    }
    let changes = uri == "liked" || uri.starts_with("spotify:playlist:") || is_show(uri);
    let tracks = if changes || cached.is_empty() {
        let (nobody, _) = mpsc::channel(); // the songs would stream to a view; none is showing this
        fetch_list(api, sess.clone(), uri, &cached, &nobody, 0).await?.unwrap_or(cached)
    } else {
        cached
    };
    let name = match name {
        Some(n) => n,
        None if uri == "liked" => "Liked Songs".into(),
        None if uri.starts_with("spotify:playlist:") => {
            use librespot_metadata::Metadata;
            let session = ready(sess).await?;
            let u = SpotifyUri::from_uri(uri).map_err(|e| e.to_string())?;
            librespot_metadata::Playlist::get(&session, &u).await.map_or("playlist".into(), |p| p.name().to_string())
        }
        None if uri.starts_with("spotify:artist:") => {
            let by = tracks.iter().find(|t| id_uri("artist", t.artist_id).as_deref() == Some(uri));
            format!("{} · all songs", by.map_or("artist", |t| &t.artists[..t.lead]))
        }
        None if is_show(uri) => tracks.first().map_or("podcast".into(), |t| t.artists.clone()),
        None => tracks.first().map_or("album".into(), |t| t.album.clone()),
    };
    if tracks.is_empty() {
        return Err(format!("{name} has no songs to play"));
    }
    Ok((name, tracks))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn episodes(ids: &[u8]) -> Vec<Track> {
        let ep = |i: &u8| parse_episode(&serde_json::json!({ "uri": format!("spotify:episode:{i}") }), Some(("show", 0)));
        ids.iter().filter_map(ep).collect()
    }

    fn uris(t: &[Track]) -> Vec<&str> {
        t.iter().map(|t| t.uri.rsplit(':').next().unwrap_or("")).collect()
    }

    #[test]
    fn new_episodes_are_the_newest_unheard_across_shows() {
        let mut eps = episodes(&[1, 2, 3, 4]);
        for (t, date) in eps.iter_mut().zip(["2026-01-05", "2026-03-01", "2026-02-10", "2026-04-20"]) {
            t.album = date.into();
        }
        let heard = HashSet::from(["spotify:episode:4".to_string()]);
        assert_eq!(uris(&newest_unheard(eps, &heard, 2)), ["2", "3"]);
    }

    #[test]
    fn new_episodes_go_on_top_of_the_cached_show() {
        let all = merge_newest(&episodes(&[6, 5, 4]), &episodes(&[4, 3, 2, 1]), 6).unwrap();
        assert_eq!(uris(&all), ["6", "5", "4", "3", "2", "1"]);
    }

    #[test]
    fn a_show_that_doesnt_add_up_is_fetched_in_full() {
        let cached = episodes(&[4, 3, 2, 1]);
        assert!(merge_newest(&episodes(&[6, 5, 4]), &cached, 5).is_none()); // one was taken down
        assert!(merge_newest(&episodes(&[9, 8]), &cached, 6).is_none()); // nothing in common yet
        assert!(merge_newest(&episodes(&[9, 8]), &[], 2).is_none());
    }
}
