//! TUI state and key handling. No terminal or network access lives here, so
//! everything in this file is covered by plain unit tests.

use std::time::Instant;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::ListState;

use crate::api::{Device, Page, Playback, Playlist, Track};

const VOLUME_STEP: u8 = 5;
/// Tracks sent in one `uris` play request when starting from Liked Songs.
const PLAY_BATCH: usize = 50;
/// Fetch the next page once the selection is this close to the end of what is loaded.
const PREFETCH_MARGIN: usize = 5;
pub const MENU: [&str; 3] = ["Search", "Liked Songs", "Playlists"];

const NOT_OWNED: &str = "Spotify only lets this app list playlists you own or collaborate on. \
                         Press P to play it instead.";

/// Something the key handler wants done; the runtime performs it.
#[derive(Debug, PartialEq)]
pub enum Action {
    Search(String),
    Play(String),
    PlayUris(Vec<String>),
    PlayContext {
        context_uri: String,
        from: Option<String>,
    },
    Queue {
        uri: String,
        label: String,
    },
    Pause,
    Resume,
    Next,
    Previous,
    Volume(u8),
    Transfer(String),
    LoadDevices,
    LoadLiked {
        offset: u32,
    },
    LoadPlaylists {
        offset: u32,
    },
    LoadPlaylistTracks {
        id: String,
        offset: u32,
    },
}

/// Results of background work, delivered back to the event loop.
pub enum Msg {
    Key(KeyEvent),
    Playback(Result<Option<Playback>, String>),
    Results(Result<Vec<Track>, String>),
    Devices(Result<Vec<Device>, String>),
    Liked {
        offset: u32,
        result: Result<Page<Track>, String>,
    },
    Playlists {
        offset: u32,
        result: Result<Page<Playlist>, String>,
    },
    PlaylistTracks {
        id: String,
        offset: u32,
        result: Result<Page<Track>, String>,
    },
    Status(String),
    Error(String),
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Mode {
    Browse,
    Search,
    Devices,
}

/// Which screen fills the main area. Left/Right walk a stack of these.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum View {
    Menu,
    Search,
    Liked,
    Playlists,
    PlaylistTracks,
}

/// A list that Spotify serves in pages, loaded as the selection nears the end.
pub struct Listing<T> {
    pub items: Vec<T>,
    pub total: u32,
    pub state: ListState,
    pub loading: bool,
    pub loaded: bool,
    next_offset: u32,
}

impl<T> Listing<T> {
    fn new() -> Self {
        Self {
            items: Vec::new(),
            total: 0,
            state: ListState::default(),
            loading: false,
            loaded: false,
            next_offset: 0,
        }
    }

    fn apply(&mut self, offset: u32, page: Page<T>) {
        self.loading = false;
        self.loaded = true;
        if offset == 0 {
            self.items.clear();
            self.state.select(None);
        } else if offset != self.next_offset {
            return; // a stale or duplicate page
        }
        self.next_offset = offset + page.fetched;
        // An empty page ends the list even if Spotify's total claims more.
        self.total = if page.fetched == 0 {
            self.next_offset
        } else {
            page.total
        };
        self.items.extend(page.items);
        if self.state.selected().is_none() && !self.items.is_empty() {
            self.state.select(Some(0));
        }
    }

    fn selected(&self) -> Option<&T> {
        self.items.get(self.state.selected()?)
    }

    /// Offset of the next page when the selection is near the end of what is loaded.
    fn want_more(&mut self) -> Option<u32> {
        let near_end = self
            .state
            .selected()
            .is_some_and(|i| i + PREFETCH_MARGIN >= self.items.len());
        if self.loading || !near_end || self.next_offset >= self.total {
            return None;
        }
        self.loading = true;
        Some(self.next_offset)
    }
}

pub struct App {
    pub mode: Mode,
    pub view: View,
    /// Screens to return to with Left/Backspace/Esc, oldest first.
    history: Vec<View>,
    pub menu_state: ListState,
    pub query: String,
    pub results: Vec<Track>,
    pub results_state: ListState,
    pub liked: Listing<Track>,
    pub playlists: Listing<Playlist>,
    pub open_playlist: Option<Playlist>,
    pub playlist_tracks: Listing<Track>,
    pub playback: Option<Playback>,
    /// When `playback` was fetched, to interpolate the progress bar between polls.
    pub playback_at: Instant,
    pub devices: Vec<Device>,
    pub devices_state: ListState,
    pub status: String,
    pub status_is_error: bool,
    pub quit: bool,
}

