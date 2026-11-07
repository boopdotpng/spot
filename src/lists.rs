// Track lists: Liked Songs, playlists, albums, artists (every song) and shows (every episode),
// fetched fresh and compared with the cached copy; and an artist's releases.

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
