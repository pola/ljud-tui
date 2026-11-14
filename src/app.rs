use crate::api::{self, Album, Api, Device, Playback, Playlist, Track, User};
use crate::config::{data_dir, Config};
use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::{ListState, TableState};
use serde_json::{json, Value};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

const LIKED_MAX: usize = 5000;

/// Shorten long (often hex-id-like) names for titles.
pub fn short(name: &str) -> String {
    if name.chars().count() > 24 {
        format!("{}…", name.chars().take(12).collect::<String>())
    } else {
        name.to_string()
    }
}

/// Messages from background threads to the UI thread.
#[allow(clippy::large_enum_variant)]
pub enum Msg {
    Playlists(Vec<Playlist>),
    Rows { seq: u64, rows: Vec<Row> },
    RowsFailed { seq: u64, err: String },
    Playback(Option<Playback>),
    PlaybackError(String),
    Devices(Vec<Device>),
    NeedDevice(Option<Value>),
    Done { info: String, reload_playlists: bool, reload_view: bool },
    Error(String),
    Nothing,
}

#[derive(Clone)]
pub enum Row {
    Header(String),
    Track(Track),
    Album(Album),
    Playlist(Playlist),
}

#[derive(Clone, PartialEq)]
pub enum View {
    Liked,
    Playlist(Playlist),
    Album(Album),
    Search(String),
}

#[derive(Clone, Copy, PartialEq)]
pub enum Focus {
    Library,
    Rows,
}

pub enum Popup {
    Closed,
    Help,
    Devices { list: Vec<Device>, state: ListState, loading: bool },
    Input { purpose: InputPurpose, text: String },
    Confirm { purpose: ConfirmPurpose, prompt: String },
    PickPlaylist { uris: Vec<String>, label: String, choices: Vec<Playlist>, state: ListState },
}

pub enum InputPurpose {
    Search,
    NewPlaylist,
    RenamePlaylist(Playlist),
    RenameDevice(Device),
}

impl InputPurpose {
    pub fn title(&self) -> String {
        match self {
            InputPurpose::Search => " Search ".into(),
            InputPurpose::NewPlaylist => " New playlist name ".into(),
            InputPurpose::RenamePlaylist(p) => format!(" Rename playlist “{}” ", p.name),
            InputPurpose::RenameDevice(d) => {
                format!(" Name for speaker “{}” (empty = original) ", short(&d.name))
            }
        }
    }
}

pub enum ConfirmPurpose {
    DeletePlaylist(Playlist),
    RemoveTrack { playlist: Playlist, track: Track },
    Unlike(Track),
}

pub struct App {
    api: Api,
    pub cfg: Config,
    pub me: User,
    tx: Sender<Msg>,
    poke: Sender<()>,

    pub playlists: Vec<Playlist>,
    pub lib_state: ListState,

    pub view: View,
    view_seq: u64,
    pub rows: Vec<Row>,
    pub rows_state: TableState,
    pub loading: bool,
    pending_select: Option<usize>,

    pub focus: Focus,
    pub playback: Option<Playback>,
    pub playback_at: Instant,
    pub popup: Popup,
    pub status: Option<(String, bool, Instant)>,
    pending_play: Option<Option<Value>>,
    last_device: Option<String>,
    pub quit: bool,
}

fn last_device_path() -> std::path::PathBuf {
    data_dir().join("last_device")
}

pub fn run(api: Api, cfg: Config, me: User, note: Option<String>) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let (poke_tx, poke_rx) = mpsc::channel();
    spawn_poller(api.clone(), tx.clone(), poke_rx);

    let mut app = App {
        api,
        cfg,
        me,
        tx,
        poke: poke_tx,
        playlists: Vec::new(),
        lib_state: ListState::default().with_selected(Some(0)),
        view: View::Liked,
        view_seq: 0,
        rows: Vec::new(),
        rows_state: TableState::default(),
        loading: false,
        pending_select: None,
        focus: Focus::Library,
        playback: None,
        playback_at: Instant::now(),
        popup: Popup::Closed,
        status: None,
        pending_play: None,
        last_device: std::fs::read_to_string(last_device_path())
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        quit: false,
    };
    app.load_playlists();
    app.open_view(View::Liked);
    if let Some(n) = note {
        app.info(n);
    }

    let mut terminal = ratatui::init();
    let result = (|| -> Result<()> {
        while !app.quit {
            terminal.draw(|f| crate::ui::draw(f, &mut app))?;
            if event::poll(Duration::from_millis(150))? {
                if let Event::Key(k) = event::read()? {
                    if k.kind == KeyEventKind::Press {
                        app.on_key(k);
                    }
                }
            }
            while let Ok(m) = rx.try_recv() {
                app.on_msg(m);
            }
        }
        Ok(())
    })();
    ratatui::restore();
    result
}