fn step(state: &mut ListState, len: usize, delta: isize) {
    if len == 0 {
        state.select(None);
        return;
    }
    let next = match state.selected() {
        None => 0,
        Some(i) => (i as isize + delta).clamp(0, len as isize - 1) as usize,
    };
    state.select(Some(next));
}

impl App {
    pub fn new(now: Instant) -> Self {
        let mut menu_state = ListState::default();
        menu_state.select(Some(0));
        Self {
            mode: Mode::Browse,
            view: View::Menu,
            history: Vec::new(),
            menu_state,
            query: String::new(),
            results: Vec::new(),
            results_state: ListState::default(),
            liked: Listing::new(),
            playlists: Listing::new(),
            open_playlist: None,
            playlist_tracks: Listing::new(),
            playback: None,
            playback_at: now,
            devices: Vec::new(),
            devices_state: ListState::default(),
            status: String::new(),
            status_is_error: false,
            quit: false,
        }
    }

    fn info(&mut self, text: impl Into<String>) {
        self.status = text.into();
        self.status_is_error = false;
    }

    fn error(&mut self, text: &str) {
        self.status = text.replace('\n', " ");
        self.status_is_error = true;
    }

    /// Playback position now, advanced by the time since the last poll while playing.
    pub fn progress_ms(&self, now: Instant) -> Option<u64> {
        let p = self.playback.as_ref()?;
        let item = p.item.as_ref()?;
        let base = p.progress_ms?;
        let extra = if p.is_playing {
            now.saturating_duration_since(self.playback_at).as_millis() as u64
        } else {
            0
        };
        Some((base + extra).min(item.duration_ms))
    }

