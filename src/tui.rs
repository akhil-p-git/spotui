//! Terminal rendering and the event loop that ties input, timers and API calls together.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event};
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Gauge, List, ListItem, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::api::Api;
use crate::app::{Action, App, Mode, Msg};
use crate::{auth, format_ms};

const HELP: &str =
    "/ search  enter play  a queue  space pause  n/p skip  +/- vol  d device  q quit";
const POLL_EVERY_TICKS: u32 = 5;
/// Spotify reports the old state if asked immediately after a command.
const SETTLE_DELAY: Duration = Duration::from_millis(400);

pub fn draw(frame: &mut Frame, app: &mut App, now: Instant) {
    let [search_area, list_area, now_area, status_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(4),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_search(frame, app, search_area);
    draw_results(frame, app, list_area);
    draw_now_playing(frame, app, now_area, now);
    draw_status(frame, app, status_area);
    if app.mode == Mode::Devices {
        draw_devices(frame, app);
    }
}

fn draw_search(frame: &mut Frame, app: &App, area: Rect) {
    let searching = app.mode == Mode::Search;
    let border = if searching {
        Color::Yellow
    } else {
        Color::DarkGray
    };
    let cursor = if searching { "\u{258f}" } else { "" };
    let block = Block::bordered()
        .title(" Search (/) ")
        .border_style(Style::new().fg(border));
    frame.render_widget(
        Paragraph::new(format!("{}{cursor}", app.query)).block(block),
        area,
    );
}

fn draw_results(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::bordered().title(" Results ");
    if app.results.is_empty() {
        frame.render_widget(
            Paragraph::new("Press / to search for tracks.").block(block),
            area,
        );
        return;
    }
    let items: Vec<ListItem> = app
        .results
        .iter()
        .map(|t| {
            let artists: Vec<&str> = t.artists.iter().map(|a| a.name.as_str()).collect();
            ListItem::new(Line::from(vec![
                Span::raw(t.name.clone()),
                Span::styled(
                    format!(
                        "  {}  {}  {}",
                        artists.join(", "),
                        t.album.name,
                        format_ms(t.duration_ms)
                    ),
                    Style::new().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, area, &mut app.results_state);
}

fn draw_now_playing(frame: &mut Frame, app: &App, area: Rect, now: Instant) {
    let block = Block::bordered().title(" Now playing ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [text_area, gauge_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(inner);

    let Some(playback) = &app.playback else {
        frame.render_widget(
            Paragraph::new("Nothing playing. Press d to pick a device."),
            text_area,
        );
        return;
    };
    let state = if playback.is_playing {
        "Playing"
    } else {
        "Paused"
    };
    let title = match &playback.item {
        Some(item) => {
            let artists: Vec<&str> = item.artists.iter().map(|a| a.name.as_str()).collect();
            if artists.is_empty() {
                item.name.clone()
            } else {
                format!("{} - {}", item.name, artists.join(", "))
            }
        }
        None => "(no track)".to_string(),
    };
    let device = playback
        .device
        .as_ref()
        .map(|d| match d.volume_percent {
            Some(v) => format!("  [{} {v}%]", d.name),
            None => format!("  [{}]", d.name),
        })
        .unwrap_or_default();
    frame.render_widget(
        Paragraph::new(format!("{state}: {title}{device}")),
        text_area,
    );

    if let (Some(progress), Some(item)) = (app.progress_ms(now), &playback.item) {
        let ratio = if item.duration_ms == 0 {
            0.0
        } else {
            progress as f64 / item.duration_ms as f64
        };
        let gauge = Gauge::default()
            .ratio(ratio.clamp(0.0, 1.0))
            .label(format!(
                "{} / {}",
                format_ms(progress),
                format_ms(item.duration_ms)
            ))
            .gauge_style(Style::new().fg(Color::Green));
        frame.render_widget(gauge, gauge_area);
    }
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let paragraph = if app.status.is_empty() {
        Paragraph::new(HELP).style(Style::new().fg(Color::DarkGray))
    } else if app.status_is_error {
        Paragraph::new(app.status.as_str()).style(Style::new().fg(Color::Red))
    } else {
        Paragraph::new(app.status.as_str())
    };
    frame.render_widget(paragraph, area);
}

fn draw_devices(frame: &mut Frame, app: &mut App) {
    let [area] = Layout::vertical([Constraint::Length(10)])
        .flex(Flex::Center)
        .areas(frame.area());
    let [area] = Layout::horizontal([Constraint::Percentage(60)])
        .flex(Flex::Center)
        .areas(area);
    frame.render_widget(Clear, area);
    let block = Block::bordered().title(" Devices (enter select, esc close) ");
    if app.devices.is_empty() {
        frame.render_widget(Paragraph::new("Loading devices...").block(block), area);
        return;
    }
    let items: Vec<ListItem> = app
        .devices
        .iter()
        .map(|d| {
            let mark = if d.is_active { "*" } else { " " };
            let volume = d
                .volume_percent
                .map(|v| format!("  {v}%"))
                .unwrap_or_default();
            ListItem::new(format!("{mark} {}  ({}){volume}", d.name, d.kind))
        })
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, area, &mut app.devices_state);
}

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

/// Shared handles for background tasks. Every task fetches a fresh token (which
/// refreshes it when needed) so a session can outlive the one-hour access token.
#[derive(Clone)]
struct Ctx {
    config: Arc<auth::Config>,
    http: reqwest::Client,
    tx: UnboundedSender<Msg>,
}

impl Ctx {
    fn spawn<F, Fut>(&self, task: F)
    where
        F: FnOnce(Api) -> Fut + Send + 'static,
        Fut: Future<Output = Msg> + Send + 'static,
    {
        let ctx = self.clone();
        tokio::spawn(async move {
            let msg = match auth::access_token(&ctx.config).await {
                Ok(token) => task(Api::with_client(ctx.http.clone(), token)).await,
                Err(e) => Msg::Error(err(e)),
            };
            let _ = ctx.tx.send(msg);
        });
    }

    fn refresh_playback(&self) {
        self.spawn(|api| async move { Msg::Playback(api.playback().await.map_err(err)) });
    }

    fn perform(&self, action: Action) {
        match action {
            Action::Search(query) => self.spawn(|api| async move {
                Msg::Results(api.search_tracks(&query, 0).await.map_err(err))
            }),
            Action::Queue { uri, label } => self.spawn(|api| async move {
                match api.queue(&uri).await {
                    Ok(()) => Msg::Status(format!("Queued: {label}")),
                    Err(e) => Msg::Error(err(e)),
                }
            }),
            Action::LoadDevices => {
                self.spawn(|api| async move { Msg::Devices(api.devices().await.map_err(err)) })
            }
            Action::Play(uri) => self.control(move |api| async move { api.play(Some(&uri)).await }),
            Action::Pause => self.control(|api| async move { api.pause().await }),
            Action::Resume => self.control(|api| async move { api.play(None).await }),
            Action::Next => self.control(|api| async move { api.next().await }),
            Action::Previous => self.control(|api| async move { api.previous().await }),
            Action::Volume(percent) => {
                self.control(move |api| async move { api.set_volume(percent).await })
            }
            Action::Transfer(id) => self.control(move |api| async move { api.transfer(&id).await }),
        }
    }

    /// Runs a player command, waits briefly for Spotify to settle, then reports the new state.
    fn control<F, Fut>(&self, command: F)
    where
        F: FnOnce(Arc<Api>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        self.spawn(|api| async move {
            let api = Arc::new(api);
            if let Err(e) = command(api.clone()).await {
                return Msg::Error(err(e));
            }
            tokio::time::sleep(SETTLE_DELAY).await;
            Msg::Playback(api.playback().await.map_err(err))
        });
    }
}

/// Forwards terminal key events from a blocking reader thread into the async loop.
fn spawn_input(tx: UnboundedSender<Msg>) {
    std::thread::spawn(move || {
        while let Ok(event) = event::read() {
            if let Event::Key(key) = event
                && tx.send(Msg::Key(key)).is_err()
            {
                break;
            }
        }
    });
}

async fn event_loop(terminal: &mut DefaultTerminal, config: auth::Config) -> Result<()> {
    let (tx, mut rx) = unbounded_channel();
    spawn_input(tx.clone());
    let ctx = Ctx {
        config: Arc::new(config),
        http: reqwest::Client::new(),
        tx,
    };
    let mut app = App::new(Instant::now());
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut ticks = 0u32;
    ctx.refresh_playback();

    while !app.quit {
        terminal.draw(|frame| draw(frame, &mut app, Instant::now()))?;
        tokio::select! {
            Some(msg) = rx.recv() => match msg {
                Msg::Key(key) => {
                    if let Some(action) = app.on_key(key) {
                        ctx.perform(action);
                    }
                }
                other => app.apply(other, Instant::now()),
            },
            _ = tick.tick() => {
                ticks += 1;
                if ticks.is_multiple_of(POLL_EVERY_TICKS) {
                    ctx.refresh_playback();
                }
            }
        }
    }
    Ok(())
}

pub async fn run(config: auth::Config) -> Result<()> {
    // Fail before taking over the terminal if there is no saved login.
    auth::access_token(&config).await?;
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, config).await;
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::api::{Album, Artist, Device, Item, Playback, Track};

    fn render(app: &mut App, now: Instant) -> String {
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal.draw(|frame| draw(frame, app, now)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        buffer
            .content()
            .chunks(buffer.area.width as usize)
            .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn sample_device() -> Device {
        Device {
            id: Some("d1".into()),
            name: "spotifyd".into(),
            kind: "Computer".into(),
            is_active: true,
            volume_percent: Some(40),
        }
    }

    #[test]
    fn empty_state_shows_search_hint_and_key_help() {
        let now = Instant::now();
        let screen = render(&mut App::new(now), now);
        assert!(screen.contains("Press / to search for tracks."), "{screen}");
        assert!(screen.contains("Nothing playing"), "{screen}");
        assert!(screen.contains("q quit"), "{screen}");
    }

    #[test]
    fn results_and_now_playing_are_drawn() {
        let t0 = Instant::now();
        let mut app = App::new(t0);
        let track = Track {
            uri: "spotify:track:x".into(),
            name: "Get Lucky".into(),
            duration_ms: 369_000,
            artists: vec![Artist {
                name: "Daft Punk".into(),
            }],
            album: Album { name: "RAM".into() },
        };
        app.apply(Msg::Results(Ok(vec![track])), t0);
        let playback = Playback {
            is_playing: true,
            progress_ms: Some(60_000),
            device: Some(sample_device()),
            item: Some(Item {
                name: "One More Time".into(),
                duration_ms: 320_000,
                artists: vec![Artist {
                    name: "Daft Punk".into(),
                }],
            }),
        };
        app.apply(Msg::Playback(Ok(Some(playback))), t0);

        let screen = render(&mut app, t0 + Duration::from_secs(5));
        assert!(screen.contains("Get Lucky"), "{screen}");
        assert!(screen.contains("RAM"), "{screen}");
        assert!(
            screen.contains("Playing: One More Time - Daft Punk  [spotifyd 40%]"),
            "{screen}"
        );
        assert!(screen.contains("1:05 / 5:20"), "{screen}");
    }

    #[test]
    fn error_status_replaces_the_key_help() {
        let now = Instant::now();
        let mut app = App::new(now);
        app.apply(Msg::Error("boom".into()), now);
        let screen = render(&mut app, now);
        assert!(screen.contains("boom"), "{screen}");
        assert!(!screen.contains("q quit"), "{screen}");
    }

    #[test]
    fn device_popup_lists_devices() {
        let now = Instant::now();
        let mut app = App::new(now);
        app.mode = Mode::Devices;
        app.apply(Msg::Devices(Ok(vec![sample_device()])), now);
        let screen = render(&mut app, now);
        assert!(screen.contains("Devices"), "{screen}");
        assert!(screen.contains("spotifyd  (Computer)  40%"), "{screen}");
    }
}
