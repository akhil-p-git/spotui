//! Terminal rendering and the event loop that ties input, timers and API calls together.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event};
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Gauge, List, ListItem, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::api::{Api, Track};
use crate::app::{Action, App, Listing, MENU, Mode, Msg, View};
use crate::{auth, format_ms};

/// Key hints for the current screen, each short enough for an 80-column terminal.
fn help(view: View) -> &'static str {
    match view {
        View::Menu => "up/down move  right open  / search  space pause  n/p skip  d device  q quit",
        View::Search | View::Liked => {
            "left back  enter play  l like  A add  a queue  / search  space pause  q quit"
        }
        View::Playlists => {
            "left back  right open  P play playlist  / search  space pause  n/p skip  q quit"
        }
        View::PlaylistTracks => {
            "left back  enter play  l like  A add  x remove  P play all  a queue  q quit"
        }
    }
}
const POLL_EVERY_TICKS: u32 = 5;
/// Spotify reports the old state if asked immediately after a command.
const SETTLE_DELAY: Duration = Duration::from_millis(400);

pub fn draw(frame: &mut Frame, app: &mut App, now: Instant) {
    let search_height = if app.view == View::Search { 3 } else { 0 };
    let [search_area, list_area, now_area, status_area] = Layout::vertical([
        Constraint::Length(search_height),
        Constraint::Min(3),
        Constraint::Length(4),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    if app.view == View::Search {
        draw_search(frame, app, search_area);
    }
    draw_main(frame, app, list_area);
    draw_now_playing(frame, app, now_area, now);
    draw_status(frame, app, status_area);
    match app.mode {
        Mode::Devices => draw_devices(frame, app),
        Mode::Picker => draw_picker(frame, app),
        Mode::NewPlaylist => draw_new_playlist(frame, app),
        Mode::Browse | Mode::Search => {}
    }
}

/// A popup `height` rows tall and `percent` of the screen wide, centred.
fn centered(frame: &Frame, height: u16, percent: u16) -> Rect {
    let [area] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(frame.area());
    let [area] = Layout::horizontal([Constraint::Percentage(percent)])
        .flex(Flex::Center)
        .areas(area);
    area
}

fn draw_picker(frame: &mut Frame, app: &mut App) {
    let mut items = vec![ListItem::new("+ New playlist...")];
    items.extend(app.picker_playlists().into_iter().map(|p| {
        let count = p.items.as_ref().map(|c| format!("  {} tracks", c.total));
        ListItem::new(Line::from(vec![
            Span::raw(p.name.clone()),
            dim(count.unwrap_or_default()),
        ]))
    }));
    let rows = items.len() as u16;
    let area = centered(frame, (rows + 2).min(14), 60);
    frame.render_widget(Clear, area);
    let title = match &app.picker_track {
        Some(track) => format!(" Add \"{}\" to... (enter, esc) ", track.name),
        None => " Add to... ".to_string(),
    };
    let list = List::new(items)
        .block(Block::bordered().title(title))
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, area, &mut app.picker_state);
}

fn draw_new_playlist(frame: &mut Frame, app: &App) {
    let area = centered(frame, 3, 60);
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .title(" New playlist name (enter create, esc back) ")
        .border_style(Style::new().fg(Color::Yellow));
    frame.render_widget(
        Paragraph::new(format!("{}\u{258f}", app.new_name)).block(block),
        area,
    );
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

fn dim(text: String) -> Span<'static> {
    Span::styled(text, Style::new().fg(Color::DarkGray))
}

/// Rows for `tracks`, with a red heart on the ones known to be in Liked Songs.
fn track_items(tracks: &[Track], liked: &HashMap<String, bool>) -> Vec<ListItem<'static>> {
    tracks
        .iter()
        .map(|t| {
            let heart = if liked.get(&t.uri).copied().unwrap_or(false) {
                Span::styled("\u{2665} ", Style::new().fg(Color::Red))
            } else {
                Span::raw("  ")
            };
            let artists: Vec<&str> = t.artists.iter().map(|a| a.name.as_str()).collect();
            let mut details = Vec::new();
            if !artists.is_empty() {
                details.push(artists.join(", "));
            }
            if let Some(album) = &t.album {
                details.push(album.name.clone());
            }
            details.push(format_ms(t.duration_ms));
            ListItem::new(Line::from(vec![
                heart,
                Span::raw(t.name.clone()),
                dim(format!("  {}", details.join("  "))),
            ]))
        })
        .collect()
}

