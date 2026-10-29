// Track lists: Liked Songs, playlists, albums and artists (every song), fetched fresh and
// compared with the cached copy; and an artist's releases.

use crate::*;

/// A list's cache file name: `liked`, a playlist id, or `album_…` / `artist_…`.
pub fn list_key(uri: &str) -> String {
    uri.strip_prefix("spotify:playlist:").map_or_else(|| uri_key(uri), String::from)
}

/// The songs of `uri` (`liked`, or a playlist, album or artist uri) fresh from Spotify, or None
/// when `cached` is still current; saved to the cache when they changed. With nothing cached,
/// they also stream to `view` as they load.
pub async fn fetch_list(api: Arc<Api>, sess: Sess, uri: &str, cached: &[Track], tx: &mpsc::Sender<Msg>, view: u64) -> Result<Option<Vec<Track>>, String> {
    let fresh = if uri == "liked" {
        liked_tracks(api, cached, tx, view).await?
    } else if let Some(id) = uri.strip_prefix("spotify:playlist:") {
        playlist_tracks(sess, id, cached, tx, view).await?
    } else {
        let session = ready(sess).await?;
        let artist = uri.starts_with("spotify:artist:");
        let albums = if artist { artist_albums(&session, uri).await? } else { vec![uri.to_string()] };
        let uris = album_track_uris(&session, &albums).await?;
        let tracks = send_tracks(&session, &uris, artist, cached.is_empty(), tx, view).await?;
        // disc numbers too: caches from before they were kept have none
        let same = same_uris(tracks.iter().map(|t| &t.uri), cached.iter().map(|t| &t.uri))
            && tracks.iter().zip(cached).all(|(a, b)| a.disc == b.disc);
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

/// An artist's releases as openable album links.
pub async fn artist_releases(sess: Sess, tx: mpsc::Sender<Msg>, view: u64, uri: String, cached: Vec<Link>) {
    use librespot_protocol::metadata::album::Type;
    let res = async {
        let session = ready(sess).await?;
        let albums = artist_albums(&session, &uri).await?;
        let found = batch_meta::<PbAlbum>(&session, &albums, ExtensionKind::ALBUM_V4).await?;
        let mut links: Vec<_> = albums
            .iter()
            .filter_map(|u| {
                let a = found.get(u)?;
                let kind = match a.type_() {
                    Type::SINGLE => "single",
                    Type::EP => "EP",
                    Type::COMPILATION => "compilation",
                    _ => "album",
                };
                let n: usize = a.disc.iter().map(|d| d.track.len()).sum();
                let year = if a.date.year() > 0 { format!("{} ", a.date.year()) } else { String::new() };
                let sub = format!("{year}{kind} · {n} song{}", if n == 1 { "" } else { "s" });
                let d = &a.date;
                let single = a.type_() == Type::SINGLE;
                Some(((d.year(), d.month(), d.day()), Link { uri: u.clone(), name: a.name().into(), sub, single }))
            })
            .collect();
        // Newest release first, mixing albums, EPs and singles.
        links.sort_by_key(|(date, _)| std::cmp::Reverse(*date));
        let links: Vec<Link> = links.into_iter().map(|(_, l)| l).collect();
        save_links(&releases_path(&uri), &links);
        if cached.is_empty() {
            let _ = tx.send(Msg::Links { view, links, done: true });
        } else {
            if !same_uris(links.iter().map(|l| &l.uri), cached.iter().map(|l| &l.uri))
                || links.iter().zip(&cached).any(|(a, b)| a.name != b.name || a.sub != b.sub)
            {
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

pub async fn liked_page(api: Arc<Api>, offset: usize) -> Result<(Vec<Track>, usize), String> {
    let v = api.get(&format!("{API}/me/tracks?limit=50&offset={offset}")).await?;
    let page = v["items"].as_array().into_iter().flatten().filter_map(|it| parse_track(&it["track"])).collect();
    Ok((page, v["total"].as_u64().unwrap_or(0) as usize))
}

/// The full Liked Songs list, or None if `cached` is still current.
pub async fn liked_tracks(api: Arc<Api>, cached: &[Track], tx: &mpsc::Sender<Msg>, view: u64) -> Result<Option<Vec<Track>>, String> {
    let (mut got, total) = liked_page(api.clone(), 0).await?;
    if cached.is_empty() {
        let _ = tx.send(Msg::Tracks { view, tracks: got.clone(), done: false });
    } else if let Some(j) = (cached.len() + got.len()).checked_sub(total)
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
    let uris: Vec<String> = pl
        .tracks()
        .filter(|u| matches!(u, SpotifyUri::Track { .. }))
        .filter_map(|u| u.to_uri().ok())
        .collect();
    if cached.is_empty() {
        return send_tracks(&session, &uris, false, true, tx, view).await.map(Some);
    }
    if uris.len() == cached.len() && uris.iter().zip(cached).all(|(u, t)| *u == t.uri) {
        return Ok(None);
    }
    let known: HashMap<&str, &Track> = cached.iter().map(|t| (t.uri.as_str(), t)).collect();
    let missing: Vec<String> = uris.iter().filter(|u| !known.contains_key(u.as_str())).cloned().collect();
    let found = batch_meta::<PbTrack>(&session, &missing, ExtensionKind::TRACK_V4).await?;
    Ok(Some(
        uris.iter()
            .filter_map(|u| known.get(u.as_str()).map(|t| (*t).clone()).or_else(|| found.get(u).map(|t| pb_track(u, t))))
            .collect(),
    ))
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
    // each group lists regional/edition variants of one release; the first is the canonical one
    Ok(a.album_group
        .iter()
        .chain(&a.single_group)
        .filter_map(|g| g.album.first())
        .filter_map(|al| gid_uri("album", al.gid()))
        .collect())
}
