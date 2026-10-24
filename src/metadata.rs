// Spotify's internal metadata API (what the official client uses), for playlists, albums and
// artists: dev-mode Web API apps can't read playlists you don't own, and batch lookups there were removed.

use crate::*;

pub use librespot_protocol::{
    extension_kind::ExtensionKind,
    metadata::{Album as PbAlbum, Artist as PbArtist, Track as PbTrack},
};

pub async fn batch_meta<M: protobuf::Message>(
    session: &Session,
    uris: &[String],
    kind: ExtensionKind,
) -> Result<HashMap<String, M>, String> {
    use librespot_protocol::extended_metadata::{BatchedEntityRequest, EntityRequest, ExtensionQuery};
    use protobuf::EnumOrUnknown;

    let mut out = HashMap::new();
    for chunk in uris.chunks(200) {
        let req = BatchedEntityRequest {
            entity_request: chunk
                .iter()
                .map(|u| EntityRequest {
                    entity_uri: u.clone(),
                    query: vec![ExtensionQuery { extension_kind: EnumOrUnknown::new(kind), ..Default::default() }],
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let res = session.spclient().get_extended_metadata(req).await.map_err(|e| format!("metadata: {e}"))?;
        for arr in &res.extended_metadata {
            for d in &arr.extension_data {
                if let Some(any) = d.extension_data.as_ref()
                    && let Ok(m) = M::parse_from_bytes(&any.value)
                {
                    out.insert(d.entity_uri.clone(), m);
                }
            }
        }
    }
    Ok(out)
}

pub fn gid_uri(kind: &str, gid: &[u8]) -> Option<String> {
    let b62 = librespot_core::SpotifyId::from_raw(gid).ok()?.to_base62().ok()?;
    Some(format!("spotify:{kind}:{b62}"))
}

/// spotify:<kind>:<base62> for a stored SpotifyId (0 = unknown).
pub fn id_uri(kind: &str, id: u128) -> Option<String> {
    if id == 0 {
        return None;
    }
    Some(format!("spotify:{kind}:{}", librespot_core::SpotifyId { id }.to_base62().ok()?))
}

pub fn pb_track(uri: &str, t: &PbTrack) -> Track {
    let id = |gid: &[u8]| librespot_core::SpotifyId::from_raw(gid).map_or(0, |i| i.id);
    Track {
        uri: uri.into(),
        name: t.name().to_string(),
        artists: t.artist.iter().map(|a| a.name()).collect::<Vec<_>>().join(", "),
        lead: t.artist.first().map_or(0, |a| a.name().len()),
        album: t.album.name().to_string(),
        cover: t.album.cover_group.image.iter().max_by_key(|i| i.width()).map(|i| {
            let hex: String = i.file_id().iter().map(|b| format!("{b:02x}")).collect();
            format!("https://i.scdn.co/image/{hex}")
        }),
        ms: t.duration() as u64,
        album_id: id(t.album.gid()),
        artist_id: t.artist.first().map_or(0, |a| id(a.gid())),
        disc: t.disc_number().max(0) as u32,
    }
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
    let mut all = vec![];
    for chunk in uris.chunks(200) {
        let mut found = batch_meta::<PbTrack>(session, chunk, ExtensionKind::TRACK_V4).await?;
        let tracks: Vec<Track> = chunk
            .iter()
            .filter_map(|u| found.remove(u).map(|t| pb_track(u, &t)))
            .filter(|t| !dedupe || seen.insert((t.name.to_lowercase(), t.artists.to_lowercase())))
            .collect();
        all.extend_from_slice(&tracks);
        if stream {
            let _ = tx.send(Msg::Tracks { view, tracks, done: false });
        }
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
