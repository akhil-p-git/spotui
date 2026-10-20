use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngExt;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";
const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
const SCOPES: &str = "user-read-playback-state user-modify-playback-state \
                      user-read-currently-playing user-library-read playlist-read-private \
                      user-library-modify playlist-modify-public playlist-modify-private";
const UNRESERVED: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
/// Refresh this many seconds before the access token actually expires.
const EXPIRY_MARGIN_SECS: u64 = 60;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub client_id: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

fn default_port() -> u16 {
    8888
}

impl Config {
    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.port)
    }

    pub fn load() -> Result<Self> {
        let path = config_dir()?.join("config.json");
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds at which `access_token` stops working.
    pub expires_at: u64,
}

impl Tokens {
    fn needs_refresh(&self, now: u64) -> bool {
        now + EXPIRY_MARGIN_SECS >= self.expires_at
    }

    fn path() -> Result<PathBuf> {
        Ok(config_dir()?.join("tokens.json"))
    }

    fn load() -> Result<Self> {
        let path = Self::path()?;
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("no saved login at {}; run `spotui login`", path.display()))?;
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
    }

    fn save(&self) -> Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let path = Self::path()?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("writing {}", path.display()))?;
        file.write_all(serde_json::to_string_pretty(self)?.as_bytes())?;
        Ok(())
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
    /// Absent on refresh responses that keep the old refresh token.
    refresh_token: Option<String>,
}

fn config_dir() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".config"),
    };
    let dir = base.join("spotui");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before 1970")
        .as_secs()
}

fn random_string(len: usize) -> String {
    let mut rng = rand::rng();
    (0..len)
        .map(|_| UNRESERVED[rng.random_range(0..UNRESERVED.len())] as char)
        .collect()
}

pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub fn authorize_url(config: &Config, challenge: &str, state: &str) -> Url {
    Url::parse_with_params(
        AUTHORIZE_URL,
        [
            ("client_id", config.client_id.as_str()),
            ("response_type", "code"),
            ("redirect_uri", &config.redirect_uri()),
            ("scope", SCOPES),
            ("code_challenge_method", "S256"),
            ("code_challenge", challenge),
            ("state", state),
        ],
    )
    .expect("authorize URL is valid")
}

/// Extracts the authorization code from the first line of the browser's
/// callback request (`GET /callback?code=...&state=... HTTP/1.1`).
pub fn parse_callback(request_line: &str, expected_state: &str) -> Result<String> {
    let target = request_line
        .split_whitespace()
        .nth(1)
        .context("malformed callback request")?;
    let url = Url::parse(&format!("http://127.0.0.1{target}")).context("malformed callback URL")?;
    let param = |name: &str| {
        url.query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    };

    if let Some(err) = param("error") {
        bail!("Spotify denied the login: {err}");
    }
    if param("state").as_deref() != Some(expected_state) {
        bail!("callback state did not match; ignoring it");
    }
    param("code").context("callback had no authorization code")
}

async fn token_request(form: &[(&str, &str)]) -> Result<TokenResponse> {
    let resp = reqwest::Client::new()
        .post(TOKEN_URL)
        .form(form)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        bail!(
            "token endpoint returned {status}: {}",
            resp.text().await.unwrap_or_default()
        );
    }
    Ok(resp.json().await?)
}

