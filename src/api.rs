use anyhow::{Result, bail};
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

const API_BASE: &str = "https://api.spotify.com/v1";

#[derive(Debug, Deserialize)]
pub struct Me {
    pub id: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Device {
    pub id: Option<String>,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub is_active: bool,
    pub volume_percent: Option<u8>,
}

#[derive(Deserialize)]
struct Devices {
    devices: Vec<Device>,
}

#[derive(Debug, Deserialize)]
pub struct Artist {
    pub name: String,
}

/// A track or episode. Episodes carry no `artists`.
#[derive(Debug, Deserialize)]
pub struct Item {
    pub name: String,
    pub duration_ms: u64,
    #[serde(default)]
    pub artists: Vec<Artist>,
}

#[derive(Debug, Deserialize)]
pub struct Playback {
    pub is_playing: bool,
    pub progress_ms: Option<u64>,
    pub device: Option<Device>,
    /// Null between tracks and during ads.
    pub item: Option<Item>,
}

#[derive(Debug, Deserialize)]
pub struct Album {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct Track {
    pub uri: String,
    pub name: String,
    pub duration_ms: u64,
    pub artists: Vec<Artist>,
    pub album: Album,
}

#[derive(Deserialize)]
struct SearchResponse {
    tracks: SearchTracks,
}

#[derive(Deserialize)]
struct SearchTracks {
    items: Vec<Track>,
}

/// Request body for `PUT /me/player/play`: single tracks and episodes are
/// played by `uris`, anything else (album, playlist, artist) as a `context_uri`.
fn play_body(uri: &str) -> Value {
    if uri.starts_with("spotify:track:") || uri.starts_with("spotify:episode:") {
        json!({ "uris": [uri] })
    } else {
        json!({ "context_uri": uri })
    }
}

pub struct Api {
    http: reqwest::Client,
    token: String,
    base: String,
}

impl Api {
    pub fn new(token: String) -> Self {
        Self::with_base(token, API_BASE.to_string())
    }