/// Draws `items` as a selectable list, or `empty` when there is nothing to show.
fn draw_list(
    frame: &mut Frame,
    area: Rect,
    title: String,
    items: Vec<ListItem<'static>>,
    state: &mut ListState,
    empty: &str,
) {
    let block = Block::bordered().title(title);
    if items.is_empty() {
        frame.render_widget(Paragraph::new(empty.to_string()).block(block), area);
        return;
    }
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, area, state);
}

/// What to show in place of a list that has no rows.
fn empty<T>(listing: &Listing<T>, idle: &'static str) -> &'static str {
    if listing.loading {
        "Loading..."
    } else if !listing.loaded {
        "Could not load this list. Press left, then right to retry."
    } else {
        idle
    }
}

fn draw_main(frame: &mut Frame, app: &mut App, area: Rect) {
    match app.view {
        View::Menu => {
            let items = MENU.iter().map(|entry| ListItem::new(*entry)).collect();
            draw_list(
                frame,
                area,
                " Library ".into(),
                items,
                &mut app.menu_state,
                "",
            );
        }
        View::Search => draw_list(
            frame,
            area,
            " Results ".into(),
            track_items(&app.results, &app.liked_flags),
            &mut app.results_state,
            "Press / to search for tracks.",
        ),
        View::Liked => {
            let none = empty(&app.liked, "No liked songs.");
            draw_list(
                frame,
                area,
                format!(
                    " Liked Songs ({}/{}) ",
                    app.liked.items.len(),
                    app.liked.total
                ),
                track_items(&app.liked.items, &app.liked_flags),
                &mut app.liked.state,
                none,
            );
        }
        View::Playlists => {
            let items = app
                .playlists
                .items
                .iter()
                .map(|p| {
                    let count = p.items.as_ref().map(|c| format!("  {} tracks", c.total));
                    let owner = p
                        .owner
                        .as_ref()
                        .and_then(|o| o.display_name.as_ref())
                        .map(|name| format!("  by {name}"));
                    ListItem::new(Line::from(vec![
                        Span::raw(p.name.clone()),
                        dim(format!(
                            "{}{}",
                            count.unwrap_or_default(),
                            owner.unwrap_or_default()
                        )),
                    ]))
                })
                .collect();
            let none = empty(&app.playlists, "No playlists.");
            draw_list(
                frame,
                area,
                format!(
                    " Playlists ({}/{}) ",
                    app.playlists.items.len(),
                    app.playlists.total
                ),
                items,
                &mut app.playlists.state,
                none,
            );
        }
        View::PlaylistTracks => {
            let name = app.open_playlist.as_ref().map_or("Playlist", |p| &p.name);
            let title = format!(
                " {name} ({}/{}) ",
                app.playlist_tracks.items.len(),
                app.playlist_tracks.total
            );
            let none = empty(&app.playlist_tracks, "No tracks to show.");
            draw_list(
                frame,
                area,
                title,
                track_items(&app.playlist_tracks.items, &app.liked_flags),
                &mut app.playlist_tracks.state,
                none,
            );
        }
    }
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
        Paragraph::new(help(app.view)).style(Style::new().fg(Color::DarkGray))
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
            Action::LoadLiked { offset } => self.spawn(move |api| async move {
                Msg::Liked {
                    offset,
                    result: api.saved_tracks(offset).await.map_err(err),
                }
            }),
            Action::LoadPlaylists { offset } => self.spawn(move |api| async move {
                Msg::Playlists {
                    offset,
                    result: api.playlists(offset).await.map_err(err),
                }
            }),
            Action::LoadPlaylistTracks { id, offset } => self.spawn(move |api| async move {
                let result = api.playlist_tracks(&id, offset).await.map_err(err);
                Msg::PlaylistTracks { id, offset, result }
            }),
            Action::CheckLiked(uris) => self.spawn(move |api| async move {
                let result = api.saved(&uris).await.map_err(err);
                Msg::LikedFlags { uris, result }
            }),
            Action::SetLiked { uri, liked, label } => self.spawn(move |api| async move {
                let uris = [uri.clone()];
                let result = if liked {
                    api.save(&uris).await
                } else {
                    api.unsave(&uris).await
                };
                Msg::LikeChanged {
                    uri,
                    liked,
                    label,
                    result: result.map_err(err),
                }
            }),
            Action::CreatePlaylist { name, uri, label } => self.spawn(move |api| async move {
                match api.create_playlist(&name).await {
                    Ok(playlist) => {
                        let added = api.add_to_playlist(&playlist.id, &uri).await.map_err(err);
                        Msg::Created {
                            label,
                            created: Ok(playlist),
                            added,
                        }
                    }
                    Err(e) => Msg::Created {
                        label,
                        created: Err(err(e)),
                        added: Ok(()),
                    },
                }
            }),
            Action::AddToPlaylist {
                playlist_id,
                playlist_name,
                uri,
                label,
            } => self.spawn(move |api| async move {
                let result = api.add_to_playlist(&playlist_id, &uri).await.map_err(err);
                Msg::Added {
                    playlist_id,
                    playlist_name,
                    label,
                    result,
                }
            }),
            Action::RemoveFromPlaylist {
                playlist_id,
                playlist_name,
                uri,
                label,
            } => self.spawn(move |api| async move {
                let result = api
                    .remove_from_playlist(&playlist_id, &uri)
                    .await
                    .map_err(err);
                Msg::Removed {
                    playlist_id,
                    playlist_name,
                    uri,
                    label,
                    result,
                }
            }),
            Action::PlayUris(uris) => {
                self.control(move |api| async move { api.play_uris(&uris).await })
            }
            Action::PlayContext { context_uri, from } => self.control(move |api| async move {
                api.play_context(&context_uri, from.as_deref()).await
            }),
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
    // Needed to tell which playlists the user can add to.
    ctx.spawn(|api| async move { Msg::Me(api.me().await.map(|me| me.id).map_err(err)) });

    while !app.quit {
        terminal.draw(|frame| draw(frame, &mut app, Instant::now()))?;
        tokio::select! {
            Some(msg) = rx.recv() => {
                let follow_up = match msg {
                    Msg::Key(key) => app.on_key(key),
                    other => app.apply(other, Instant::now()),
                };
                if let Some(action) = follow_up {
                    ctx.perform(action);
                }
            }
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
    use crate::api::{Album, Artist, Count, Device, Item, Owner, Page, Playback, Playlist, Track};

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

    fn track(name: &str, artists: &[&str], album: Option<&str>) -> Track {
        Track {
            uri: format!("spotify:track:{name}"),
            name: name.into(),
            duration_ms: 185_000,
            artists: artists
                .iter()
                .map(|a| Artist {
                    name: (*a).to_string(),
                })
                .collect(),
            album: album.map(|a| Album { name: a.into() }),
        }
    }

    fn page<T>(items: Vec<T>, total: u32) -> Page<T> {
        let fetched = items.len() as u32;
        Page {
            items,
            total,
            fetched,
        }
    }

    fn playlist(id: &str, name: &str, owner: &str, count: Option<u32>) -> Playlist {
        Playlist {
            id: id.into(),
            name: name.into(),
            uri: format!("spotify:playlist:{id}"),
            items: count.map(|total| Count { total }),
            owner: Some(Owner {
                id: Some(owner.into()),
                display_name: Some(owner.into()),
            }),
            collaborative: false,
        }
    }

    #[test]
    fn liked_tracks_get_a_heart_and_others_do_not() {
        let t0 = Instant::now();
        let mut app = App::new(t0);
        app.view = View::Search;
        let tracks = vec![
            track("Loved Song", &["A"], None),
            track("Plain Song", &["A"], None),
        ];
        app.apply(Msg::Results(Ok(tracks)), t0);
        app.apply(
            Msg::LikedFlags {
                uris: vec![
                    "spotify:track:Loved Song".into(),
                    "spotify:track:Plain Song".into(),
                ],
                result: Ok(vec![true, false]),
            },
            t0,
        );
        let screen = render(&mut app, t0);
        assert!(screen.contains("\u{2665} Loved Song"), "{screen}");
        assert!(!screen.contains("\u{2665} Plain Song"), "{screen}");
        assert!(screen.contains("Plain Song"), "{screen}");
    }

    #[test]
    fn the_picker_lists_new_playlist_and_only_writable_playlists() {
        let t0 = Instant::now();
        let mut app = App::new(t0);
        app.view = View::Search;
        app.apply(Msg::Me(Ok("me".into())), t0);
        app.apply(
            Msg::Results(Ok(vec![track("Wanted Song", &["A"], None)])),
            t0,
        );
        let playlists = vec![
            playlist("p1", "Road Trip", "me", Some(42)),
            playlist("p2", "Today's Hits", "spotify", None),
        ];
        app.apply(
            Msg::Playlists {
                offset: 0,
                result: Ok(page(playlists, 2)),
            },
            t0,
        );
        app.mode = Mode::Picker;
        app.picker_track = Some(track("Wanted Song", &["A"], None));
        app.picker_state.select(Some(0));
        let screen = render(&mut app, t0);
        assert!(screen.contains("Add \"Wanted Song\" to..."), "{screen}");
        assert!(screen.contains("+ New playlist..."), "{screen}");
        assert!(screen.contains("Road Trip  42 tracks"), "{screen}");
        assert!(
            !screen.contains("Today's Hits"),
            "followed playlists are not offered:\n{screen}"
        );
    }

    #[test]
    fn the_name_prompt_shows_what_has_been_typed() {
        let now = Instant::now();
        let mut app = App::new(now);
        app.mode = Mode::NewPlaylist;
        app.new_name = "Late Night Mix".into();
        let screen = render(&mut app, now);
        assert!(screen.contains("New playlist name"), "{screen}");
        assert!(screen.contains("Late Night Mix"), "{screen}");
    }

    #[test]
    fn a_pending_removal_prompt_is_visible_in_the_status_line() {
        let now = Instant::now();
        let mut app = App::new(now);
        app.open_playlist = Some(playlist("p1", "Mix", "me", Some(1)));
        app.view = View::PlaylistTracks;
        app.apply(
            Msg::PlaylistTracks {
                id: "p1".into(),
                offset: 0,
                result: Ok(page(vec![track("Doomed", &["A"], None)], 1)),
            },
            now,
        );
        app.on_key(ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Char('x'),
            ratatui::crossterm::event::KeyModifiers::NONE,
        ));
        let screen = render(&mut app, now);
        assert!(
            screen.contains("Remove \"Doomed\" from Mix? Press x again"),
            "{screen}"
        );
    }

    #[test]
    fn start_screen_is_the_library_menu_with_key_help() {
        let now = Instant::now();
        let screen = render(&mut App::new(now), now);
        for entry in ["Library", "Search", "Liked Songs", "Playlists"] {
            assert!(screen.contains(entry), "{entry} missing:\n{screen}");
        }
        assert!(screen.contains("Nothing playing"), "{screen}");
        assert!(screen.contains("q quit"), "{screen}");
        assert!(
            !screen.contains("Search (/)"),
            "search box only on the search view"
        );
    }

    #[test]
    fn search_view_shows_the_search_box_and_hint() {
        let now = Instant::now();
        let mut app = App::new(now);
        app.view = View::Search;
        let screen = render(&mut app, now);
        assert!(screen.contains("Search (/)"), "{screen}");
        assert!(screen.contains("Press / to search for tracks."), "{screen}");
    }

    #[test]
    fn every_help_line_fits_an_80_column_terminal() {
        for view in [
            View::Menu,
            View::Search,
            View::Liked,
            View::Playlists,
            View::PlaylistTracks,
        ] {
            let len = help(view).chars().count();
            assert!(len <= 80, "{view:?} help is {len} columns");
        }
    }

    #[test]
    fn liked_songs_show_loaded_and_total_counts() {
        let now = Instant::now();
        let mut app = App::new(now);
        app.view = View::Liked;
        let tracks = vec![track("Song A", &["Artist"], Some("Album"))];
        app.apply(
            Msg::Liked {
                offset: 0,
                result: Ok(page(tracks, 812)),
            },
            now,
        );
        let screen = render(&mut app, now);
        assert!(screen.contains("Liked Songs (1/812)"), "{screen}");
        assert!(screen.contains("Song A"), "{screen}");
        assert!(screen.contains("Artist  Album  3:05"), "{screen}");
    }

    #[test]
    fn a_failed_load_says_so_and_shows_the_fix_before_the_cut_off_response() {
        let now = Instant::now();
        let mut app = App::new(now);
        app.view = View::Liked;
        let long_error = format!(
            "Run `spotui login` again to grant the new permissions. \
             (GET /me/tracks returned 403 Forbidden: {})",
            "x".repeat(200)
        );
        app.apply(
            Msg::Liked {
                offset: 0,
                result: Err(long_error),
            },
            now,
        );
        let screen = render(&mut app, now);
        assert!(screen.contains("Could not load this list"), "{screen}");
        assert!(screen.contains("Run `spotui login` again"), "{screen}");
        assert!(!screen.contains("No liked songs."), "{screen}");
    }

    #[test]
    fn liked_songs_say_loading_until_the_first_page_arrives() {
        let now = Instant::now();
        let mut app = App::new(now);
        app.view = View::Liked;
        app.liked.loading = true;
        let screen = render(&mut app, now);
        assert!(screen.contains("Loading..."), "{screen}");
    }

    #[test]
    fn playlists_show_counts_and_owners_and_omit_unknown_counts() {
        let now = Instant::now();
        let mut app = App::new(now);
        app.view = View::Playlists;
        let owned = Playlist {
            id: "p1".into(),
            name: "Road Trip".into(),
            uri: "spotify:playlist:p1".into(),
            items: Some(Count { total: 42 }),
            owner: Some(Owner {
                id: Some("me".into()),
                display_name: Some("Akhil".into()),
            }),
            collaborative: false,
        };
        let followed = Playlist {
            id: "p2".into(),
            name: "Today's Hits".into(),
            uri: "spotify:playlist:p2".into(),
            items: None,
            owner: Some(Owner {
                id: Some("spotify".into()),
                display_name: Some("Spotify".into()),
            }),
            collaborative: false,
        };
        app.apply(
            Msg::Playlists {
                offset: 0,
                result: Ok(page(vec![owned, followed], 2)),
            },
            now,
        );
        let screen = render(&mut app, now);
        assert!(screen.contains("Playlists (2/2)"), "{screen}");
        assert!(
            screen.contains("Road Trip  42 tracks  by Akhil"),
            "{screen}"
        );
        assert!(screen.contains("Today's Hits  by Spotify"), "{screen}");
    }

    #[test]
    fn playlist_view_is_titled_with_the_playlist_and_lists_episodes_without_artist_or_album() {
        let now = Instant::now();
        let mut app = App::new(now);
        app.open_playlist = Some(Playlist {
            id: "p1".into(),
            name: "Mix".into(),
            uri: "spotify:playlist:p1".into(),
            items: None,
            owner: None,
            collaborative: false,
        });
        app.view = View::PlaylistTracks;
        let items = vec![track("Some Episode", &[], None)];
        app.apply(
            Msg::PlaylistTracks {
                id: "p1".into(),
                offset: 0,
                result: Ok(page(items, 1)),
            },
            now,
        );
        let screen = render(&mut app, now);
        assert!(screen.contains("Mix (1/1)"), "{screen}");
        assert!(screen.contains("Some Episode  3:05"), "{screen}");
    }

    #[test]
    fn results_and_now_playing_are_drawn() {
        let t0 = Instant::now();
        let mut app = App::new(t0);
        app.view = View::Search;
        let track = track("Get Lucky", &["Daft Punk"], Some("RAM"));
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
