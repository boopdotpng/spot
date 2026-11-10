// Spotify's Web API (HTTP + JSON): the token, the user's playlists and Liked Songs, search.

use crate::*;

pub struct Tok {
    pub access: String,
    pub refresh: String,
    pub exp: Instant,
    pub failed_login: Option<Instant>,
}

pub struct Api {
    pub http: reqwest::Client,
    pub oauth: Arc<OAuthClient>,
    pub redirect: String,
    pub tok: tokio::sync::Mutex<Tok>,
    pub path: PathBuf,
    pub tx: mpsc::Sender<Msg>,
}

impl Api {
    pub async fn token(&self, force: bool) -> Result<String, String> {
        let mut t = self.tok.lock().await;
        if force || Instant::now() + Duration::from_secs(60) >= t.exp {
            let n = match self.oauth.refresh_token_async(&t.refresh).await {
                Ok(n) => n,
                // Spotify no longer accepts the refresh token (revoked, or rotated by another
                // instance): log in through the browser again. Holding the lock keeps it to one.
                Err(e) if e.to_string().contains("invalid_") => {
                    if t.failed_login.is_some_and(|at| at.elapsed() < Duration::from_secs(60)) {
                        return Err("Spotify login expired; restart spot to log in".into());
                    }
                    let _ = self.tx.send(Msg::Relogin(true));
                    let r = browser_login(self.oauth.clone(), &self.redirect).await;
                    let _ = self.tx.send(Msg::Relogin(false));
                    t.failed_login = r.is_err().then(Instant::now);
                    r?
                }
                Err(e) => return Err(format!("token refresh: {e}")),
            };
            if !n.refresh_token.is_empty() && n.refresh_token != t.refresh {
                t.refresh = n.refresh_token;
                save_refresh(&self.path, &t.refresh);
            }
            t.access = n.access_token;
            t.exp = n.expires_at;
        }
        Ok(t.access.clone())
    }

    pub async fn get(&self, url: &str) -> Result<Value, String> {
        let mut force = false;
        let mut last = 0;
        for _ in 0..4 {
            let tok = self.token(force).await?;
            let r = self
                .http
                .get(url)
                .bearer_auth(tok)
                .send()
                .await
                .map_err(|e| format!("network: {e}"))?;
            let status = r.status().as_u16();
            last = status;
            match status {
                200..=299 => {
                    let body = r.text().await.map_err(|e| e.to_string())?;
                    return serde_json::from_str(&body).map_err(|e| e.to_string());
                }
                401 => force = true,
                429 => {
                    let wait = r
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok()?.parse().ok())
                        .unwrap_or(2u64);
                    if wait > 30 {
                        return Err(format!("rate limited by Spotify, retry in {wait}s"));
                    }
                    tokio::time::sleep(Duration::from_secs(wait)).await;
                }
                500..=599 => tokio::time::sleep(Duration::from_millis(500)).await,
                _ => {
                    let body = r.text().await.unwrap_or_default();
                    return Err(format!("HTTP {status}: {}", body.chars().take(120).collect::<String>()));
                }
            }
        }
        Err(match last {
            401 => "Spotify refused the login (HTTP 401), even after a fresh token".into(),
            s => format!("Spotify isn't answering (HTTP {s}); try again in a bit"),
        })
    }
}

pub fn parse_track(v: &Value) -> Option<Track> {
    let uri = v["uri"].as_str()?;
    if !uri.starts_with("spotify:track:") {
        return None;
    }
    let artists: Vec<&str> =
        v["artists"].as_array().map(|a| a.iter().filter_map(|x| x["name"].as_str()).collect()).unwrap_or_default();
    let id = |v: &Value| v["id"].as_str().and_then(|s| librespot_core::SpotifyId::from_base62(s).ok()).map_or(0, |i| i.id);
    Some(Track {
        uri: uri.into(),
        name: v["name"].as_str().unwrap_or("?").into(),
        artists: artists.join(", "),
        lead: artists.first().map_or(0, |a| a.len()),
        album: v["album"]["name"].as_str().unwrap_or("").into(),
        cover: v["album"]["images"].as_array().and_then(|i| i.first()?["url"].as_str()).map(String::from),
        ms: v["duration_ms"].as_u64().unwrap_or(0),
        album_id: id(&v["album"]),
        artist_id: id(&v["artists"][0]),
        more: v["artists"].as_array().map(|a| a.iter().skip(1).map(|x| (x["name"].as_str().unwrap_or("").len(), id(x))).collect()).unwrap_or_default(),
        disc: v["disc_number"].as_u64().unwrap_or(0) as u32,
        number: v["track_number"].as_u64().unwrap_or(0) as u32,
        explicit: v["explicit"].as_bool().unwrap_or(false),
        added: 0,
    })
}