    fn with_base(token: String, base: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            token,
            base,
        }
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> Result<reqwest::Response> {
        let mut req = self
            .http
            .request(method.clone(), format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .query(query);
        if let Some(body) = body {
            req = req.json(&body);
        } else if method != Method::GET {
            // Spotify's player endpoints reject body-less PUT/POST without a length.
            req = req.header(reqwest::header::CONTENT_LENGTH, 0);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let text = resp.text().await.unwrap_or_default();
        let hint = if text.contains("NO_ACTIVE_DEVICE") {
            "\nNo device is playing. Start one, then run `spotui transfer <device name>`."
        } else {
            ""
        };
        bail!("{method} {path} returned {status}: {text}{hint}")
    }

    pub async fn me(&self) -> Result<Me> {
        Ok(self
            .send(Method::GET, "/me", &[], None)
            .await?
            .json()
            .await?)
    }

    pub async fn devices(&self) -> Result<Vec<Device>> {
        let resp = self
            .send(Method::GET, "/me/player/devices", &[], None)
            .await?;
        Ok(resp.json::<Devices>().await?.devices)
    }

    /// `None` when nothing is active (Spotify answers 204).
    pub async fn playback(&self) -> Result<Option<Playback>> {
        let resp = self.send(Method::GET, "/me/player", &[], None).await?;
        if resp.status() == StatusCode::NO_CONTENT {
            return Ok(None);
        }
        Ok(Some(resp.json().await?))
    }

    /// Resumes playback, or starts `uri` when given.
    pub async fn play(&self, uri: Option<&str>) -> Result<()> {
        self.send(Method::PUT, "/me/player/play", &[], uri.map(play_body))
            .await?;
        Ok(())
    }

    pub async fn pause(&self) -> Result<()> {
        self.send(Method::PUT, "/me/player/pause", &[], None)
            .await?;
        Ok(())
    }

    pub async fn next(&self) -> Result<()> {
        self.send(Method::POST, "/me/player/next", &[], None)
            .await?;
        Ok(())
    }

    pub async fn previous(&self) -> Result<()> {
        self.send(Method::POST, "/me/player/previous", &[], None)
            .await?;
        Ok(())
    }

    pub async fn set_volume(&self, percent: u8) -> Result<()> {
        let query = [("volume_percent", percent.to_string())];
        self.send(Method::PUT, "/me/player/volume", &query, None)
            .await?;
        Ok(())
    }

    /// Track search. Spotify caps `limit` at 10 for Development Mode apps.
    pub async fn search_tracks(&self, query: &str, offset: u32) -> Result<Vec<Track>> {
        let params = [
            ("q", query.to_string()),
            ("type", "track".to_string()),
            ("limit", "10".to_string()),
            ("offset", offset.to_string()),
        ];
        let resp = self.send(Method::GET, "/search", &params, None).await?;
        Ok(resp.json::<SearchResponse>().await?.tracks.items)
    }

    /// Appends a track or episode URI to the playback queue.
    pub async fn queue(&self, uri: &str) -> Result<()> {
        let params = [("uri", uri.to_string())];
        self.send(Method::POST, "/me/player/queue", &params, None)
            .await?;
        Ok(())
    }

    /// Moves playback to `device_id` without starting it.
    pub async fn transfer(&self, device_id: &str) -> Result<()> {
        let body = json!({ "device_ids": [device_id], "play": false });
        self.send(Method::PUT, "/me/player", &[], Some(body))
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;

    /// Serves one canned response on a loopback port and returns the raw
    /// request it received (headers and body).
    async fn serve_once(response: impl Into<String>) -> (Api, JoinHandle<String>) {
        let response = response.into();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = socket.read(&mut buf).await.unwrap();
                raw.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&raw);
                if let Some(split) = text.find("\r\n\r\n") {
                    let length = text[..split]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if raw.len() >= split + 4 + length {
                        break;
                    }
                }
            }
            socket.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8(raw).unwrap()
        });
        (Api::with_base("tok".into(), base), handle)
    }

    const NO_CONTENT: &str = "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n";

