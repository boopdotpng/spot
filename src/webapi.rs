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
                    let r = self.relogin().await;
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

    pub async fn relogin(&self) -> Result<librespot_oauth::OAuthToken, String> {
        browser_login(self.oauth.clone(), &self.redirect).await
    }

    pub async fn get(&self, url: &str) -> Result<Value, String> {
        let mut force = false;
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
        Err("request failed after retries".into())
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
        disc: v["disc_number"].as_u64().unwrap_or(0) as u32,
    })
}

pub fn liked() -> Playlist {
    Playlist { id: None, name: "♥  Liked Songs".into() } // the heart overhangs its cell in many fonts
}

pub async fn load_playlists(api: Arc<Api>, tx: mpsc::Sender<Msg>) {
    let mut out = vec![liked()];
    let mut url = Some(format!("{API}/me/playlists?limit=50"));
    while let Some(u) = url {
        match api.get(&u).await {
            Ok(v) => {
                for p in v["items"].as_array().into_iter().flatten() {
                    let Some(id) = p["id"].as_str() else { continue };
                    out.push(Playlist {
                        id: Some(id.into()),
                        name: p["name"].as_str().unwrap_or("?").into(),
                    });
                }
                url = v["next"].as_str().map(String::from);
            }
            Err(e) => {
                // keep showing the cached list
                let _ = tx.send(Msg::Error(format!("playlists: {e}")));
                return;
            }
        }
    }
    save_playlists(&out);
    let _ = tx.send(Msg::Playlists(out));
}

/// An album, artist or playlist search result.
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
        _ => v["genres"].as_array().map(|g| g.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default(),
    };
    let single = v["album_type"] == "single";
    Some(Link { uri: v["uri"].as_str()?.into(), name: v["name"].as_str().unwrap_or("?").into(), sub, single })
}

/// Search Spotify for `n` results of a type (track, album, artist or playlist), each page handed
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
