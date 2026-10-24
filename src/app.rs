//! TUI state and key handling. No terminal or network access lives here, so
//! everything in this file is covered by plain unit tests.

use std::collections::HashMap;
use std::time::Instant;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::ListState;

use crate::api::{Count, Device, Page, Playback, Playlist, Repeat, Track};

const VOLUME_STEP: u8 = 5;
/// Tracks sent in one `uris` play request when starting from Liked Songs.
const PLAY_BATCH: usize = 50;
/// Fetch the next page once the selection is this close to the end of what is loaded.
const PREFETCH_MARGIN: usize = 5;
pub const MENU: [&str; 3] = ["Search", "Liked Songs", "Playlists"];

const NOT_OWNED: &str = "Spotify only lets this app list playlists you own or collaborate on. \
                         Press P to play it instead.";

/// Only tracks and episodes can be liked or added to a playlist, not local files.
fn is_likeable(uri: &str) -> bool {
    uri.starts_with("spotify:track:") || uri.starts_with("spotify:episode:")
}

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
    Restart,
    Shuffle(bool),
    Repeat(Repeat),
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
    CheckLiked(Vec<String>),
    SetLiked {
        uri: String,
        liked: bool,
        label: String,
    },
    CreatePlaylist {
        name: String,
        uri: String,
        label: String,
    },
    AddToPlaylist {
        playlist_id: String,
        playlist_name: String,
        uri: String,
        label: String,
    },
    RemoveFromPlaylist {
        playlist_id: String,
        playlist_name: String,
        uri: String,
        label: String,
    },
}