    pub fn apply(&mut self, msg: Msg, now: Instant) {
        match msg {
            Msg::Key(_) => {}
            Msg::Playback(Ok(playback)) => {
                self.playback = playback;
                self.playback_at = now;
            }
            Msg::Results(Ok(tracks)) => {
                self.results_state
                    .select(if tracks.is_empty() { None } else { Some(0) });
                self.info(if tracks.is_empty() {
                    "No tracks found.".to_string()
                } else {
                    format!("{} results", tracks.len())
                });
                self.results = tracks;
            }
            Msg::Devices(Ok(devices)) => {
                let active = devices.iter().position(|d| d.is_active);
                self.devices_state.select(if devices.is_empty() {
                    None
                } else {
                    Some(active.unwrap_or(0))
                });
                self.devices = devices;
            }
            Msg::Liked { offset, result } => match result {
                Ok(page) => self.liked.apply(offset, page),
                Err(e) => {
                    self.liked.loading = false;
                    self.error(&e);
                }
            },
            Msg::Playlists { offset, result } => match result {
                Ok(page) => self.playlists.apply(offset, page),
                Err(e) => {
                    self.playlists.loading = false;
                    self.error(&e);
                }
            },
            Msg::PlaylistTracks { id, offset, result } => {
                // The user may have gone back and opened another playlist meanwhile.
                if self.open_playlist.as_ref().map(|p| p.id.as_str()) != Some(id.as_str()) {
                    return;
                }
                match result {
                    Ok(page) => self.playlist_tracks.apply(offset, page),
                    Err(e) => {
                        self.playlist_tracks.loading = false;
                        self.error(if e.contains("403") && !e.contains("scope") {
                            NOT_OWNED
                        } else {
                            &e
                        });
                    }
                }
            }
            Msg::Status(text) => self.info(text),
            Msg::Playback(Err(e)) | Msg::Results(Err(e)) | Msg::Devices(Err(e)) | Msg::Error(e) => {
                self.error(&e)
            }
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.kind != KeyEventKind::Press {
            return None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return None;
        }
        match self.mode {
            Mode::Search => self.search_key(key.code),
            Mode::Devices => self.devices_key(key.code),
            Mode::Browse => self.browse_key(key.code),
        }
    }

    fn selected_track(&self) -> Option<&Track> {
        match self.view {
            View::Search => self.results.get(self.results_state.selected()?),
            View::Liked => self.liked.selected(),
            View::PlaylistTracks => self.playlist_tracks.selected(),
            View::Menu | View::Playlists => None,
        }
    }

    fn volume_change(&mut self, delta: i16) -> Option<Action> {
        let current = self.playback.as_ref()?.device.as_ref()?.volume_percent;
        match current {
            Some(v) => Some(Action::Volume((v as i16 + delta).clamp(0, 100) as u8)),
            None => {
                self.error("This device does not report a volume.");
                None
            }
        }
    }

    /// Shows `view`, remembering where we came from, and requests its first page if needed.
    fn open(&mut self, view: View) -> Option<Action> {
        if self.view != view {
            self.history.push(self.view);
            self.view = view;
        }
        match view {
            View::Search => {
                self.mode = Mode::Search;
                None
            }
            View::Liked if !self.liked.loaded && !self.liked.loading => {
                self.liked.loading = true;
                Some(Action::LoadLiked { offset: 0 })
            }
            View::Playlists if !self.playlists.loaded && !self.playlists.loading => {
                self.playlists.loading = true;
                Some(Action::LoadPlaylists { offset: 0 })
            }
            _ => None,
        }
    }

    fn open_selected_playlist(&mut self) -> Option<Action> {
        let playlist = self.playlists.selected()?.clone();
        let id = playlist.id.clone();
        self.open_playlist = Some(playlist);
        self.playlist_tracks = Listing::new();
        self.playlist_tracks.loading = true;
        self.history.push(self.view);
        self.view = View::PlaylistTracks;
        Some(Action::LoadPlaylistTracks { id, offset: 0 })
    }

    fn back(&mut self) {
        if let Some(view) = self.history.pop() {
            self.view = view;
        }
    }

    /// Right arrow: go into the highlighted menu entry or playlist.
    fn open_selected(&mut self) -> Option<Action> {
        match self.view {
            View::Menu => match self.menu_state.selected()? {
                0 => self.open(View::Search),
                1 => self.open(View::Liked),
                _ => self.open(View::Playlists),
            },
            View::Playlists => self.open_selected_playlist(),
            View::Search | View::Liked | View::PlaylistTracks => None,
        }
    }

    /// Enter: open a menu entry or playlist, or play the highlighted track.
    fn activate(&mut self) -> Option<Action> {
        match self.view {
            View::Menu | View::Playlists => self.open_selected(),
            View::Search => self.selected_track().map(|t| Action::Play(t.uri.clone())),
            View::Liked => {
                let start = self.liked.state.selected()?;
                let uris = self
                    .liked
                    .items
                    .iter()
                    .skip(start)
                    .take(PLAY_BATCH)
                    .map(|t| t.uri.clone())
                    .collect();
                Some(Action::PlayUris(uris))
            }
            View::PlaylistTracks => Some(Action::PlayContext {
                context_uri: self.open_playlist.as_ref()?.uri.clone(),
                from: Some(self.playlist_tracks.selected()?.uri.clone()),
            }),
        }
    }

    /// `P`: play the whole highlighted or open playlist from its start.
    fn play_playlist(&self) -> Option<Action> {
        let uri = match self.view {
            View::Playlists => self.playlists.selected()?.uri.clone(),
            View::PlaylistTracks => self.open_playlist.as_ref()?.uri.clone(),
            _ => return None,
        };
        Some(Action::PlayContext {
            context_uri: uri,
            from: None,
        })
    }

    fn move_selection(&mut self, delta: isize) -> Option<Action> {
        match self.view {
            View::Menu => step(&mut self.menu_state, MENU.len(), delta),
            View::Search => step(&mut self.results_state, self.results.len(), delta),
            View::Liked => {
                step(&mut self.liked.state, self.liked.items.len(), delta);
                return self
                    .liked
                    .want_more()
                    .map(|offset| Action::LoadLiked { offset });
            }
            View::Playlists => {
                step(&mut self.playlists.state, self.playlists.items.len(), delta);
                return self
                    .playlists
                    .want_more()
                    .map(|offset| Action::LoadPlaylists { offset });
            }
            View::PlaylistTracks => {
                step(
                    &mut self.playlist_tracks.state,
                    self.playlist_tracks.items.len(),
                    delta,
                );
                let id = self.open_playlist.as_ref()?.id.clone();
                return self
                    .playlist_tracks
                    .want_more()
                    .map(|offset| Action::LoadPlaylistTracks { id, offset });
            }
        }
        None
    }

    fn browse_key(&mut self, code: KeyCode) -> Option<Action> {
        match code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc if self.history.is_empty() => self.quit = true,
            KeyCode::Esc | KeyCode::Left | KeyCode::Backspace => self.back(),
            KeyCode::Right => return self.open_selected(),
            KeyCode::Enter => return self.activate(),
            KeyCode::Char('/') | KeyCode::Char('s') => return self.open(View::Search),
            KeyCode::Down | KeyCode::Char('j') => return self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => return self.move_selection(-1),
            KeyCode::Char('a') => {
                return self.selected_track().map(|t| Action::Queue {
                    uri: t.uri.clone(),
                    label: t.name.clone(),
                });
            }
            KeyCode::Char('P') => return self.play_playlist(),
            KeyCode::Char(' ') => {
                let playing = self.playback.as_ref().is_some_and(|p| p.is_playing);
                return Some(if playing {
                    Action::Pause
                } else {
                    Action::Resume
                });
            }
            KeyCode::Char('n') => return Some(Action::Next),
            KeyCode::Char('p') => return Some(Action::Previous),
            KeyCode::Char('+') | KeyCode::Char('=') => {
                return self.volume_change(VOLUME_STEP as i16);
            }
            KeyCode::Char('-') => return self.volume_change(-(VOLUME_STEP as i16)),
            KeyCode::Char('d') => {
                self.mode = Mode::Devices;
                self.devices.clear();
                self.devices_state.select(None);
                return Some(Action::LoadDevices);
            }
            _ => {}
        }
        None
    }