/// A podcast episode. Episodes listed under a show don't name it, so `show` supplies its name
/// and id; otherwise they come from the episode.
pub fn parse_episode(v: &Value, show: Option<(&str, u128)>) -> Option<Track> {
    let uri = v["uri"].as_str()?;
    if !uri.starts_with("spotify:episode:") {
        return None;
    }
    let (name, show_id) = show.unwrap_or_else(|| {
        let id = v["show"]["id"].as_str().and_then(|s| librespot_core::SpotifyId::from_base62(s).ok());
        (v["show"]["name"].as_str().unwrap_or(""), id.map_or(0, |i| i.id))
    });
    Some(Track {
        uri: uri.into(),
        name: v["name"].as_str().unwrap_or("?").into(),
        artists: name.into(),
        lead: name.len(),
        album: v["release_date"].as_str().unwrap_or("").into(),
        cover: v["images"].as_array().and_then(|i| i.first()?["url"].as_str()).map(String::from),
        ms: v["duration_ms"].as_u64().unwrap_or(0),
        album_id: show_id,
        artist_id: 0,
        more: Vec::new(),
        disc: 0,
        number: 0,
        explicit: v["explicit"].as_bool().unwrap_or(false),
        added: 0,
    })
}

pub fn liked() -> Playlist {
    Playlist { id: None, name: "♥  Liked Songs".into(), image: None, rank: 0 } // the heart overhangs its cell in many fonts
}

/// The smallest of a list of images that's still sharp in the sidebar (Spotify lists them
/// largest first; an uploaded cover comes in one size, with no width).
fn smallest_image(images: &Value) -> Option<String> {
    let all = images.as_array()?;
    let width = |i: &Value| i["width"].as_u64().unwrap_or(u64::MAX);
    all.iter().filter(|i| width(i) >= 60).min_by_key(|i| width(i)).or(all.first())?["url"].as_str().map(String::from)
}

/// Every item of a paged Web API list, a page at a time.
async fn each_item(api: &Api, url: String, mut each: impl FnMut(&Value)) -> Result<(), String> {
    let mut url = Some(url);
    while let Some(u) = url {
        let v = api.get(&u).await?;
        v["items"].as_array().into_iter().flatten().for_each(&mut each);
        url = v["next"].as_str().map(String::from);
    }
    Ok(())
}

pub async fn load_playlists(api: Arc<Api>, tx: mpsc::Sender<Msg>) {
    let mut out = vec![liked()];
    let r = each_item(&api, format!("{API}/me/playlists?limit=50"), |p| {
        let Some(id) = p["id"].as_str() else { return };
        out.push(Playlist { id: Some(id.into()), name: p["name"].as_str().unwrap_or("?").into(), image: smallest_image(&p["images"]), rank: out.len() });
    });
    match r.await {
        Ok(()) => {
            save_playlists(&out);
            let _ = tx.send(Msg::Playlists(out));
        }
        // keep showing the cached list
        Err(e) => drop(tx.send(Msg::Error(format!("playlists: {e}")))),
    }
}

/// The podcasts the user follows, for the sidebar.
pub async fn load_shows(api: Arc<Api>, tx: mpsc::Sender<Msg>) {
    let mut out = vec![];
    let r = each_item(&api, format!("{API}/me/shows?limit=50"), |i| out.extend(parse_link("show", &i["show"])));
    match r.await {
        Ok(()) => {
            save_links(&shows_path(), &out);
            let _ = tx.send(Msg::Shows(out));
        }
        // keep showing the cached list
        Err(e) => drop(tx.send(Msg::Error(format!("podcasts: {e}")))),
    }
}

/// An album, artist, playlist or show search result.
pub fn parse_link(ty: &str, v: &Value) -> Option<Link> {
    let names = |a: &Value| a.as_array().map(|a| a.iter().filter_map(|x| x["name"].as_str()).collect::<Vec<_>>().join(", "));
    let sub = match ty {
        "album" => {
            let year = v["release_date"].as_str().unwrap_or("").get(..4).unwrap_or("");
            let kind = v["album_type"].as_str().unwrap_or("");
            format!("{} · {year} {kind}", names(&v["artists"]).unwrap_or_default())
        }
        "playlist" => {
            let by = v["owner"]["display_name"].as_str().unwrap_or("?");
            format!("by {by} · {}", plural(v["tracks"]["total"].as_u64().unwrap_or(0) as usize, "song"))
        }
        "show" => plural(v["total_episodes"].as_u64().unwrap_or(0) as usize, "episode"),
        _ => v["genres"].as_array().map(|g| g.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default(),
    };
    let single = v["album_type"] == "single";
    let image = v["images"].as_array().and_then(|i| i.first()?["url"].as_str()).map(String::from);
    Some(Link { uri: v["uri"].as_str()?.into(), name: v["name"].as_str().unwrap_or("?").into(), sub, single, image })
}

/// One search for songs, albums and artists together, up to `n` of each.
pub async fn search_all(api: &Api, q: &str, n: usize) -> Result<Value, String> {
    let url = reqwest::Url::parse_with_params(&format!("{API}/search"), &[("q", q), ("type", "track,album,artist"), ("limit", &n.to_string())]).map_err(|e| e.to_string())?;
    api.get(url.as_str()).await.map_err(|e| format!("search: {e}"))
}

/// Of the first artist, song and album (in that order of preference on a tie), the one the
/// words name best; the song when none of them matches.
pub fn top_pick(q: &str, names: [Option<&str>; 3]) -> Option<usize> {
    let best = names.iter().enumerate().filter_map(|(i, n)| Some((i, match_score(q, (*n)?)))).fold(None, |b: Option<(usize, u8)>, (i, sc)| match b {
        Some((_, bs)) if bs >= sc => b,
        _ => Some((i, sc)),
    });
    match best {
        Some((i, sc)) if sc > 0 => Some(i),
        _ => names[1].is_some().then_some(1).or(names.iter().position(Option::is_some)),
    }
}

/// One search's songs, albums and artists, sorted out: the top result (`"artist"`, `"track"` or
/// `"album"`, and which) is whichever of the first artist, song and album the words name best.
/// A song or album that's on several releases (a deluxe edition, a single and its album) is
/// kept once, the first time; the top result isn't repeated after it. Artists far less known
/// than the best of them are dropped: searching an album's name finds accounts named after it.
pub struct Mixed<'a> {
    pub top: Option<(&'static str, &'a Value)>,
    pub tracks: Vec<&'a Value>,
    pub albums: Vec<&'a Value>,
    pub artists: Vec<&'a Value>,
}

