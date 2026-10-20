//! TUI state and key handling. No terminal or network access lives here, so
//! everything in this file is covered by plain unit tests.

use std::time::Instant;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::ListState;

use crate::api::{Device, Playback, Track};

const VOLUME_STEP: u8 = 5;

/// Something the key handler wants done; the runtime performs it.
#[derive(Debug, PartialEq)]
pub enum Action {
    Search(String),
    Play(String),
    Queue { uri: String, label: String },
    Pause,
    Resume,
    Next,
    Previous,
    Volume(u8),
    Transfer(String),
    LoadDevices,
}

/// Results of background work, delivered back to the event loop.
pub enum Msg {
    Key(KeyEvent),
    Playback(Result<Option<Playback>, String>),
    Results(Result<Vec<Track>, String>),
    Devices(Result<Vec<Device>, String>),
    Status(String),
    Error(String),
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Mode {
    Browse,
    Search,
    Devices,
}

pub struct App {
    pub mode: Mode,
    pub query: String,
    pub results: Vec<Track>,
    pub results_state: ListState,
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
        Self {
            mode: Mode::Browse,
            query: String::new(),
            results: Vec::new(),
            results_state: ListState::default(),
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
        self.results.get(self.results_state.selected()?)
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

    fn browse_key(&mut self, code: KeyCode) -> Option<Action> {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('/') | KeyCode::Char('s') => self.mode = Mode::Search,
            KeyCode::Down | KeyCode::Char('j') => {
                step(&mut self.results_state, self.results.len(), 1)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                step(&mut self.results_state, self.results.len(), -1)
            }
            KeyCode::Enter => return self.selected_track().map(|t| Action::Play(t.uri.clone())),
            KeyCode::Char('a') => {
                return self.selected_track().map(|t| Action::Queue {
                    uri: t.uri.clone(),
                    label: t.name.clone(),
                });
            }
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
    use crate::api::{Album, Artist, Item};

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
            album: Album { name: "Alb".into() },
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

    fn app_with_results() -> App {
        let now = Instant::now();
        let mut app = App::new(now);
        app.apply(Msg::Results(Ok(vec![track("one"), track("two")])), now);
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
}