    fn search_key(&mut self, code: KeyCode) -> Option<Action> {
        match code {
            KeyCode::Char(c) => self.query.push(c),
            KeyCode::Backspace => {
                self.query.pop();
            }
            KeyCode::Esc => self.mode = Mode::Browse,
            KeyCode::Enter if !self.query.trim().is_empty() => {
                self.mode = Mode::Browse;
                self.info("Searching...");
                return Some(Action::Search(self.query.trim().to_string()));
            }
            _ => {}
        }
        None
    }

    fn devices_key(&mut self, code: KeyCode) -> Option<Action> {
        match code {
            KeyCode::Down | KeyCode::Char('j') => {
                step(&mut self.devices_state, self.devices.len(), 1)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                step(&mut self.devices_state, self.devices.len(), -1)
            }
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Browse,
            KeyCode::Enter => {
                let device = self.devices.get(self.devices_state.selected()?)?;
                let Some(id) = device.id.clone() else {
                    self.error("That device has no id and cannot be targeted.");
                    return None;
                };
                self.mode = Mode::Browse;
                return Some(Action::Transfer(id));
            }
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::api::{Album, Artist, Count, Item};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn chars(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
    }

    fn track(name: &str) -> Track {
        Track {
            uri: format!("spotify:track:{name}"),
            name: name.into(),
            duration_ms: 200_000,
            artists: vec![Artist { name: "A".into() }],
            album: Some(Album { name: "Alb".into() }),
        }
    }

    fn tracks(n: usize) -> Vec<Track> {
        (0..n).map(|i| track(&format!("t{i}"))).collect()
    }

    fn playlist(id: &str) -> Playlist {
        Playlist {
            id: id.into(),
            name: format!("List {id}"),
            uri: format!("spotify:playlist:{id}"),
            items: Some(Count { total: 3 }),
            owner: None,
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

    fn device(id: Option<&str>, active: bool, volume: Option<u8>) -> Device {
        Device {
            id: id.map(Into::into),
            name: "dev".into(),
            kind: "Computer".into(),
            is_active: active,
            volume_percent: volume,
        }
    }

    fn playback(is_playing: bool, progress_ms: u64, volume: Option<u8>) -> Playback {
        Playback {
            is_playing,
            progress_ms: Some(progress_ms),
            device: Some(device(Some("d1"), true, volume)),
            item: Some(Item {
                name: "S".into(),
                duration_ms: 100_000,
                artists: vec![],
            }),
        }
    }

    /// Search view in browse mode with two results.
    fn app_with_results() -> App {
        let now = Instant::now();
        let mut app = App::new(now);
        app.on_key(key(KeyCode::Char('/')));
        app.on_key(key(KeyCode::Esc));
        app.apply(Msg::Results(Ok(vec![track("one"), track("two")])), now);
        app
    }

    /// Menu -> Liked Songs with `n` tracks loaded out of `total`.
    fn app_in_liked(n: usize, total: u32) -> App {
        let now = Instant::now();
        let mut app = App::new(now);
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Right));
        app.apply(
            Msg::Liked {
                offset: 0,
                result: Ok(page(tracks(n), total)),
            },
            now,
        );
        app
    }

    /// Menu -> Playlists -> first playlist opened, with `n` of `total` tracks loaded.
    fn app_in_playlist(n: usize, total: u32) -> App {
        let now = Instant::now();
        let mut app = App::new(now);
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Right));
        app.apply(
            Msg::Playlists {
                offset: 0,
                result: Ok(page(vec![playlist("p1"), playlist("p2")], 2)),
            },
            now,
        );
        app.on_key(key(KeyCode::Right));
        app.apply(
            Msg::PlaylistTracks {
                id: "p1".into(),
                offset: 0,
                result: Ok(page(tracks(n), total)),
            },
            now,
        );
        app
    }

