// Logging in: the user's Spotify app, the Web API token and the audio session.

use crate::*;

/// The file spot kept the client id in before config.toml; still read, so nobody sets up again.
pub fn old_client_id() -> Option<String> {
    fs::read_to_string(config_dir().join("client_id")).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// The user's Spotify app client id: env, then config.toml, then a one-time prompt that saves it.
pub fn client_id(redirect: &str, saved: Option<String>) -> Result<String, String> {
    if let Ok(id) = std::env::var("SPOT_CLIENT_ID") {
        return Ok(id);
    }
    if let Some(id) = saved.or_else(old_client_id) {
        return Ok(id);
    }
    if !std::io::stdin().is_terminal() {
        return Err(NOT_SET_UP.into());
    }
    println!("spot needs your own Spotify app (one-time setup, requires Premium):");
    println!("  1. Open https://developer.spotify.com/dashboard and create an app.");
    println!("  2. Add this redirect URI: {redirect}");
    println!("  3. Tick \"Web API\" and save.");
    println!("  4. Copy the app's Client ID from its settings page.");
    let id = loop {
        use std::io::Write;
        print!("\nClient ID: ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Err("no client id given".into());
        }
        let id = line.trim().to_string();
        if id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit()) {
            break id;
        }
        println!("That doesn't look like a client id (32 hex characters).");
    };
    println!("Saved to {}.\n", settings::save_client_id(&id)?.display());
    Ok(id)
}

pub fn save_refresh(path: &PathBuf, tok: &str) {
    let _ = fs::write(path, tok);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
}

pub async fn connect(cache: &Cache, creds: Option<Credentials>) -> Result<Session, String> {
    let s = Session::new(SessionConfig::default(), Some(cache.clone()));
    login(&s, cache, creds).await?;
    Ok(s)
}

pub async fn login(s: &Session, cache: &Cache, creds: Option<Credentials>) -> Result<(), String> {
    let creds = creds.or_else(|| cache.credentials()).ok_or("no saved login")?;
    s.connect(creds, true).await.map_err(|e| format!("login failed: {e}"))
}

/// Streaming + internal APIs need a session from Spotify's own client id. Only needed once:
/// librespot stores reusable credentials afterwards. Never used for the Web API.
pub async fn approve_session(cache: &Cache) -> Result<Session, String> {
    let oauth = OAuthClientBuilder::new(SESSION_CLIENT_ID, SESSION_REDIRECT, vec!["streaming"])
        .open_in_browser()
        .build()
        .map_err(|e| e.to_string())?;
    let t = browser_login(Arc::new(oauth), SESSION_REDIRECT).await?;
    connect(cache, Some(Credentials::with_access_token(t.access_token))).await
}

/// Log the player's (not yet connected) session in with the saved credentials, in the background.
pub async fn start_session(s: Session, cache: Cache, tx: mpsc::Sender<Msg>) {
    let r = match login(&s, &cache, None).await {
        Ok(()) => Ok(s),
        Err(_) => {
            let _ = tx.send(Msg::Status("approve the audio session in your browser…".into()));
            approve_session(&cache).await
        }
    };
    let _ = tx.send(Msg::Connected(r));
}

/// Browser login, off the runtime: librespot's callback listener blocks its thread.
pub async fn browser_login(oauth: Arc<OAuthClient>, redirect: &str) -> Result<librespot_oauth::OAuthToken, String> {
    let (otx, orx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let r = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())
            .and_then(|rt| rt.block_on(oauth.get_access_token_async()).map_err(|e| format!("login: {e}")));
        let _ = otx.send(r);
    });
    match tokio::time::timeout(Duration::from_secs(300), orx).await {
        Ok(r) => r.map_err(|_| "login: aborted".to_string())?,
        Err(_) => {
            // nobody came back from the browser; poke the listener so it frees the port
            if let Some(addrs) = reqwest::Url::parse(redirect).ok().and_then(|u| u.socket_addrs(|| None).ok()) {
                for a in addrs {
                    let _ = std::net::TcpStream::connect_timeout(&a, Duration::from_secs(1));
                }
            }
            Err("login timed out".into())
        }
    }
}
