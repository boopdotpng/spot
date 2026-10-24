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