    #[test]
    fn search_flow_submits_trimmed_query_and_returns_to_browse() {
        let mut app = App::new(Instant::now());
        app.on_key(key(KeyCode::Char('/')));
        assert_eq!(app.mode, Mode::Search);
        chars(&mut app, " abc");
        app.on_key(key(KeyCode::Backspace));
        let action = app.on_key(key(KeyCode::Enter));
        assert_eq!(action, Some(Action::Search("ab".into())));
        assert_eq!(app.mode, Mode::Browse);
    }

    #[test]
    fn empty_search_stays_in_search_mode() {
        let mut app = App::new(Instant::now());
        app.on_key(key(KeyCode::Char('/')));
        chars(&mut app, "  ");
        assert_eq!(app.on_key(key(KeyCode::Enter)), None);
        assert_eq!(app.mode, Mode::Search);
    }

    #[test]
    fn typing_q_in_search_mode_does_not_quit_but_ctrl_c_does() {
        let mut app = App::new(Instant::now());
        app.on_key(key(KeyCode::Char('/')));
        chars(&mut app, "queen");
        assert!(!app.quit);
        assert_eq!(app.query, "queen");
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.quit);
    }

    #[test]
    fn results_select_first_and_selection_clamps_at_both_ends() {
        let mut app = app_with_results();
        assert_eq!(app.results_state.selected(), Some(0));
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.results_state.selected(), Some(0));
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.results_state.selected(), Some(1));
    }

    #[test]
    fn enter_plays_and_a_queues_the_selected_track() {
        let mut app = app_with_results();
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(
            app.on_key(key(KeyCode::Enter)),
            Some(Action::Play("spotify:track:two".into()))
        );
        assert_eq!(
            app.on_key(key(KeyCode::Char('a'))),
            Some(Action::Queue {
                uri: "spotify:track:two".into(),
                label: "two".into()
            })
        );
    }

    #[test]
    fn enter_with_no_results_does_nothing() {
        let mut app = App::new(Instant::now());
        app.on_key(key(KeyCode::Char('/')));
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.on_key(key(KeyCode::Enter)), None);
    }

    #[test]
    fn space_pauses_when_playing_and_resumes_otherwise() {
        let now = Instant::now();
        let mut app = App::new(now);
        assert_eq!(app.on_key(key(KeyCode::Char(' '))), Some(Action::Resume));
        app.apply(Msg::Playback(Ok(Some(playback(true, 0, Some(50))))), now);
        assert_eq!(app.on_key(key(KeyCode::Char(' '))), Some(Action::Pause));
    }

    #[test]
    fn volume_steps_by_five_and_clamps() {
        let now = Instant::now();
        let mut app = App::new(now);
        app.apply(Msg::Playback(Ok(Some(playback(true, 0, Some(98))))), now);
        assert_eq!(
            app.on_key(key(KeyCode::Char('+'))),
            Some(Action::Volume(100))
        );
        app.apply(Msg::Playback(Ok(Some(playback(true, 0, Some(3))))), now);
        assert_eq!(app.on_key(key(KeyCode::Char('-'))), Some(Action::Volume(0)));
    }

    #[test]
    fn volume_without_a_reported_level_sets_an_error() {
        let now = Instant::now();
        let mut app = App::new(now);
        app.apply(Msg::Playback(Ok(Some(playback(true, 0, None)))), now);
        assert_eq!(app.on_key(key(KeyCode::Char('+'))), None);
        assert!(app.status_is_error);
    }

    #[test]
    fn device_picker_selects_active_device_and_transfers_on_enter() {
        let now = Instant::now();
        let mut app = App::new(now);
        assert_eq!(
            app.on_key(key(KeyCode::Char('d'))),
            Some(Action::LoadDevices)
        );
        assert_eq!(app.mode, Mode::Devices);
        let devices = vec![
            device(Some("a"), false, None),
            device(Some("b"), true, None),
        ];
        app.apply(Msg::Devices(Ok(devices)), now);
        assert_eq!(app.devices_state.selected(), Some(1));
        app.on_key(key(KeyCode::Up));
        assert_eq!(
            app.on_key(key(KeyCode::Enter)),
            Some(Action::Transfer("a".into()))
        );
        assert_eq!(app.mode, Mode::Browse);
    }

    #[test]
    fn device_without_id_cannot_be_transferred_to() {
        let now = Instant::now();
        let mut app = App::new(now);
        app.on_key(key(KeyCode::Char('d')));
        app.apply(Msg::Devices(Ok(vec![device(None, false, None)])), now);
        assert_eq!(app.on_key(key(KeyCode::Enter)), None);
        assert!(app.status_is_error);
    }

    #[test]
    fn progress_advances_only_while_playing_and_stops_at_track_end() {
        let t0 = Instant::now();
        let mut app = App::new(t0);
        app.apply(Msg::Playback(Ok(Some(playback(true, 10_000, None)))), t0);
        assert_eq!(app.progress_ms(t0 + Duration::from_secs(5)), Some(15_000));
        assert_eq!(
            app.progress_ms(t0 + Duration::from_secs(500)),
            Some(100_000)
        );
        app.apply(Msg::Playback(Ok(Some(playback(false, 10_000, None)))), t0);
        assert_eq!(app.progress_ms(t0 + Duration::from_secs(5)), Some(10_000));
    }

    #[test]
    fn errors_are_flattened_to_one_line() {
        let mut app = App::new(Instant::now());
        app.apply(Msg::Error("404\nstart a device".into()), Instant::now());
        assert_eq!(app.status, "404 start a device");
        assert!(app.status_is_error);
    }

    #[test]
    fn right_opens_menu_entries_and_left_goes_back() {
        let mut app = App::new(Instant::now());
        assert_eq!(app.view, View::Menu);
        app.on_key(key(KeyCode::Down));
        assert_eq!(
            app.on_key(key(KeyCode::Right)),
            Some(Action::LoadLiked { offset: 0 })
        );
        assert_eq!(app.view, View::Liked);
        assert_eq!(app.on_key(key(KeyCode::Left)), None);
        assert_eq!(app.view, View::Menu);
        // Left at the root has nowhere to go and must not quit.
        app.on_key(key(KeyCode::Left));
        assert_eq!((app.view, app.quit), (View::Menu, false));
    }

    #[test]
    fn enter_on_the_menu_opens_playlists() {
        let mut app = App::new(Instant::now());
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Down));
        assert_eq!(
            app.on_key(key(KeyCode::Enter)),
            Some(Action::LoadPlaylists { offset: 0 })
        );
        assert_eq!(app.view, View::Playlists);
    }

    #[test]
    fn escape_goes_back_and_quits_only_at_the_root() {
        let mut app = App::new(Instant::now());
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Right));
        app.on_key(key(KeyCode::Esc));
        assert_eq!((app.view, app.quit), (View::Menu, false));
        app.on_key(key(KeyCode::Esc));
        assert!(app.quit);
    }

    #[test]
    fn q_quits_from_any_depth() {
        let mut app = app_in_playlist(3, 3);
        app.on_key(key(KeyCode::Char('q')));
        assert!(app.quit);
    }

    #[test]
    fn slash_opens_search_in_input_mode_and_back_returns_to_the_previous_view() {
        let mut app = app_in_liked(3, 3);
        app.on_key(key(KeyCode::Char('/')));
        assert_eq!((app.view, app.mode), (View::Search, Mode::Search));
        app.on_key(key(KeyCode::Esc));
        app.on_key(key(KeyCode::Left));
        assert_eq!(app.view, View::Liked);
    }

    #[test]
    fn menu_search_entry_starts_typing_immediately() {
        let mut app = App::new(Instant::now());
        app.on_key(key(KeyCode::Right));
        assert_eq!((app.view, app.mode), (View::Search, Mode::Search));
    }

    #[test]
    fn reopening_a_loaded_view_does_not_refetch_but_a_failed_one_retries() {
        let mut app = app_in_liked(3, 3);
        app.on_key(key(KeyCode::Left));
        // The menu still highlights Liked Songs, which is already loaded.
        assert_eq!(app.on_key(key(KeyCode::Right)), None, "already loaded");
        assert_eq!(app.view, View::Liked);

        let now = Instant::now();
        let mut failed = App::new(now);
        failed.on_key(key(KeyCode::Down));
        failed.on_key(key(KeyCode::Right));
        failed.apply(
            Msg::Liked {
                offset: 0,
                result: Err("403 Insufficient client scope".into()),
            },
            now,
        );
        assert!(failed.status_is_error);
        failed.on_key(key(KeyCode::Left));
        assert_eq!(
            failed.on_key(key(KeyCode::Right)),
            Some(Action::LoadLiked { offset: 0 })
        );
    }

    #[test]
    fn scrolling_near_the_end_requests_the_next_page_once() {
        let mut app = app_in_liked(10, 25);
        // Selection 0 of 10 is not near the end yet.
        assert_eq!(app.on_key(key(KeyCode::Down)), None);
        for _ in 0..3 {
            app.on_key(key(KeyCode::Down));
        }
        // Index 5 + margin 5 reaches the 10 loaded items.
        assert_eq!(
            app.on_key(key(KeyCode::Down)),
            Some(Action::LoadLiked { offset: 10 })
        );
        // The request is in flight; scrolling further must not repeat it.
        assert_eq!(app.on_key(key(KeyCode::Down)), None);
        let now = Instant::now();
        app.apply(
            Msg::Liked {
                offset: 10,
                result: Ok(page(tracks(10), 25)),
            },
            now,
        );
        assert_eq!(app.liked.items.len(), 20);
    }

    #[test]
    fn no_more_pages_are_requested_once_everything_is_loaded() {
        let mut app = app_in_liked(8, 8);
        for _ in 0..7 {
            assert_eq!(app.on_key(key(KeyCode::Down)), None);
        }
    }

    #[test]
    fn stale_or_duplicate_pages_are_ignored() {
        let now = Instant::now();
        let mut app = app_in_liked(10, 30);
        app.apply(
            Msg::Liked {
                offset: 20,
                result: Ok(page(tracks(10), 30)),
            },
            now,
        );
        assert_eq!(app.liked.items.len(), 10, "gap in offsets");
        app.apply(
            Msg::Liked {
                offset: 10,
                result: Ok(page(tracks(10), 30)),
            },
            now,
        );
        app.apply(
            Msg::Liked {
                offset: 10,
                result: Ok(page(tracks(10), 30)),
            },
            now,
        );
        assert_eq!(app.liked.items.len(), 20, "second copy of page 10 dropped");
    }

    #[test]
    fn dropped_null_entries_still_advance_the_offset() {
        let now = Instant::now();
        let mut app = app_in_playlist(1, 10);
        // 5 entries came back for offset 0 but one was null, so only 4 tracks remain.
        app.apply(
            Msg::PlaylistTracks {
                id: "p1".into(),
                offset: 0,
                result: Ok(Page {
                    items: tracks(4),
                    total: 10,
                    fetched: 5,
                }),
            },
            now,
        );
        app.playlist_tracks.state.select(Some(3));
        // 4 tracks are loaded but 5 entries were consumed, so the next page starts at 5, not 4.
        assert_eq!(
            app.on_key(key(KeyCode::Down)),
            Some(Action::LoadPlaylistTracks {
                id: "p1".into(),
                offset: 5
            })
        );
    }

    #[test]
    fn an_empty_page_ends_the_list() {
        let now = Instant::now();
        let mut app = app_in_liked(10, 99);
        app.apply(
            Msg::Liked {
                offset: 10,
                result: Ok(page(Vec::new(), 99)),
            },
            now,
        );
        assert_eq!(app.liked.total, 10);
    }

    #[test]
    fn enter_in_liked_songs_plays_from_the_selection_onward() {
        let mut app = app_in_liked(60, 60);
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Down));
        let Some(Action::PlayUris(uris)) = app.on_key(key(KeyCode::Enter)) else {
            panic!("expected PlayUris");
        };
        assert_eq!(uris.len(), PLAY_BATCH);
        assert_eq!(uris[0], "spotify:track:t2");
        assert_eq!(uris[1], "spotify:track:t3");
    }

    #[test]
    fn enter_on_a_playlist_opens_it_and_requests_its_tracks() {
        let now = Instant::now();
        let mut app = App::new(now);
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Right));
        app.apply(
            Msg::Playlists {
                offset: 0,
                result: Ok(page(vec![playlist("p1"), playlist("p2")], 2)),
            },
            now,
        );
        app.on_key(key(KeyCode::Down));
        assert_eq!(
            app.on_key(key(KeyCode::Enter)),
            Some(Action::LoadPlaylistTracks {
                id: "p2".into(),
                offset: 0
            })
        );
        assert_eq!(app.view, View::PlaylistTracks);
        app.on_key(key(KeyCode::Left));
        assert_eq!(app.view, View::Playlists);
    }

    #[test]
    fn right_on_a_track_list_does_nothing() {
        let mut app = app_in_playlist(3, 3);
        assert_eq!(app.on_key(key(KeyCode::Right)), None);
        assert_eq!(app.view, View::PlaylistTracks);
    }

    #[test]
    fn enter_in_a_playlist_plays_the_playlist_from_that_track() {
        let mut app = app_in_playlist(3, 3);
        app.on_key(key(KeyCode::Down));
        assert_eq!(
            app.on_key(key(KeyCode::Enter)),
            Some(Action::PlayContext {
                context_uri: "spotify:playlist:p1".into(),
                from: Some("spotify:track:t1".into())
            })
        );
    }

    #[test]
    fn shift_p_plays_a_whole_playlist_from_the_list_or_from_inside_it() {
        let mut app = app_in_playlist(3, 3);
        let whole = Some(Action::PlayContext {
            context_uri: "spotify:playlist:p1".into(),
            from: None,
        });
        assert_eq!(app.on_key(key(KeyCode::Char('P'))), whole);
        app.on_key(key(KeyCode::Left));
        assert_eq!(app.on_key(key(KeyCode::Char('P'))), whole);
        app.on_key(key(KeyCode::Left));
        assert_eq!(
            app.on_key(key(KeyCode::Char('P'))),
            None,
            "not on a playlist"
        );
    }

    #[test]
    fn lowercase_p_still_skips_back_inside_a_playlist() {
        let mut app = app_in_playlist(3, 3);
        assert_eq!(app.on_key(key(KeyCode::Char('p'))), Some(Action::Previous));
    }

    #[test]
    fn a_403_on_playlist_tracks_explains_the_ownership_limit() {
        let now = Instant::now();
        let mut app = App::new(now);
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Right));
        app.apply(
            Msg::Playlists {
                offset: 0,
                result: Ok(page(vec![playlist("p1")], 1)),
            },
            now,
        );
        app.on_key(key(KeyCode::Right));
        app.apply(
            Msg::PlaylistTracks {
                id: "p1".into(),
                offset: 0,
                result: Err("GET /playlists/p1/items returned 403 Forbidden: {}".into()),
            },
            now,
        );
        assert!(
            app.status.contains("own or collaborate on"),
            "{}",
            app.status
        );
        assert!(app.status_is_error);
        assert!(!app.playlist_tracks.loading);
    }

    #[test]
    fn a_missing_scope_403_is_not_blamed_on_playlist_ownership() {
        let now = Instant::now();
        let mut app = app_in_playlist(1, 5);
        app.apply(
            Msg::PlaylistTracks {
                id: "p1".into(),
                offset: 1,
                result: Err("returned 403: Insufficient client scope\nRun `spotui login`".into()),
            },
            now,
        );
        assert!(app.status.contains("spotui login"), "{}", app.status);
    }

    #[test]
    fn tracks_for_a_playlist_the_user_already_left_are_ignored() {
        let now = Instant::now();
        let mut app = app_in_playlist(3, 3);
        app.apply(
            Msg::PlaylistTracks {
                id: "other".into(),
                offset: 0,
                result: Ok(page(tracks(9), 9)),
            },
            now,
        );
        assert_eq!(app.playlist_tracks.items.len(), 3);
    }
}
