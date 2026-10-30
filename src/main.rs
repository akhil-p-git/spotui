mod api;
mod app;
mod auth;
mod tui;

use anyhow::{Context, Result, bail};

const USAGE: &str = "usage: spotui [tui] | spotui <login|whoami|devices|now|play [uri]|pause|next|prev|restart|shuffle <on|off>|repeat <off|all|one>|volume <0-100>|transfer <device name>|search <query>|queue <uri>>";

fn format_ms(ms: u64) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn format_track(number: usize, track: &api::Track) -> String {
    let artists: Vec<&str> = track.artists.iter().map(|a| a.name.as_str()).collect();
    let album = track
        .album
        .as_ref()
        .map(|a| format!(" ({})", a.name))
        .unwrap_or_default();
    format!(
        "{number:>2}. {} - {}{album} {}\n    {}",
        track.name,
        artists.join(", "),
        format_ms(track.duration_ms),
        track.uri
    )
}

fn describe(playback: Option<api::Playback>) -> String {
    let Some(p) = playback else {
        return "Nothing playing.".into();
    };
    let state = if p.is_playing { "Playing" } else { "Paused" };
    let track = match &p.item {
        Some(item) => {
            let artists: Vec<&str> = item.artists.iter().map(|a| a.name.as_str()).collect();
            let progress = format_ms(p.progress_ms.unwrap_or(0));
            let by = if artists.is_empty() {
                String::new()
            } else {
                format!(" - {}", artists.join(", "))
            };
            format!(
                "{}{by} [{progress}/{}]",
                item.name,
                format_ms(item.duration_ms)
            )
        }
        None => "(no track)".into(),
    };
    let on = p
        .device
        .map(|d| match d.volume_percent {
            Some(v) => format!(" on {} ({v}%)", d.name),
            None => format!(" on {}", d.name),
        })
        .unwrap_or_default();
    format!("{state}: {track}{on}")
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_default();
    let arg = args.next();
    let rest: Vec<String> = args.collect();
    let config = auth::Config::load()?;

    if command == "login" {
        return auth::login(&config).await;
    }
    if command.is_empty() || command == "tui" {
        return tui::run(config).await;
    }
    if !matches!(
        command.as_str(),
        "whoami"
            | "devices"
            | "now"
            | "play"
            | "pause"
            | "next"
            | "prev"
            | "restart"
            | "shuffle"
            | "repeat"
            | "volume"
            | "transfer"
            | "search"
            | "queue"
    ) {
        bail!("{USAGE}");
    }
    let api = api::Api::new(auth::access_token(&config).await?);

    match command.as_str() {
        "whoami" => {
            let me = api.me().await?;
            println!("{} ({})", me.display_name.unwrap_or_default(), me.id);
        }
        "devices" => {
            let devices = api.devices().await?;
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
        }
        "now" => println!("{}", describe(api.playback().await?)),
        "search" => {
            let query = arg.into_iter().chain(rest).collect::<Vec<_>>().join(" ");
            if query.is_empty() {
                bail!("search takes a query, e.g. `spotui search daft punk`");
            }
            let tracks = api.search_tracks(&query, 0).await?;
            if tracks.is_empty() {
                println!("No tracks found.");
            }
            for (i, track) in tracks.iter().enumerate() {
                println!("{}", format_track(i + 1, track));
            }
        }
        "queue" => {
            let uri = arg.context("queue takes a track URI, e.g. from `spotui search`")?;
            api.queue(&uri).await?;
        }
        "play" => api.play(arg.as_deref()).await?,
        "pause" => api.pause().await?,
        "next" => api.next().await?,
        "prev" => api.previous().await?,
        "restart" => api.seek(0).await?,
        "shuffle" => {
            let on = match arg.as_deref() {
                Some("on") => true,
                Some("off") => false,
                _ => bail!("shuffle takes on or off"),
            };
            api.set_shuffle(on).await?;
        }
        "repeat" => {
            let mode = match arg.as_deref() {
                Some("off") => api::Repeat::Off,
                Some("all") => api::Repeat::Context,
                Some("one") => api::Repeat::Track,
                _ => bail!("repeat takes off, all or one"),
            };
            api.set_repeat(mode).await?;
        }
        "volume" => {
            let percent: u8 = arg
                .as_deref()
                .and_then(|v| v.parse().ok())
                .filter(|v| *v <= 100)
                .context("volume takes a number from 0 to 100")?;
            api.set_volume(percent).await?;
        }
        "transfer" => {
            let name = arg.context("transfer takes a device name; see `spotui devices`")?;
            let devices = api.devices().await?;
            let device = devices
                .iter()
                .find(|d| d.name.eq_ignore_ascii_case(&name))
                .with_context(|| {
                    let names: Vec<&str> = devices.iter().map(|d| d.name.as_str()).collect();
                    format!("no device named {name:?}; available: {names:?}")
                })?;
            let id = device
                .id
                .as_deref()
                .context("that device has no id and cannot be targeted")?;
            api.transfer(id).await?;
        }
        _ => unreachable!("checked above"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Artist, Device, Item, Playback};

    #[test]
    fn format_ms_pads_seconds() {
        assert_eq!(format_ms(0), "0:00");
        assert_eq!(format_ms(83_000), "1:23");
        assert_eq!(format_ms(3_605_000), "60:05");
    }

    #[test]
    fn describe_handles_idle_and_playing() {
        assert_eq!(describe(None), "Nothing playing.");
        let p = Playback {
            is_playing: true,
            shuffle_state: false,
            repeat_state: api::Repeat::Off,
            progress_ms: Some(83_000),
            device: Some(Device {
                id: Some("d".into()),
                name: "spotifyd".into(),
                kind: "Computer".into(),
                is_active: true,
                volume_percent: Some(40),
            }),
            item: Some(Item {
                uri: "spotify:track:s".into(),
                name: "Song".into(),
                duration_ms: 225_000,
                artists: vec![Artist { name: "A".into() }, Artist { name: "B".into() }],
            }),
        };
        assert_eq!(
            describe(Some(p)),
            "Playing: Song - A, B [1:23/3:45] on spotifyd (40%)"
        );
    }

    #[test]
    fn format_track_shows_artists_album_length_and_uri() {
        let track = api::Track {
            uri: "spotify:track:abc".into(),
            name: "Get Lucky".into(),
            duration_ms: 369_000,
            artists: vec![Artist {
                name: "Daft Punk".into(),
            }],
            album: Some(api::Album { name: "RAM".into() }),
        };
        assert_eq!(
            format_track(1, &track),
            " 1. Get Lucky - Daft Punk (RAM) 6:09\n    spotify:track:abc"
        );
    }

    #[test]
    fn describe_handles_missing_item() {
        let p = Playback {
            is_playing: false,
            shuffle_state: false,
            repeat_state: api::Repeat::Off,
            progress_ms: None,
            device: None,
            item: None,
        };
        assert_eq!(describe(Some(p)), "Paused: (no track)");
    }
}