    #[test]
    fn devices_parse_from_api_shape() {
        let json = r#"{"devices":[
            {"id":"d1","is_active":true,"is_private_session":false,"is_restricted":false,
             "name":"spotifyd","type":"Computer","volume_percent":55,"supports_volume":true},
            {"id":null,"is_active":false,"name":"Phone","type":"Smartphone","volume_percent":null}
        ]}"#;
        let parsed: Devices = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.devices.len(), 2);
        assert_eq!(parsed.devices[0].kind, "Computer");
        assert_eq!(parsed.devices[0].volume_percent, Some(55));
        assert_eq!(parsed.devices[1].id, None);
    }

    #[test]
    fn me_parses_without_removed_fields() {
        // Since Feb 2026 /me no longer returns country, email, or product.
        let me: Me = serde_json::from_str(r#"{"id":"u1","display_name":null}"#).unwrap();
        assert_eq!(me.id, "u1");
        assert!(me.display_name.is_none());
    }

    #[test]
    fn playback_parses_track_and_episode_and_null_item() {
        let track = r#"{"is_playing":true,"progress_ms":83000,
            "device":{"id":"d1","is_active":true,"name":"spotifyd","type":"Computer","volume_percent":40},
            "item":{"name":"Song","duration_ms":225000,"artists":[{"name":"A"},{"name":"B"}]}}"#;
        let p: Playback = serde_json::from_str(track).unwrap();
        assert_eq!(p.item.unwrap().artists.len(), 2);

        let episode = r#"{"is_playing":false,"item":{"name":"Ep 1","duration_ms":1000}}"#;
        let p: Playback = serde_json::from_str(episode).unwrap();
        assert!(p.item.unwrap().artists.is_empty());
        assert!(p.progress_ms.is_none());

        let ad = r#"{"is_playing":true,"progress_ms":0,"item":null}"#;
        assert!(serde_json::from_str::<Playback>(ad).unwrap().item.is_none());
    }

    #[test]
    fn play_body_picks_uris_or_context() {
        assert_eq!(
            play_body("spotify:track:abc"),
            json!({"uris": ["spotify:track:abc"]})
        );
        assert_eq!(
            play_body("spotify:album:xyz"),
            json!({"context_uri": "spotify:album:xyz"})
        );
    }

    #[tokio::test]
    async fn pause_sends_authorized_put_with_empty_body() {
        let (api, request) = serve_once(NO_CONTENT).await;
        api.pause().await.unwrap();
        let raw = request.await.unwrap().to_ascii_lowercase();
        assert!(raw.starts_with("put /me/player/pause "), "{raw}");
        assert!(raw.contains("authorization: bearer tok"), "{raw}");
        assert!(raw.contains("content-length: 0"), "{raw}");
    }

    #[tokio::test]
    async fn volume_sends_percent_in_query() {
        let (api, request) = serve_once(NO_CONTENT).await;
        api.set_volume(40).await.unwrap();
        let raw = request.await.unwrap();
        assert!(
            raw.starts_with("PUT /me/player/volume?volume_percent=40 "),
            "{raw}"
        );
    }

    #[tokio::test]
    async fn play_with_track_sends_uris_body() {
        let (api, request) = serve_once(NO_CONTENT).await;
        api.play(Some("spotify:track:abc")).await.unwrap();
        let raw = request.await.unwrap();
        assert!(raw.starts_with("PUT /me/player/play "), "{raw}");
        assert!(raw.ends_with(r#"{"uris":["spotify:track:abc"]}"#), "{raw}");
    }

    #[tokio::test]
    async fn transfer_names_device_and_does_not_autoplay() {
        let (api, request) = serve_once(NO_CONTENT).await;
        api.transfer("dev1").await.unwrap();
        let raw = request.await.unwrap();
        assert!(raw.starts_with("PUT /me/player "), "{raw}");
        assert!(
            raw.ends_with(r#"{"device_ids":["dev1"],"play":false}"#),
            "{raw}"
        );
    }

    #[tokio::test]
    async fn search_sends_encoded_query_and_parses_tracks() {
        let body = r#"{"tracks":{"href":"x","total":1,"items":[
            {"uri":"spotify:track:abc","name":"Get Lucky","duration_ms":369000,
             "artists":[{"name":"Daft Punk"}],"album":{"name":"RAM","id":"a1"},"is_local":false}]}}"#;
        let (api, request) = serve_once(format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ))
        .await;
        let tracks = api.search_tracks("daft punk & co", 10).await.unwrap();
        let raw = request.await.unwrap();
        let line = raw.lines().next().unwrap();
        assert!(
            line.starts_with("GET /search?q=daft+punk+%26+co&type=track&limit=10&offset=10 "),
            "{line}"
        );
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].uri, "spotify:track:abc");
        assert_eq!(tracks[0].album.name, "RAM");
    }

    #[tokio::test]
    async fn queue_sends_uri_in_query_with_post() {
        let (api, request) = serve_once(NO_CONTENT).await;
        api.queue("spotify:track:abc").await.unwrap();
        let raw = request.await.unwrap();
        assert!(
            raw.starts_with("POST /me/player/queue?uri=spotify%3Atrack%3Aabc "),
            "{raw}"
        );
    }

    #[tokio::test]
    async fn playback_returns_none_on_204() {
        let (api, _request) = serve_once(NO_CONTENT).await;
        assert!(api.playback().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn no_active_device_error_explains_next_step() {
        let body = r#"{"error":{"status":404,"message":"x","reason":"NO_ACTIVE_DEVICE"}}"#;
        let (api, _request) = serve_once(format!(
            "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ))
        .await;
        let err = api.pause().await.unwrap_err().to_string();
        assert!(err.contains("404"), "{err}");
        assert!(err.contains("spotui transfer"), "{err}");
    }
}