/// Polls the playback state every few seconds, or shortly after a "poke".
fn spawn_poller(api: Api, tx: Sender<Msg>, poke: Receiver<()>) {
    std::thread::spawn(move || loop {
        match poke.recv_timeout(Duration::from_secs(3)) {
            Ok(()) => {
                // Give Spotify a moment to apply the command.
                std::thread::sleep(Duration::from_millis(400));
                while poke.try_recv().is_ok() {}
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        let msg = match api.playback() {
            Ok(p) => Msg::Playback(p),
            Err(e) => Msg::PlaybackError(format!("{e:#}")),
        };
        if tx.send(msg).is_err() {
            break;
        }
    });
}

fn move_sel(len: usize, cur: Option<usize>, delta: isize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let cur = cur.unwrap_or(0) as isize;
    Some((cur + delta).clamp(0, len as isize - 1) as usize)
}

impl App {
    // ------------------------------------------------------------ helpers

    pub fn info(&mut self, s: impl Into<String>) {
        self.status = Some((s.into(), false, Instant::now()));
    }

    pub fn error(&mut self, s: impl Into<String>) {
        self.status = Some((s.into(), true, Instant::now()));
    }

    /// Run `f` on a background thread; its result comes back as a `Msg`.
    fn bg<F>(&self, f: F)
    where
        F: FnOnce(&Api) -> Result<Msg> + Send + 'static,
    {
        let api = self.api.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let msg = f(&api).unwrap_or_else(|e| Msg::Error(format!("{e:#}")));
            let _ = tx.send(msg);
        });
    }

    /// A playback command; refreshes the playback state afterwards.
    fn control<F>(&self, label: &'static str, f: F)
    where
        F: FnOnce(&Api) -> Result<()> + Send + 'static,
    {
        let poke = self.poke.clone();
        self.bg(move |api| match f(api) {
            Ok(()) => {
                let _ = poke.send(());
                Ok(Msg::Nothing)
            }
            Err(e) if api::is_no_device(&e) && label == "Resume" => Ok(Msg::NeedDevice(None)),
            Err(e) => Err(e.context(label)),
        });
    }

    pub fn device_label(&self, d: &Device) -> String {
        self.cfg
            .alias_for(d.id.as_deref(), &d.name)
            .map(String::from)
            .unwrap_or_else(|| d.name.clone())
    }

    pub fn is_editable(&self, p: &Playlist) -> bool {
        p.owner_id == self.me.id || p.collaborative
    }

    fn editable_playlists(&self) -> Vec<Playlist> {
        self.playlists
            .iter()
            .filter(|p| self.is_editable(p))
            .cloned()
            .collect()
    }

    /// The playlist selected in the library pane (None for "Liked songs").
    fn selected_library_playlist(&self) -> Option<Playlist> {
        match self.lib_state.selected() {
            Some(i) if i > 0 => self.playlists.get(i - 1).cloned(),
            _ => None,
        }
    }

    fn selected_track(&self) -> Option<Track> {
        match self.rows_state.selected().and_then(|i| self.rows.get(i)) {
            Some(Row::Track(t)) => Some(t.clone()),
            _ => None,
        }
    }

    pub fn current_progress_ms(&self) -> u64 {
        match &self.playback {
            Some(p) if p.is_playing => {
                let dur = p.track.as_ref().map(|t| t.duration_ms).unwrap_or(u64::MAX);
                (p.progress_ms + self.playback_at.elapsed().as_millis() as u64).min(dur)
            }
            Some(p) => p.progress_ms,
            None => 0,
        }
    }

    fn set_last_device(&mut self, id: &str) {
        if self.last_device.as_deref() != Some(id) {
            self.last_device = Some(id.to_string());
            let _ = std::fs::create_dir_all(data_dir());
            let _ = std::fs::write(last_device_path(), id);
        }
    }

    // ------------------------------------------------------------ loading

    fn load_playlists(&self) {
        self.bg(|api| Ok(Msg::Playlists(api.playlists()?)));
    }

    fn open_view(&mut self, view: View) {
        self.view_seq += 1;
        let seq = self.view_seq;
        self.view = view.clone();
        self.rows.clear();
        self.rows_state = TableState::default();
        self.loading = true;
        self.bg(move |api| {
            let rows = (|| -> Result<Vec<Row>> {
                Ok(match &view {
                    View::Liked => api.liked(LIKED_MAX)?.into_iter().map(Row::Track).collect(),
                    View::Playlist(p) => {
                        api.playlist_tracks(&p.id)?.into_iter().map(Row::Track).collect()
                    }
                    View::Album(a) => api.album_tracks(a)?.into_iter().map(Row::Track).collect(),
                    View::Search(q) => {
                        let r = api.search(q)?;
                        let mut rows = Vec::new();
                        if !r.tracks.is_empty() {
                            rows.push(Row::Header("Tracks".into()));
                            rows.extend(r.tracks.into_iter().map(Row::Track));
                        }
                        if !r.albums.is_empty() {
                            rows.push(Row::Header("Albums".into()));
                            rows.extend(r.albums.into_iter().map(Row::Album));
                        }
                        if !r.playlists.is_empty() {
                            rows.push(Row::Header("Playlists".into()));
                            rows.extend(r.playlists.into_iter().map(Row::Playlist));
                        }
                        rows
                    }
                })
            })();
            Ok(match rows {
                Ok(rows) => Msg::Rows { seq, rows },
                Err(e) => Msg::RowsFailed { seq, err: format!("{e:#}") },
            })
        });
    }

    fn reload_view(&mut self) {
        self.pending_select = self.rows_state.selected();
        self.open_view(self.view.clone());
    }

    pub fn view_title(&self) -> String {
        match &self.view {
            View::Liked => "♥ Liked songs".into(),
            View::Playlist(p) if p.owner_id == self.me.id => format!("♫ {}", p.name),
            View::Playlist(p) => format!("♫ {} — by {}", p.name, p.owner_name),
            View::Album(a) => format!("◎ {} — {}", a.name, a.artists),
            View::Search(q) => format!("Search: “{q}”"),
        }
    }

    // ------------------------------------------------------------ messages

    pub fn on_msg(&mut self, m: Msg) {
        match m {
            Msg::Playlists(list) => {
                self.playlists = list;
                let len = self.playlists.len() + 1;
                if self.lib_state.selected().is_none_or(|i| i >= len) {
                    self.lib_state.select(Some(len - 1));
                }
                if let View::Playlist(cur) = &self.view {
                    if let Some(p) = self.playlists.iter().find(|p| p.id == cur.id) {
                        self.view = View::Playlist(p.clone());
                    }
                }
            }
            Msg::Rows { seq, rows } if seq == self.view_seq => {
                self.rows = rows;
                self.loading = false;
                let sel = self
                    .pending_select
                    .take()
                    .map(|i| i.min(self.rows.len().saturating_sub(1)))
                    .or_else(|| self.rows.iter().position(|r| !matches!(r, Row::Header(_))))
                    .filter(|_| !self.rows.is_empty());
                self.rows_state.select(sel);
            }
            Msg::RowsFailed { seq, err } if seq == self.view_seq => {
                self.loading = false;
                self.error(err);
            }
            Msg::Rows { .. } | Msg::RowsFailed { .. } => {} // stale
            Msg::Playback(p) => {
                if let Some(id) = p.as_ref().and_then(|p| p.device.as_ref()).and_then(|d| d.id.clone()) {
                    self.set_last_device(&id);
                }
                self.playback = p;
                self.playback_at = Instant::now();
            }
            Msg::PlaybackError(e) => {
                // Don't let a background poll overwrite a fresher message.
                if self.status.as_ref().is_none_or(|(_, _, t)| t.elapsed() > Duration::from_secs(10)) {
                    self.error(format!("Playback status: {e}"));
                }
            }
            Msg::Devices(list) => {
                if let Popup::Devices { list: l, state, loading } = &mut self.popup {
                    let sel = list.iter().position(|d| d.is_active).unwrap_or(0);
                    *l = list;
                    *loading = false;
                    state.select(if l.is_empty() { None } else { Some(sel) });
                }
            }
            Msg::NeedDevice(body) => {
                self.pending_play = Some(body);
                self.open_devices();
                self.info("No active speaker — choose one to play on.");
            }
            Msg::Done { info, reload_playlists, reload_view } => {
                self.info(info);
                if reload_playlists {
                    self.load_playlists();
                }
                if reload_view {
                    self.reload_view();
                }
            }
            Msg::Error(e) => self.error(e),
            Msg::Nothing => {}
        }
    }

    // ------------------------------------------------------------ actions

    fn open_devices(&mut self) {
        self.popup = Popup::Devices {
            list: Vec::new(),
            state: ListState::default(),
            loading: true,
        };
        self.bg(|api| Ok(Msg::Devices(api.devices()?)));
    }

    fn play(&mut self, device: Option<String>, body: Option<Value>) {
        let last = self.last_device.clone();
        let poke = self.poke.clone();
        self.bg(move |api| {
            let result = api.play(device.as_deref(), body.as_ref());
            match result {
                Ok(()) => {
                    let _ = poke.send(());
                    Ok(Msg::Nothing)
                }
                Err(e) if api::is_no_device(&e) && device.is_none() => {
                    // Try the speaker we last used before asking.
                    if let Some(id) = last {
                        if api.play(Some(&id), body.as_ref()).is_ok() {
                            let _ = poke.send(());
                            return Ok(Msg::Nothing);
                        }
                    }
                    Ok(Msg::NeedDevice(body))
                }
                Err(e) => Err(e.context("Play")),
            }
        });
    }

    fn play_row(&mut self, idx: usize) {
        let Some(Row::Track(t)) = self.rows.get(idx).cloned() else {
            return;
        };
        let body = match &self.view {
            View::Playlist(p) => json!({ "context_uri": p.uri, "offset": { "position": t.pos } }),
            View::Album(a) => json!({ "context_uri": a.uri, "offset": { "position": t.pos } }),
            View::Liked | View::Search(_) => {
                let uris: Vec<&str> = self.rows[idx..]
                    .iter()
                    .filter_map(|r| match r {
                        Row::Track(t) => Some(t.uri.as_str()),
                        _ => None,
                    })
                    .take(100)
                    .collect();
                json!({ "uris": uris })
            }
        };
        self.play(None, Some(body));
    }

    fn toggle_pause(&mut self) {
        let progress = self.current_progress_ms();
        match self.playback.as_mut() {
            Some(p) if p.is_playing => {
                p.progress_ms = progress;
                p.is_playing = false;
                self.playback_at = Instant::now();
                self.control("Pause", |api| api.pause());
            }
            Some(p) => {
                p.is_playing = true;
                self.playback_at = Instant::now();
                self.control("Resume", |api| api.play(None, None));
            }
            None => self.play(None, None),
        }
    }

    fn change_volume(&mut self, delta: i16) {
        let Some(dev) = self.playback.as_mut().and_then(|p| p.device.as_mut()) else {
            self.error("Nothing is playing.");
            return;
        };
        let Some(vol) = dev.volume else {
            self.error("This speaker doesn't allow volume control.");
            return;
        };
        let new = (vol as i16 + delta).clamp(0, 100) as u8;
        dev.volume = Some(new);
        self.control("Volume", move |api| api.volume(new));
    }

    fn seek_by(&mut self, delta_ms: i64) {
        if self.playback.is_none() {
            return;
        }
        let pos = (self.current_progress_ms() as i64 + delta_ms).max(0) as u64;
        if let Some(p) = &mut self.playback {
            p.progress_ms = pos;
        }
        self.playback_at = Instant::now();
        self.control("Seek", move |api| api.seek(pos));
    }

    fn activate(&mut self) {
        match self.focus {
            Focus::Library => {
                let view = match self.selected_library_playlist() {
                    Some(p) => View::Playlist(p),
                    None => View::Liked,
                };
                self.open_view(view);
                self.focus = Focus::Rows;
            }
            Focus::Rows => {
                let Some(idx) = self.rows_state.selected() else { return };
                match self.rows.get(idx).cloned() {
                    Some(Row::Track(_)) => self.play_row(idx),
                    Some(Row::Album(a)) => self.open_view(View::Album(a)),
                    Some(Row::Playlist(p)) => self.open_view(View::Playlist(p)),
                    _ => {}
                }
            }
        }
    }

    fn move_focused(&mut self, delta: isize) {
        match self.focus {
            Focus::Library => {
                let len = self.playlists.len() + 1;
                self.lib_state.select(move_sel(len, self.lib_state.selected(), delta));
            }
            Focus::Rows => {
                let len = self.rows.len();
                self.rows_state.select(move_sel(len, self.rows_state.selected(), delta));
            }
        }
    }

    // ------------------------------------------------------------ keys

    pub fn on_key(&mut self, k: KeyEvent) {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        let popup = std::mem::replace(&mut self.popup, Popup::Closed);
        match popup {
            Popup::Closed => self.key_main(k),
            other => {
                let next = self.key_popup(other, k);
                // A handler may have opened a different popup itself.
                if matches!(self.popup, Popup::Closed) {
                    self.popup = next;
                }
            }
        }
    }

    fn key_main(&mut self, k: KeyEvent) {
        use KeyCode::*;
        let page = 15;
        match k.code {
            Char('q') => self.quit = true,
            Char('?') => self.popup = Popup::Help,
            Char('/') => {
                self.popup = Popup::Input { purpose: InputPurpose::Search, text: String::new() }
            }
            Tab | BackTab | Left | Right => {
                self.focus = if self.focus == Focus::Library { Focus::Rows } else { Focus::Library }
            }
            Down | Char('j') => self.move_focused(1),
            Up | Char('k') => self.move_focused(-1),
            PageDown => self.move_focused(page),
            PageUp => self.move_focused(-page),
            Home | Char('g') => self.move_focused(isize::MIN / 2),
            End | Char('G') => self.move_focused(isize::MAX / 2),
            Enter => self.activate(),
            Char('d') => self.open_devices(),
            Char(' ') => self.toggle_pause(),
            Char('n') => self.control("Next", |api| api.next()),
            Char('p') => self.control("Previous", |api| api.previous()),
            Char('+') | Char('=') => self.change_volume(5),
            Char('-') => self.change_volume(-5),
            Char('<') | Char(',') => self.seek_by(-10_000),
            Char('>') | Char('.') => self.seek_by(10_000),
            Char('s') => {
                if let Some(p) = &mut self.playback {
                    p.shuffle = !p.shuffle;
                    let on = p.shuffle;
                    self.control("Shuffle", move |api| api.shuffle(on));
                }
            }
            Char('r') => {
                if let Some(p) = &mut self.playback {
                    let next = match p.repeat.as_str() {
                        "off" => "context",
                        "context" => "track",
                        _ => "off",
                    };
                    p.repeat = next.to_string();
                    self.control("Repeat", move |api| api.repeat(next));
                }
            }
            Char('R') => {
                self.load_playlists();
                self.reload_view();
                self.info("Refreshing…");
            }
            Char('N') => {
                self.popup = Popup::Input { purpose: InputPurpose::NewPlaylist, text: String::new() }
            }
            Char('e') if self.focus == Focus::Library => match self.selected_library_playlist() {
                Some(p) if p.owner_id == self.me.id => {
                    self.popup = Popup::Input {
                        text: p.name.clone(),
                        purpose: InputPurpose::RenamePlaylist(p),
                    }
                }
                Some(_) => self.error("You can only rename your own playlists."),
                None => {}
            },
            Char('D') if self.focus == Focus::Library => {
                if let Some(p) = self.selected_library_playlist() {
                    let prompt = if p.owner_id == self.me.id {
                        format!("Delete your playlist “{}”?", p.name)
                    } else {
                        format!("Remove “{}” (by {}) from your library?", p.name, p.owner_name)
                    };
                    self.popup = Popup::Confirm { purpose: ConfirmPurpose::DeletePlaylist(p), prompt };
                }
            }
            Char('a') if self.focus == Focus::Rows => {
                if let Some(t) = self.selected_track() {
                    let choices = self.editable_playlists();
                    if choices.is_empty() {
                        self.error("You have no playlists you can add to. Create one with N.");
                    } else {
                        self.popup = Popup::PickPlaylist {
                            uris: vec![t.uri.clone()],
                            label: t.name.clone(),
                            choices,
                            state: ListState::default().with_selected(Some(0)),
                        };
                    }
                }
            }
            Char('x') if self.focus == Focus::Rows => {
                let Some(t) = self.selected_track() else { return };
                match self.view.clone() {
                    View::Playlist(p) if self.is_editable(&p) => {
                        self.popup = Popup::Confirm {
                            prompt: format!("Remove “{}” from “{}”?", t.name, p.name),
                            purpose: ConfirmPurpose::RemoveTrack { playlist: p, track: t },
                        }
                    }
                    View::Liked => {
                        self.popup = Popup::Confirm {
                            prompt: format!("Remove “{}” from Liked songs?", t.name),
                            purpose: ConfirmPurpose::Unlike(t),
                        }
                    }
                    View::Playlist(_) => self.error("This playlist isn't yours to edit."),
                    _ => self.error("Open a playlist (or Liked songs) to remove tracks."),
                }
            }
            Char('L') if self.focus == Focus::Rows => {
                if let Some(t) = self.selected_track() {
                    let reload = self.view == View::Liked;
                    self.bg(move |api| {
                        api.save_track(&t, true)?;
                        Ok(Msg::Done {
                            info: format!("♥ Saved “{}” to Liked songs", t.name),
                            reload_playlists: false,
                            reload_view: reload,
                        })
                    });
                }
            }
            _ => {}
        }
    }

    /// Handle a key while a popup is open; returns the popup to show next.
    fn key_popup(&mut self, popup: Popup, k: KeyEvent) -> Popup {
        use KeyCode::*;
        match popup {
            Popup::Closed => Popup::Closed,
            Popup::Help => Popup::Closed,
            Popup::Input { purpose, mut text } => match k.code {
                Esc => Popup::Closed,
                Enter => {
                    self.submit_input(purpose, text.trim().to_string());
                    Popup::Closed
                }
                Backspace => {
                    text.pop();
                    Popup::Input { purpose, text }
                }
                Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    Popup::Input { purpose, text: String::new() }
                }
                Char(c) => {
                    text.push(c);
                    Popup::Input { purpose, text }
                }
                _ => Popup::Input { purpose, text },
            },
            Popup::Confirm { purpose, prompt } => match k.code {
                Char('y') | Char('Y') => {
                    self.confirm(purpose);
                    Popup::Closed
                }
                Char('n') | Char('N') | Esc | Char('q') => Popup::Closed,
                _ => Popup::Confirm { purpose, prompt },
            },
            Popup::PickPlaylist { uris, label, choices, mut state } => match k.code {
                Esc | Char('q') => Popup::Closed,
                Down | Char('j') => {
                    state.select(move_sel(choices.len(), state.selected(), 1));
                    Popup::PickPlaylist { uris, label, choices, state }
                }
                Up | Char('k') => {
                    state.select(move_sel(choices.len(), state.selected(), -1));
                    Popup::PickPlaylist { uris, label, choices, state }
                }
                Enter => {
                    if let Some(p) = state.selected().and_then(|i| choices.get(i)).cloned() {
                        let reload = matches!(&self.view, View::Playlist(v) if v.id == p.id);
                        self.bg(move |api| {
                            api.add_to_playlist(&p.id, &uris)?;
                            Ok(Msg::Done {
                                info: format!("Added “{label}” to “{}”", p.name),
                                reload_playlists: true,
                                reload_view: reload,
                            })
                        });
                    }
                    Popup::Closed
                }
                _ => Popup::PickPlaylist { uris, label, choices, state },
            },
            Popup::Devices { list, mut state, loading } => match k.code {
                Esc | Char('q') | Char('d') => {
                    self.pending_play = None;
                    Popup::Closed
                }
                Down | Char('j') => {
                    state.select(move_sel(list.len(), state.selected(), 1));
                    Popup::Devices { list, state, loading }
                }
                Up | Char('k') => {
                    state.select(move_sel(list.len(), state.selected(), -1));
                    Popup::Devices { list, state, loading }
                }
                Char('r') => {
                    self.open_devices();
                    Popup::Closed
                }
                Char('e') => match state.selected().and_then(|i| list.get(i)).cloned() {
                    Some(d) => Popup::Input {
                        text: self.cfg.alias_for(d.id.as_deref(), &d.name).unwrap_or("").to_string(),
                        purpose: InputPurpose::RenameDevice(d),
                    },
                    None => Popup::Devices { list, state, loading },
                },
                Enter => {
                    let Some(d) = state.selected().and_then(|i| list.get(i)).cloned() else {
                        return Popup::Devices { list, state, loading };
                    };
                    let Some(id) = d.id.clone() else {
                        self.error("Spotify doesn't allow controlling this device.");
                        return Popup::Devices { list, state, loading };
                    };
                    let label = self.device_label(&d);
                    self.set_last_device(&id);
                    match self.pending_play.take() {
                        Some(body) => self.play(Some(id), body),
                        None => {
                            let poke = self.poke.clone();
                            self.bg(move |api| {
                                api.transfer(&id, true)?;
                                let _ = poke.send(());
                                Ok(Msg::Done {
                                    info: format!("Playing on {label}"),
                                    reload_playlists: false,
                                    reload_view: false,
                                })
                            });
                        }
                    }
                    Popup::Closed
                }
                _ => Popup::Devices { list, state, loading },
            },
        }
    }

    fn submit_input(&mut self, purpose: InputPurpose, text: String) {
        match purpose {
            InputPurpose::Search => {
                if !text.is_empty() {
                    self.open_view(View::Search(text));
                    self.focus = Focus::Rows;
                }
            }
            InputPurpose::NewPlaylist => {
                if text.is_empty() {
                    return;
                }
                let user = self.me.id.clone();
                self.bg(move |api| {
                    let p = api.create_playlist(&user, &text)?;
                    Ok(Msg::Done {
                        info: format!("Created playlist “{}”", p.name),
                        reload_playlists: true,
                        reload_view: false,
                    })
                });
            }
            InputPurpose::RenamePlaylist(p) => {
                if text.is_empty() || text == p.name {
                    return;
                }
                self.bg(move |api| {
                    api.rename_playlist(&p.id, &text)?;
                    Ok(Msg::Done {
                        info: format!("Renamed “{}” to “{text}”", p.name),
                        reload_playlists: true,
                        reload_view: false,
                    })
                });
            }
            InputPurpose::RenameDevice(d) => {
                self.cfg.set_alias(d.id.as_deref(), &d.name, &text);
                match self.cfg.save() {
                    Ok(()) if text.is_empty() => self.info(format!("“{}” uses its own name again", d.name)),
                    Ok(()) => self.info(format!("“{}” is now called “{text}”", d.name)),
                    Err(e) => self.error(format!("Saving config: {e:#}")),
                }
                self.open_devices();
            }
        }
    }

    fn confirm(&mut self, purpose: ConfirmPurpose) {
        match purpose {
            ConfirmPurpose::DeletePlaylist(p) => {
                if matches!(&self.view, View::Playlist(v) if v.id == p.id) {
                    self.open_view(View::Liked);
                }
                self.bg(move |api| {
                    api.unfollow_playlist(&p.id)?;
                    Ok(Msg::Done {
                        info: format!("Removed “{}”", p.name),
                        reload_playlists: true,
                        reload_view: false,
                    })
                });
            }
            ConfirmPurpose::RemoveTrack { playlist, track } => {
                self.bg(move |api| {
                    api.remove_from_playlist(&playlist.id, &track.uri)?;
                    Ok(Msg::Done {
                        info: format!("Removed “{}” from “{}”", track.name, playlist.name),
                        reload_playlists: true,
                        reload_view: true,
                    })
                });
            }
            ConfirmPurpose::Unlike(track) => {
                self.bg(move |api| {
                    api.save_track(&track, false)?;
                    Ok(Msg::Done {
                        info: format!("Removed “{}” from Liked songs", track.name),
                        reload_playlists: false,
                        reload_view: true,
                    })
                });
            }
        }
    }
}
