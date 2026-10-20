use anyhow::{Result, bail};
use serde::Deserialize;

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

pub struct Api {
    http: reqwest::Client,
    token: String,
}

impl Api {
    pub fn new(token: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            token,
        }
    }

    async fn get<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        let resp = self
            .http
            .get(format!("{API_BASE}{path}"))
            .bearer_auth(&self.token)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            bail!(
                "GET {path} returned {status}: {}",
                resp.text().await.unwrap_or_default()
            );
        }
        Ok(resp.json().await?)
    }

    pub async fn me(&self) -> Result<Me> {
        self.get("/me").await
    }

    pub async fn devices(&self) -> Result<Vec<Device>> {
        Ok(self.get::<Devices>("/me/player/devices").await?.devices)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