/// Results of background work, delivered back to the event loop.
pub enum Msg {
    Key(KeyEvent),
    Me(Result<String, String>),
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
    LikedFlags {
        uris: Vec<String>,
        result: Result<Vec<bool>, String>,
    },
    LikeChanged {
        uri: String,
        liked: bool,
        label: String,
        result: Result<(), String>,
    },
    Added {
        playlist_id: String,
        playlist_name: String,
        label: String,
        result: Result<(), String>,
    },
    Created {
        label: String,
        created: Result<Playlist, String>,
        /// Whether the track was added to the new playlist; unused if creating failed.
        added: Result<(), String>,
    },
    Removed {
        playlist_id: String,
        playlist_name: String,
        uri: String,
        label: String,
        result: Result<(), String>,
    },
    Status(String),
    Error(String),
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Mode {
    Browse,
    Search,
    Devices,
    /// Choosing a playlist (or "New playlist") for `picker_track`.
    Picker,
    /// Typing the name of a new playlist for `picker_track`.
    NewPlaylist,
    /// The shortcut list; any of Esc, q, ?, Enter closes it.
    Help,
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

    /// Offset of the next page, if there is one and it is not already being fetched.
    fn next_page(&mut self) -> Option<u32> {
        if self.loading || self.next_offset >= self.total {
            return None;
        }
        self.loading = true;
        Some(self.next_offset)
    }

    /// Like `next_page`, but only when the selection is near the end of what is loaded.
    fn want_more(&mut self) -> Option<u32> {
        let near_end = self
            .state
            .selected()
            .is_some_and(|i| i + PREFETCH_MARGIN >= self.items.len());
        if near_end { self.next_page() } else { None }
    }

    /// Adds a new first item, as when something is created or liked just now.
    fn insert_front(&mut self, item: T) {
        self.items.insert(0, item);
        self.total += 1;
        self.next_offset += 1;
        let selected = self.state.selected().map_or(0, |i| i + 1);
        self.state.select(Some(selected));
    }

    /// Drops matching items, keeping the paging offsets and the selection valid.
    fn remove_where(&mut self, matches: impl Fn(&T) -> bool) -> u32 {
        let before = self.items.len();
        self.items.retain(|item| !matches(item));
        let removed = (before - self.items.len()) as u32;
        self.total = self.total.saturating_sub(removed);
        self.next_offset = self.next_offset.saturating_sub(removed);
        match self.state.selected() {
            _ if self.items.is_empty() => self.state.select(None),
            Some(i) => self.state.select(Some(i.min(self.items.len() - 1))),
            None => {}
        }
        removed
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
    /// The user's Spotify id, used to tell which playlists can be written to.
    pub me_id: Option<String>,
    /// Whether each track seen so far is in Liked Songs. Absent means not checked yet.
    pub liked_flags: HashMap<String, bool>,
    /// The track being added to a playlist while the picker or name prompt is open.
    pub picker_track: Option<Track>,
    pub picker_state: ListState,
    pub new_name: String,
    /// The track `x` asked to remove; a second `x` on it confirms.
    pending_remove: Option<String>,
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
            me_id: None,
            liked_flags: HashMap::new(),
            picker_track: None,
            picker_state: ListState::default(),
            new_name: String::new(),
            pending_remove: None,
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

    /// A request to find out which of `tracks` are liked, for those not checked yet.
    fn check_liked(&self, tracks: &[Track]) -> Option<Action> {
        let uris: Vec<String> = tracks
            .iter()
            .filter(|t| is_likeable(&t.uri) && !self.liked_flags.contains_key(&t.uri))
            .map(|t| t.uri.clone())
            .collect();
        (!uris.is_empty()).then_some(Action::CheckLiked(uris))
    }

    fn find_track(&self, uri: &str) -> Option<&Track> {
        self.results
            .iter()
            .chain(&self.playlist_tracks.items)
            .chain(&self.liked.items)
            .find(|t| t.uri == uri)
    }

    /// Shows a track liked just now at the top of Liked Songs, if that list is loaded.
    fn add_to_liked_list(&mut self, uri: &str) {
        if !self.liked.loaded || self.liked.items.iter().any(|t| t.uri == uri) {
            return;
        }
        if let Some(track) = self.find_track(uri).cloned() {
            self.liked.insert_front(track);
        }
    }

    fn bump_count(&mut self, playlist_id: &str, delta: i64) {
        let adjust = |count: &mut Count| {
            count.total = (i64::from(count.total) + delta).max(0) as u32;
        };
        if let Some(count) = self
            .playlists
            .items
            .iter_mut()
            .find(|p| p.id == playlist_id)
            .and_then(|p| p.items.as_mut())
        {
            adjust(count);
        }
        if let Some(count) = self
            .open_playlist
            .as_mut()
            .filter(|p| p.id == playlist_id)
            .and_then(|p| p.items.as_mut())
        {
            adjust(count);
        }
    }

    /// Playlists the user can add to: ones they own or collaborate on.
    pub fn picker_playlists(&self) -> Vec<&Playlist> {
        let Some(me) = &self.me_id else {
            return Vec::new();
        };
        self.playlists
            .items
            .iter()
            .filter(|p| p.writable_by(me))
            .collect()
    }

    /// Applies a background result. May ask for follow-up work, such as checking
    /// which freshly loaded tracks are liked.
    pub fn apply(&mut self, msg: Msg, now: Instant) -> Option<Action> {
        match msg {
            Msg::Key(_) => {}
            Msg::Me(Ok(id)) => self.me_id = Some(id),
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
                let check = self.check_liked(&tracks);
                self.results = tracks;
                return check;
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
                Ok(page) => {
                    for track in &page.items {
                        self.liked_flags.insert(track.uri.clone(), true);
                    }
                    self.liked.apply(offset, page);
                }
                Err(e) => {
                    self.liked.loading = false;
                    self.error(&e);
                }
            },
            Msg::Playlists { offset, result } => match result {
                Ok(page) => {
                    self.playlists.apply(offset, page);
                    // The picker needs every page to know which playlists are writable.
                    if self.mode == Mode::Picker {
                        return self
                            .playlists
                            .next_page()
                            .map(|offset| Action::LoadPlaylists { offset });
                    }
                }
                Err(e) => {
                    self.playlists.loading = false;
                    self.error(&e);
                }
            },
            Msg::PlaylistTracks { id, offset, result } => {
                // The user may have gone back and opened another playlist meanwhile.
                if self.open_playlist.as_ref().map(|p| p.id.as_str()) != Some(id.as_str()) {
                    return None;
                }
                match result {
                    Ok(page) => {
                        let check = self.check_liked(&page.items);
                        self.playlist_tracks.apply(offset, page);
                        return check;
                    }
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
            Msg::LikedFlags { uris, result } => match result {
                Ok(flags) => {
                    for (uri, liked) in uris.into_iter().zip(flags) {
                        self.liked_flags.insert(uri, liked);
                    }
                }
                Err(e) => self.error(&e),
            },
            Msg::LikeChanged {
                uri,
                liked,
                label,
                result,
            } => match result {
                Ok(()) => {
                    self.liked_flags.insert(uri.clone(), liked);
                    if liked {
                        self.info(format!("Liked: {label}"));
                        self.add_to_liked_list(&uri);
                    } else {
                        self.info(format!("Removed from Liked Songs: {label}"));
                    }
                }
                Err(e) => self.error(&e),
            },
            Msg::Added {
                playlist_id,
                playlist_name,
                label,
                result,
            } => match result {
                Ok(()) => {
                    self.info(format!("Added {label} to {playlist_name}"));
                    self.bump_count(&playlist_id, 1);
                }
                Err(e) => self.error(&e),
            },
            Msg::Created {
                label,
                created,
                added,
            } => match created {
                Ok(mut playlist) => {
                    match &added {
                        Ok(()) => self.info(format!("Created {} and added {label}", playlist.name)),
                        Err(e) => self.error(&format!(
                            "Created {}, but adding {label} failed: {e}",
                            playlist.name
                        )),
                    }
                    if self.playlists.loaded {
                        playlist.items = Some(Count {
                            total: u32::from(added.is_ok()),
                        });
                        self.playlists.insert_front(playlist);
                    }
                }
                Err(e) => self.error(&e),
            },
            Msg::Removed {
                playlist_id,
                playlist_name,
                uri,
                label,
                result,
            } => match result {
                Ok(()) => {
                    if self
                        .open_playlist
                        .as_ref()
                        .is_some_and(|p| p.id == playlist_id)
                    {
                        let removed = self.playlist_tracks.remove_where(|t| t.uri == uri);
                        self.bump_count(&playlist_id, -i64::from(removed));
                    }
                    self.info(format!("Removed {label} from {playlist_name}"));
                }
                Err(e) => self.error(&e),
            },
            Msg::Status(text) => self.info(text),
            Msg::Me(Err(e))
            | Msg::Playback(Err(e))
            | Msg::Results(Err(e))
            | Msg::Devices(Err(e))
            | Msg::Error(e) => self.error(&e),
        }
        None
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
            Mode::Picker => self.picker_key(key.code),
            Mode::NewPlaylist => self.new_playlist_key(key.code),
            Mode::Help => self.help_key(key.code),
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

    /// `l`: like the highlighted track, or unlike it if it is already liked.
    fn toggle_like(&mut self) -> Option<Action> {
        let track = self.selected_track()?;
        let (uri, label) = (track.uri.clone(), track.name.clone());
        if !is_likeable(&uri) {
            self.error("Only tracks and episodes can be liked.");
            return None;
        }
        match self.liked_flags.get(&uri) {
            Some(&liked) => Some(Action::SetLiked {
                uri,
                liked: !liked,
                label,
            }),
            None => {
                self.info("Still checking whether this is liked. Try again in a moment.");
                None
            }
        }
    }

    /// `A`: choose a playlist to add the highlighted track to.
    fn open_picker(&mut self) -> Option<Action> {
        let track = self.selected_track()?.clone();
        if !is_likeable(&track.uri) {
            self.error("Only tracks and episodes can be added to playlists.");
            return None;
        }
        if self.me_id.is_none() {
            self.error(
                "Your profile has not loaded yet, so your playlists are unknown. Try again.",
            );
            return None;
        }
        self.picker_track = Some(track);
        self.picker_state.select(Some(0));
        self.mode = Mode::Picker;
        if !self.playlists.loaded && !self.playlists.loading {
            self.playlists.loading = true;
            return Some(Action::LoadPlaylists { offset: 0 });
        }
        // Pull any pages not loaded yet; the rest follow as each one arrives.
        self.playlists
            .next_page()
            .map(|offset| Action::LoadPlaylists { offset })
    }

    /// `x`: remove the highlighted track from the open playlist after a second `x`.
    fn remove_selected(&mut self, confirming: Option<String>) -> Option<Action> {
        if self.view != View::PlaylistTracks {
            return None;
        }
        let track = self.playlist_tracks.selected()?;
        let playlist = self.open_playlist.as_ref()?;
        let (uri, label) = (track.uri.clone(), track.name.clone());
        let (playlist_id, playlist_name) = (playlist.id.clone(), playlist.name.clone());
        if confirming.as_deref() == Some(uri.as_str()) {
            return Some(Action::RemoveFromPlaylist {
                playlist_id,
                playlist_name,
                uri,
                label,
            });
        }
        self.info(format!(
            "Remove \"{label}\" from {playlist_name}? Press x again to confirm."
        ));
        self.pending_remove = Some(uri);
        None
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
        // Only an immediately following `x` confirms a removal; any other key cancels it.
        let confirming = self.pending_remove.take();
        if confirming.is_some() && !matches!(code, KeyCode::Char('x') | KeyCode::Delete) {
            self.status.clear();
        }
        match code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc if self.history.is_empty() => self.quit = true,
            KeyCode::Esc | KeyCode::Left | KeyCode::Backspace | KeyCode::Char('a') => self.back(),
            KeyCode::Right | KeyCode::Char('d') => return self.open_selected(),
            KeyCode::Enter => return self.activate(),
            KeyCode::Char('/') => return self.open(View::Search),
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('s') => {
                return self.move_selection(1);
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('w') => {
                return self.move_selection(-1);
            }
            KeyCode::Char('e') => {
                return self.selected_track().map(|t| Action::Queue {
                    uri: t.uri.clone(),
                    label: t.name.clone(),
                });
            }
            KeyCode::Char('l') => return self.toggle_like(),
            KeyCode::Char('A') => return self.open_picker(),
            KeyCode::Char('x') | KeyCode::Delete => return self.remove_selected(confirming),
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
            KeyCode::Char('r') => return Some(Action::Restart),
            KeyCode::Char('z') => {
                let on = self.playback.as_ref().is_some_and(|p| p.shuffle_state);
                return Some(Action::Shuffle(!on));
            }
            KeyCode::Char('R') => {
                let mode = self
                    .playback
                    .as_ref()
                    .map_or(Repeat::Off, |p| p.repeat_state);
                return Some(Action::Repeat(mode.next()));
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                return self.volume_change(VOLUME_STEP as i16);
            }
            KeyCode::Char('-') => return self.volume_change(-(VOLUME_STEP as i16)),
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('D') => {
                self.mode = Mode::Devices;
                self.devices.clear();
                self.devices_state.select(None);
                return Some(Action::LoadDevices);
            }
            _ => {}
        }
        None
    }

    fn help_key(&mut self, code: KeyCode) -> Option<Action> {
        if matches!(
            code,
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('?')
        ) {
            self.mode = Mode::Browse;
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
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('s') => {
                step(&mut self.devices_state, self.devices.len(), 1)
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('w') => {
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

    /// The picker lists "New playlist..." first, then the playlists the user can write to.
    fn picker_key(&mut self, code: KeyCode) -> Option<Action> {
        let rows = self.picker_playlists().len() + 1;
        match code {
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('s') => {
                step(&mut self.picker_state, rows, 1)
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('w') => {
                step(&mut self.picker_state, rows, -1)
            }
            KeyCode::Esc => {
                self.mode = Mode::Browse;
                self.picker_track = None;
            }
            KeyCode::Enter => {
                let selected = self.picker_state.selected()?;
                if selected == 0 {
                    self.new_name.clear();
                    self.mode = Mode::NewPlaylist;
                    return None;
                }
                let playlist = (*self.picker_playlists().get(selected - 1)?).clone();
                let track = self.picker_track.take()?;
                self.mode = Mode::Browse;
                return Some(Action::AddToPlaylist {
                    playlist_id: playlist.id,
                    playlist_name: playlist.name,
                    uri: track.uri,
                    label: track.name,
                });
            }
            _ => {}
        }
        None
    }

    fn new_playlist_key(&mut self, code: KeyCode) -> Option<Action> {
        match code {
            KeyCode::Char(c) => self.new_name.push(c),
            KeyCode::Backspace => {
                self.new_name.pop();
            }
            KeyCode::Esc => self.mode = Mode::Picker,
            KeyCode::Enter if !self.new_name.trim().is_empty() => {
                let track = self.picker_track.take()?;
                self.mode = Mode::Browse;
                return Some(Action::CreatePlaylist {
                    name: self.new_name.trim().to_string(),
                    uri: track.uri,
                    label: track.name,
                });
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
    use crate::api::{Album, Artist, Count, Item, Owner};

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
            owner: Some(Owner {
                id: Some("me".into()),
                display_name: Some("Me".into()),
            }),
            collaborative: false,
        }
    }

    /// A playlist the user follows but neither owns nor collaborates on.
    fn followed_playlist(id: &str) -> Playlist {
        Playlist {
            owner: Some(Owner {
                id: Some("someone-else".into()),
                display_name: None,
            }),
            ..playlist(id)
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
            shuffle_state: false,
            repeat_state: Repeat::Off,
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
            app.on_key(key(KeyCode::Char('e'))),
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
    fn wasd_moves_like_the_arrow_keys() {
        let mut app = app_with_results();
        app.on_key(key(KeyCode::Char('s')));
        assert_eq!(app.results_state.selected(), Some(1));
        app.on_key(key(KeyCode::Char('w')));
        assert_eq!(app.results_state.selected(), Some(0));

        let mut app = App::new(Instant::now());
        app.on_key(key(KeyCode::Char('d')));
        assert_eq!(app.view, View::Search);
        app.mode = Mode::Browse;
        app.on_key(key(KeyCode::Char('a')));
        assert_eq!(app.view, View::Menu);
    }

    #[test]
    fn question_mark_opens_help_and_esc_q_or_question_mark_close_it() {
        let mut app = App::new(Instant::now());
        for close in [KeyCode::Esc, KeyCode::Char('q'), KeyCode::Char('?')] {
            assert_eq!(app.on_key(key(KeyCode::Char('?'))), None);
            assert_eq!(app.mode, Mode::Help);
            // Other keys do nothing while help is open.
            assert_eq!(app.on_key(key(KeyCode::Char('n'))), None);
            assert_eq!(app.mode, Mode::Help);
            app.on_key(key(close));
            assert_eq!(app.mode, Mode::Browse);
            assert!(!app.quit);
        }
    }

    #[test]
    fn r_restarts_the_track() {
        let mut app = App::new(Instant::now());
        assert_eq!(app.on_key(key(KeyCode::Char('r'))), Some(Action::Restart));
    }

    #[test]
    fn z_toggles_shuffle_from_the_reported_state() {
        let mut app = App::new(Instant::now());
        assert_eq!(
            app.on_key(key(KeyCode::Char('z'))),
            Some(Action::Shuffle(true))
        );
        let mut p = playback(true, 0, None);
        p.shuffle_state = true;
        app.apply(Msg::Playback(Ok(Some(p))), Instant::now());
        assert_eq!(
            app.on_key(key(KeyCode::Char('z'))),
            Some(Action::Shuffle(false))
        );
    }

    #[test]
    fn shift_r_cycles_repeat_off_all_one_off() {
        let mut app = App::new(Instant::now());
        let next = |app: &mut App, from: Repeat| {
            let mut p = playback(true, 0, None);
            p.repeat_state = from;
            app.apply(Msg::Playback(Ok(Some(p))), Instant::now());
            app.on_key(key(KeyCode::Char('R')))
        };
        assert_eq!(
            next(&mut app, Repeat::Off),
            Some(Action::Repeat(Repeat::Context))
        );
        assert_eq!(
            next(&mut app, Repeat::Context),
            Some(Action::Repeat(Repeat::Track))
        );
        assert_eq!(
            next(&mut app, Repeat::Track),
            Some(Action::Repeat(Repeat::Off))
        );
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
            app.on_key(key(KeyCode::Char('D'))),
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
        app.on_key(key(KeyCode::Char('D')));
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

    /// Search results "one" and "two", flags known, profile loaded, and three playlists
    /// loaded of which two (p1, p3) are writable.
    fn app_ready_to_add() -> App {
        let now = Instant::now();
        let mut app = app_with_results();
        app.apply(Msg::Me(Ok("me".into())), now);
        app.apply(
            Msg::LikedFlags {
                uris: vec!["spotify:track:one".into(), "spotify:track:two".into()],
                result: Ok(vec![false, true]),
            },
            now,
        );
        app.apply(
            Msg::Playlists {
                offset: 0,
                result: Ok(page(
                    vec![playlist("p1"), followed_playlist("p2"), playlist("p3")],
                    3,
                )),
            },
            now,
        );
        app
    }

    fn local_track() -> Track {
        Track {
            uri: "spotify:local:a:b:c:1".into(),
            ..track("local")
        }
    }

    #[test]
    fn l_likes_an_unliked_track_and_unlikes_a_liked_one() {
        let mut app = app_ready_to_add();
        assert_eq!(
            app.on_key(key(KeyCode::Char('l'))),
            Some(Action::SetLiked {
                uri: "spotify:track:one".into(),
                liked: true,
                label: "one".into()
            })
        );
        app.on_key(key(KeyCode::Down));
        assert_eq!(
            app.on_key(key(KeyCode::Char('l'))),
            Some(Action::SetLiked {
                uri: "spotify:track:two".into(),
                liked: false,
                label: "two".into()
            })
        );
    }

    #[test]
    fn l_waits_until_the_liked_status_is_known() {
        let mut app = app_with_results();
        assert_eq!(app.on_key(key(KeyCode::Char('l'))), None);
        assert!(app.status.contains("Still checking"), "{}", app.status);
        assert!(!app.status_is_error);
    }

    #[test]
    fn local_files_cannot_be_liked_or_added() {
        let now = Instant::now();
        let mut app = app_with_results();
        app.apply(Msg::Results(Ok(vec![local_track()])), now);
        assert_eq!(app.on_key(key(KeyCode::Char('l'))), None);
        assert!(app.status_is_error, "{}", app.status);
        app.apply(Msg::Me(Ok("me".into())), now);
        assert_eq!(app.on_key(key(KeyCode::Char('A'))), None);
        assert_eq!(app.mode, Mode::Browse);
    }

    #[test]
    fn fresh_results_request_a_liked_check_for_unchecked_likeable_tracks_only() {
        let now = Instant::now();
        let mut app = App::new(now);
        let results = vec![track("one"), local_track(), track("two")];
        assert_eq!(
            app.apply(Msg::Results(Ok(results)), now),
            Some(Action::CheckLiked(vec![
                "spotify:track:one".into(),
                "spotify:track:two".into()
            ]))
        );
        app.apply(
            Msg::LikedFlags {
                uris: vec!["spotify:track:one".into(), "spotify:track:two".into()],
                result: Ok(vec![true, false]),
            },
            now,
        );
        let again = vec![track("one"), track("two")];
        assert_eq!(app.apply(Msg::Results(Ok(again)), now), None);
        assert!(app.liked_flags["spotify:track:one"]);
        assert!(!app.liked_flags["spotify:track:two"]);
    }

    #[test]
    fn liked_songs_pages_are_known_liked_without_a_check() {
        let now = Instant::now();
        let mut app = App::new(now);
        let follow_up = app.apply(
            Msg::Liked {
                offset: 0,
                result: Ok(page(tracks(2), 2)),
            },
            now,
        );
        assert_eq!(follow_up, None);
        assert_eq!(app.liked_flags.len(), 2);
        assert!(app.liked_flags.values().all(|liked| *liked));
    }

    #[test]
    fn playlist_pages_request_a_liked_check_too() {
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
        let follow_up = app.apply(
            Msg::PlaylistTracks {
                id: "p1".into(),
                offset: 0,
                result: Ok(page(tracks(2), 2)),
            },
            now,
        );
        assert_eq!(
            follow_up,
            Some(Action::CheckLiked(vec![
                "spotify:track:t0".into(),
                "spotify:track:t1".into()
            ]))
        );
    }

    #[test]
    fn a_track_liked_now_appears_once_at_the_top_of_a_loaded_liked_list() {
        let now = Instant::now();
        let mut app = app_in_liked(3, 3);
        app.apply(Msg::Results(Ok(vec![track("new")])), now);
        let liked = |app: &mut App| {
            app.apply(
                Msg::LikeChanged {
                    uri: "spotify:track:new".into(),
                    liked: true,
                    label: "new".into(),
                    result: Ok(()),
                },
                now,
            )
        };
        liked(&mut app);
        liked(&mut app);
        assert_eq!(app.liked.items.len(), 4);
        assert_eq!(app.liked.items[0].uri, "spotify:track:new");
        assert_eq!(app.liked.total, 4);
        assert_eq!(
            app.liked.state.selected(),
            Some(1),
            "selection stays on the same song"
        );
        assert!(app.status.contains("Liked: new"), "{}", app.status);
    }

    #[test]
    fn unliking_clears_the_flag_but_keeps_the_row_until_the_list_reloads() {
        let now = Instant::now();
        let mut app = app_in_liked(3, 3);
        app.apply(
            Msg::LikeChanged {
                uri: "spotify:track:t1".into(),
                liked: false,
                label: "t1".into(),
                result: Ok(()),
            },
            now,
        );
        assert!(!app.liked_flags["spotify:track:t1"]);
        assert_eq!(app.liked.items.len(), 3);
    }

    #[test]
    fn a_failed_like_shows_the_error_and_keeps_the_flag() {
        let now = Instant::now();
        let mut app = app_ready_to_add();
        app.apply(
            Msg::LikeChanged {
                uri: "spotify:track:one".into(),
                liked: true,
                label: "one".into(),
                result: Err("403 Insufficient client scope".into()),
            },
            now,
        );
        assert!(app.status_is_error);
        assert!(!app.liked_flags["spotify:track:one"]);
    }

    #[test]
    fn the_picker_offers_new_playlist_then_only_playlists_the_user_can_write_to() {
        let mut app = app_ready_to_add();
        assert_eq!(app.on_key(key(KeyCode::Char('A'))), None);
        assert_eq!(app.mode, Mode::Picker);
        let ids: Vec<&str> = app
            .picker_playlists()
            .iter()
            .map(|p| p.id.as_str())
            .collect();
        assert_eq!(ids, ["p1", "p3"]);
        // Rows: 0 = New playlist, 1 = p1, 2 = p3; Down stops at the last row.
        for _ in 0..5 {
            app.on_key(key(KeyCode::Down));
        }
        assert_eq!(app.picker_state.selected(), Some(2));
    }

    #[test]
    fn enter_on_a_playlist_in_the_picker_adds_the_track() {
        let mut app = app_ready_to_add();
        app.on_key(key(KeyCode::Char('A')));
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Down));
        assert_eq!(
            app.on_key(key(KeyCode::Enter)),
            Some(Action::AddToPlaylist {
                playlist_id: "p3".into(),
                playlist_name: "List p3".into(),
                uri: "spotify:track:one".into(),
                label: "one".into()
            })
        );
        assert_eq!(app.mode, Mode::Browse);
        assert!(app.picker_track.is_none());
    }

    #[test]
    fn new_playlist_asks_for_a_name_then_creates_it_with_the_track() {
        let mut app = app_ready_to_add();
        app.on_key(key(KeyCode::Char('A')));
        assert_eq!(app.on_key(key(KeyCode::Enter)), None);
        assert_eq!(app.mode, Mode::NewPlaylist);
        chars(&mut app, "  ");
        assert_eq!(
            app.on_key(key(KeyCode::Enter)),
            None,
            "blank name is refused"
        );
        assert_eq!(app.mode, Mode::NewPlaylist);
        chars(&mut app, "My Mix x");
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(
            app.on_key(key(KeyCode::Enter)),
            Some(Action::CreatePlaylist {
                name: "My Mix".into(),
                uri: "spotify:track:one".into(),
                label: "one".into()
            })
        );
        assert_eq!(app.mode, Mode::Browse);
    }

    #[test]
    fn escape_steps_back_from_the_name_prompt_to_the_picker_and_then_out() {
        let mut app = app_ready_to_add();
        app.on_key(key(KeyCode::Char('A')));
        app.on_key(key(KeyCode::Enter));
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Picker);
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Browse);
        assert!(app.picker_track.is_none());
    }

    #[test]
    fn q_while_typing_a_playlist_name_is_text_not_quit() {
        let mut app = app_ready_to_add();
        app.on_key(key(KeyCode::Char('A')));
        app.on_key(key(KeyCode::Enter));
        chars(&mut app, "quiet");
        assert!(!app.quit);
        assert_eq!(app.new_name, "quiet");
    }

    #[test]
    fn add_needs_the_profile_to_have_loaded() {
        let now = Instant::now();
        let mut app = app_with_results();
        app.apply(Msg::Results(Ok(vec![track("one")])), now);
        assert_eq!(app.on_key(key(KeyCode::Char('A'))), None);
        assert_eq!(app.mode, Mode::Browse);
        assert!(app.status_is_error);
    }

    #[test]
    fn the_picker_loads_playlists_first_and_then_every_remaining_page() {
        let now = Instant::now();
        let mut app = app_with_results();
        app.apply(Msg::Me(Ok("me".into())), now);
        assert_eq!(
            app.on_key(key(KeyCode::Char('A'))),
            Some(Action::LoadPlaylists { offset: 0 })
        );
        let first = app.apply(
            Msg::Playlists {
                offset: 0,
                result: Ok(page(vec![playlist("p1")], 120)),
            },
            now,
        );
        assert_eq!(first, Some(Action::LoadPlaylists { offset: 1 }));
        let last = app.apply(
            Msg::Playlists {
                offset: 1,
                result: Ok(page(vec![playlist("p2")], 2)),
            },
            now,
        );
        assert_eq!(last, None);
    }

    #[test]
    fn add_is_not_offered_on_the_menu_or_playlist_list() {
        let mut app = App::new(Instant::now());
        assert_eq!(app.on_key(key(KeyCode::Char('A'))), None);
        assert_eq!(app.mode, Mode::Browse);
    }

    #[test]
    fn a_playlist_created_for_a_track_appears_first_with_its_count() {
        let now = Instant::now();
        let mut app = app_ready_to_add();
        app.apply(
            Msg::Created {
                label: "one".into(),
                created: Ok(Playlist {
                    items: None,
                    ..playlist("new")
                }),
                added: Ok(()),
            },
            now,
        );
        assert_eq!(app.playlists.items[0].id, "new");
        assert_eq!(app.playlists.items[0].items.as_ref().unwrap().total, 1);
        assert_eq!(app.playlists.total, 4);
        assert!(
            app.status.contains("Created List new and added one"),
            "{}",
            app.status
        );
        assert!(!app.status_is_error);
    }

    #[test]
    fn a_playlist_created_but_not_filled_reports_the_add_failure() {
        let now = Instant::now();
        let mut app = app_ready_to_add();
        app.apply(
            Msg::Created {
                label: "one".into(),
                created: Ok(playlist("new")),
                added: Err("403".into()),
            },
            now,
        );
        assert!(app.status_is_error);
        assert!(app.status.contains("adding one failed"), "{}", app.status);
        assert_eq!(app.playlists.items[0].items.as_ref().unwrap().total, 0);
    }

    #[test]
    fn creating_a_playlist_before_the_list_is_loaded_leaves_it_to_load_fresh_later() {
        let now = Instant::now();
        let mut app = App::new(now);
        app.apply(
            Msg::Created {
                label: "one".into(),
                created: Ok(playlist("new")),
                added: Ok(()),
            },
            now,
        );
        assert!(app.playlists.items.is_empty());
    }

    #[test]
    fn adding_a_track_bumps_that_playlists_count() {
        let now = Instant::now();
        let mut app = app_ready_to_add();
        app.apply(
            Msg::Added {
                playlist_id: "p1".into(),
                playlist_name: "List p1".into(),
                label: "one".into(),
                result: Ok(()),
            },
            now,
        );
        assert_eq!(app.playlists.items[0].items.as_ref().unwrap().total, 4);
        assert!(
            app.status.contains("Added one to List p1"),
            "{}",
            app.status
        );
    }

    #[test]
    fn x_asks_first_and_a_second_x_removes_the_track() {
        let mut app = app_in_playlist(3, 3);
        assert_eq!(app.on_key(key(KeyCode::Char('x'))), None);
        assert!(
            app.status.contains("Remove \"t0\" from List p1"),
            "{}",
            app.status
        );
        assert_eq!(
            app.on_key(key(KeyCode::Char('x'))),
            Some(Action::RemoveFromPlaylist {
                playlist_id: "p1".into(),
                playlist_name: "List p1".into(),
                uri: "spotify:track:t0".into(),
                label: "t0".into()
            })
        );
    }

    #[test]
    fn any_other_key_cancels_a_pending_removal() {
        let mut app = app_in_playlist(3, 3);
        app.on_key(key(KeyCode::Char('x')));
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.status, "", "the prompt is cleared");
        // x now asks about the newly highlighted track instead of removing t0.
        assert_eq!(app.on_key(key(KeyCode::Char('x'))), None);
        assert!(app.status.contains("\"t1\""), "{}", app.status);
    }

    #[test]
    fn x_does_nothing_outside_an_open_playlist() {
        let mut app = app_in_liked(3, 3);
        assert_eq!(app.on_key(key(KeyCode::Char('x'))), None);
        assert_eq!(app.on_key(key(KeyCode::Char('x'))), None);
        assert_eq!(app.status, "");
    }

    #[test]
    fn a_removed_track_leaves_the_list_and_the_counts_drop() {
        let now = Instant::now();
        let mut app = app_in_playlist(3, 3);
        app.on_key(key(KeyCode::Down));
        app.apply(
            Msg::Removed {
                playlist_id: "p1".into(),
                playlist_name: "List p1".into(),
                uri: "spotify:track:t1".into(),
                label: "t1".into(),
                result: Ok(()),
            },
            now,
        );
        let uris: Vec<&str> = app
            .playlist_tracks
            .items
            .iter()
            .map(|t| t.uri.as_str())
            .collect();
        assert_eq!(uris, ["spotify:track:t0", "spotify:track:t2"]);
        assert_eq!(app.playlist_tracks.total, 2);
        assert_eq!(app.playlists.items[0].items.as_ref().unwrap().total, 2);
        assert_eq!(
            app.open_playlist
                .as_ref()
                .unwrap()
                .items
                .as_ref()
                .unwrap()
                .total,
            2
        );
        assert!(
            app.status.contains("Removed t1 from List p1"),
            "{}",
            app.status
        );
    }

    #[test]
    fn removing_the_last_row_moves_the_selection_up_and_the_last_track_empties_the_list() {
        let now = Instant::now();
        let mut app = app_in_playlist(2, 2);
        app.on_key(key(KeyCode::Down));
        let removed = |app: &mut App, name: &str| {
            app.apply(
                Msg::Removed {
                    playlist_id: "p1".into(),
                    playlist_name: "List p1".into(),
                    uri: format!("spotify:track:{name}"),
                    label: name.into(),
                    result: Ok(()),
                },
                now,
            )
        };
        removed(&mut app, "t1");
        assert_eq!(app.playlist_tracks.state.selected(), Some(0));
        removed(&mut app, "t0");
        assert_eq!(app.playlist_tracks.state.selected(), None);
    }

    #[test]
    fn a_removal_confirmed_for_a_playlist_the_user_left_does_not_touch_the_open_one() {
        let now = Instant::now();
        let mut app = app_in_playlist(3, 3);
        app.apply(
            Msg::Removed {
                playlist_id: "other".into(),
                playlist_name: "Other".into(),
                uri: "spotify:track:t1".into(),
                label: "t1".into(),
                result: Ok(()),
            },
            now,
        );
        assert_eq!(app.playlist_tracks.items.len(), 3);
    }

    #[test]
    fn a_failed_removal_keeps_the_track_and_shows_the_error() {
        let now = Instant::now();
        let mut app = app_in_playlist(3, 3);
        app.apply(
            Msg::Removed {
                playlist_id: "p1".into(),
                playlist_name: "List p1".into(),
                uri: "spotify:track:t0".into(),
                label: "t0".into(),
                result: Err("403 Forbidden".into()),
            },
            now,
        );
        assert_eq!(app.playlist_tracks.items.len(), 3);
        assert!(app.status_is_error);
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