pub fn mixed<'a>(q: &str, v: &'a Value) -> Mixed<'a> {
    fn items<'a>(v: &'a Value, k: &str) -> Vec<&'a Value> {
        v[k]["items"].as_array().map(|a| a.iter().collect()).unwrap_or_default()
    }
    let (tracks, albums, mut artists) = (items(v, "tracks"), items(v, "albums"), items(v, "artists"));
    let fame = |i: &Value| i["popularity"].as_u64();
    if let Some(best) = artists.iter().filter_map(|i| fame(i)).max() {
        artists.retain(|i| fame(i).is_none_or(|p| p * 4 >= best));
    }
    // the same song or album: its name and its artists'
    let same = |i: &Value| {
        let by: Vec<String> = i["artists"].as_array().map(|a| a.iter().filter_map(|x| x["name"].as_str()).map(str::to_lowercase).collect()).unwrap_or_default();
        format!("{}\t{}", i["name"].as_str().unwrap_or("").trim().to_lowercase(), by.join(", "))
    };
    let mut seen = HashSet::new();
    let mut fresh = |kind: &str, i: &Value| seen.insert(format!("{kind}\t{}", same(i)));
    fn name<'a>(l: &[&'a Value]) -> Option<&'a str> {
        l.first().and_then(|i| i["name"].as_str())
    }
    let top = match top_pick(q, [name(&artists), name(&tracks), name(&albums)]) {
        Some(0) => Some(("artist", artists[0])),
        Some(1) => Some(("track", tracks[0])),
        Some(2) => Some(("album", albums[0])),
        _ => None,
    };
    if let Some((kind, i)) = top {
        fresh(kind, i);
    }
    let tracks = tracks.into_iter().filter(|i| fresh("track", i)).collect();
    let albums = albums.into_iter().filter(|i| fresh("album", i)).collect();
    let artists = artists.into_iter().filter(|i| fresh("artist", i)).collect();
    Mixed { top, tracks, albums, artists }
}

/// Search Spotify for `n` results of a type (track, album, artist, playlist, show or episode), each page handed
/// to `page` as it comes, with whether it's the last. Dev-mode apps get 10 results per call.
pub async fn search(api: &Api, q: &str, ty: &str, n: usize, mut page: impl FnMut(&[Value], bool)) -> Result<(), String> {
    for offset in (0..n).step_by(10) {
        let limit = (n - offset).min(10).to_string();
        let url = reqwest::Url::parse_with_params(
            &format!("{API}/search"),
            &[("q", q), ("type", ty), ("limit", &limit), ("offset", &offset.to_string())],
        )
        .map_err(|e| e.to_string())?;
        let v = api.get(url.as_str()).await.map_err(|e| format!("search: {e}"))?;
        let found = &v[format!("{ty}s")];
        let items = found["items"].as_array().map_or(&[][..], Vec::as_slice);
        let last = items.len() < 10 || found["next"].is_null() || offset + 10 >= n;
        page(items, last);
        if last {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_top_result_is_what_the_words_name() {
        let names = [Some("The Weeknd"), Some("Blinding Lights"), Some("After Hours")];
        assert_eq!(top_pick("the weeknd", names), Some(0));
        assert_eq!(top_pick("weeknd", names), Some(0));
        assert_eq!(top_pick("blinding", names), Some(1));
        assert_eq!(top_pick("after hours", names), Some(2));
        assert_eq!(top_pick("something else", names), Some(1));
        // a single shares its song's name: the song
        assert_eq!(top_pick("blinding lights", [Some("Nobody"), Some("Blinding Lights"), Some("Blinding Lights")]), Some(1));
        assert_eq!(top_pick("x", [Some("Artist"), None, None]), Some(0));
        assert_eq!(top_pick("x", [None, None, None]), None);
    }
}