/// Runs the browser login and stores the resulting tokens.
pub async fn login(config: &Config) -> Result<()> {
    let verifier = random_string(64);
    let state = random_string(16);
    let url = authorize_url(config, &pkce_challenge(&verifier), &state);

    let listener = TcpListener::bind(("127.0.0.1", config.port))
        .await
        .with_context(|| {
            format!(
                "port {} is busy; set another `port` in config.json",
                config.port
            )
        })?;

    println!("Open this URL in your browser to log in:\n\n{url}\n");
    let _ = std::process::Command::new("xdg-open")
        .arg(url.as_str())
        .spawn();

    let code = loop {
        let (mut socket, _) = listener.accept().await?;
        let mut buf = vec![0u8; 8192];
        let n = socket.read(&mut buf).await?;
        let request = String::from_utf8_lossy(&buf[..n]);
        let first_line = request.lines().next().unwrap_or_default();
        // Ignore stray requests (favicon, port scanners) until the real callback arrives.
        if !first_line.contains("/callback") {
            let _ = socket
                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                .await;
            continue;
        }
        let result = parse_callback(first_line, &state);
        let body = match &result {
            Ok(_) => "spotui is logged in. You can close this tab.",
            Err(_) => "Login failed. See the terminal.",
        };
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let _ = socket.write_all(reply.as_bytes()).await;
        break result?;
    };

    let redirect_uri = config.redirect_uri();
    let resp = token_request(&[
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("redirect_uri", &redirect_uri),
        ("client_id", &config.client_id),
        ("code_verifier", &verifier),
    ])
    .await?;

    let tokens = Tokens {
        access_token: resp.access_token,
        refresh_token: resp
            .refresh_token
            .context("token response had no refresh token")?,
        expires_at: now_secs() + resp.expires_in,
    };
    tokens.save()?;
    println!("Logged in. Tokens saved.");
    Ok(())
}

/// Returns a valid access token, refreshing and re-saving it when it is about to expire.
pub async fn access_token(config: &Config) -> Result<String> {
    let mut tokens = Tokens::load()?;
    if tokens.needs_refresh(now_secs()) {
        let resp = token_request(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", &tokens.refresh_token),
            ("client_id", &config.client_id),
        ])
        .await?;
        tokens.access_token = resp.access_token;
        tokens.expires_at = now_secs() + resp.expires_in;
        if let Some(refresh) = resp.refresh_token {
            tokens.refresh_token = refresh;
        }
        tokens.save()?;
    }
    Ok(tokens.access_token)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config {
            client_id: "abc".into(),
            port: 8888,
        }
    }

    #[test]
    fn pkce_challenge_matches_rfc7636_vector() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verifier_uses_only_unreserved_characters() {
        let v = random_string(64);
        assert_eq!(v.len(), 64);
        assert!(v.bytes().all(|b| UNRESERVED.contains(&b)));
    }

    #[test]
    fn authorize_url_carries_pkce_and_loopback_redirect() {
        let url = authorize_url(&config(), "chal", "st");
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["client_id"], "abc");
        assert_eq!(q["redirect_uri"], "http://127.0.0.1:8888/callback");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["code_challenge"], "chal");
        assert_eq!(q["state"], "st");
        assert_eq!(q["scope"], SCOPES);
    }

    #[test]
    fn callback_returns_code_when_state_matches() {
        let line = "GET /callback?code=AQD123&state=st HTTP/1.1";
        assert_eq!(parse_callback(line, "st").unwrap(), "AQD123");
    }

    #[test]
    fn callback_rejects_wrong_state() {
        let line = "GET /callback?code=AQD123&state=evil HTTP/1.1";
        assert!(parse_callback(line, "st").is_err());
    }

    #[test]
    fn callback_reports_access_denied() {
        let line = "GET /callback?error=access_denied&state=st HTTP/1.1";
        let err = parse_callback(line, "st").unwrap_err().to_string();
        assert!(err.contains("access_denied"), "{err}");
    }

    #[test]
    fn scopes_cover_library_and_playlist_reads_with_single_space_separators() {
        let scopes: Vec<&str> = SCOPES.split(' ').collect();
        assert!(scopes.contains(&"user-library-read"));
        assert!(scopes.contains(&"playlist-read-private"));
        assert!(scopes.iter().all(|s| !s.is_empty()), "{SCOPES:?}");
    }

    #[test]
    fn tokens_refresh_inside_the_margin_only() {
        let t = Tokens {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: 1000,
        };
        assert!(!t.needs_refresh(1000 - EXPIRY_MARGIN_SECS - 1));
        assert!(t.needs_refresh(1000 - EXPIRY_MARGIN_SECS));
        assert!(t.needs_refresh(2000));
    }
}
