mod api;
mod auth;

use anyhow::{Result, bail};

#[tokio::main]
async fn main() -> Result<()> {
    let command = std::env::args().nth(1).unwrap_or_default();
    let config = auth::Config::load()?;

    match command.as_str() {
        "login" => auth::login(&config).await,
        "whoami" => {
            let me = api::Api::new(auth::access_token(&config).await?)
                .me()
                .await?;
            println!("{} ({})", me.display_name.unwrap_or_default(), me.id);
            Ok(())
        }
        "devices" => {
            let devices = api::Api::new(auth::access_token(&config).await?)
                .devices()
                .await?;
            if devices.is_empty() {
                println!("No devices. Start spotifyd (or open a Spotify app) and retry.");
            }
            for d in devices {
                let active = if d.is_active { "*" } else { " " };
                let volume = d
                    .volume_percent
                    .map(|v| format!("{v}%"))
                    .unwrap_or_else(|| "-".into());
                println!("{active} {:<24} {:<12} {volume}", d.name, d.kind);
            }
            Ok(())
        }
        _ => bail!("usage: spotui <login|whoami|devices>"),
    }
}
