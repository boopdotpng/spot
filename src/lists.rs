// Albums and artist pages: their songs, and an artist's releases.

use crate::*;

/// Album or artist (albums + singles/EPs, newest first) as a flat track list. Like playlists,
/// a cached copy may already be showing; it's replaced only if the fresh one differs.
pub async fn link_tracks(sess: Sess, tx: mpsc::Sender<Msg>, view: u64, uri: String, cached: Arc<Vec<Track>>) {
    let res = async {
        let session = ready(sess).await?;
        let (albums, dedupe) = if uri.starts_with("spotify:artist:") {
            (artist_albums(&session, &uri).await?, true)
        } else {
            (vec![uri.clone()], false)
        };
        let uris = album_track_uris(&session, &albums).await?;
        let tracks = send_tracks(&session, &uris, dedupe, cached.is_empty(), &tx, view).await?;
        save_tracks(&tracks_path(&uri_key(&uri)), &tracks);
        // disc numbers too: caches from before they were kept have none
        let changed = !same_uris(tracks.iter().map(|t| &t.uri), cached.iter().map(|t| &t.uri))
            || tracks.iter().zip(cached.iter()).any(|(a, b)| a.disc != b.disc);
        if !cached.is_empty() && changed {
            let _ = tx.send(Msg::Replace { view, tracks: Arc::new(tracks) });
        }
        Ok::<_, String>(())
    }
    .await;
    if let Err(e) = res {
        let _ = tx.send(Msg::Error(e));
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
