//! Application state and the reducer that reacts to input and actor events.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::widgets::TableState;
use ratatui_image::picker::Picker;
use ratatui_image::thread::ThreadProtocol;
use tokio::sync::mpsc::UnboundedSender;

use crate::art::{ArtEvent, ArtReq};
use crate::audio::{AbLoop, AudioQuality, AudioStreamInfo, EqPreset, EqSettings};
use crate::config::{AccountChoice, Config, Paths, Session, State, VizMode};
use crate::discord::{PresenceCmd, PresenceTrack};
use crate::interlink::ExternalLink;
use crate::keymap::{Action, Keymap};
use crate::lyrics::{Lyrics, LyricsEvent, LyricsReq};
use crate::model::{Item, PlaylistRef, Track, video_id_from_url};
use crate::offline::{OfflineEvent, OfflineIndex, OfflineReq};
use crate::player::mpv::{PlayerCmd, PlayerEvent};
use crate::player::queue::{Queue, Repeat};
use crate::resolve::{ResolveReq, Resolved};
use crate::sleep::{SleepAction, SleepPreset, SleepTimer};
use crate::stats::Stats;
use crate::theme::{self, Theme};
use crate::viz::Frame;
use crate::ytm::{AccountInfo, LinkAction, SearchFilter, Suggestion, View, YtmEvent, YtmReq};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    NowPlaying,
    Queue,
    Lyrics,
    Search,
    Library,
}

impl Tab {
    pub const ALL: [Tab; 5] = [Tab::NowPlaying, Tab::Queue, Tab::Lyrics, Tab::Search, Tab::Library];
    /// (label, index of the hotkey letter within the label)
    pub fn label(self) -> (&'static str, usize) {
        match self {
            Tab::NowPlaying => ("now playing", 0),
            Tab::Queue => ("queue", 0),
            Tab::Lyrics => ("lyrics", 0),
            Tab::Search => ("search", 0),
            Tab::Library => ("library", 2),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibSection {
    Playlists,
    Liked,
    History,
    Offline,
}

impl LibSection {
    pub const ALL: [LibSection; 4] = [
        LibSection::Playlists,
        LibSection::Liked,
        LibSection::History,
        LibSection::Offline,
    ];
    pub fn label(self) -> &'static str {
        match self {
            LibSection::Playlists => "Playlists",
            LibSection::Liked => "Liked songs",
            LibSection::History => "History",
            LibSection::Offline => "Offline",
        }
    }
    fn view(self) -> View {
        match self {
            LibSection::Playlists => View::LibraryPlaylists,
            LibSection::Liked => View::LikedSongs,
            LibSection::History => View::History,
            LibSection::Offline => View::Offline,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatsTab {
    TopTracks,
    TopArtists,
    Recent,
}

impl StatsTab {
    pub const ALL: [StatsTab; 3] = [StatsTab::TopTracks, StatsTab::TopArtists, StatsTab::Recent];
    pub fn label(self) -> &'static str {
        match self {
            StatsTab::TopTracks => "Top tracks",
            StatsTab::TopArtists => "Top artists",
            StatsTab::Recent => "Recent",
        }
    }
    pub fn next(self) -> StatsTab {
        match self {
            StatsTab::TopTracks => StatsTab::TopArtists,
            StatsTab::TopArtists => StatsTab::Recent,
            StatsTab::Recent => StatsTab::TopTracks,
        }
    }
    pub fn prev(self) -> StatsTab {
        match self {
            StatsTab::TopTracks => StatsTab::Recent,
            StatsTab::TopArtists => StatsTab::TopTracks,
            StatsTab::Recent => StatsTab::TopArtists,
        }
    }
}

/// A loaded (or loading) list of items.
pub struct ListView {
    pub view: View,
    pub title: String,
    pub items: Vec<Item>,
    pub state: TableState,
    pub token: u64,
    pub loading: bool,
    pub error: Option<String>,
}

impl ListView {
    fn new(view: View, token: u64) -> ListView {
        ListView {
            view,
            title: String::new(),
            items: Vec::new(),
            state: TableState::default(),
            token,
            loading: true,
            error: None,
        }
    }
    pub fn selected(&self) -> Option<&Item> {
        self.state.selected().and_then(|i| self.items.get(i))
    }
    fn tracks(&self) -> Vec<Track> {
        self.items.iter().filter_map(Item::as_track).cloned().collect()
    }
}

#[derive(Default)]
pub struct SearchState {
    pub input: String,
    /// Cursor position in chars.
    pub cursor: usize,
    pub editing: bool,
    pub filter: SearchFilter,
    pub stack: Vec<ListView>,
    /// Autocomplete entries for `suggest_for`.
    pub suggestions: Vec<Suggestion>,
    pub suggest_for: String,
    /// Highlighted suggestion (↑/↓); None = the typed text.
    pub suggest_sel: Option<usize>,
    /// When to ask YTM for suggestions (debounced while typing).
    pub suggest_due: Option<Instant>,
}

/// Byte length of the prefix of `text` matching `prefix` case-insensitively.
/// Walks Unicode scalar boundaries so characters whose case-folding changes
/// UTF-8 byte length (e.g. `İ` -> `i\u{307}`) never slice at the wrong offset.
pub fn case_insensitive_prefix_len(text: &str, prefix: &str) -> Option<usize> {
    let mut want = prefix.chars().flat_map(char::to_lowercase);
    let mut next_want = want.next();
    if next_want.is_none() {
        return Some(0);
    }
    for (byte_idx, ch) in text.char_indices() {
        for lc in ch.to_lowercase() {
            if next_want != Some(lc) {
                return None;
            }
            next_want = want.next();
        }
        if next_want.is_none() {
            return Some(byte_idx + ch.len_utf8());
        }
    }
    None
}

impl SearchState {
    /// Suggestions are shown only while typing and only for the current text.
    pub fn visible_suggestions(&self) -> &[Suggestion] {
        if self.editing && !self.input.trim().is_empty() && self.suggest_for == self.input.trim() {
            &self.suggestions
        } else {
            &[]
        }
    }

    /// Inline completion: the rest of the top (or selected) suggestion when it
    /// extends what was typed, case-insensitively.
    pub fn ghost(&self) -> Option<&str> {
        let list = self.visible_suggestions();
        let top = list.get(self.suggest_sel.unwrap_or(0))?;
        if self.cursor != self.input.chars().count() {
            return None;
        }
        let n = case_insensitive_prefix_len(&top.text, &self.input)?;
        if n < top.text.len() { Some(&top.text[n..]) } else { None }
    }
}

/// How often pova re-checks the browser while waiting for sign-in to complete.
pub const AUTH_POLL_INTERVAL: Duration = Duration::from_secs(10);
/// Stop polling automatically after 3 minutes ("Check now" still works anytime).
pub const AUTH_POLL_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthInput {
    /// Custom `yt-dlp --cookies-from-browser` spec, e.g. `chrome:Profile 2`.
    CustomBrowser(String),
    /// Raw Cookie header pasted manually (rendered masked).
    PasteCookie(String),
}

#[derive(Clone)]
pub struct AuthWaiting {
    pub spec: String,
    pub label: String,
    pub browser_opened: bool,
    pub started: Instant,
    pub next_poll: Instant,
    pub checking: bool,
}

pub struct AuthUi {
    pub profiles: Vec<crate::auth::BrowserProfile>,
    pub profiles_loaded: bool,
    pub selected: usize,
    pub input: Option<AuthInput>,
    /// Status text shown alongside the spinner while checking a profile or saving a cookie.
    pub busy: Option<String>,
    /// Waiting for the user to finish signing in at `https://music.youtube.com`.
    pub waiting: Option<AuthWaiting>,
}

impl AuthUi {
    pub fn new() -> Self {
        Self {
            profiles: Vec::new(),
            profiles_loaded: false,
            selected: 0,
            input: None,
            busy: None,
            waiting: None,
        }
    }

    /// Detect installed browser profiles on demand (first time the sign-in
    /// screen is shown), keeping startup free of browser-directory I/O for
    /// users who are already signed in.
    pub fn ensure_profiles(&mut self) {
        if !self.profiles_loaded {
            self.profiles_loaded = true;
            if self.profiles.is_empty() {
                self.profiles = crate::auth::detect_profiles();
            }
        }
    }
}

pub struct LibraryState {
    pub section: LibSection,
    pub stack: Vec<ListView>,
    pub auth: AuthUi,
}

pub enum LyricsState {
    None,
    Loading { video_id: String },
    Found { lyrics: Lyrics, source: &'static str },
    Missing,
}

#[derive(Default, Clone)]
pub struct PlayerState {
    pub time: f64,
    pub duration: f64,
    pub paused: bool,
    pub volume: f64,
    pub muted: bool,
    pub speed: f64,
    pub audio: AudioStreamInfo,
    pub idle: bool,
    pub buffering: bool,
    pub ready: bool,
    /// Set if mpv failed to start or exited unexpectedly.
    pub error: Option<String>,
}

/// One actionable entry in the Interlink overlay (`i`).
#[derive(Clone, Debug)]
pub enum InterlinkAction {
    GoToArtist {
        name: String,
        artist_id: Option<String>,
    },
    GoToAlbum {
        title: String,
        album_id: Option<String>,
        artist: String,
    },
    StartRadio {
        track: Track,
    },
    ToggleOffline {
        track: Track,
        saved: bool,
    },
    ViewLyrics,
    CopyUniversal {
        video_id: String,
    },
    CopyYtm {
        video_id: String,
    },
    CopyTimestamped {
        video_id: String,
        secs: f64,
    },
    OpenYtm {
        video_id: String,
    },
    OpenUniversal {
        video_id: String,
    },
    SearchSpotify {
        title: String,
        artist: String,
    },
    SearchTidal {
        title: String,
        artist: String,
    },
}

impl InterlinkAction {
    pub fn label(&self) -> (String, String) {
        match self {
            InterlinkAction::GoToArtist { name, .. } => ("Go to Artist".into(), name.clone()),
            InterlinkAction::GoToAlbum { title, .. } => ("Go to Album".into(), title.clone()),
            InterlinkAction::StartRadio { .. } => ("Start Song Radio".into(), "similar tracks".into()),
            InterlinkAction::ToggleOffline { saved, .. } => {
                if *saved {
                    ("Remove Offline Copy".into(), "free local cache".into())
                } else {
                    ("Save for Offline Playback".into(), "instant local cache".into())
                }
            }
            InterlinkAction::ViewLyrics => ("Open Live Lyrics".into(), "synced".into()),
            InterlinkAction::CopyUniversal { .. } => (
                "Copy Universal Link".into(),
                "song.link (Spotify · Tidal · YT)".into(),
            ),
            InterlinkAction::CopyYtm { .. } => ("Copy YouTube Music Link".into(), "music.youtube.com".into()),
            InterlinkAction::CopyTimestamped { secs, .. } => (
                "Copy Link at Current Time".into(),
                format!("@ {}", crate::model::format_time(*secs)),
            ),
            InterlinkAction::OpenYtm { .. } => ("Open in YouTube Music".into(), "browser".into()),
            InterlinkAction::OpenUniversal { .. } => ("Open on song.link".into(), "Spotify · Tidal · Apple".into()),
            InterlinkAction::SearchSpotify { .. } => ("Find on Spotify".into(), "open.spotify.com".into()),
            InterlinkAction::SearchTidal { .. } => ("Find on Tidal".into(), "listen.tidal.com".into()),
        }
    }
}

pub enum Overlay {
    Help {
        scroll: u16,
    },
    Themes {
        names: Vec<(String, Option<PathBuf>)>,
        state: TableState,
        original: Theme,
    },
    Menu {
        state: TableState,
    },
    /// Yes / No question before something that can't be undone.
    Confirm {
        question: String,
        /// What "Yes" does.
        on_yes: Confirmed,
        /// Highlighted answer; starts on No.
        yes: bool,
    },
    /// Pick one of the user's playlists (or make a new one) for `track`.
    AddToPlaylist {
        track: Track,
        /// None while loading.
        playlists: Option<Vec<PlaylistRef>>,
        state: TableState,
        /// Some while typing a new playlist's name.
        naming: Option<String>,
    },
    /// Pick which YouTube account or Brand channel to use.
    Accounts {
        /// None while loading.
        items: Option<Vec<AccountInfo>>,
        state: TableState,
    },
    /// Spotify-style sleep timer picker (`z`).
    SleepTimer {
        state: TableState,
        /// Some while typing a custom number of minutes.
        custom: Option<String>,
    },
    /// Spotify/Tidal-style 10-band interactive graphic equalizer (`e`).
    Equalizer {
        band: usize,
    },
    /// Interlink hub & Tidal-style Signal Path (`i`).
    Interlink {
        track: Track,
        is_current: bool,
        actions: Vec<InterlinkAction>,
        state: TableState,
    },
    /// Spotify Wrapped Mini / Local listening statistics (`W`).
    Stats {
        tab: StatsTab,
        state: TableState,
    },
}

/// What a confirmed `Overlay::Confirm` does.
#[derive(Clone, Debug, PartialEq)]
pub enum Confirmed {
    DeletePlaylist { id: String, title: String },
    SignOut,
}

impl Confirmed {
    pub fn confirm_label(&self) -> &'static str {
        match self {
            Confirmed::DeletePlaylist { .. } => "  Yes, delete  ",
            Confirmed::SignOut => "  Yes, sign out  ",
        }
    }
}

/// Entries of the Esc menu (btop-style options).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuItem {
    Themes,
    VizMode,
    Autoplay,
    AlbumArt,
    Loudness,
    Notifications,
    Discord,
    AudioQuality,
    Crossfade,
    Equalizer,
    SleepTimer,
    Speed,
    PitchLock,
    Stats,
    DedupeQueue,
    SwitchAccount,
    Account,
    Help,
    Quit,
}

impl MenuItem {
    pub const ALL: [MenuItem; 19] = [
        MenuItem::Themes,
        MenuItem::VizMode,
        MenuItem::Autoplay,
        MenuItem::AlbumArt,
        MenuItem::Loudness,
        MenuItem::Notifications,
        MenuItem::Discord,
        MenuItem::AudioQuality,
        MenuItem::Crossfade,
        MenuItem::Equalizer,
        MenuItem::SleepTimer,
        MenuItem::Speed,
        MenuItem::PitchLock,
        MenuItem::Stats,
        MenuItem::DedupeQueue,
        MenuItem::SwitchAccount,
        MenuItem::Account,
        MenuItem::Help,
        MenuItem::Quit,
    ];

    /// A setting that changes in place with enter / ← → (the menu stays open).
    pub fn is_setting(self) -> bool {
        matches!(
            self,
            MenuItem::VizMode
                | MenuItem::Autoplay
                | MenuItem::AlbumArt
                | MenuItem::Loudness
                | MenuItem::Notifications
                | MenuItem::Discord
                | MenuItem::AudioQuality
                | MenuItem::Crossfade
                | MenuItem::Speed
                | MenuItem::PitchLock
        )
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Hit {
    Tab(Tab),
    Action(Action),
    Progress,
    Volume,
    List,
    SearchInput,
    SearchFilter(SearchFilter),
    LibSection(LibSection),
    Suggestion(usize),
    SwitchAccount,
    Auth,
    AuthProfile(usize),
    AuthCustomBrowser,
    AuthPasteCookie,
    AuthCheckNow,
    AuthOpenBrowser,
    AuthCancel,
    MenuItem(usize),
    ThemeItem(usize),
    AccountItem(usize),
    PlaylistPick(usize),
    Confirm(bool),
    CloseError,
    SleepTimer,
    SleepItem(usize),
    AudioQuality,
    NowPlayingArtist,
    NowPlayingAlbum,
    InterlinkItem(usize),
    EqBand(usize),
    EqToggle,
    EqPrevPreset,
    EqNextPreset,
    EqReset,
    LyricLine(usize),
    StatsTab(StatsTab),
    StatsItem(usize),
}

/// Clickable regions, rebuilt every frame.
#[derive(Default)]
pub struct HitMap {
    pub regions: Vec<(Rect, Hit)>,
    /// Inner area of the list currently on screen and its first visible row.
    pub list: Option<(Rect, usize)>,
}

impl HitMap {
    pub fn add(&mut self, rect: Rect, hit: Hit) {
        self.regions.push((rect, hit));
    }
    fn at(&self, x: u16, y: u16) -> Option<(Rect, Hit)> {
        self.regions
            .iter()
            .rev()
            .find(|(r, _)| r.contains(Position { x, y }))
            .copied()
    }
}

pub struct Flash {
    pub text: String,
    pub error: bool,
    pub until: Instant,
}

#[derive(Clone, Copy, Debug)]
enum Pending {
    /// Fill the list view carrying this token.
    View,
    /// Enqueue the loaded tracks.
    Enqueue(EnqueueMode),
    /// Play the first track of the result (`--play`).
    PlayFirst,
    /// Enqueue the first track of the result (remote `add` / `play-next`).
    First(EnqueueMode),
    /// The signed-in user's liked songs, for like status everywhere.
    LikedSet,
    /// The user's playlists, for the add-to-playlist picker.
    PlaylistPicker,
}

/// Expand `{key}` placeholders in a single pass so substituted values
/// containing braces (e.g. a track titled `{state}`) are never re-expanded.
fn expand_template(template: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        rest = &rest[open..];
        if let Some(close) = rest.find('}') {
            let key = &rest[1..close];
            if let Some(val) = lookup(key) {
                out.push_str(&val);
                rest = &rest[close + 1..];
                continue;
            }
        }
        out.push('{');
        rest = &rest[1..];
    }
    out.push_str(rest);
    out
}

/// Is `track` liked? Known statuses (`likes`) win over what the list said.
pub fn is_liked(likes: &HashMap<String, bool>, track: &Track) -> bool {
    likes.get(&track.video_id).copied().or(track.liked).unwrap_or(false)
}

fn stats_row_count(stats: &Stats, tab: StatsTab) -> usize {
    match tab {
        StatsTab::TopTracks => stats.top_tracks(20).len(),
        StatsTab::TopArtists => stats.top_artists(20).len(),
        StatsTab::Recent => stats.recent_tracks(25).len(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnqueueMode {
    Append,
    Next,
    PlayNow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RadioMode {
    /// Automatic continuation when the queue runs out.
    Autoplay,
    /// User pressed `r`: replace the queue with seed + radio.
    Start,
    /// User pressed `r` on the song that's playing: keep it playing and
    /// replace only what comes after it.
    Refill,
}

/// Seconds before the end of the last song at which autoplay fetches more,
/// so the next song is ready before this one finishes.
const AUTOPLAY_LEAD: f64 = 25.0;

pub struct Channels {
    pub player: UnboundedSender<PlayerCmd>,
    pub ytm: UnboundedSender<YtmReq>,
    pub lyrics: UnboundedSender<LyricsReq>,
    pub art: UnboundedSender<ArtReq>,
    pub resolve: UnboundedSender<ResolveReq>,
    pub notify: UnboundedSender<crate::notify::Note>,
    pub discord: UnboundedSender<PresenceCmd>,
    pub offline: UnboundedSender<OfflineReq>,
}

pub struct App {
    pub cfg: Config,
    pub paths: Paths,
    pub theme: Theme,
    pub keymap: Keymap,
    pub tab: Tab,
    pub show_viz: bool,
    pub show_player: bool,
    pub show_controls: bool,
    pub viz_mode: VizMode,
    pub viz_frame: Frame,
    pub queue: Queue,
    pub queue_state: TableState,
    pub player: PlayerState,
    pub search: SearchState,
    pub library: LibraryState,
    pub lyrics: LyricsState,
    pub lyrics_scroll: Option<u16>,
    /// Selected synced lyric line for interactive seek (Enter jumps to it).
    pub lyrics_sel: Option<usize>,
    /// Scroll position the synced view chose last frame.
    pub lyrics_auto: u16,
    /// Spotify-style sleep timer (countdown with fade-out, or end of track).
    pub sleep: SleepTimer,
    /// A-B loop section markers for the current track.
    pub ab_loop: AbLoop,
    /// Debounce timer so A-B seek doesn't double-fire on buffered mpv time updates.
    ab_last_wrap: Option<Instant>,
    /// Tracks when smooth track crossfade is attenuating mpv volume.
    crossfade_active: bool,
    crossfade_last_sent: Option<f64>,
    /// Last sleep timer countdown second drawn, so the badge ticks once per second.
    last_sleep_sec: Option<u64>,
    /// Local listening statistics (Wrapped Mini).
    pub stats: Stats,
    stats_vid: Option<String>,
    stats_listened: f64,
    stats_counted: bool,
    stats_dirty: bool,
    /// Saved offline tracks index (`~/.cache/pova/offline/`).
    pub offline: OfflineIndex,
    /// Video IDs currently being downloaded to offline cache.
    pub offline_pending: HashSet<String>,
    /// Last Discord Rich Presence snapshot sent (deduplicates IPC frames).
    last_discord: Option<Option<PresenceTrack>>,
    pub art: Option<ThreadProtocol>,
    /// Sad-cat mascot for tracks without lyrics.
    pub cat_art: Option<ThreadProtocol>,
    pub art_key: Option<String>,
    pub art_loading: bool,
    pub picker: Option<Picker>,
    pub flash: Option<Flash>,
    pub logged_in: bool,
    /// The first sign-in report arrived (later ones are re-logins).
    auth_seen: bool,
    /// Like status per video id, from Liked songs + lists + likes made here.
    pub likes: HashMap<String, bool>,
    /// Liked songs finished loading (unknown status then means "not liked").
    pub likes_loaded: bool,
    pub overlay: Option<Overlay>,
    pub hits: HitMap,
    pub quit: bool,
    pub dirty: bool,
    tx: Channels,
    next_sent: Option<String>,
    next_token: u64,
    pending: HashMap<u64, Pending>,
    radio: HashMap<String, RadioMode>,
    autoplay_requested: Option<String>,
    last_click: Option<(Instant, u16, u16)>,
    load_started: Option<Instant>,
    lyrics_requested: Option<String>,
    /// Pre-resolved direct audio URLs: video id -> (url, use-before).
    streams: HashMap<String, (String, SystemTime)>,
    /// Video ids sent to the resolver and not answered yet.
    resolving: HashSet<String>,
    /// Direct stream URL -> video id (mpv reports the URL as `path`).
    url_vid: HashMap<String, String>,
    /// Selected list track and since when (prefetch after a short dwell).
    dwell: Option<(String, Instant)>,
    dwell_fired: Option<String>,
    /// Current track was started from a pre-resolved URL.
    direct_loaded: Option<String>,
    /// Tracks whose direct URL failed once (fall back to mpv's yt-dlp hook).
    direct_failed: HashSet<String>,
    /// Consecutive playback errors without a track starting (stops infinite skip loops).
    consecutive_errors: usize,
    /// For the "track started in N ms" log line.
    start_timer: Option<(Instant, bool)>,
    /// Video id of the last song-change notification (also set for a paused
    /// resume, which shouldn't pop one up).
    notified: Option<String>,
    /// Disliked last song waiting for autoplay songs to skip to.
    skip_disliked: Option<String>,
    /// Even loudness / notifications / audio controls as last picked in UI;
    /// None until then, so config.toml keeps deciding.
    picked_loudness: Option<bool>,
    picked_notifications: Option<bool>,
    picked_discord: Option<bool>,
    picked_quality: Option<AudioQuality>,
    picked_crossfade: Option<u8>,
    picked_speed: Option<f64>,
    picked_pitch: Option<bool>,
    picked_eq: Option<EqSettings>,
    /// Explicitly chosen YouTube account / Brand channel (remembered in state.toml).
    pub account: Option<AccountChoice>,
}

impl App {
    pub fn new(
        cfg: Config,
        paths: Paths,
        state: State,
        keymap: Keymap,
        tx: Channels,
        picker: Option<Picker>,
        art: Option<ThreadProtocol>,
    ) -> App {
        let mut cfg = cfg;
        cfg.autoplay = state.autoplay.unwrap_or(cfg.autoplay);
        cfg.album_art = state.album_art.unwrap_or(cfg.album_art);
        cfg.normalize_loudness = state.normalize_loudness.unwrap_or(cfg.normalize_loudness);
        cfg.notifications = state.notifications.unwrap_or(cfg.notifications);
        cfg.discord_rpc = state.discord_rpc.unwrap_or(cfg.discord_rpc);
        cfg.audio_quality = state.audio_quality.unwrap_or(cfg.audio_quality);
        cfg.crossfade_secs = state.crossfade_secs.unwrap_or(cfg.crossfade_secs).min(12);
        cfg.playback_speed = state.playback_speed.unwrap_or(cfg.playback_speed).clamp(0.25, 3.0);
        cfg.pitch_correction = state.pitch_correction.unwrap_or(cfg.pitch_correction);
        if let Some(eq) = state.eq.clone() {
            cfg.eq = eq;
        }
        let theme_name = state.theme.clone().unwrap_or_else(|| cfg.theme.clone());
        let theme = theme::load_named(&paths.config_dir, &theme_name, cfg.theme_background);
        let volume = state.volume.unwrap_or(cfg.volume).min(100) as f64;
        let speed = cfg.playback_speed;
        let sleep_fade = cfg.sleep_fade_secs;
        let stats = Stats::load(&paths.stats_file());
        let offline = OfflineIndex::load(&paths.offline_dir());
        App {
            theme,
            keymap,
            tab: Tab::NowPlaying,
            show_viz: state.show_viz.unwrap_or(true),
            show_player: state.show_player.unwrap_or(true),
            show_controls: state.show_controls.unwrap_or(true),
            viz_mode: state.viz_mode.unwrap_or(cfg.viz.mode),
            viz_frame: Frame::default(),
            queue: Queue::default(),
            queue_state: TableState::default(),
            player: PlayerState {
                volume,
                speed,
                idle: true,
                ..Default::default()
            },
            search: SearchState::default(),
            library: LibraryState {
                section: LibSection::Playlists,
                stack: Vec::new(),
                auth: AuthUi::new(),
            },
            lyrics: LyricsState::None,
            lyrics_scroll: None,
            lyrics_sel: None,
            lyrics_auto: 0,
            sleep: SleepTimer {
                fade_secs: sleep_fade,
                ..SleepTimer::default()
            },
            ab_loop: AbLoop::Off,
            ab_last_wrap: None,
            crossfade_active: false,
            crossfade_last_sent: None,
            last_sleep_sec: None,
            stats,
            stats_vid: None,
            stats_listened: 0.0,
            stats_counted: false,
            stats_dirty: false,
            offline,
            offline_pending: HashSet::new(),
            last_discord: None,
            art,
            cat_art: None,
            art_key: None,
            art_loading: false,
            picker,
            flash: None,
            auth_seen: false,
            logged_in: false,
            likes: HashMap::new(),
            likes_loaded: false,
            overlay: None,
            hits: HitMap::default(),
            quit: false,
            dirty: true,
            tx,
            next_sent: None,
            next_token: 0,
            pending: HashMap::new(),
            radio: HashMap::new(),
            autoplay_requested: None,
            last_click: None,
            load_started: None,
            lyrics_requested: None,
            streams: HashMap::new(),
            resolving: HashSet::new(),
            url_vid: HashMap::new(),
            dwell: None,
            dwell_fired: None,
            direct_loaded: None,
            direct_failed: HashSet::new(),
            consecutive_errors: 0,
            start_timer: None,
            notified: None,
            skip_disliked: None,
            picked_loudness: state.normalize_loudness,
            picked_notifications: state.notifications,
            picked_discord: state.discord_rpc,
            picked_quality: state.audio_quality,
            picked_crossfade: state.crossfade_secs,
            picked_speed: state.playback_speed,
            picked_pitch: state.pitch_correction,
            picked_eq: state.eq,
            account: state.account,
            cfg,
            paths,
        }
    }

    // ───────────────────────────── helpers ─────────────────────────────

    pub fn flash(&mut self, text: impl Into<String>, error: bool) {
        let text = text.into();
        if error {
            tracing::warn!("{text}");
        }
        // Errors stay long enough to read: 8 s plus a second per ~15 characters.
        let secs = if error {
            (8 + text.chars().count() as u64 / 15).min(30)
        } else {
            3
        };
        self.flash = Some(Flash {
            text,
            error,
            until: Instant::now() + Duration::from_secs(secs),
        });
        self.dirty = true;
    }

    fn token(&mut self) -> u64 {
        self.next_token += 1;
        self.next_token
    }

    fn player(&self, cmd: PlayerCmd) {
        let _ = self.tx.player.send(cmd);
    }

    pub fn current(&self) -> Option<&Track> {
        self.queue.current_track()
    }

    /// Label of a menu entry, including the current value of settings.
    pub fn menu_label(&self, item: MenuItem) -> String {
        let on_off = |b: bool| if b { "on" } else { "off" };
        match item {
            MenuItem::Themes => format!("Theme          ‹ {} ›", self.theme.name),
            MenuItem::VizMode => format!("Visualizer     ‹ {} ›", self.viz_mode.label()),
            MenuItem::Autoplay => format!("Autoplay       ‹ {} ›", on_off(self.cfg.autoplay)),
            MenuItem::AlbumArt => format!("Album art      ‹ {} ›", on_off(self.cfg.album_art)),
            MenuItem::Loudness => format!("Even loudness  ‹ {} ›", on_off(self.cfg.normalize_loudness)),
            MenuItem::Notifications => format!("Notifications  ‹ {} ›", on_off(self.cfg.notifications)),
            MenuItem::Discord => format!("Discord status ‹ {} ›", on_off(self.cfg.discord_rpc)),
            MenuItem::AudioQuality => format!("Audio quality  ‹ {} ›", self.cfg.audio_quality.label()),
            MenuItem::Crossfade => format!(
                "Crossfade      ‹ {} ›",
                if self.cfg.crossfade_secs == 0 {
                    "off".to_string()
                } else {
                    format!("{}s", self.cfg.crossfade_secs)
                }
            ),
            MenuItem::Equalizer => format!("Equalizer      ‹ {} ›", self.cfg.eq.summary()),
            MenuItem::SleepTimer => format!("Sleep timer    ‹ {} ›", self.sleep.menu_value(Instant::now())),
            MenuItem::Speed => format!("Speed          ‹ {} ›", crate::audio::format_speed(self.cfg.playback_speed)),
            MenuItem::PitchLock => format!("Pitch lock     ‹ {} ›", on_off(self.cfg.pitch_correction)),
            MenuItem::Stats => format!("Listening stats ({} plays)", self.stats.total_plays),
            MenuItem::DedupeQueue => "Remove queue duplicates".into(),
            MenuItem::SwitchAccount => match self.account.as_ref().and_then(|a| a.name.as_deref()) {
                Some(name) if self.logged_in => format!("Switch account ({name})"),
                _ => "Switch account".into(),
            },
            MenuItem::Account => if self.logged_in { "Sign out" } else { "Sign in" }.into(),
            MenuItem::Help => "Help".into(),
            MenuItem::Quit => "Quit".into(),
        }
    }

    fn activate_menu(&mut self, item: MenuItem, forward: bool) {
        match item {
            MenuItem::Themes => self.open_themes(),
            MenuItem::SwitchAccount => self.open_accounts(),
            MenuItem::Account => self.open_account_action(),
            MenuItem::Help => self.overlay = Some(Overlay::Help { scroll: 0 }),
            MenuItem::Quit => self.quit = true,
            MenuItem::Loudness => {
                self.cfg.normalize_loudness = !self.cfg.normalize_loudness;
                self.picked_loudness = Some(self.cfg.normalize_loudness);
                self.player(PlayerCmd::SetNormalize(self.cfg.normalize_loudness));
                self.save_state();
            }
            MenuItem::Notifications => {
                if !self.cfg.notifications && crate::player::mpv::which("notify-send").is_none() {
                    self.flash(crate::notify::MISSING, true);
                    return;
                }
                self.cfg.notifications = !self.cfg.notifications;
                self.picked_notifications = Some(self.cfg.notifications);
                self.save_state();
            }
            MenuItem::Discord => {
                self.set_discord_rpc(!self.cfg.discord_rpc);
            }
            MenuItem::Stats => self.open_stats(),
            MenuItem::AudioQuality => {
                let next = if forward {
                    self.cfg.audio_quality.next()
                } else {
                    self.cfg.audio_quality.prev()
                };
                self.set_audio_quality(next);
            }
            MenuItem::Crossfade => {
                let next = crate::audio::cycle_crossfade(self.cfg.crossfade_secs, forward);
                self.cfg.crossfade_secs = next;
                self.picked_crossfade = Some(next);
                if next == 0 {
                    self.restore_crossfade_volume();
                }
                self.save_state();
            }
            MenuItem::Equalizer => self.open_equalizer(),
            MenuItem::SleepTimer => self.open_sleep_timer(),
            MenuItem::Speed => {
                let next = crate::audio::step_speed(self.cfg.playback_speed, forward);
                self.set_playback_speed(next);
            }
            MenuItem::PitchLock => {
                self.cfg.pitch_correction = !self.cfg.pitch_correction;
                self.picked_pitch = Some(self.cfg.pitch_correction);
                self.player(PlayerCmd::SetPitchCorrection(self.cfg.pitch_correction));
                self.save_state();
            }
            MenuItem::DedupeQueue => {
                let removed = self.queue.deduplicate_upcoming();
                self.sync_next();
                if removed == 0 {
                    self.flash("No duplicate upcoming tracks in queue", false);
                } else {
                    self.flash(
                        format!(
                            "Removed {removed} duplicate track{} from queue",
                            if removed == 1 { "" } else { "s" }
                        ),
                        false,
                    );
                }
            }
            MenuItem::VizMode => {
                self.viz_mode = if forward {
                    self.viz_mode.next()
                } else {
                    self.viz_mode.prev()
                };
                self.save_state();
            }
            MenuItem::Autoplay => {
                self.cfg.autoplay = !self.cfg.autoplay;
                if self.cfg.autoplay {
                    self.autoplay_requested = None;
                    self.maybe_autoplay();
                }
                self.save_state();
            }
            MenuItem::AlbumArt => {
                self.cfg.album_art = !self.cfg.album_art;
                if self.cfg.album_art {
                    self.art_key = None;
                    self.request_art();
                }
                self.save_state();
            }
        }
    }

    /// Save the queue and position for `resume` (no-op when it's off).
    pub fn save_session(&self) {
        if !self.cfg.resume {
            return;
        }
        Session {
            tracks: self.queue.entries.iter().map(|e| e.track.clone()).collect(),
            current: self.queue.current,
            position: self.player.time,
            shuffle: self.queue.shuffle,
            repeat: self.queue.repeat.label().into(),
        }
        .save(&self.paths.session_file());
    }

    /// Bring back the last run's queue, paused at the same spot. Returns
    /// whether anything was restored.
    pub fn restore_session(&mut self) -> bool {
        if !self.cfg.resume {
            return false;
        }
        let Some(s) = Session::load(&self.paths.session_file()) else {
            return false;
        };
        let current = s.current.unwrap_or(0).min(s.tracks.len() - 1);
        let duration = s.tracks[current].duration.unwrap_or(f64::MAX);
        // Within the last few seconds: start the song over instead.
        let start = (s.position > 1.0 && s.position < duration - 3.0).then_some(s.position);
        self.queue.shuffle = false; // keep the saved order as is
        self.queue.replace(s.tracks, current);
        self.queue.shuffle = s.shuffle;
        self.queue.repeat = match s.repeat.as_str() {
            "all" => Repeat::All,
            "one" => Repeat::One,
            _ => Repeat::Off,
        };
        if self.queue.repeat == Repeat::One {
            self.player(PlayerCmd::SetLoopFile(true));
        }
        self.load_index(current, start, true);
        self.flash("Picked up where you left off. Press space to play", false);
        true
    }

    pub fn save_state(&self) {
        State {
            theme: Some(self.theme.name.clone()),
            volume: Some(self.player.volume.round() as u8),
            viz_mode: Some(self.viz_mode),
            show_viz: Some(self.show_viz),
            show_player: Some(self.show_player),
            show_controls: Some(self.show_controls),
            autoplay: Some(self.cfg.autoplay),
            album_art: Some(self.cfg.album_art),
            normalize_loudness: self.picked_loudness,
            notifications: self.picked_notifications,
            discord_rpc: self.picked_discord,
            audio_quality: self.picked_quality,
            crossfade_secs: self.picked_crossfade,
            playback_speed: self.picked_speed,
            pitch_correction: self.picked_pitch,
            eq: self.picked_eq.clone(),
            account: self.account.clone(),
        }
        .save(&self.paths.state_file());
    }

    /// Something on screen is animating (spinners) and needs periodic redraws.
    pub fn animating(&self) -> bool {
        self.art_loading
            || self.player.buffering
            || self.load_started.is_some()
            || self.library.auth.busy.is_some()
            || self
                .library
                .auth
                .waiting
                .as_ref()
                .is_some_and(|w| w.checking || w.started.elapsed() <= AUTH_POLL_TIMEOUT)
            || matches!(self.lyrics, LyricsState::Loading { .. })
            || self.search.stack.iter().chain(&self.library.stack).any(|l| l.loading)
    }

    /// Per-frame housekeeping. Returns true if a redraw is needed.
    pub fn tick(&mut self) -> bool {
        let now = Instant::now();
        self.update_dwell();
        if self.sleep.is_active() {
            let is_playing = !self.player.paused && !self.player.idle;
            match self.sleep.tick(now, self.player.volume, is_playing) {
                SleepAction::None => {}
                SleepAction::SetFadeVolume(v) => {
                    self.crossfade_active = false;
                    self.player(PlayerCmd::SetVolume(v));
                }
                SleepAction::Trigger { restore_volume } => {
                    self.crossfade_active = false;
                    self.player(PlayerCmd::SetPause(true));
                    self.player.paused = true;
                    if let Some(vol) = restore_volume {
                        self.player.volume = vol;
                        self.player(PlayerCmd::SetVolume(vol));
                    }
                    self.last_sleep_sec = None;
                    self.flash("Sleep timer paused playback", false);
                }
            }
            let rem_sec = self.sleep.remaining(now).map(|d| d.as_secs());
            if rem_sec != self.last_sleep_sec {
                self.last_sleep_sec = rem_sec;
                self.dirty = true;
            }
        } else if self.last_sleep_sec.take().is_some() {
            self.dirty = true;
        }
        if let Some(w) = self.library.auth.waiting.as_mut()
            && !w.checking
            && w.started.elapsed() <= AUTH_POLL_TIMEOUT
            && now >= w.next_poll
        {
            w.checking = true;
            let _ = self.tx.ytm.send(YtmReq::PollSignIn {
                spec: w.spec.clone(),
                label: w.label.clone(),
            });
        }
        if let Some(due) = self.search.suggest_due
            && now >= due
        {
            self.search.suggest_due = None;
            let query = self.search.input.trim().to_string();
            if self.search.editing && !query.is_empty() && query != self.search.suggest_for {
                let _ = self.tx.ytm.send(YtmReq::Suggest { query });
            }
        }
        if let Some(f) = &self.flash
            && now > f.until
        {
            self.flash = None;
            self.dirty = true;
        }
        std::mem::take(&mut self.dirty)
    }

    // ───────────────────────────── playback ─────────────────────────────

    fn play_index(&mut self, i: usize) {
        self.load_index(i, None, false);
    }

    /// Load track `i`, optionally `start` seconds in and/or paused (resume).
    fn load_index(&mut self, i: usize, start: Option<f64>, paused: bool) {
        if i >= self.queue.len() {
            return;
        }
        self.queue.current = Some(i);
        let Some(track) = self.queue.get(i).cloned() else {
            return;
        };
        if paused {
            // Resumed paused: nothing new is playing yet, so no popup.
            self.notified = Some(track.video_id.clone());
        }
        if let Some(err) = self.player.error.clone() {
            self.load_started = None;
            self.player.time = 0.0;
            self.player.duration = track.duration.unwrap_or(0.0);
            self.flash(err, true);
            self.on_track_changed();
            return;
        }
        let (url, direct) = self.play_url(&track);
        self.direct_loaded = direct.then(|| track.video_id.clone());
        self.start_timer = Some((Instant::now(), direct));
        let next = self.desired_next();
        self.next_sent = next.clone();
        self.player(PlayerCmd::Load {
            url,
            next,
            start,
            paused,
        });
        self.player.time = start.unwrap_or(0.0);
        self.player.duration = self.queue.get(i).and_then(|t| t.duration).unwrap_or(0.0);
        self.load_started = Some(Instant::now());
        self.on_track_changed();
    }

    /// URL mpv should preload after the current track.
    fn desired_next(&mut self) -> Option<String> {
        if self.sleep.is_end_of_track() {
            return None; // stop cleanly after the current track finishes
        }
        if self.queue.repeat == Repeat::One {
            return None; // mpv loops the file itself (loop-file)
        }
        let n = self.queue.next_index()?;
        if Some(n) == self.queue.current {
            return None;
        }
        let track = self.queue.get(n)?.clone();
        Some(self.play_url(&track).0)
    }

    /// Pre-resolved direct URL (or local offline cache file!) if available, else
    /// the YouTube Music link (mpv's yt-dlp hook resolves it, ~3 s). Returns (url, is_direct).
    fn play_url(&mut self, track: &Track) -> (String, bool) {
        if let Some(local) = self.offline.local_path(&self.paths.offline_dir(), &track.video_id) {
            let path = local.to_string_lossy().into_owned();
            self.url_vid.insert(path.clone(), track.video_id.clone());
            return (path, true);
        }
        let fresh = self
            .streams
            .get(&track.video_id)
            .filter(|(_, until)| *until > SystemTime::now() && !self.direct_failed.contains(&track.video_id))
            .map(|(u, _)| u.clone());
        match fresh {
            Some(url) => {
                self.url_vid.insert(url.clone(), track.video_id.clone());
                (url, true)
            }
            None => (track.url(), false),
        }
    }

    /// Ask the resolver for a direct URL ahead of time (no-op if cached/pending/offline).
    pub fn prefetch(&mut self, video_id: &str) {
        if self.offline.contains(video_id) {
            return;
        }
        let fresh = self
            .streams
            .get(video_id)
            .is_some_and(|(_, until)| *until > SystemTime::now());
        // Keep the resolver queue short: it's a courtesy, not a crawler.
        if fresh
            || self.direct_failed.contains(video_id)
            || self.resolving.contains(video_id)
            || self.resolving.len() >= 4
        {
            return;
        }
        self.resolving.insert(video_id.to_string());
        let _ = self.tx.resolve.send(ResolveReq {
            video_id: video_id.to_string(),
            format: self.cfg.audio_quality.ytdl_format().to_string(),
        });
    }

    /// Prefetch the next two tracks of the queue.
    fn prefetch_upcoming(&mut self) {
        let Some(n) = self.queue.next_index() else { return };
        let ids: Vec<String> = [Some(n), (n + 1 < self.queue.len()).then_some(n + 1)]
            .into_iter()
            .flatten()
            .filter(|&i| Some(i) != self.queue.current)
            .filter_map(|i| self.queue.get(i).map(|t| t.video_id.clone()))
            .collect();
        for id in ids {
            self.prefetch(&id);
        }
    }

    pub fn on_resolved(&mut self, ev: Resolved) {
        self.resolving.remove(&ev.video_id);
        if let Some(stream) = ev.stream {
            self.streams.insert(ev.video_id, stream);
            // Upgrade mpv's preloaded next entry to the direct URL.
            self.sync_next();
        }
    }

    /// Prefetch the track the user has been resting on in a list.
    fn update_dwell(&mut self) {
        let selected = match self.tab {
            Tab::Search => self
                .search
                .stack
                .last()
                .and_then(|l| l.selected())
                .and_then(Item::as_track),
            Tab::Library => self
                .library
                .stack
                .last()
                .and_then(|l| l.selected())
                .and_then(Item::as_track),
            Tab::Queue => self.queue_state.selected().and_then(|i| self.queue.get(i)),
            _ => None,
        }
        .map(|t| t.video_id.clone());
        match (&self.dwell, selected) {
            (Some((id, since)), Some(sel)) if *id == sel => {
                if since.elapsed() >= Duration::from_millis(500) && self.dwell_fired.as_deref() != Some(&sel) {
                    self.dwell_fired = Some(sel.clone());
                    self.prefetch(&sel);
                }
            }
            (_, Some(sel)) => self.dwell = Some((sel, Instant::now())),
            (_, None) => self.dwell = None,
        }
    }

    /// Make mpv's "next" entry match the queue.
    fn sync_next(&mut self) {
        if self.queue.current.is_none() || self.player.idle && self.load_started.is_none() {
            return;
        }
        self.prefetch_upcoming();
        let want = self.desired_next();
        if want != self.next_sent {
            self.next_sent = want.clone();
            self.player(PlayerCmd::SetNext(want));
        }
    }

    fn on_track_changed(&mut self) {
        self.dirty = true;
        self.save_stats();
        self.stats_vid = self.current().map(|t| t.video_id.clone());
        self.stats_listened = 0.0;
        self.stats_counted = false;
        self.ab_loop.clear();
        self.ab_last_wrap = None;
        self.lyrics_sel = None;
        self.player.audio.clear();
        self.restore_crossfade_volume();
        self.save_session();
        self.prefetch_upcoming();
        self.lyrics_scroll = None;
        self.sync_discord(true);
        let Some(track) = self.current().cloned() else { return };
        // Lyrics
        if self.lyrics_requested.as_deref() != Some(&track.video_id) {
            self.lyrics_requested = Some(track.video_id.clone());
            self.lyrics = LyricsState::Loading {
                video_id: track.video_id.clone(),
            };
            let _ = self.tx.lyrics.send(LyricsReq {
                video_id: track.video_id.clone(),
                title: track.title.clone(),
                artist: track.artists.clone(),
                album: track.album.clone(),
                duration: track.duration,
            });
        }
        self.request_art();
        self.maybe_autoplay();
        self.notify_song();
    }

    /// Thumbs-down the current song and move on, like YouTube Music does.
    /// The rating is sent right away; skipping doesn't wait for the answer.
    fn dislike(&mut self) {
        let Some(t) = self.current() else { return };
        if !self.logged_in {
            self.flash(
                "Disliking songs needs you to be signed in. Open the Library tab (b) to sign in, \
                 or run `pova login`.",
                true,
            );
            return;
        }
        let video_id = t.video_id.clone();
        let _ = self.tx.ytm.send(YtmReq::Dislike {
            video_id: video_id.clone(),
        });
        self.consecutive_errors = 0;
        // A one-song queue on repeat-all "skips" to itself: treat as the end.
        match self.queue.skip_index().filter(|&i| Some(i) != self.queue.current) {
            Some(i) => {
                self.play_index(i);
                self.flash("Disliked · skipped", false);
            }
            None => {
                // Nothing to skip to: stop the disliked song rather than let it play on.
                if !self.player.paused {
                    self.player(PlayerCmd::TogglePause);
                }
                if self.cfg.autoplay && self.queue.repeat == Repeat::Off {
                    // Play the first autoplay song as soon as it arrives.
                    self.skip_disliked = Some(video_id.clone());
                    self.maybe_autoplay();
                    self.flash("Disliked · loading more songs…", false);
                } else {
                    self.flash("Disliked · end of queue", false);
                }
            }
        }
    }

    /// Desktop notification for the song that just started (if turned on).
    fn notify_song(&mut self) {
        let Some(track) = self.current() else { return };
        if !self.cfg.notifications || self.notified.as_deref() == Some(&track.video_id) {
            return;
        }
        let note = crate::notify::Note {
            title: track.title.clone(),
            artist: track.artists.clone(),
            album: track.album.clone(),
            art_url: Some(track.art_url()),
        };
        self.notified = Some(track.video_id.clone());
        let _ = self.tx.notify.send(note);
    }

    /// Fetch album art for the current track (if enabled and not shown yet).
    fn request_art(&mut self) {
        let Some(track) = self.current() else { return };
        if !self.cfg.album_art || self.picker.is_none() {
            return;
        }
        let url = track.art_url();
        if self.art_key.as_deref() != Some(&url) {
            self.art_key = Some(url.clone());
            self.art_loading = true;
            if let Some(a) = self.art.as_mut() {
                a.empty_protocol();
            }
            let _ = self.tx.art.send(ArtReq { key: url.clone(), url });
        }
    }

    /// When the last track starts, fetch a radio to keep the music going.
    fn maybe_autoplay(&mut self) {
        if !self.cfg.autoplay || self.queue.repeat != Repeat::Off {
            return;
        }
        let (Some(cur), Some(track)) = (self.queue.current, self.current()) else {
            return;
        };
        if cur + 1 < self.queue.len() {
            return;
        }
        let seed = track.video_id.clone();
        if self.autoplay_requested.as_deref() == Some(&seed) {
            return;
        }
        self.autoplay_requested = Some(seed.clone());
        self.radio.insert(seed.clone(), RadioMode::Autoplay);
        let _ = self.tx.ytm.send(YtmReq::Radio { seed });
    }

    fn next_track(&mut self) {
        self.consecutive_errors = 0;
        match self.queue.skip_index() {
            Some(i) => self.play_index(i),
            None => {
                if self.cfg.autoplay && self.current().is_some() {
                    self.flash("Loading more songs…", false);
                    self.maybe_autoplay();
                } else {
                    self.flash("End of queue", false);
                }
            }
        }
    }

    fn prev_track(&mut self) {
        if self.current().is_none() {
            return;
        }
        self.consecutive_errors = 0;
        if self.player.time > 3.0 || self.queue.prev_index().is_none() {
            self.player(PlayerCmd::SeekAbsolute(0.0));
        } else if let Some(i) = self.queue.prev_index() {
            self.play_index(i);
        }
    }

    fn enqueue(&mut self, tracks: Vec<Track>, mode: EnqueueMode) {
        if tracks.is_empty() {
            return;
        }
        self.consecutive_errors = 0;
        let n = tracks.len();
        let was_empty = self.queue.current.is_none();
        match mode {
            EnqueueMode::Append => {
                self.queue.append(tracks);
                self.flash(
                    format!("Added {n} track{} to queue", if n == 1 { "" } else { "s" }),
                    false,
                );
            }
            EnqueueMode::Next => {
                self.queue.insert_next(tracks);
                self.flash(
                    format!("Playing next: {n} track{}", if n == 1 { "" } else { "s" }),
                    false,
                );
            }
            EnqueueMode::PlayNow => {
                self.queue.insert_next(tracks);
                let i = self.queue.current.map_or(0, |c| c + 1);
                self.play_index(i);
                return;
            }
        }
        if was_empty {
            self.play_index(0);
        } else {
            self.sync_next();
        }
    }

    fn set_repeat(&mut self, r: Repeat) {
        self.queue.repeat = r;
        self.player(PlayerCmd::SetLoopFile(r == Repeat::One));
        self.sync_next();
        self.flash(format!("Repeat: {}", r.label()), false);
    }

    // ───────────────────────────── events ─────────────────────────────

    pub fn on_player(&mut self, ev: PlayerEvent) {
        match ev {
            PlayerEvent::Ready => {
                self.player.ready = true;
                self.player.error = None;
                self.player(PlayerCmd::SetVolume(self.player.volume));
            }
            PlayerEvent::TimePos(t) => {
                let prev = self.player.time;
                self.player.time = t;
                if self.player.duration > 0.0 && self.player.duration - t <= AUTOPLAY_LEAD {
                    self.maybe_autoplay();
                }
                if t > 0.0 {
                    self.consecutive_errors = 0;
                    if let Some((t0, direct)) = self.start_timer.take() {
                        let how = if direct { "pre-resolved URL" } else { "yt-dlp hook" };
                        tracing::info!("track started in {} ms ({how})", t0.elapsed().as_millis());
                    }
                }
                // Accumulate local listening statistics (Wrapped Mini).
                let delta = t - prev;
                if !self.player.paused && !self.player.idle && delta > 0.0 && delta <= 5.0
                    && let Some(track) = self.current().cloned()
                {
                    if self.stats_vid.as_deref() != Some(track.video_id.as_str()) {
                        self.stats_vid = Some(track.video_id.clone());
                        self.stats_listened = 0.0;
                        self.stats_counted = false;
                    }
                    self.stats_listened += delta;
                    let dur = if self.player.duration > 0.0 {
                        self.player.duration
                    } else {
                        track.duration.unwrap_or(60.0)
                    };
                    let threshold = (dur * 0.5).clamp(5.0, 25.0);
                    if !self.stats_counted && self.stats_listened >= threshold {
                        self.stats_counted = true;
                        self.stats
                            .record_play(&track, self.stats_listened, crate::stats::now_unix());
                        self.stats.save(&self.paths.stats_file());
                        self.stats_dirty = false;
                    } else if self.stats_counted {
                        self.stats.add_listening_time(&track, delta);
                        self.stats_dirty = true;
                    }
                } else if delta.abs() > 3.0 {
                    // User scrubbed/seeked: refresh Discord timestamp bar.
                    self.sync_discord(false);
                }
                // A-B section loop wrap-around (debounced 300ms so buffered updates don't double-seek).
                if let Some(a) = self.ab_loop.wrap_target(t)
                    && self
                        .ab_last_wrap
                        .is_none_or(|w| w.elapsed() >= Duration::from_millis(300))
                {
                    self.ab_last_wrap = Some(Instant::now());
                    self.player(PlayerCmd::SeekAbsolute(a));
                }
                // Spotify-style smooth track fade envelope (yields to sleep timer fade-out).
                if self.cfg.crossfade_secs > 0
                    && !self.sleep.is_fading()
                    && !self.player.paused
                    && !self.player.muted
                    && !self.player.idle
                    && self.ab_loop == AbLoop::Off
                {
                    match crate::audio::crossfade_volume(
                        self.player.volume,
                        t,
                        self.player.duration,
                        self.cfg.crossfade_secs,
                    ) {
                        Some(v) => {
                            if !self.crossfade_active
                                || self.crossfade_last_sent.is_none_or(|p| (p - v).abs() >= 0.5)
                            {
                                self.crossfade_active = true;
                                self.crossfade_last_sent = Some(v);
                                self.player(PlayerCmd::SetVolume(v));
                            }
                        }
                        None if self.crossfade_active => self.restore_crossfade_volume(),
                        None => {}
                    }
                } else if self.crossfade_active && !self.sleep.is_fading() {
                    self.restore_crossfade_volume();
                }
                let synced_lyrics = self.tab == Tab::Lyrics
                    && matches!(
                        self.lyrics,
                        LyricsState::Found {
                            lyrics: Lyrics::Synced(_),
                            ..
                        }
                    );
                let step_changed = (prev * 4.0) as u64 != (t * 4.0) as u64;
                if synced_lyrics || ((self.tab == Tab::NowPlaying || self.show_controls) && step_changed) {
                    self.dirty = true;
                }
                return;
            }
            PlayerEvent::Duration(d) => {
                self.player.duration = d;
                self.sync_discord(false);
            }
            PlayerEvent::Paused(p) => {
                self.player.paused = p;
                self.sync_discord(true);
            }
            PlayerEvent::Volume(v) => {
                // Keep the user's logical volume intact while sleep timer or crossfade is ramping mpv.
                if !self.sleep.is_fading() && !self.crossfade_active {
                    self.player.volume = v;
                }
            }
            PlayerEvent::Muted(m) => self.player.muted = m,
            PlayerEvent::Speed(s) => self.player.speed = s,
            PlayerEvent::AudioCodec(c) => {
                self.player.audio.codec = c;
                self.sync_discord(false);
            }
            PlayerEvent::SampleRate(r) => self.player.audio.sample_rate = r,
            PlayerEvent::Channels(ch) => self.player.audio.channels = ch,
            PlayerEvent::AudioBitrate(b) => self.player.audio.bitrate = b,
            PlayerEvent::Buffering(b) => self.player.buffering = b,
            PlayerEvent::Path(Some(path)) => {
                self.load_started = None;
                let Some(vid) = video_id_from_url(&path).or_else(|| self.url_vid.get(&path).cloned()) else {
                    return;
                };
                if self.current().map(|t| t.video_id.as_str()) != Some(vid.as_str()) {
                    // mpv advanced to the preloaded next track (gapless).
                    let idx = self
                        .queue
                        .next_index()
                        .filter(|&n| self.queue.get(n).is_some_and(|t| t.video_id == vid))
                        .or_else(|| self.queue.position_of(&vid));
                    if let Some(i) = idx {
                        self.queue.current = Some(i);
                        self.player.time = 0.0;
                        self.on_track_changed();
                    }
                }
                // Whatever was preloaded is now playing (or stale).
                self.next_sent = None;
                self.sync_next();
            }
            PlayerEvent::Path(None) => {}
            PlayerEvent::Idle(idle) => {
                let was_playing = !self.player.idle;
                self.player.idle = idle;
                if idle && was_playing && self.load_started.is_none() {
                    self.next_sent = None;
                    if self.sleep.is_end_of_track() {
                        self.sleep.cancel();
                        self.restore_crossfade_volume();
                        if self.queue.repeat == Repeat::One {
                            self.player(PlayerCmd::SetLoopFile(true));
                        }
                        self.player.time = 0.0;
                        self.player.paused = true;
                        self.flash("Sleep timer stopped after track", false);
                        self.dirty = true;
                        return;
                    }
                    // Fell off the end of mpv's playlist (end of queue, or the
                    // preloaded track failed). Continue from the queue unless
                    // every track we tried errored out.
                    if self.consecutive_errors >= self.queue.len().max(1) {
                        self.player.time = 0.0;
                    } else {
                        match self.queue.next_index() {
                            Some(n) if Some(n) != self.queue.current || self.queue.repeat != Repeat::Off => {
                                self.play_index(n)
                            }
                            _ => self.player.time = 0.0,
                        }
                    }
                }
            }
            PlayerEvent::EndFile { reason, error } => {
                if reason == "error" {
                    // A pre-resolved URL can go stale; retry once via yt-dlp.
                    if let (Some(direct), Some(cur)) = (self.direct_loaded.clone(), self.queue.current)
                        && self.current().is_some_and(|t| t.video_id == direct)
                        && self.direct_failed.insert(direct.clone())
                    {
                        tracing::info!("direct stream for {direct} failed; retrying via yt-dlp hook");
                        self.play_index(cur);
                        return;
                    }
                    self.load_started = None;
                    self.consecutive_errors += 1;
                    let what = self.current().map(|t| t.title.clone()).unwrap_or_default();
                    self.flash(
                        format!("Can't play {what}: {}", error.unwrap_or_else(|| "unknown error".into())),
                        true,
                    );
                    // mpv goes idle next; skip to a different track unless every
                    // track in the queue has failed.
                    if self.player.idle
                        && self.consecutive_errors < self.queue.len()
                        && let Some(n) = self.queue.skip_index().filter(|&n| Some(n) != self.queue.current)
                    {
                        self.play_index(n);
                    }
                }
            }
            PlayerEvent::Error(e) => {
                let msg = format!("mpv: {e}");
                self.player.error = Some(msg.clone());
                self.load_started = None;
                self.flash(msg, true);
            }
            PlayerEvent::Exited => {
                self.player.ready = false;
                self.load_started = None;
                // Keep a more specific startup error (e.g. "mpv not installed")
                // instead of overwriting it milliseconds later.
                if self.player.error.is_none() {
                    let msg = "mpv exited — playback unavailable".to_string();
                    self.player.error = Some(msg.clone());
                    self.flash(msg, true);
                }
            }
        }
        self.dirty = true;
    }

    pub fn on_ytm(&mut self, ev: YtmEvent) {
        self.dirty = true;
        match ev {
            YtmEvent::Suggestions { query, items } => {
                // Ignore stale answers for text that has since changed.
                if query == self.search.input.trim() {
                    self.search.suggest_for = query;
                    self.search.suggestions = items;
                    self.search.suggest_sel = None;
                }
            }
            YtmEvent::Auth { logged_in, message } => {
                let was = self.logged_in;
                self.logged_in = logged_in;
                self.library.auth.busy = None;
                self.library.auth.waiting = None;
                if !logged_in || !was {
                    // Drop stale likes on sign-out or when switching accounts.
                    self.likes.clear();
                    self.likes_loaded = false;
                }
                if logged_in {
                    self.library.auth.input = None;
                    if !was && self.auth_seen {
                        self.flash("Signed in to YouTube Music", false);
                    }
                    // Learn what's already liked so ♥ shows and `f` toggles correctly.
                    let token = self.token();
                    self.pending.insert(token, Pending::LikedSet);
                    let _ = self.tx.ytm.send(YtmReq::Load {
                        token,
                        view: View::LikedSongs,
                    });
                    // Load the Library immediately if the user is looking at it
                    // (or clear stale signed-out errors).
                    if self.tab == Tab::Library {
                        self.switch_library(self.library.section);
                    } else {
                        self.library.stack.clear();
                    }
                } else {
                    self.library.stack.clear();
                    if was {
                        self.account = None;
                        self.save_state();
                    }
                }
                self.auth_seen = true;
                if let Some(m) = message {
                    self.flash(m, true);
                }
            }
            YtmEvent::AuthNeedsBrowser {
                spec,
                label,
                browser_opened,
            } => {
                self.library.auth.busy = None;
                self.library.auth.waiting = Some(AuthWaiting {
                    spec,
                    label,
                    browser_opened,
                    started: Instant::now(),
                    next_poll: Instant::now() + AUTH_POLL_INTERVAL,
                    checking: false,
                });
            }
            YtmEvent::AuthPollPending => {
                if let Some(w) = self.library.auth.waiting.as_mut() {
                    w.checking = false;
                    w.next_poll = Instant::now() + AUTH_POLL_INTERVAL;
                }
            }
            YtmEvent::Loaded { token, title, items } => {
                if self.logged_in {
                    // Signed-in lists carry the user's like status; remember it.
                    for t in items.iter().filter_map(Item::as_track) {
                        if let Some(l) = t.liked {
                            self.likes.insert(t.video_id.clone(), l);
                        }
                    }
                }
                self.on_loaded(token, title, items);
            }
            YtmEvent::Failed { token, message } => {
                if let Some(token) = token {
                    let pending = self.pending.remove(&token);
                    if let Some(Pending::PlaylistPicker) = pending {
                        if matches!(self.overlay, Some(Overlay::AddToPlaylist { .. })) {
                            self.overlay = None;
                        }
                        self.flash(format!("Couldn't load your playlists: {message}"), true);
                        return;
                    }
                    if let Some(Pending::LikedSet) = pending {
                        // Don't block `f` forever; fall back to known statuses.
                        tracing::warn!("loading liked songs failed: {message}");
                        self.likes_loaded = true;
                        return;
                    }
                    if let Some(lv) = self.find_view(token) {
                        lv.loading = false;
                        if lv.title.is_empty() {
                            lv.title = "Couldn't load".into();
                        }
                        lv.error = Some(message);
                        return;
                    }
                }
                self.flash(message, true);
            }
            YtmEvent::Radio { seed, tracks } => {
                let mode = self.radio.remove(&seed);
                match mode {
                    Some(RadioMode::Autoplay) => {
                        let fresh: Vec<Track> = tracks
                            .into_iter()
                            .filter(|t| !self.queue.contains(&t.video_id))
                            .collect();
                        if fresh.is_empty() {
                            return;
                        }
                        let disliked = self.skip_disliked.take();
                        let skip =
                            disliked.is_some() && disliked.as_deref() == self.current().map(|t| t.video_id.as_str());
                        let at_end = skip || (self.player.idle && self.load_started.is_none());
                        let resume = self.queue.current.map(|c| c + 1);
                        self.queue.append(fresh);
                        if at_end {
                            if let Some(i) = resume {
                                self.play_index(i);
                            }
                        } else {
                            self.sync_next();
                        }
                    }
                    Some(RadioMode::Refill) => {
                        // Only if that song is still the one playing.
                        if self.current().map(|t| t.video_id.as_str()) != Some(seed.as_str()) {
                            return;
                        }
                        self.queue.truncate_after_current();
                        let fresh: Vec<Track> = tracks
                            .into_iter()
                            .filter(|t| !self.queue.contains(&t.video_id))
                            .collect();
                        let n = fresh.len();
                        self.queue.append(fresh);
                        self.autoplay_requested = Some(seed);
                        self.queue_state.select(self.queue.current);
                        self.sync_next();
                        self.flash(format!("Radio: {n} similar songs up next"), false);
                    }
                    Some(RadioMode::Start) => {
                        let seed_track = self
                            .queue
                            .entries
                            .iter()
                            .map(|e| &e.track)
                            .chain(self.all_list_tracks())
                            .find(|t| t.video_id == seed)
                            .cloned();
                        let mut list: Vec<Track> = seed_track.into_iter().collect();
                        list.extend(tracks);
                        if !list.is_empty() {
                            self.autoplay_requested = None;
                            self.queue.replace(list, 0);
                            self.play_index(0);
                            self.flash("Radio started", false);
                        }
                    }
                    None => {}
                }
            }
            YtmEvent::PlaylistEdited { message, removed } => {
                if let Some((playlist_id, video_id)) = removed {
                    let bare = playlist_id.strip_prefix("VL").unwrap_or(&playlist_id);
                    for lv in self.library.stack.iter_mut().chain(self.search.stack.iter_mut()) {
                        if let View::Playlist(id) = &lv.view
                            && id.strip_prefix("VL").unwrap_or(id) == bare
                            && let Some(pos) = lv
                                .items
                                .iter()
                                .position(|i| matches!(i, Item::Track(t) if t.video_id == video_id))
                        {
                            let sel = lv.state.selected().unwrap_or(0);
                            lv.items.remove(pos);
                            lv.state
                                .select((!lv.items.is_empty()).then(|| sel.min(lv.items.len() - 1)));
                        }
                    }
                }
                self.flash(message, false);
            }
            YtmEvent::PlaylistDeleted { playlist_id, title } => {
                let bare = playlist_id.strip_prefix("VL").unwrap_or(&playlist_id);
                let same = |id: &str| id.strip_prefix("VL").unwrap_or(id) == bare;
                // Close the deleted playlist if it's open, and drop it from lists.
                while self.library.stack.len() > 1
                    && self
                        .library
                        .stack
                        .last()
                        .is_some_and(|l| matches!(&l.view, View::Playlist(id) if same(id)))
                {
                    self.library.stack.pop();
                }
                for lv in self.library.stack.iter_mut().chain(self.search.stack.iter_mut()) {
                    let before = lv.items.len();
                    lv.items.retain(|i| !matches!(i, Item::Playlist(p) if same(&p.id)));
                    if lv.items.len() != before {
                        let sel = lv.state.selected().unwrap_or(0);
                        lv.state
                            .select((!lv.items.is_empty()).then(|| sel.min(lv.items.len() - 1)));
                    }
                }
                self.flash(format!("Deleted the playlist {title}"), false);
            }
            YtmEvent::Accounts { items } => {
                if let Some(Overlay::Accounts { items: slot, state }) = self.overlay.as_mut() {
                    let active_idx = items.iter().position(|a| a.active).unwrap_or(0);
                    *slot = Some(items);
                    state.select(slot.as_ref().filter(|v| !v.is_empty()).map(|_| active_idx));
                }
            }
            YtmEvent::AccountSwitched {
                auth_user,
                page_id,
                name,
            } => {
                self.account = Some(AccountChoice {
                    auth_user,
                    page_id,
                    name: Some(name.clone()),
                });
                self.save_state();
                self.likes.clear();
                self.likes_loaded = false;
                let token = self.token();
                self.pending.insert(token, Pending::LikedSet);
                let _ = self.tx.ytm.send(YtmReq::Load {
                    token,
                    view: View::LikedSongs,
                });
                if self.tab == Tab::Library {
                    self.switch_library(self.library.section);
                } else {
                    self.library.stack.clear();
                }
                self.flash(format!("Switched account: {name}"), false);
            }
            YtmEvent::Rated { video_id, liked } => {
                self.likes.insert(video_id.clone(), liked);
                for e in &mut self.queue.entries {
                    if e.track.video_id == video_id {
                        e.track.liked = Some(liked);
                    }
                }
                self.flash(if liked { "♥ Liked" } else { "Removed like" }, false);
            }
            YtmEvent::Disliked { video_id } => {
                self.likes.insert(video_id.clone(), false);
                for e in &mut self.queue.entries {
                    if e.track.video_id == video_id {
                        e.track.liked = Some(false);
                    }
                }
                self.dirty = true;
            }
            YtmEvent::Lyrics { video_id, text } => {
                if let LyricsState::Loading { video_id: v, .. } = &self.lyrics
                    && *v == video_id
                {
                    self.lyrics = match text {
                        Some(t) => LyricsState::Found {
                            lyrics: Lyrics::Plain(t),
                            source: "YouTube Music",
                        },
                        None => LyricsState::Missing,
                    };
                }
            }
            YtmEvent::LinkResolved { action } => match action {
                LinkAction::PlayTrack { track, start_secs } => {
                    let title = track.title.clone();
                    self.queue.insert_next(vec![track]);
                    let idx = self.queue.current.map_or(0, |c| c + 1);
                    self.load_index(idx, start_secs, false);
                    self.flash(format!("Playing “{title}”"), false);
                }
                LinkAction::OpenView(view) => {
                    self.set_tab(Tab::Search);
                    let lv = self.load_view(view);
                    self.search.stack.push(lv);
                }
                LinkAction::SearchAndPlay { query, filter } => {
                    if filter == SearchFilter::Songs {
                        self.play_query(query);
                    } else {
                        self.set_tab(Tab::Search);
                        self.search.input = query.clone();
                        self.search.cursor = query.chars().count();
                        self.search.filter = filter;
                        let lv = self.load_view(View::Search { filter, query });
                        self.search.stack = vec![lv];
                    }
                }
            },
        }
    }

    fn on_loaded(&mut self, token: u64, title: String, items: Vec<Item>) {
        match self.pending.remove(&token) {
            Some(Pending::PlaylistPicker) => {
                if let Some(Overlay::AddToPlaylist { playlists, state, .. }) = self.overlay.as_mut() {
                    let own: Vec<PlaylistRef> = items
                        .into_iter()
                        .filter_map(|i| match i {
                            // Liked Music and Episodes aren't real playlists.
                            Item::Playlist(p) if !matches!(p.id.trim_start_matches("VL"), "LM" | "SE") => Some(p),
                            _ => None,
                        })
                        .collect();
                    *playlists = Some(own);
                    state.select(Some(0));
                }
            }
            Some(Pending::View) => {
                if let Some(lv) = self.find_view(token) {
                    lv.loading = false;
                    if lv.title.is_empty() || !matches!(lv.view, View::Search { .. }) {
                        lv.title = title;
                    }
                    lv.items = items;
                    let first = lv.items.iter().position(|i| !matches!(i, Item::Header(_)));
                    lv.state.select(first);
                    if lv.items.is_empty() {
                        lv.error = Some("Nothing found".into());
                    }
                    // The top search hit is the likeliest next play.
                    if matches!(lv.view, View::Search { .. })
                        && let Some(id) = lv.items.iter().find_map(Item::as_track).map(|t| t.video_id.clone())
                    {
                        self.prefetch(&id);
                    }
                }
            }
            Some(Pending::Enqueue(mode)) => {
                let tracks: Vec<Track> = items.iter().filter_map(Item::as_track).cloned().collect();
                if tracks.is_empty() {
                    self.flash("No playable tracks", true);
                } else {
                    self.enqueue(tracks, mode);
                }
            }
            Some(Pending::First(mode)) => match items.iter().find_map(Item::as_track) {
                Some(t) => self.enqueue(vec![t.clone()], mode),
                None => self.flash("No results", true),
            },
            Some(Pending::PlayFirst) => match items.iter().find_map(Item::as_track) {
                Some(t) => self.enqueue(vec![t.clone()], EnqueueMode::PlayNow),
                None => self.flash("No results", true),
            },
            None => {}
            Some(Pending::LikedSet) => {
                for t in items.iter().filter_map(Item::as_track) {
                    self.likes.insert(t.video_id.clone(), true);
                }
                self.likes_loaded = true;
                self.dirty = true;
            }
        }
    }

    pub fn on_lyrics(&mut self, ev: LyricsEvent) {
        let LyricsState::Loading { video_id, .. } = &self.lyrics else {
            return;
        };
        if *video_id != ev.video_id {
            return;
        }
        self.dirty = true;
        match ev.lyrics {
            Some(lyrics) => {
                self.lyrics = LyricsState::Found {
                    lyrics,
                    source: "LRCLIB",
                }
            }
            None => {
                let _ = self.tx.ytm.send(YtmReq::Lyrics { video_id: ev.video_id });
            }
        }
    }

    pub fn on_art(&mut self, ev: ArtEvent) {
        if self.art_key.as_deref() != Some(&ev.key) {
            return;
        }
        self.art_loading = false;
        self.dirty = true;
        if let (Some(img), Some(picker), Some(art)) = (ev.image, self.picker.as_ref(), self.art.as_mut()) {
            art.replace_protocol(picker.new_resize_protocol(img));
        }
    }

    fn find_view(&mut self, token: u64) -> Option<&mut ListView> {
        self.search
            .stack
            .iter_mut()
            .chain(self.library.stack.iter_mut())
            .find(|lv| lv.token == token)
    }

    fn all_list_tracks(&self) -> impl Iterator<Item = &Track> {
        self.search
            .stack
            .iter()
            .chain(self.library.stack.iter())
            .flat_map(|lv| lv.items.iter().filter_map(Item::as_track))
    }

    fn load_view(&mut self, view: View) -> ListView {
        let token = self.token();
        self.pending.insert(token, Pending::View);
        let _ = self.tx.ytm.send(YtmReq::Load {
            token,
            view: view.clone(),
        });
        ListView::new(view, token)
    }

    /// Search for songs and play the first hit (or open an external URL directly).
    pub fn play_query(&mut self, query: String) {
        if let Some(link) = crate::interlink::parse_external_link(&query) {
            self.open_external_link(link);
            return;
        }
        let token = self.token();
        self.pending.insert(token, Pending::PlayFirst);
        self.search.input = query.clone();
        self.search.cursor = query.chars().count();
        let _ = self.tx.ytm.send(YtmReq::Load {
            token,
            view: View::Search {
                filter: SearchFilter::Songs,
                query,
            },
        });
        self.flash("Searching…", false);
    }

    /// Search songs for `query` and enqueue the top hit (remote control).
    fn enqueue_first(&mut self, query: String, mode: EnqueueMode) {
        let token = self.token();
        self.pending.insert(token, Pending::First(mode));
        let _ = self.tx.ytm.send(YtmReq::Load {
            token,
            view: View::Search {
                filter: SearchFilter::Songs,
                query,
            },
        });
    }

    /// Handle a command from `pova <command>`; the answer goes back to the
    /// client as "ok …" or "error …".
    pub fn on_control(&mut self, cmd: crate::control::Command) -> String {
        use crate::control::{Command as C, parse_amount};
        self.dirty = true;
        let has_track = self.current().is_some();
        match cmd {
            C::Toggle | C::Play | C::Pause if !has_track => {
                "error the queue is empty; add something with `pova add <song>`".into()
            }
            C::Toggle => {
                self.dispatch(Action::PlayPause);
                "ok".into()
            }
            C::Play => {
                if self.player.paused || (self.player.idle && self.load_started.is_none()) {
                    self.dispatch(Action::PlayPause);
                }
                "ok".into()
            }
            C::Pause => {
                if !self.player.paused && !(self.player.idle && self.load_started.is_none()) {
                    self.dispatch(Action::PlayPause);
                }
                "ok".into()
            }
            C::Next => {
                self.dispatch(Action::Next);
                "ok".into()
            }
            C::Prev => {
                self.dispatch(Action::Prev);
                "ok".into()
            }
            C::Like => {
                if !self.logged_in {
                    return "error liking songs needs you to be signed in (run `pova login` or open the Library tab)"
                        .into();
                }
                if !has_track {
                    return "error nothing is playing".into();
                }
                self.dispatch(Action::Like);
                "ok".into()
            }
            C::Dislike => {
                if !self.logged_in {
                    return "error disliking songs needs you to be signed in (run `pova login` or open the Library tab)"
                        .into();
                }
                if !has_track {
                    return "error nothing is playing".into();
                }
                self.dispatch(Action::Dislike);
                "ok".into()
            }
            C::Volume(arg) => {
                let Some((v, relative)) = parse_amount(&arg) else {
                    return format!("error `{arg}` isn't a volume; use 40, +5 or -5");
                };
                self.crossfade_active = false;
                let target = if relative { self.player.volume + v } else { v };
                self.player.volume = target.clamp(0.0, 100.0).round();
                self.player(PlayerCmd::SetVolume(self.player.volume));
                if self.player.muted {
                    self.player(PlayerCmd::SetMute(false));
                }
                format!("ok volume {}", self.player.volume)
            }
            C::Seek(arg) => {
                if !has_track {
                    return "error nothing is playing".into();
                }
                let Some((v, relative)) = parse_amount(&arg) else {
                    return format!("error `{arg}` isn't a time; use 90, +10 or -10 (seconds)");
                };
                self.player(if relative {
                    PlayerCmd::SeekRelative(v)
                } else {
                    PlayerCmd::SeekAbsolute(v.max(0.0))
                });
                "ok".into()
            }
            C::Add(q) => {
                self.enqueue_first(q.clone(), EnqueueMode::Append);
                format!("ok adding the top result for “{q}”")
            }
            C::PlayNext(q) => {
                self.enqueue_first(q.clone(), EnqueueMode::Next);
                format!("ok playing the top result for “{q}” next")
            }
            C::PlayNow(q) => {
                self.enqueue_first(q.clone(), EnqueueMode::PlayNow);
                format!("ok playing the top result for “{q}”")
            }
            C::Open(url) => match crate::interlink::parse_external_link(&url) {
                Some(link) => {
                    self.open_external_link(link);
                    format!("ok opening {url}")
                }
                None => format!(
                    "error `{url}` isn't a recognized YouTube, YouTube Music, Spotify, Tidal, or song.link URL"
                ),
            },
            C::Sleep(arg) => match crate::sleep::parse_cli(&arg) {
                Ok(crate::sleep::SleepCommand::Status) => {
                    format!("ok {}", self.sleep.menu_value(Instant::now()))
                }
                Ok(crate::sleep::SleepCommand::Off) => {
                    self.cancel_sleep_timer();
                    "ok sleep timer off".into()
                }
                Ok(crate::sleep::SleepCommand::EndOfTrack) => {
                    self.set_sleep_end_of_track();
                    "ok sleep timer set to end of track".into()
                }
                Ok(crate::sleep::SleepCommand::SetMinutes(m)) => {
                    self.set_sleep_minutes(m);
                    format!("ok sleep timer set to {m} min")
                }
                Ok(crate::sleep::SleepCommand::AdjustMinutes(delta)) => {
                    let msg = self.adjust_sleep_minutes(delta);
                    format!("ok {msg}")
                }
                Err(e) => format!("error {e}"),
            },
            C::Speed(arg) => {
                let s = arg.trim();
                if s.eq_ignore_ascii_case("status") {
                    format!("ok {}", crate::audio::format_speed(self.cfg.playback_speed))
                } else if s == "+" || s.eq_ignore_ascii_case("up") {
                    let next = crate::audio::step_speed(self.cfg.playback_speed, true);
                    self.set_playback_speed(next);
                    format!("ok speed {}", crate::audio::format_speed(next))
                } else if s == "-" || s.eq_ignore_ascii_case("down") {
                    let next = crate::audio::step_speed(self.cfg.playback_speed, false);
                    self.set_playback_speed(next);
                    format!("ok speed {}", crate::audio::format_speed(next))
                } else if let Ok(v) = s.trim_end_matches('x').parse::<f64>()
                    && v.is_finite()
                    && (0.25..=3.0).contains(&v)
                {
                    self.set_playback_speed(v);
                    format!("ok speed {}", crate::audio::format_speed(v))
                } else {
                    format!("error `{arg}` isn't a valid speed; use 0.5..2.0, +, -, or status")
                }
            }
            C::Eq(arg) => {
                let s = arg.trim().to_ascii_lowercase();
                if s == "status" {
                    format!("ok {}", self.cfg.eq.summary())
                } else if s == "off" {
                    self.cfg.eq.enabled = false;
                    self.apply_eq();
                    "ok eq off".into()
                } else if s == "on" {
                    self.cfg.eq.enabled = true;
                    self.apply_eq();
                    format!("ok eq {}", self.cfg.eq.summary())
                } else if let Some(preset) = EqPreset::from_name(&s) {
                    self.cfg.eq.apply_preset(preset);
                    self.apply_eq();
                    format!("ok eq {}", self.cfg.eq.summary())
                } else {
                    format!(
                        "error unknown EQ preset `{arg}`; try off, flat, bass, vocal, rock, electronic, acoustic, pop, jazz, classical, treble, late_night"
                    )
                }
            }
            C::Quality(arg) => {
                let s = arg.trim().to_ascii_lowercase();
                let next = match s.as_str() {
                    "status" => return format!("ok {}", self.cfg.audio_quality.label()),
                    "max" | "hires" | "best" => AudioQuality::Max,
                    "high" => AudioQuality::High,
                    "normal" | "norm" | "med" | "medium" => AudioQuality::Normal,
                    "low" | "saver" => AudioQuality::Low,
                    _ => return format!("error unknown quality `{arg}`; use max, high, normal, or low"),
                };
                self.set_audio_quality(next);
                format!("ok quality {}", next.label())
            }
            C::AbLoop => {
                if !has_track {
                    return "error nothing is playing".into();
                }
                let msg = self.toggle_ab_loop();
                format!("ok {msg}")
            }
            C::Stats => format!("ok {}", self.stats.cli_summary(crate::stats::now_unix())),
            C::Offline => {
                if !has_track {
                    return "error nothing is playing".into();
                }
                let msg = self.toggle_offline_for_target(true);
                format!("ok {msg}")
            }
            C::Discord(arg) => {
                let s = arg.trim().to_ascii_lowercase();
                match s.as_str() {
                    "status" => format!("ok {}", if self.cfg.discord_rpc { "on" } else { "off" }),
                    "on" | "true" | "1" => {
                        self.set_discord_rpc(true);
                        "ok discord on".into()
                    }
                    "off" | "false" | "0" => {
                        self.set_discord_rpc(false);
                        "ok discord off".into()
                    }
                    "toggle" => {
                        self.set_discord_rpc(!self.cfg.discord_rpc);
                        format!("ok discord {}", if self.cfg.discord_rpc { "on" } else { "off" })
                    }
                    _ => format!("error unknown discord mode `{arg}`; use on, off, toggle, or status"),
                }
            }
            C::Status(format) => format!("ok {}", self.status_line(&format)),
        }
    }

    /// Fill a `pova status` format string. Empty when nothing is loaded.
    pub fn status_line(&self, format: &str) -> String {
        let Some(t) = self.current() else { return String::new() };
        let state = if self.player.paused || self.player.idle {
            "paused"
        } else {
            "playing"
        };
        let liked = if is_liked(&self.likes, t) { "♥" } else { "" };
        let duration = if self.player.duration > 0.0 {
            self.player.duration
        } else {
            t.duration.unwrap_or(0.0)
        };
        let sleep = self.sleep.badge(Instant::now()).unwrap_or_default();
        let quality = self.player.audio.badge(self.cfg.audio_quality);
        let speed = crate::audio::format_speed(self.cfg.playback_speed);
        let eq = self.cfg.eq.summary();
        expand_template(format, |key| match key {
            "title" => Some(t.title.clone()),
            "artist" => Some(t.artists.clone()),
            "album" => Some(t.album.clone().unwrap_or_default()),
            "state" => Some(state.to_string()),
            "position" => Some(crate::model::format_time(self.player.time)),
            "duration" => Some(crate::model::format_time(duration)),
            "volume" => Some(format!("{}", self.player.volume.round())),
            "liked" => Some(liked.to_string()),
            "sleep" => Some(sleep.clone()),
            "quality" => Some(quality.clone()),
            "speed" => Some(speed.clone()),
            "eq" => Some(eq.clone()),
            _ => None,
        })
    }

    fn load_enqueue(&mut self, view: View, mode: EnqueueMode) {
        let token = self.token();
        self.pending.insert(token, Pending::Enqueue(mode));
        let _ = self.tx.ytm.send(YtmReq::Load { token, view });
        self.flash("Loading…", false);
    }

    // ───────────────────────────── input ─────────────────────────────

    pub fn on_terminal(&mut self, ev: Event) {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => self.on_key(k),
            Event::Mouse(m) => self.on_mouse(m),
            Event::Resize(..) => self.dirty = true,
            Event::Paste(text) => {
                if let Some(Overlay::AddToPlaylist { naming: Some(name), .. }) = self.overlay.as_mut() {
                    let room = 150usize.saturating_sub(name.chars().count());
                    name.extend(text.chars().filter(|c| !c.is_control()).take(room));
                    self.dirty = true;
                } else if self.tab == Tab::Library
                    && !self.logged_in
                    && self.library.auth.busy.is_none()
                    && self.library.auth.waiting.is_none()
                {
                    match self.library.auth.input.as_mut() {
                        Some(AuthInput::CustomBrowser(val)) => {
                            let room = 200usize.saturating_sub(val.chars().count());
                            val.extend(text.chars().filter(|c| !c.is_control()).take(room));
                            self.dirty = true;
                        }
                        Some(AuthInput::PasteCookie(val)) => {
                            let room = 32_768usize.saturating_sub(val.chars().count());
                            val.extend(text.chars().filter(|c| !c.is_control()).take(room));
                            self.dirty = true;
                        }
                        None if text.contains("SAPISID=") || text.contains("__Secure-3PAPISID=") => {
                            let cleaned: String = text.chars().filter(|c| !c.is_control()).take(32_768).collect();
                            self.library.auth.input = Some(AuthInput::PasteCookie(cleaned));
                            self.dirty = true;
                        }
                        None => {}
                    }
                } else if self.search.editing {
                    let before = self.search.input.clone();
                    for c in text.chars().filter(|c| !c.is_control()) {
                        self.insert_char(c);
                    }
                    self.on_input_changed(&before);
                    self.dirty = true;
                }
            }
            _ => {}
        }
    }

    fn on_key(&mut self, k: KeyEvent) {
        tracing::debug!("key {:?} {:?} -> {:?}", k.code, k.modifiers, self.keymap.action(&k));
        self.dirty = true;
        if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }
        // Esc closes an error box first (it's drawn on top of everything).
        if k.code == KeyCode::Esc && self.flash.as_ref().is_some_and(|f| f.error) {
            self.flash = None;
            return;
        }
        if self.overlay.is_some() {
            self.on_overlay_key(k);
            return;
        }
        if self.tab == Tab::Library && !self.logged_in && self.library.auth.waiting.is_some() {
            match k.code {
                KeyCode::Esc => {
                    self.library.auth.waiting = None;
                    return;
                }
                KeyCode::Enter => {
                    self.poll_auth_now();
                    return;
                }
                KeyCode::Char('o') | KeyCode::Char('O') if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.open_sign_in_link();
                    return;
                }
                _ => {}
            }
        }
        if self.tab == Tab::Library
            && !self.logged_in
            && self.library.auth.busy.is_none()
            && self.library.auth.input.is_some()
        {
            self.on_auth_input_key(k);
            return;
        }
        if self.tab == Tab::Library
            && !self.logged_in
            && self.library.auth.busy.is_none()
            && self.library.auth.waiting.is_none()
            && !k.modifiers.contains(KeyModifiers::CONTROL)
        {
            match k.code {
                KeyCode::Char('o') | KeyCode::Char('O') => {
                    self.open_sign_in_link();
                    return;
                }
                KeyCode::Char('p') => {
                    self.library.auth.input = Some(AuthInput::CustomBrowser(String::new()));
                    return;
                }
                KeyCode::Char('c') => {
                    self.library.auth.input = Some(AuthInput::PasteCookie(String::new()));
                    return;
                }
                _ => {}
            }
        }
        if self.tab == Tab::Library
            && self.logged_in
            && k.code == KeyCode::Char('u')
            && !k.modifiers.contains(KeyModifiers::CONTROL)
        {
            self.open_accounts();
            return;
        }
        if self.tab == Tab::Search && self.search.editing && self.on_edit_key(k) {
            return;
        }
        if self.tab == Tab::Search && !self.search.editing && self.search.stack.is_empty() && k.code == KeyCode::Enter {
            self.search.editing = true;
            self.search.cursor = self.search.input.chars().count();
            return;
        }
        if k.code == KeyCode::Backspace
            && ((self.tab == Tab::Search && self.search.stack.len() > 1)
                || (self.tab == Tab::Library && self.library.stack.len() > 1))
        {
            self.back_or_menu();
            return;
        }
        if let Some(action) = self.keymap.action(&k) {
            self.dispatch(action);
        }
    }

    fn on_auth_input_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let Some(input) = self.library.auth.input.as_mut() else {
            return;
        };
        match k.code {
            KeyCode::Esc => {
                self.library.auth.input = None;
            }
            KeyCode::Backspace => match input {
                AuthInput::CustomBrowser(s) | AuthInput::PasteCookie(s) => {
                    s.pop();
                }
            },
            KeyCode::Char('u') if ctrl => match input {
                AuthInput::CustomBrowser(s) | AuthInput::PasteCookie(s) => s.clear(),
            },
            KeyCode::Enter => match self.library.auth.input.take() {
                Some(AuthInput::CustomBrowser(spec)) => {
                    let spec = spec.trim().to_string();
                    if spec.is_empty() {
                        self.library.auth.input = Some(AuthInput::CustomBrowser(String::new()));
                        return;
                    }
                    self.library.auth.busy = Some(spec.clone());
                    let _ = self.tx.ytm.send(YtmReq::ImportBrowser {
                        label: spec.clone(),
                        spec,
                    });
                }
                Some(AuthInput::PasteCookie(raw)) => {
                    if raw.trim().is_empty() {
                        self.library.auth.input = Some(AuthInput::PasteCookie(String::new()));
                        return;
                    }
                    self.library.auth.busy = Some("cookie".into());
                    let _ = self.tx.ytm.send(YtmReq::SaveCookie { raw });
                }
                None => {}
            },
            KeyCode::Char(c) if !ctrl => match input {
                AuthInput::CustomBrowser(s) if s.chars().count() < 200 => s.push(c),
                AuthInput::PasteCookie(s) if s.chars().count() < 32_768 => s.push(c),
                _ => {}
            },
            _ => {}
        }
    }

    fn select_auth_profile(&mut self, idx: usize) {
        if self.logged_in || self.library.auth.busy.is_some() || self.library.auth.waiting.is_some() {
            return;
        }
        self.library.auth.ensure_profiles();
        let Some(p) = self.library.auth.profiles.get(idx).cloned() else {
            return;
        };
        self.library.auth.selected = idx;
        self.library.auth.input = None;
        self.library.auth.busy = Some(format!("signing in with {}", p.label));
        let _ = self.tx.ytm.send(YtmReq::ImportBrowser {
            spec: p.spec,
            label: p.label,
        });
    }

    fn poll_auth_now(&mut self) {
        if let Some(w) = self.library.auth.waiting.as_mut()
            && !w.checking
        {
            w.checking = true;
            w.started = Instant::now();
            let _ = self.tx.ytm.send(YtmReq::PollSignIn {
                spec: w.spec.clone(),
                label: w.label.clone(),
            });
        }
    }

    fn open_sign_in_link(&mut self) {
        let spec = if let Some(w) = self.library.auth.waiting.as_ref() {
            Some(w.spec.clone())
        } else {
            self.library.auth.ensure_profiles();
            self.library
                .auth
                .profiles
                .get(self.library.auth.selected)
                .map(|p| p.spec.clone())
        };
        let _ = self.tx.ytm.send(YtmReq::OpenSignInLink { spec });
        self.flash("Opened https://music.youtube.com", false);
    }

    pub fn open_accounts(&mut self) {
        if !self.logged_in {
            self.flash("Sign in first from the Library tab (b) to switch accounts.", true);
            return;
        }
        self.overlay = Some(Overlay::Accounts {
            items: None,
            state: TableState::default(),
        });
        let _ = self.tx.ytm.send(YtmReq::ListAccounts);
    }

    fn open_account_action(&mut self) {
        if self.logged_in {
            self.overlay = Some(Overlay::Confirm {
                question: "Sign out of YouTube Music? You can sign back in anytime from the Library tab.".into(),
                on_yes: Confirmed::SignOut,
                yes: false,
            });
        } else {
            self.set_tab(Tab::Library);
        }
    }

    fn insert_char(&mut self, c: char) {
        let byte = self
            .search
            .input
            .char_indices()
            .nth(self.search.cursor)
            .map_or(self.search.input.len(), |(i, _)| i);
        self.search.input.insert(byte, c);
        self.search.cursor += 1;
    }

    /// Text input for the search box. Returns true if the key was consumed.
    fn on_edit_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let n_sugg = self.search.visible_suggestions().len();
        let before = self.search.input.clone();
        let s = &mut self.search;
        match k.code {
            KeyCode::Enter => {
                // Search the highlighted suggestion, like the YTM search bar.
                if let Some(text) = s
                    .suggest_sel
                    .and_then(|i| s.visible_suggestions().get(i))
                    .map(|x| x.text.clone())
                {
                    s.cursor = text.chars().count();
                    s.input = text;
                }
                s.editing = false;
                s.suggest_sel = None;
                self.submit_search();
            }
            KeyCode::Esc if s.suggest_sel.is_some() => s.suggest_sel = None,
            KeyCode::Esc => s.editing = false,
            KeyCode::Down if n_sugg > 0 => {
                s.suggest_sel = Some(s.suggest_sel.map_or(0, |i| (i + 1).min(n_sugg - 1)));
            }
            KeyCode::Up if n_sugg > 0 => s.suggest_sel = s.suggest_sel.and_then(|i| i.checked_sub(1)),
            KeyCode::Down if !s.stack.is_empty() => s.editing = false,
            KeyCode::Backspace if ctrl => {
                s.input.clear();
                s.cursor = 0;
            }
            KeyCode::Char('u') if ctrl => {
                if s.cursor > 0 {
                    let end = s.input.char_indices().nth(s.cursor).map_or(s.input.len(), |(i, _)| i);
                    s.input.drain(..end);
                    s.cursor = 0;
                }
            }
            KeyCode::Char('w') if ctrl => {
                let chars: Vec<char> = s.input.chars().collect();
                let mut new_cur = s.cursor.min(chars.len());
                while new_cur > 0 && chars[new_cur - 1].is_whitespace() {
                    new_cur -= 1;
                }
                while new_cur > 0 && !chars[new_cur - 1].is_whitespace() {
                    new_cur -= 1;
                }
                if new_cur < s.cursor {
                    let start = s.input.char_indices().nth(new_cur).map_or(s.input.len(), |(i, _)| i);
                    let end = s.input.char_indices().nth(s.cursor).map_or(s.input.len(), |(i, _)| i);
                    s.input.drain(start..end);
                    s.cursor = new_cur;
                }
            }
            KeyCode::Backspace => {
                if s.cursor > 0 {
                    s.cursor -= 1;
                    let byte = s.input.char_indices().nth(s.cursor).map(|(i, _)| i).unwrap();
                    s.input.remove(byte);
                }
            }
            KeyCode::Delete => {
                if let Some((byte, _)) = s.input.char_indices().nth(s.cursor) {
                    s.input.remove(byte);
                }
            }
            KeyCode::Left => s.cursor = s.cursor.saturating_sub(1),
            // → at the end of the line accepts the inline completion.
            KeyCode::Right | KeyCode::End if s.cursor == s.input.chars().count() && s.ghost().is_some() => {
                let full = format!("{}{}", s.input, s.ghost().unwrap_or_default());
                s.cursor = full.chars().count();
                s.input = full;
                s.suggest_sel = None;
            }
            KeyCode::Right => s.cursor = (s.cursor + 1).min(s.input.chars().count()),
            KeyCode::Home => s.cursor = 0,
            KeyCode::End => s.cursor = s.input.chars().count(),
            KeyCode::Tab => {
                if s.input.trim().is_empty() {
                    s.editing = false;
                    self.dispatch(Action::TabNext);
                    return true;
                }
                self.cycle_filter(true);
            }
            KeyCode::BackTab => {
                if s.input.trim().is_empty() {
                    s.editing = false;
                    self.dispatch(Action::TabPrev);
                    return true;
                }
                self.cycle_filter(false);
            }
            KeyCode::Char(c) if !ctrl => self.insert_char(c),
            _ => return false,
        }
        self.on_input_changed(&before);
        true
    }

    /// Debounce an autocomplete request after the text changed.
    fn on_input_changed(&mut self, before: &str) {
        if self.search.input == before {
            return;
        }
        self.search.suggest_sel = None;
        self.search.suggest_due = if self.search.input.trim().is_empty() {
            None
        } else {
            Some(Instant::now() + Duration::from_millis(180))
        };
    }

    fn cycle_filter(&mut self, forward: bool) {
        let all = SearchFilter::ALL;
        let i = all.iter().position(|f| *f == self.search.filter).unwrap_or(0);
        let n = all.len();
        self.search.filter = all[if forward { (i + 1) % n } else { (i + n - 1) % n }];
        if !self.search.editing && !self.search.input.trim().is_empty() {
            self.submit_search();
        }
    }

    fn submit_search(&mut self) {
        let query = self.search.input.trim().to_string();
        if query.is_empty() {
            return;
        }
        if let Some(link) = crate::interlink::parse_external_link(&query) {
            self.open_external_link(link);
            return;
        }
        let lv = self.load_view(View::Search {
            filter: self.search.filter,
            query,
        });
        self.search.stack = vec![lv];
    }

    fn switch_library(&mut self, section: LibSection) {
        self.library.section = section;
        if section == LibSection::Offline {
            self.refresh_offline_view();
            return;
        }
        let lv = self.load_view(section.view());
        self.library.stack = vec![lv];
    }

    fn set_tab(&mut self, tab: Tab) {
        if tab != Tab::Search {
            self.search.editing = false;
        }
        self.tab = tab;
        match tab {
            Tab::Search => {}
            Tab::Library => {
                if self.library.section == LibSection::Offline {
                    self.refresh_offline_view();
                } else if self.logged_in {
                    if self.library.stack.is_empty() {
                        self.switch_library(self.library.section);
                    }
                } else {
                    self.library.auth.ensure_profiles();
                }
            }
            Tab::Queue if self.queue_state.selected().is_none() => {
                self.queue_state.select(self.queue.current);
            }
            _ => {}
        }
    }

    /// The list the navigation keys currently act on.
    fn active_list(&mut self) -> Option<(&mut TableState, usize)> {
        match self.tab {
            Tab::Queue => {
                let n = self.queue.len();
                Some((&mut self.queue_state, n))
            }
            Tab::Search => self.search.stack.last_mut().map(|lv| {
                let n = lv.items.len();
                (&mut lv.state, n)
            }),
            Tab::Library => self.library.stack.last_mut().map(|lv| {
                let n = lv.items.len();
                (&mut lv.state, n)
            }),
            _ => None,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.tab == Tab::Library && !self.logged_in && self.library.section != LibSection::Offline {
            if self.library.auth.busy.is_none()
                && self.library.auth.waiting.is_none()
                && self.library.auth.input.is_none()
            {
                self.library.auth.ensure_profiles();
                let len = self.library.auth.profiles.len();
                if len > 0 {
                    let cur = self.library.auth.selected.min(len - 1) as isize;
                    self.library.auth.selected = (cur + delta).clamp(0, len as isize - 1) as usize;
                }
            }
            return;
        }
        if self.tab == Tab::Lyrics {
            if let LyricsState::Found {
                lyrics: Lyrics::Synced(lines),
                ..
            } = &self.lyrics
                && !lines.is_empty()
            {
                let active = crate::lyrics::active_line(lines, self.player.time + 0.25).unwrap_or(0);
                let cur = self.lyrics_sel.unwrap_or(active) as isize;
                let next = (cur + delta).clamp(0, lines.len() as isize - 1) as usize;
                self.lyrics_sel = Some(next);
                return;
            }
            let cur = self.lyrics_scroll.unwrap_or(self.lyrics_auto) as isize;
            self.lyrics_scroll = Some((cur + delta).max(0) as u16);
            return;
        }
        // Skip section headers in browse lists.
        let headers: Vec<bool> = match self.tab {
            Tab::Search => self
                .search
                .stack
                .last()
                .map(|l| l.items.iter().map(|i| matches!(i, Item::Header(_))).collect()),
            Tab::Library => self
                .library
                .stack
                .last()
                .map(|l| l.items.iter().map(|i| matches!(i, Item::Header(_))).collect()),
            _ => None,
        }
        .unwrap_or_default();
        let Some((state, len)) = self.active_list() else { return };
        if len == 0 {
            return;
        }
        let cur = state.selected().unwrap_or(0) as isize;
        let mut next = (cur + delta).clamp(0, len as isize - 1) as usize;
        let dir = if delta >= 0 { 1isize } else { -1 };
        while headers.get(next).copied().unwrap_or(false) {
            let n = next as isize + dir;
            if n < 0 || n >= len as isize {
                // Nowhere to go in that direction; land on the closest non-header.
                next = if dir > 0 {
                    (0..len)
                        .rev()
                        .find(|&i| !headers.get(i).copied().unwrap_or(false))
                        .unwrap_or(0)
                } else {
                    (0..len)
                        .find(|&i| !headers.get(i).copied().unwrap_or(false))
                        .unwrap_or(0)
                };
                break;
            }
            next = n as usize;
        }
        state.select(Some(next));
    }

    pub fn dispatch(&mut self, action: Action) {
        use Action::*;
        self.dirty = true;
        match action {
            Quit => self.quit = true,
            PlayPause => {
                if self.player.idle && self.load_started.is_none() {
                    match self
                        .queue
                        .current
                        .or(if self.queue.is_empty() { None } else { Some(0) })
                    {
                        Some(i) => self.play_index(i),
                        None => self.flash("Queue is empty — press / to search", false),
                    }
                } else {
                    self.player(PlayerCmd::TogglePause);
                }
            }
            SeekForward => self.player(PlayerCmd::SeekRelative(5.0)),
            SeekBackward => self.player(PlayerCmd::SeekRelative(-5.0)),
            SeekForwardBig => self.player(PlayerCmd::SeekRelative(30.0)),
            SeekBackwardBig => self.player(PlayerCmd::SeekRelative(-30.0)),
            Next => self.next_track(),
            Prev => self.prev_track(),
            VolumeUp | VolumeDown => {
                self.crossfade_active = false;
                let d = if action == VolumeUp { 5.0 } else { -5.0 };
                self.player.volume = ((self.player.volume / 5.0).round() * 5.0 + d).clamp(0.0, 100.0);
                self.player(PlayerCmd::SetVolume(self.player.volume));
                if self.player.muted {
                    self.player(PlayerCmd::SetMute(false));
                }
            }
            Mute => self.player(PlayerCmd::SetMute(!self.player.muted)),
            Shuffle => {
                let on = !self.queue.shuffle;
                self.queue.set_shuffle(on);
                self.sync_next();
                self.flash(if on { "Shuffle on" } else { "Shuffle off" }, false);
            }
            Repeat => self.set_repeat(self.queue.repeat.next()),
            Like => {
                let Some(t) = self.current() else { return };
                if !self.logged_in {
                    self.flash(
                        "Liking songs needs you to be signed in. Open the Library tab (b) to sign in, \
                         or run `pova login`.",
                        true,
                    );
                    return;
                }
                let known = self.likes_loaded || self.likes.contains_key(&t.video_id) || t.liked.is_some();
                if !known {
                    self.flash("Still loading your liked songs — try again in a moment", false);
                    return;
                }
                let liked = !is_liked(&self.likes, t);
                let _ = self.tx.ytm.send(YtmReq::Rate {
                    video_id: t.video_id.clone(),
                    liked,
                });
            }
            Dislike => self.dislike(),
            VizMode => {
                self.viz_mode = self.viz_mode.next();
                self.flash(format!("Visualizer: {}", self.viz_mode.label()), false);
            }
            Themes => self.open_themes(),
            Help => self.overlay = Some(Overlay::Help { scroll: 0 }),
            Menu => self.back_or_menu(),
            TabNext | TabPrev => {
                let i = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
                let n = Tab::ALL.len();
                self.set_tab(
                    Tab::ALL[if action == TabNext {
                        (i + 1) % n
                    } else {
                        (i + n - 1) % n
                    }],
                );
            }
            TabNowPlaying => self.set_tab(Tab::NowPlaying),
            TabQueue => self.set_tab(Tab::Queue),
            TabLyrics => self.set_tab(Tab::Lyrics),
            TabSearch => {
                self.set_tab(Tab::Search);
                self.search.editing = true;
                self.search.cursor = self.search.input.chars().count();
            }
            TabLibrary => self.set_tab(Tab::Library),
            ToggleViz => self.show_viz = !self.show_viz,
            TogglePlayer => self.show_player = !self.show_player,
            ToggleControls => self.show_controls = !self.show_controls,
            Up => self.move_selection(-1),
            Down => self.move_selection(1),
            PageUp => self.move_selection(-10),
            PageDown => self.move_selection(10),
            Top => self.move_selection(-100_000),
            Bottom => self.move_selection(100_000),
            Select => self.select(EnqueueMode::PlayNow),
            Append => self.select(EnqueueMode::Append),
            PlayNext => self.select(EnqueueMode::Next),
            Remove => {
                if self.tab == Tab::Library {
                    self.remove_from_playlist();
                } else {
                    self.remove_selected();
                }
            }
            AddToPlaylist => self.open_playlist_picker(),
            DeletePlaylist => self.ask_delete_playlist(),
            MoveUp | MoveDown => {
                if self.tab == Tab::Queue
                    && let Some(i) = self.queue_state.selected()
                    && let Some(j) = self.queue.move_item(i, action == MoveUp)
                {
                    self.queue_state.select(Some(j));
                    self.sync_next();
                }
            }
            Radio => {
                let seed = match self.tab {
                    Tab::Queue => self
                        .queue_state
                        .selected()
                        .and_then(|i| self.queue.get(i))
                        .or_else(|| self.current())
                        .cloned(),
                    Tab::Search => self
                        .search
                        .stack
                        .last()
                        .and_then(|l| l.selected())
                        .and_then(Item::as_track)
                        .cloned(),
                    Tab::Library => self
                        .library
                        .stack
                        .last()
                        .and_then(|l| l.selected())
                        .and_then(Item::as_track)
                        .cloned(),
                    _ => self.current().cloned(),
                };
                if let Some(t) = seed {
                    let playing = self.current().is_some_and(|c| c.video_id == t.video_id);
                    let mode = if playing { RadioMode::Refill } else { RadioMode::Start };
                    self.radio.insert(t.video_id.clone(), mode);
                    let _ = self.tx.ytm.send(YtmReq::Radio { seed: t.video_id });
                    self.flash(format!("Starting radio from {}…", t.title), false);
                }
            }
            SubPrev | SubNext => {
                let fwd = action == SubNext;
                match self.tab {
                    Tab::Search => self.cycle_filter(fwd),
                    Tab::Library => {
                        let all = LibSection::ALL;
                        let i = all.iter().position(|s| *s == self.library.section).unwrap_or(0);
                        let n = all.len();
                        let next = all[if fwd { (i + 1) % n } else { (i + n - 1) % n }];
                        self.library.section = next;
                        if self.logged_in || next == LibSection::Offline {
                            self.switch_library(next);
                        }
                    }
                    _ => {}
                }
            }
            ClearQueue => {
                self.queue.clear_upcoming();
                self.queue_state.select(self.queue.current);
                self.sync_next();
                // The cleared songs may have come from autoplay: forget that
                // radio (and drop one still on its way) so a fresh one is
                // fetched near the end of this song.
                if let Some(seed) = self.autoplay_requested.take()
                    && self.radio.get(&seed) == Some(&RadioMode::Autoplay)
                {
                    self.radio.remove(&seed);
                }
                if self.cfg.autoplay
                    && self.queue.repeat == crate::player::queue::Repeat::Off
                    && self.current().is_some()
                {
                    self.flash("Queue cleared (autoplay on)", false);
                } else {
                    self.flash("Queue cleared", false);
                }
            }
            Refresh => match self.tab {
                Tab::Search => {
                    if let Some(view) = self.search.stack.last().map(|l| l.view.clone()) {
                        let lv = self.load_view(view);
                        *self.search.stack.last_mut().unwrap() = lv;
                    }
                }
                Tab::Library => {
                    if let Some(view) = self.library.stack.last().map(|l| l.view.clone()) {
                        let lv = self.load_view(view);
                        *self.library.stack.last_mut().unwrap() = lv;
                    }
                }
                _ => {}
            },
            JumpToCurrent => match self.tab {
                Tab::Lyrics => {
                    self.lyrics_scroll = None;
                    self.lyrics_sel = None;
                }
                _ => {
                    self.set_tab(Tab::Queue);
                    self.queue_state.select(self.queue.current);
                }
            },
            SleepTimer => self.open_sleep_timer(),
            Interlink => self.open_interlink(),
            Equalizer => self.open_equalizer(),
            AbLoop => {
                if self.current().is_some() {
                    let msg = self.toggle_ab_loop();
                    self.flash(msg, false);
                } else {
                    self.flash("Play a track first to set an A-B loop", false);
                }
            }
            CopyLink => self.copy_universal_link(),
            SpeedUp => {
                let next = crate::audio::step_speed(self.cfg.playback_speed, true);
                self.set_playback_speed(next);
                self.flash(format!("Speed: {}", crate::audio::format_speed(next)), false);
            }
            SpeedDown => {
                let next = crate::audio::step_speed(self.cfg.playback_speed, false);
                self.set_playback_speed(next);
                self.flash(format!("Speed: {}", crate::audio::format_speed(next)), false);
            }
            Stats => self.open_stats(),
            SaveOffline => {
                let msg = self.toggle_offline_for_target(false);
                self.flash(msg, false);
            }
        }
    }

    fn back_or_menu(&mut self) {
        match self.tab {
            Tab::Search if self.search.stack.len() > 1 => {
                self.search.stack.pop();
            }
            Tab::Library if self.library.stack.len() > 1 => {
                self.library.stack.pop();
            }
            Tab::Lyrics if self.lyrics_scroll.is_some() || self.lyrics_sel.is_some() => {
                self.lyrics_scroll = None;
                self.lyrics_sel = None;
            }
            _ => {
                let mut state = TableState::default();
                state.select(Some(0));
                self.overlay = Some(Overlay::Menu { state });
            }
        }
    }

    fn select(&mut self, mode: EnqueueMode) {
        match self.tab {
            Tab::Queue => {
                if let Some(i) = self.queue_state.selected() {
                    match mode {
                        EnqueueMode::PlayNow => self.play_index(i),
                        _ => {
                            if let Some(t) = self.queue.get(i).cloned() {
                                self.enqueue(vec![t], mode);
                            }
                        }
                    }
                }
            }
            Tab::Search => self.select_in_browser(mode),
            Tab::Library => {
                if !self.logged_in && self.library.section != LibSection::Offline {
                    if mode == EnqueueMode::PlayNow {
                        self.select_auth_profile(self.library.auth.selected);
                    }
                } else {
                    self.select_in_browser(mode);
                }
            }
            Tab::Lyrics => {
                if mode == EnqueueMode::PlayNow {
                    if let Some(idx) = self.lyrics_sel
                        && let LyricsState::Found {
                            lyrics: Lyrics::Synced(lines),
                            ..
                        } = &self.lyrics
                        && let Some((ts, _)) = lines.get(idx)
                    {
                        self.player(PlayerCmd::SeekAbsolute(*ts));
                        self.lyrics_sel = None;
                        self.lyrics_scroll = None;
                        return;
                    }
                    self.dispatch(Action::PlayPause);
                }
            }
            Tab::NowPlaying => {
                if mode == EnqueueMode::PlayNow {
                    self.dispatch(Action::PlayPause);
                }
            }
        }
    }

    fn select_in_browser(&mut self, mode: EnqueueMode) {
        let stack = if self.tab == Tab::Search {
            &self.search.stack
        } else {
            &self.library.stack
        };
        let Some(lv) = stack.last() else { return };
        let Some(sel_idx) = lv.state.selected() else { return };
        let Some(item) = lv.items.get(sel_idx).cloned() else {
            return;
        };
        let view = lv.view.clone();
        let whole_list = matches!(
            view,
            View::Album(_) | View::Playlist(_) | View::LikedSongs | View::Offline
        );
        let list_tracks = lv.tracks();
        // Count tracks preceding the selected row so duplicate songs in a
        // playlist start at the exact row the user chose.
        let track_pos = lv.items[..sel_idx]
            .iter()
            .filter(|i| matches!(i, Item::Track(_)))
            .count();
        match item {
            Item::Track(t) => {
                self.consecutive_errors = 0;
                if mode == EnqueueMode::PlayNow && whole_list {
                    // Play the album/playlist from this track onwards.
                    self.autoplay_requested = None;
                    self.queue.replace(list_tracks, track_pos);
                    self.play_index(self.queue.current.unwrap_or(0));
                } else {
                    self.enqueue(vec![t], mode);
                }
            }
            Item::Album(a) => self.open_or_enqueue(View::Album(a.id), mode),
            Item::Playlist(p) => self.open_or_enqueue(View::Playlist(p.id), mode),
            Item::Artist(a) => {
                if mode == EnqueueMode::PlayNow {
                    self.push_view(View::Artist(a.id));
                } else {
                    self.flash("Open the artist to pick songs", false);
                }
            }
            Item::Header(_) => {}
        }
    }

    fn open_or_enqueue(&mut self, view: View, mode: EnqueueMode) {
        if mode == EnqueueMode::PlayNow {
            self.push_view(view);
        } else {
            self.load_enqueue(view, mode);
        }
    }

    fn push_view(&mut self, view: View) {
        let lv = self.load_view(view);
        if self.tab == Tab::Search {
            self.search.stack.push(lv);
        } else {
            self.library.stack.push(lv);
        }
    }

    /// The track an action like "add to playlist" applies to: the selected
    /// row in a list, otherwise the one playing.
    fn target_track(&self) -> Option<Track> {
        match self.tab {
            Tab::Queue => self.queue_state.selected().and_then(|i| self.queue.get(i)).cloned(),
            Tab::Search | Tab::Library => {
                let stack = if self.tab == Tab::Search {
                    &self.search.stack
                } else {
                    &self.library.stack
                };
                match stack.last().and_then(ListView::selected) {
                    Some(Item::Track(t)) => Some(t.clone()),
                    _ => None,
                }
            }
            Tab::NowPlaying | Tab::Lyrics => self.current().cloned(),
        }
    }

    fn open_playlist_picker(&mut self) {
        if !self.logged_in {
            self.flash(
                "Playlists need you to be signed in. Open the Library tab (b) to sign in, or run `pova login`.",
                true,
            );
            return;
        }
        let Some(track) = self.target_track() else {
            self.flash("Select a song first (or play one)", false);
            return;
        };
        let token = self.token();
        self.pending.insert(token, Pending::PlaylistPicker);
        let _ = self.tx.ytm.send(YtmReq::Load {
            token,
            view: View::LibraryPlaylists,
        });
        let mut state = TableState::default();
        state.select(Some(0));
        self.overlay = Some(Overlay::AddToPlaylist {
            track,
            playlists: None,
            state,
            naming: None,
        });
    }

    /// `D` in the Library tab: the open playlist, or the one selected in
    /// "Your playlists". Asks before deleting.
    fn ask_delete_playlist(&mut self) {
        if self.tab != Tab::Library {
            self.flash("Go to the Library tab (b) to delete one of your playlists", false);
            return;
        }
        if !self.logged_in {
            self.flash(
                "Deleting playlists needs you to be signed in. Open the Library tab (b) to sign in, or run `pova login`.",
                true,
            );
            return;
        }
        let Some(lv) = self.library.stack.last() else { return };
        let target = match (&lv.view, lv.selected()) {
            (View::Playlist(id), _) => {
                // The open playlist's own title is generic; use its name from the list.
                let name = self
                    .library
                    .stack
                    .iter()
                    .flat_map(|l| &l.items)
                    .find_map(|i| match i {
                        Item::Playlist(p) if p.id == *id => Some(p.title.clone()),
                        _ => None,
                    })
                    .unwrap_or_else(|| "this playlist".into());
                Some((id.clone(), name))
            }
            (View::LibraryPlaylists, Some(Item::Playlist(p))) => Some((p.id.clone(), p.title.clone())),
            _ => None,
        };
        let Some((id, title)) = target else {
            self.flash(
                "Select one of your playlists (Library → Your playlists) to delete it",
                false,
            );
            return;
        };
        if matches!(id.trim_start_matches("VL"), "LM" | "SE") {
            self.flash("Liked songs can't be deleted", false);
            return;
        }
        self.overlay = Some(Overlay::Confirm {
            question: format!("Delete the playlist “{title}”? This can't be undone."),
            on_yes: Confirmed::DeletePlaylist { id, title },
            yes: false,
        });
    }

    fn confirmed(&mut self, what: Confirmed) {
        match what {
            Confirmed::DeletePlaylist { id, title } => {
                let _ = self.tx.ytm.send(YtmReq::DeletePlaylist {
                    playlist_id: id,
                    title: title.clone(),
                });
                self.flash(format!("Deleting {title}…"), false);
            }
            Confirmed::SignOut => {
                let _ = self.tx.ytm.send(YtmReq::SignOut);
                self.flash("Signed out", false);
            }
        }
    }

    /// `d` in the Library tab: take the selected song out of the open playlist (or Offline cache).
    fn remove_from_playlist(&mut self) {
        let Some(lv) = self.library.stack.last() else { return };
        if lv.view == View::Offline {
            if let Some(Item::Track(t)) = lv.selected().cloned() {
                let dir = self.paths.offline_dir();
                if self.offline.remove(&dir, &t.video_id).is_some() {
                    self.refresh_offline_view();
                    self.flash(format!("Removed “{}” from offline cache", t.title), false);
                }
            }
            return;
        }
        let View::Playlist(playlist_id) = &lv.view else {
            self.flash("Open one of your playlists to remove songs from it", false);
            return;
        };
        if matches!(playlist_id.trim_start_matches("VL"), "LM") {
            self.flash("To take a song out of Liked songs, unlike it with f", false);
            return;
        }
        let Some(Item::Track(t)) = lv.selected() else { return };
        let _ = self.tx.ytm.send(YtmReq::RemoveFromPlaylist {
            playlist_id: playlist_id.clone(),
            video_id: t.video_id.clone(),
            title: t.title.clone(),
        });
        self.flash(format!("Removing “{}”…", t.title), false);
    }

    fn remove_selected(&mut self) {
        if self.tab != Tab::Queue {
            return;
        }
        let Some(i) = self.queue_state.selected() else { return };
        let was_current = self.queue.current == Some(i);
        if self.queue.remove(i).is_none() {
            return;
        }
        if self.queue.is_empty() {
            self.queue_state.select(None);
            self.player(PlayerCmd::Stop);
            self.next_sent = None;
            self.lyrics = LyricsState::None;
            self.lyrics_requested = None;
            self.lyrics_scroll = None;
            self.art_key = None;
            self.art_loading = false;
            if let Some(a) = self.art.as_mut() {
                a.empty_protocol();
            }
            self.player.time = 0.0;
            self.player.duration = 0.0;
            self.load_started = None;
            self.direct_loaded = None;
            self.start_timer = None;
            self.autoplay_requested = None;
            self.consecutive_errors = 0;
            self.save_session();
            return;
        }
        self.queue_state.select(Some(i.min(self.queue.len() - 1)));
        if was_current {
            if let Some(c) = self.queue.current {
                self.play_index(c);
            }
        } else {
            self.sync_next();
        }
    }

    // ───────────────────────────── sleep, audio & interlink ─────────────────────────────

    fn restore_crossfade_volume(&mut self) {
        if self.crossfade_active {
            self.crossfade_active = false;
            self.crossfade_last_sent = None;
            if !self.sleep.is_fading() {
                self.player(PlayerCmd::SetVolume(self.player.volume));
            }
        }
    }

    pub fn open_sleep_timer(&mut self) {
        let mut state = TableState::default();
        let initial = match self.sleep.mode {
            crate::sleep::SleepMode::EndOfTrack => 0,
            crate::sleep::SleepMode::Countdown { .. } => 4,
            crate::sleep::SleepMode::Off => 3, // 15 minutes default highlight
        };
        state.select(Some(initial));
        self.overlay = Some(Overlay::SleepTimer { state, custom: None });
    }

    fn set_sleep_minutes(&mut self, minutes: u64) {
        let was_eot = self.sleep.is_end_of_track();
        if let Some(vol) = self.sleep.set_minutes(minutes, Instant::now()) {
            self.player.volume = vol;
            self.player(PlayerCmd::SetVolume(vol));
        }
        if was_eot {
            if self.queue.repeat == Repeat::One {
                self.player(PlayerCmd::SetLoopFile(true));
            }
            self.sync_next();
        }
        self.flash(format!("Sleep timer: {minutes} min"), false);
    }

    fn set_sleep_end_of_track(&mut self) {
        if let Some(vol) = self.sleep.set_end_of_track() {
            self.player.volume = vol;
            self.player(PlayerCmd::SetVolume(vol));
        }
        self.player(PlayerCmd::SetLoopFile(false));
        self.sync_next();
        self.flash("Sleep timer: end of current track", false);
    }

    fn cancel_sleep_timer(&mut self) {
        let was_eot = self.sleep.is_end_of_track();
        if let Some(vol) = self.sleep.cancel() {
            self.player.volume = vol;
            self.player(PlayerCmd::SetVolume(vol));
        }
        if was_eot {
            if self.queue.repeat == Repeat::One {
                self.player(PlayerCmd::SetLoopFile(true));
            }
            self.sync_next();
        }
        self.last_sleep_sec = None;
        self.flash("Sleep timer turned off", false);
    }

    fn adjust_sleep_minutes(&mut self, delta: i64) -> String {
        let was_eot = self.sleep.is_end_of_track();
        let (msg, restore) = self.sleep.adjust_minutes(delta, Instant::now());
        if let Some(vol) = restore {
            self.player.volume = vol;
            self.player(PlayerCmd::SetVolume(vol));
        }
        if was_eot && !self.sleep.is_end_of_track() {
            if self.queue.repeat == Repeat::One {
                self.player(PlayerCmd::SetLoopFile(true));
            }
            self.sync_next();
        }
        self.flash(msg.clone(), false);
        msg
    }

    fn apply_sleep_preset(&mut self, preset: SleepPreset) {
        match preset {
            SleepPreset::EndOfTrack => {
                self.overlay = None;
                self.set_sleep_end_of_track();
            }
            SleepPreset::Minutes(m) => {
                self.overlay = None;
                self.set_sleep_minutes(m);
            }
            SleepPreset::Custom => {
                if let Some(Overlay::SleepTimer { custom, .. }) = self.overlay.as_mut() {
                    *custom = Some(String::new());
                }
            }
            SleepPreset::Off => {
                self.overlay = None;
                self.cancel_sleep_timer();
            }
        }
    }

    pub fn open_equalizer(&mut self) {
        self.overlay = Some(Overlay::Equalizer { band: 0 });
    }

    fn apply_eq(&mut self) {
        self.picked_eq = Some(self.cfg.eq.clone());
        self.player(PlayerCmd::SetEq(self.cfg.eq.mpv_filter()));
        self.save_state();
    }

    fn set_audio_quality(&mut self, quality: AudioQuality) {
        self.cfg.audio_quality = quality;
        self.picked_quality = Some(quality);
        // Drop pre-resolved streams so upcoming tracks fetch the newly chosen quality tier.
        self.streams.clear();
        self.next_sent = None;
        self.player(PlayerCmd::SetYtdlFormat(quality.ytdl_format().into()));
        self.sync_next();
        self.save_state();
    }

    fn set_playback_speed(&mut self, speed: f64) {
        let s = (speed * 100.0).round() / 100.0;
        let s = s.clamp(0.25, 3.0);
        self.cfg.playback_speed = s;
        self.player.speed = s;
        self.picked_speed = Some(s);
        self.player(PlayerCmd::SetSpeed(s));
        self.save_state();
    }

    fn toggle_ab_loop(&mut self) -> String {
        self.ab_last_wrap = None;
        self.ab_loop.toggle(self.player.time)
    }

    pub fn open_interlink(&mut self) {
        let Some(track) = self.target_track() else {
            self.flash("Select or play a track first to interlink", false);
            return;
        };
        let is_current = self.current().is_some_and(|c| c.video_id == track.video_id);
        let mut actions = Vec::new();
        if !track.artists.trim().is_empty() {
            actions.push(InterlinkAction::GoToArtist {
                name: track.artists.clone(),
                artist_id: track.artist_id.clone(),
            });
        }
        if let Some(album) = track.album.as_deref().filter(|a| !a.trim().is_empty()) {
            actions.push(InterlinkAction::GoToAlbum {
                title: album.to_string(),
                album_id: track.album_id.clone(),
                artist: track.artists.clone(),
            });
        }
        actions.push(InterlinkAction::StartRadio { track: track.clone() });
        actions.push(InterlinkAction::ToggleOffline {
            saved: self.offline.contains(&track.video_id),
            track: track.clone(),
        });
        if is_current {
            actions.push(InterlinkAction::ViewLyrics);
        }
        actions.push(InterlinkAction::CopyUniversal {
            video_id: track.video_id.clone(),
        });
        actions.push(InterlinkAction::CopyYtm {
            video_id: track.video_id.clone(),
        });
        if is_current && self.player.time >= 1.0 {
            actions.push(InterlinkAction::CopyTimestamped {
                video_id: track.video_id.clone(),
                secs: self.player.time,
            });
        }
        actions.push(InterlinkAction::OpenUniversal {
            video_id: track.video_id.clone(),
        });
        actions.push(InterlinkAction::OpenYtm {
            video_id: track.video_id.clone(),
        });
        actions.push(InterlinkAction::SearchSpotify {
            title: track.title.clone(),
            artist: track.artists.clone(),
        });
        actions.push(InterlinkAction::SearchTidal {
            title: track.title.clone(),
            artist: track.artists.clone(),
        });
        let mut state = TableState::default();
        state.select(Some(0));
        self.overlay = Some(Overlay::Interlink {
            track,
            is_current,
            actions,
            state,
        });
    }

    fn execute_interlink(&mut self, action: InterlinkAction) {
        match action {
            InterlinkAction::GoToArtist { name, artist_id } => self.go_to_artist(name, artist_id),
            InterlinkAction::GoToAlbum {
                title,
                album_id,
                artist,
            } => self.go_to_album(title, album_id, artist),
            InterlinkAction::StartRadio { track } => {
                let playing = self.current().is_some_and(|c| c.video_id == track.video_id);
                let mode = if playing { RadioMode::Refill } else { RadioMode::Start };
                self.radio.insert(track.video_id.clone(), mode);
                let _ = self.tx.ytm.send(YtmReq::Radio { seed: track.video_id });
                self.flash(format!("Starting radio from {}…", track.title), false);
            }
            InterlinkAction::ToggleOffline { track, .. } => {
                let msg = self.toggle_offline_track(track);
                self.flash(msg, false);
            }
            InterlinkAction::ViewLyrics => self.set_tab(Tab::Lyrics),
            InterlinkAction::CopyUniversal { video_id } => {
                let url = crate::interlink::universal_url(&video_id);
                match crate::interlink::copy_to_clipboard(&url) {
                    Ok(where_) => self.flash(format!("Copied universal link to {where_}: {url}"), false),
                    Err(e) => self.flash(e, true),
                }
            }
            InterlinkAction::CopyYtm { video_id } => {
                let url = crate::interlink::ytm_url(&video_id, None);
                match crate::interlink::copy_to_clipboard(&url) {
                    Ok(where_) => self.flash(format!("Copied YouTube Music link to {where_}"), false),
                    Err(e) => self.flash(e, true),
                }
            }
            InterlinkAction::CopyTimestamped { video_id, secs } => {
                let url = crate::interlink::ytm_url(&video_id, Some(secs));
                match crate::interlink::copy_to_clipboard(&url) {
                    Ok(where_) => self.flash(
                        format!(
                            "Copied link @ {} to {where_}",
                            crate::model::format_time(secs)
                        ),
                        false,
                    ),
                    Err(e) => self.flash(e, true),
                }
            }
            InterlinkAction::OpenUniversal { video_id } => {
                let url = crate::interlink::universal_url(&video_id);
                match crate::interlink::open_in_browser(&url) {
                    Ok(()) => self.flash(format!("Opened {url}"), false),
                    Err(e) => self.flash(e, true),
                }
            }
            InterlinkAction::OpenYtm { video_id } => {
                let url = crate::interlink::ytm_url(&video_id, None);
                match crate::interlink::open_in_browser(&url) {
                    Ok(()) => self.flash("Opened in YouTube Music", false),
                    Err(e) => self.flash(e, true),
                }
            }
            InterlinkAction::SearchSpotify { title, artist } => {
                let url = crate::interlink::spotify_search_url(&title, &artist);
                match crate::interlink::open_in_browser(&url) {
                    Ok(()) => self.flash("Opened Spotify search", false),
                    Err(e) => self.flash(e, true),
                }
            }
            InterlinkAction::SearchTidal { title, artist } => {
                let url = crate::interlink::tidal_search_url(&title, &artist);
                match crate::interlink::open_in_browser(&url) {
                    Ok(()) => self.flash("Opened Tidal search", false),
                    Err(e) => self.flash(e, true),
                }
            }
        }
    }

    fn copy_universal_link(&mut self) {
        let Some(track) = self.target_track() else {
            self.flash("Select or play a track first to copy its link", false);
            return;
        };
        let url = crate::interlink::universal_url(&track.video_id);
        match crate::interlink::copy_to_clipboard(&url) {
            Ok(where_) => self.flash(format!("Copied song.link to {where_}: {url}"), false),
            Err(e) => self.flash(e, true),
        }
    }

    fn go_to_artist(&mut self, name: String, artist_id: Option<String>) {
        self.set_tab(Tab::Search);
        if let Some(id) = artist_id {
            let lv = self.load_view(View::Artist(id));
            self.search.stack.push(lv);
        } else {
            let query = crate::lyrics::primary_artist(&name);
            self.search.input = query.clone();
            self.search.cursor = query.chars().count();
            self.search.filter = SearchFilter::Artists;
            let lv = self.load_view(View::Search {
                filter: SearchFilter::Artists,
                query,
            });
            self.search.stack = vec![lv];
        }
    }

    fn go_to_album(&mut self, title: String, album_id: Option<String>, artist: String) {
        self.set_tab(Tab::Search);
        if let Some(id) = album_id {
            let lv = self.load_view(View::Album(id));
            self.search.stack.push(lv);
        } else {
            let query = if artist.trim().is_empty() {
                title
            } else {
                format!("{title} {}", crate::lyrics::primary_artist(&artist))
            };
            self.search.input = query.clone();
            self.search.cursor = query.chars().count();
            self.search.filter = SearchFilter::Albums;
            let lv = self.load_view(View::Search {
                filter: SearchFilter::Albums,
                query,
            });
            self.search.stack = vec![lv];
        }
    }

    pub fn open_external_link(&mut self, link: ExternalLink) {
        let label = match &link {
            ExternalLink::YouTubeTrack { .. } => "YouTube track",
            ExternalLink::YouTubePlaylist { .. } => "YouTube playlist",
            ExternalLink::YouTubeAlbum { .. } => "YouTube album",
            ExternalLink::YouTubeArtist { .. } => "YouTube artist",
            ExternalLink::CrossPlatform { service, .. } => service,
        };
        let _ = self.tx.ytm.send(YtmReq::ResolveLink { link });
        self.flash(format!("Opening {label} link…"), false);
    }

    pub fn save_stats(&mut self) {
        if self.stats_dirty {
            self.stats.save(&self.paths.stats_file());
            self.stats_dirty = false;
        }
    }

    pub fn open_stats(&mut self) {
        let mut state = TableState::default();
        if !self.stats.top_tracks(1).is_empty() {
            state.select(Some(0));
        }
        self.overlay = Some(Overlay::Stats {
            tab: StatsTab::TopTracks,
            state,
        });
    }

    fn stats_row_count(&self, tab: StatsTab) -> usize {
        stats_row_count(&self.stats, tab)
    }

    fn activate_stats_row(&mut self, tab: StatsTab, idx: usize) {
        match tab {
            StatsTab::TopTracks => {
                if let Some(st) = self.stats.top_tracks(20).get(idx) {
                    let tr = st.track.to_track();
                    self.overlay = None;
                    self.enqueue(vec![tr], EnqueueMode::PlayNow);
                }
            }
            StatsTab::Recent => {
                if let Some(st) = self.stats.recent_tracks(25).get(idx) {
                    let tr = st.track.to_track();
                    self.overlay = None;
                    self.enqueue(vec![tr], EnqueueMode::PlayNow);
                }
            }
            StatsTab::TopArtists => {
                if let Some(a) = self.stats.top_artists(20).get(idx) {
                    let name = a.name.clone();
                    let id = a.artist_id.clone();
                    self.overlay = None;
                    self.go_to_artist(name, id);
                }
            }
        }
    }

    pub fn is_offline_cached(&self, video_id: &str) -> bool {
        self.offline.contains(video_id)
    }

    fn refresh_offline_view(&mut self) {
        let tracks = self.offline.tracks_newest_first();
        let n = tracks.len();
        let size = crate::offline::format_bytes(self.offline.total_bytes());
        let mut lv = ListView::new(View::Offline, 0);
        lv.loading = false;
        lv.title = if n == 0 {
            "Offline · press O on any song to save it locally".into()
        } else {
            format!("Offline · {n} track{} ({size})", if n == 1 { "" } else { "s" })
        };
        lv.items = tracks.into_iter().map(Item::Track).collect();
        if !lv.items.is_empty() {
            lv.state.select(Some(0));
        }
        self.library.stack = vec![lv];
    }

    fn toggle_offline_for_target(&mut self, current_only: bool) -> String {
        let track = if current_only {
            self.current().cloned()
        } else {
            self.target_track()
        };
        let Some(track) = track else {
            return "Select or play a track first to save offline".into();
        };
        self.toggle_offline_track(track)
    }

    fn toggle_offline_track(&mut self, track: Track) -> String {
        let dir = self.paths.offline_dir();
        if self.offline.contains(&track.video_id) {
            self.offline.remove(&dir, &track.video_id);
            if self.library.section == LibSection::Offline && self.tab == Tab::Library {
                self.refresh_offline_view();
            }
            return format!("Removed “{}” from offline cache", track.title);
        }
        if !self.offline_pending.insert(track.video_id.clone()) {
            return format!("Already saving “{}” offline…", track.title);
        }
        let title = track.title.clone();
        let _ = self.tx.offline.send(OfflineReq {
            track,
            format: self.cfg.audio_quality.ytdl_format().to_string(),
        });
        format!("Saving “{title}” offline…")
    }

    pub fn on_offline(&mut self, ev: OfflineEvent) {
        self.dirty = true;
        match ev {
            OfflineEvent::Saved(entry) => {
                let vid = entry.track.video_id.clone();
                let title = entry.track.title.clone();
                let size = crate::offline::format_bytes(entry.bytes);
                self.offline_pending.remove(&vid);
                self.offline.entries.insert(vid, entry);
                self.offline.save(&self.paths.offline_dir());
                if self.library.section == LibSection::Offline && self.tab == Tab::Library {
                    self.refresh_offline_view();
                }
                self.sync_next();
                self.flash(format!("Saved “{title}” offline ({size})"), false);
            }
            OfflineEvent::Failed { video_id, title, error } => {
                self.offline_pending.remove(&video_id);
                self.flash(format!("Couldn't save “{title}” offline: {error}"), true);
            }
        }
    }

    fn set_discord_rpc(&mut self, on: bool) {
        self.cfg.discord_rpc = on;
        self.picked_discord = Some(on);
        let _ = self.tx.discord.send(PresenceCmd::SetEnabled(on));
        self.last_discord = None;
        if on {
            self.sync_discord(true);
        }
        self.save_state();
    }

    fn sync_discord(&mut self, force: bool) {
        if !self.cfg.discord_rpc {
            return;
        }
        let next = self.current().map(|t| {
            let quality_badge = if self.offline.contains(&t.video_id) {
                format!("{} · OFFLINE", self.player.audio.badge(self.cfg.audio_quality))
            } else {
                self.player.audio.badge(self.cfg.audio_quality)
            };
            PresenceTrack {
                video_id: t.video_id.clone(),
                title: t.title.clone(),
                artists: t.artists.clone(),
                album: t.album.clone(),
                art_url: t.art_url(),
                quality_badge,
                position: self.player.time,
                duration: if self.player.duration > 0.0 {
                    self.player.duration
                } else {
                    t.duration.unwrap_or(0.0)
                },
                paused: self.player.paused || self.player.idle,
            }
        });
        let changed = force
            || match (&self.last_discord, &next) {
                (Some(Some(prev)), Some(cur)) => {
                    prev.video_id != cur.video_id
                        || prev.paused != cur.paused
                        || prev.quality_badge != cur.quality_badge
                        || (prev.duration - cur.duration).abs() > 2.0
                        || (prev.position - cur.position).abs() > 3.0
                }
                (Some(None), None) => false,
                _ => true,
            };
        if changed {
            self.last_discord = Some(next.clone());
            let _ = self.tx.discord.send(PresenceCmd::Update(next));
        }
    }

    // ───────────────────────────── overlays ─────────────────────────────

    fn open_themes(&mut self) {
        let names = theme::list_themes(&self.paths.config_dir);
        let mut state = TableState::default();
        state.select(Some(names.iter().position(|(n, _)| *n == self.theme.name).unwrap_or(0)));
        self.overlay = Some(Overlay::Themes {
            names,
            state,
            original: self.theme.clone(),
        });
    }

    fn preview_theme(&mut self) {
        if let Some(Overlay::Themes { names, state, .. }) = &self.overlay
            && let Some((name, path)) = state.selected().and_then(|i| names.get(i))
        {
            let bg = self.cfg.theme_background;
            self.theme = match path {
                Some(p) => Theme::load(p, bg).unwrap_or_else(|_| Theme::builtin_default(bg)),
                None => Theme::builtin(name, bg).unwrap_or_else(|| Theme::builtin_default(bg)),
            };
            if name != &self.theme.name {
                self.theme.name = name.clone();
            }
        }
    }

    fn on_overlay_key(&mut self, k: KeyEvent) {
        let action = self.keymap.action(&k);
        let Some(overlay) = self.overlay.as_mut() else { return };
        match overlay {
            Overlay::Help { scroll } => match action {
                Some(Action::Down) => *scroll = scroll.saturating_add(1),
                Some(Action::Up) => *scroll = scroll.saturating_sub(1),
                Some(Action::PageDown) => *scroll = scroll.saturating_add(10),
                Some(Action::PageUp) => *scroll = scroll.saturating_sub(10),
                Some(Action::Top) => *scroll = 0,
                Some(Action::Bottom) => *scroll = u16::MAX,
                Some(Action::Quit) => self.quit = true,
                _ => self.overlay = None,
            },
            Overlay::Themes { names, state, original } => {
                let len = names.len();
                let cur = state.selected().unwrap_or(0);
                match (k.code, action) {
                    (KeyCode::Enter, _) => {
                        self.overlay = None;
                        self.save_state();
                        self.flash(format!("Theme: {}", self.theme.name), false);
                    }
                    (KeyCode::Esc, _) | (_, Some(Action::Themes)) => {
                        self.theme = original.clone();
                        self.overlay = None;
                    }
                    (_, Some(Action::Down)) => {
                        state.select(Some((cur + 1) % len));
                        self.preview_theme();
                    }
                    (_, Some(Action::Up)) => {
                        state.select(Some((cur + len - 1) % len));
                        self.preview_theme();
                    }
                    (_, Some(Action::PageDown)) => {
                        state.select(Some((cur + 10).min(len - 1)));
                        self.preview_theme();
                    }
                    (_, Some(Action::PageUp)) => {
                        state.select(Some(cur.saturating_sub(10)));
                        self.preview_theme();
                    }
                    (_, Some(Action::Top)) => {
                        state.select(Some(0));
                        self.preview_theme();
                    }
                    (_, Some(Action::Bottom)) => {
                        state.select(Some(len.saturating_sub(1)));
                        self.preview_theme();
                    }
                    _ => {}
                }
            }
            Overlay::Confirm { on_yes, yes, .. } => match k.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    let what = on_yes.clone();
                    self.overlay = None;
                    self.confirmed(what);
                }
                KeyCode::Enter if *yes => {
                    let what = on_yes.clone();
                    self.overlay = None;
                    self.confirmed(what);
                }
                KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => *yes = !*yes,
                KeyCode::Char('h') | KeyCode::Char('l') => *yes = !*yes,
                // n, Esc, Enter on "No", anything else: cancel.
                _ => self.overlay = None,
            },
            Overlay::AddToPlaylist {
                track,
                playlists,
                state,
                naming,
            } => {
                if let Some(name) = naming {
                    match k.code {
                        KeyCode::Esc => *naming = None,
                        KeyCode::Backspace => {
                            name.pop();
                        }
                        KeyCode::Enter => {
                            let name = name.trim().to_string();
                            if name.is_empty() {
                                return;
                            }
                            let _ = self.tx.ytm.send(YtmReq::CreatePlaylist {
                                name,
                                video_id: track.video_id.clone(),
                                title: track.title.clone(),
                            });
                            self.overlay = None;
                            self.flash("Creating playlist…", false);
                        }
                        KeyCode::Char(c)
                            if !k.modifiers.contains(KeyModifiers::CONTROL) && name.chars().count() < 150 =>
                        {
                            name.push(c);
                        }
                        _ => {}
                    }
                    return;
                }
                // Row 0 is "+ New playlist", then the user's playlists.
                let len = playlists.as_ref().map_or(1, |p| p.len() + 1);
                let cur = state.selected().unwrap_or(0).min(len - 1);
                match (k.code, action) {
                    (KeyCode::Esc, _) | (_, Some(Action::AddToPlaylist)) => self.overlay = None,
                    (_, Some(Action::Down)) => state.select(Some((cur + 1) % len)),
                    (_, Some(Action::Up)) => state.select(Some((cur + len - 1) % len)),
                    (_, Some(Action::Top)) => state.select(Some(0)),
                    (_, Some(Action::Bottom)) => state.select(Some(len - 1)),
                    (KeyCode::Enter, _) if cur == 0 => *naming = Some(String::new()),
                    (KeyCode::Enter, _) => {
                        if let Some(p) = playlists.as_ref().and_then(|p| p.get(cur - 1)) {
                            let _ = self.tx.ytm.send(YtmReq::AddToPlaylist {
                                playlist_id: p.id.clone(),
                                playlist_title: p.title.clone(),
                                video_id: track.video_id.clone(),
                                title: track.title.clone(),
                            });
                            let msg = format!("Adding to {}…", p.title);
                            self.overlay = None;
                            self.flash(msg, false);
                        }
                    }
                    (_, Some(Action::Quit)) => self.quit = true,
                    _ => {}
                }
            }
            Overlay::Accounts { items, state } => {
                let len = items.as_ref().map_or(0, Vec::len);
                let cur = state.selected().unwrap_or(0).min(len.saturating_sub(1));
                match (k.code, action) {
                    (KeyCode::Esc, _) => self.overlay = None,
                    (_, Some(Action::Down)) if len > 0 => state.select(Some((cur + 1) % len)),
                    (_, Some(Action::Up)) if len > 0 => state.select(Some((cur + len - 1) % len)),
                    (_, Some(Action::Top)) if len > 0 => state.select(Some(0)),
                    (_, Some(Action::Bottom)) if len > 0 => state.select(Some(len - 1)),
                    (KeyCode::Enter, _) => {
                        if let Some(acct) = items.as_ref().and_then(|v| v.get(cur)).cloned() {
                            self.overlay = None;
                            let _ = self.tx.ytm.send(YtmReq::SwitchAccount {
                                auth_user: acct.auth_user,
                                page_id: acct.page_id,
                                name: acct.name,
                            });
                        }
                    }
                    (_, Some(Action::Quit)) => self.quit = true,
                    _ => {}
                }
            }
            Overlay::Menu { state } => {
                let items = &MenuItem::ALL;
                let len = items.len();
                let cur = state.selected().unwrap_or(0).min(len - 1);
                let item = items[cur];
                match (k.code, action) {
                    // Settings toggle in place (menu stays open, like btop's options).
                    (KeyCode::Left | KeyCode::Right, _) if item == MenuItem::Equalizer => {
                        self.cfg.eq.cycle_preset(k.code == KeyCode::Right);
                        self.apply_eq();
                    }
                    (KeyCode::Left | KeyCode::Right, _) if item == MenuItem::SleepTimer => {
                        let (msg, restore) = self.sleep.cycle_quick(k.code == KeyCode::Right, Instant::now());
                        if let Some(vol) = restore {
                            self.player.volume = vol;
                            self.player(PlayerCmd::SetVolume(vol));
                        }
                        self.player(PlayerCmd::SetLoopFile(
                            self.queue.repeat == Repeat::One && !self.sleep.is_end_of_track(),
                        ));
                        self.sync_next();
                        self.flash(msg, false);
                    }
                    (KeyCode::Enter | KeyCode::Left | KeyCode::Right, _) if item.is_setting() => {
                        self.activate_menu(item, k.code != KeyCode::Left);
                    }
                    (KeyCode::Enter, _) | (_, Some(Action::Select)) => {
                        self.overlay = None;
                        self.activate_menu(item, true);
                    }
                    (KeyCode::Esc, _) => self.overlay = None,
                    (_, Some(Action::Down)) => state.select(Some((cur + 1) % len)),
                    (_, Some(Action::Up)) => state.select(Some((cur + len - 1) % len)),
                    (_, Some(Action::Top)) => state.select(Some(0)),
                    (_, Some(Action::Bottom)) => state.select(Some(len - 1)),
                    (_, Some(Action::Quit)) => self.quit = true,
                    _ => {}
                }
            }
            Overlay::SleepTimer { state, custom } => {
                if let Some(text) = custom {
                    match k.code {
                        KeyCode::Esc => *custom = None,
                        KeyCode::Backspace => {
                            text.pop();
                        }
                        KeyCode::Enter => {
                            let val = text.trim().to_string();
                            if let Ok(mins) = val.parse::<u64>()
                                && mins > 0
                            {
                                self.overlay = None;
                                self.set_sleep_minutes(mins);
                            } else {
                                self.flash("Enter a whole number of minutes (1–720)", true);
                            }
                        }
                        KeyCode::Char(c) if c.is_ascii_digit() && text.len() < 4 => text.push(c),
                        _ => {}
                    }
                    return;
                }
                let presets = SleepPreset::MENU_ITEMS;
                let len = presets.len();
                let cur = state.selected().unwrap_or(0).min(len - 1);
                match (k.code, action) {
                    (KeyCode::Esc, _) | (_, Some(Action::SleepTimer)) => self.overlay = None,
                    (_, Some(Action::Down)) => state.select(Some((cur + 1) % len)),
                    (_, Some(Action::Up)) => state.select(Some((cur + len - 1) % len)),
                    (_, Some(Action::Top)) => state.select(Some(0)),
                    (_, Some(Action::Bottom)) => state.select(Some(len - 1)),
                    (KeyCode::Char('+') | KeyCode::Char('='), _) => {
                        self.adjust_sleep_minutes(5);
                    }
                    (KeyCode::Char('-'), _) => {
                        self.adjust_sleep_minutes(-5);
                    }
                    (KeyCode::Enter, _) => {
                        let chosen = presets[cur];
                        self.apply_sleep_preset(chosen);
                    }
                    (_, Some(Action::Quit)) => self.quit = true,
                    _ => {}
                }
            }
            Overlay::Equalizer { band } => {
                let mut cur_band = *band;
                match (k.code, action) {
                    (KeyCode::Esc, _) | (_, Some(Action::Equalizer)) => self.overlay = None,
                    (KeyCode::Left, _) | (KeyCode::Char('h'), _) => {
                        cur_band = (cur_band + 9) % 10;
                        *band = cur_band;
                    }
                    (KeyCode::Right, _) | (KeyCode::Char('l'), _) => {
                        cur_band = (cur_band + 1) % 10;
                        *band = cur_band;
                    }
                    (KeyCode::Up, _) | (KeyCode::Char('k'), _) | (KeyCode::Char('+'), _) => {
                        self.cfg.eq.adjust_band(cur_band, 1.0);
                        self.apply_eq();
                    }
                    (KeyCode::Down, _) | (KeyCode::Char('j'), _) | (KeyCode::Char('-'), _) => {
                        self.cfg.eq.adjust_band(cur_band, -1.0);
                        self.apply_eq();
                    }
                    (KeyCode::Char(' '), _) => {
                        self.cfg.eq.enabled = !self.cfg.eq.enabled;
                        self.apply_eq();
                    }
                    (KeyCode::Char('p') | KeyCode::Char(']') | KeyCode::Tab, _) => {
                        self.cfg.eq.cycle_preset(true);
                        self.apply_eq();
                    }
                    (KeyCode::Char('P') | KeyCode::Char('[') | KeyCode::BackTab, _) => {
                        self.cfg.eq.cycle_preset(false);
                        self.apply_eq();
                    }
                    (KeyCode::Char('0') | KeyCode::Char('r'), _) => {
                        self.cfg.eq.reset_flat();
                        self.apply_eq();
                    }
                    (_, Some(Action::Quit)) => self.quit = true,
                    _ => {}
                }
            }
            Overlay::Interlink { actions, state, .. } => {
                let len = actions.len();
                let cur = state.selected().unwrap_or(0).min(len.saturating_sub(1));
                match (k.code, action) {
                    (KeyCode::Esc, _) | (_, Some(Action::Interlink)) => self.overlay = None,
                    (_, Some(Action::Down)) if len > 0 => state.select(Some((cur + 1) % len)),
                    (_, Some(Action::Up)) if len > 0 => state.select(Some((cur + len - 1) % len)),
                    (_, Some(Action::Top)) if len > 0 => state.select(Some(0)),
                    (_, Some(Action::Bottom)) if len > 0 => state.select(Some(len - 1)),
                    (KeyCode::Enter, _) => {
                        if let Some(chosen) = actions.get(cur).cloned() {
                            self.overlay = None;
                            self.execute_interlink(chosen);
                        }
                    }
                    (_, Some(Action::Quit)) => self.quit = true,
                    _ => {}
                }
            }
            Overlay::Stats { tab, state } => {
                let cur_tab = *tab;
                let len = stats_row_count(&self.stats, cur_tab);
                let cur = state.selected().unwrap_or(0).min(len.saturating_sub(1));
                match (k.code, action) {
                    (KeyCode::Esc, _) | (_, Some(Action::Stats)) => self.overlay = None,
                    (KeyCode::Right | KeyCode::Tab | KeyCode::Char(']'), _) => {
                        let next = cur_tab.next();
                        let next_len = stats_row_count(&self.stats, next);
                        *tab = next;
                        state.select((next_len > 0).then_some(0));
                    }
                    (KeyCode::Left | KeyCode::BackTab | KeyCode::Char('['), _) => {
                        let prev = cur_tab.prev();
                        let prev_len = stats_row_count(&self.stats, prev);
                        *tab = prev;
                        state.select((prev_len > 0).then_some(0));
                    }
                    (_, Some(Action::Down)) if len > 0 => state.select(Some((cur + 1) % len)),
                    (_, Some(Action::Up)) if len > 0 => state.select(Some((cur + len - 1) % len)),
                    (_, Some(Action::Top)) if len > 0 => state.select(Some(0)),
                    (_, Some(Action::Bottom)) if len > 0 => state.select(Some(len - 1)),
                    (KeyCode::Enter, _) if len > 0 => self.activate_stats_row(cur_tab, cur),
                    (_, Some(Action::Quit)) => self.quit = true,
                    _ => {}
                }
            }
        }
    }

    // ───────────────────────────── mouse ─────────────────────────────

    fn on_mouse(&mut self, m: MouseEvent) {
        match m.kind {
            MouseEventKind::ScrollDown => self.on_scroll(m.column, m.row, true),
            MouseEventKind::ScrollUp => self.on_scroll(m.column, m.row, false),
            MouseEventKind::Down(MouseButton::Left) => self.on_click(m.column, m.row),
            _ => return,
        }
        self.dirty = true;
    }

    fn on_scroll(&mut self, x: u16, y: u16, down: bool) {
        if let Some(overlay) = self.overlay.as_mut() {
            match overlay {
                Overlay::Help { scroll } => {
                    *scroll = if down {
                        scroll.saturating_add(3)
                    } else {
                        scroll.saturating_sub(3)
                    };
                }
                Overlay::Themes { names, state, .. } => {
                    let len = names.len();
                    if len > 0 {
                        let cur = state.selected().unwrap_or(0);
                        let next = if down { (cur + 1) % len } else { (cur + len - 1) % len };
                        state.select(Some(next));
                        self.preview_theme();
                    }
                }
                Overlay::Menu { state } => {
                    let len = MenuItem::ALL.len();
                    let cur = state.selected().unwrap_or(0);
                    let next = if down { (cur + 1) % len } else { (cur + len - 1) % len };
                    state.select(Some(next));
                }
                Overlay::AddToPlaylist {
                    playlists,
                    state,
                    naming,
                    ..
                } if naming.is_none() => {
                    let len = playlists.as_ref().map_or(1, |p| p.len() + 1);
                    let cur = state.selected().unwrap_or(0).min(len - 1);
                    let next = if down { (cur + 1) % len } else { (cur + len - 1) % len };
                    state.select(Some(next));
                }
                Overlay::Accounts { items, state } => {
                    let len = items.as_ref().map_or(0, Vec::len);
                    if len > 0 {
                        let cur = state.selected().unwrap_or(0).min(len - 1);
                        let next = if down { (cur + 1) % len } else { (cur + len - 1) % len };
                        state.select(Some(next));
                    }
                }
                Overlay::SleepTimer { state, custom } if custom.is_none() => {
                    let len = SleepPreset::MENU_ITEMS.len();
                    let cur = state.selected().unwrap_or(0).min(len - 1);
                    let next = if down { (cur + 1) % len } else { (cur + len - 1) % len };
                    state.select(Some(next));
                }
                Overlay::Interlink { actions, state, .. } => {
                    let len = actions.len();
                    if len > 0 {
                        let cur = state.selected().unwrap_or(0).min(len - 1);
                        let next = if down { (cur + 1) % len } else { (cur + len - 1) % len };
                        state.select(Some(next));
                    }
                }
                Overlay::Equalizer { band } => {
                    let b = *band;
                    self.cfg.eq.adjust_band(b, if down { -1.0 } else { 1.0 });
                    self.apply_eq();
                }
                Overlay::Stats { tab, state } => {
                    let len = stats_row_count(&self.stats, *tab);
                    if len > 0 {
                        let cur = state.selected().unwrap_or(0).min(len - 1);
                        let next = if down { (cur + 1) % len } else { (cur + len - 1) % len };
                        state.select(Some(next));
                    }
                }
                Overlay::Confirm { .. } | Overlay::AddToPlaylist { .. } | Overlay::SleepTimer { .. } => {}
            }
            return;
        }
        if matches!(self.hits.at(x, y), Some((_, Hit::Volume))) {
            self.dispatch(if down { Action::VolumeDown } else { Action::VolumeUp });
            return;
        }
        let step = if self.tab == Tab::Library && !self.logged_in {
            1
        } else {
            3
        };
        self.move_selection(if down { step } else { -step });
    }

    fn is_header_at(&self, idx: usize) -> bool {
        let stack = match self.tab {
            Tab::Search => &self.search.stack,
            Tab::Library => &self.library.stack,
            _ => return false,
        };
        stack
            .last()
            .and_then(|lv| lv.items.get(idx))
            .is_some_and(|item| matches!(item, Item::Header(_)))
    }

    fn on_click(&mut self, x: u16, y: u16) {
        // Error box is drawn on top of overlays, so clicking it closes it first.
        if matches!(self.hits.at(x, y), Some((_, Hit::CloseError))) {
            self.flash = None;
            return;
        }
        if self.overlay.is_some() {
            match self.hits.at(x, y).map(|(_, h)| h) {
                Some(Hit::MenuItem(i)) => {
                    if let Some(&item) = MenuItem::ALL.get(i) {
                        if item.is_setting() {
                            if let Some(Overlay::Menu { state }) = self.overlay.as_mut() {
                                state.select(Some(i));
                            }
                            self.activate_menu(item, true);
                        } else {
                            self.overlay = None;
                            self.activate_menu(item, true);
                        }
                    }
                    return;
                }
                Some(Hit::ThemeItem(i)) => {
                    if let Some(Overlay::Themes { state, .. }) = self.overlay.as_mut() {
                        state.select(Some(i));
                        self.preview_theme();
                        self.overlay = None;
                        self.save_state();
                        self.flash(format!("Theme: {}", self.theme.name), false);
                    }
                    return;
                }
                Some(Hit::AccountItem(i)) => {
                    let chosen = match self.overlay.as_ref() {
                        Some(Overlay::Accounts { items: Some(list), .. }) => list.get(i).cloned(),
                        _ => None,
                    };
                    if let Some(acct) = chosen {
                        self.overlay = None;
                        let _ = self.tx.ytm.send(YtmReq::SwitchAccount {
                            auth_user: acct.auth_user,
                            page_id: acct.page_id,
                            name: acct.name,
                        });
                    }
                    return;
                }
                Some(Hit::PlaylistPick(i)) => {
                    let mut start_naming = false;
                    let mut chosen: Option<(String, String, String, String)> = None;
                    if let Some(Overlay::AddToPlaylist {
                        track,
                        playlists,
                        naming,
                        ..
                    }) = self.overlay.as_mut()
                        && naming.is_none()
                    {
                        if i == 0 {
                            *naming = Some(String::new());
                            start_naming = true;
                        } else if let Some(p) = playlists.as_ref().and_then(|list| list.get(i - 1)) {
                            chosen = Some((
                                p.id.clone(),
                                p.title.clone(),
                                track.video_id.clone(),
                                track.title.clone(),
                            ));
                        }
                    }
                    if start_naming {
                        return;
                    }
                    if let Some((playlist_id, playlist_title, video_id, title)) = chosen {
                        let _ = self.tx.ytm.send(YtmReq::AddToPlaylist {
                            playlist_id,
                            playlist_title: playlist_title.clone(),
                            video_id,
                            title,
                        });
                        self.overlay = None;
                        self.flash(format!("Adding to {playlist_title}…"), false);
                        return;
                    }
                }
                Some(Hit::Confirm(yes)) => {
                    if let Some(Overlay::Confirm { on_yes, .. }) = self.overlay.take()
                        && yes
                    {
                        self.confirmed(on_yes);
                    }
                    return;
                }
                Some(Hit::SleepItem(i)) => {
                    if let Some(&preset) = SleepPreset::MENU_ITEMS.get(i) {
                        self.apply_sleep_preset(preset);
                    }
                    return;
                }
                Some(Hit::InterlinkItem(i)) => {
                    let chosen = match self.overlay.as_ref() {
                        Some(Overlay::Interlink { actions, .. }) => actions.get(i).cloned(),
                        _ => None,
                    };
                    if let Some(act) = chosen {
                        self.overlay = None;
                        self.execute_interlink(act);
                    }
                    return;
                }
                Some(Hit::EqBand(b)) => {
                    if let Some(Overlay::Equalizer { band }) = self.overlay.as_mut() {
                        *band = b.min(9);
                    }
                    return;
                }
                Some(Hit::EqToggle) => {
                    self.cfg.eq.enabled = !self.cfg.eq.enabled;
                    self.apply_eq();
                    return;
                }
                Some(Hit::EqPrevPreset) => {
                    self.cfg.eq.cycle_preset(false);
                    self.apply_eq();
                    return;
                }
                Some(Hit::EqNextPreset) => {
                    self.cfg.eq.cycle_preset(true);
                    self.apply_eq();
                    return;
                }
                Some(Hit::EqReset) => {
                    self.cfg.eq.reset_flat();
                    self.apply_eq();
                    return;
                }
                Some(Hit::StatsTab(next)) => {
                    let next_len = self.stats_row_count(next);
                    if let Some(Overlay::Stats { tab, state }) = self.overlay.as_mut() {
                        *tab = next;
                        state.select((next_len > 0).then_some(0));
                    }
                    return;
                }
                Some(Hit::StatsItem(i)) => {
                    if let Some(Overlay::Stats { tab, .. }) = self.overlay.as_ref() {
                        let cur_tab = *tab;
                        self.activate_stats_row(cur_tab, i);
                    }
                    return;
                }
                _ => {
                    if let Some(Overlay::Themes { original, .. }) = self.overlay.take() {
                        self.theme = original;
                    }
                    return;
                }
            }
        }
        let double = self
            .last_click
            .is_some_and(|(t, lx, ly)| t.elapsed() < Duration::from_millis(400) && lx == x && ly == y);
        self.last_click = Some((Instant::now(), x, y));

        if let Some((rect, hit)) = self.hits.at(x, y) {
            match hit {
                Hit::Tab(t) => self.set_tab(t),
                Hit::Action(a) => self.dispatch(a),
                Hit::Progress => {
                    if self.player.duration > 0.0 && rect.width > 1 {
                        let frac = (x - rect.x) as f64 / (rect.width - 1) as f64;
                        self.player(PlayerCmd::SeekAbsolute(frac * self.player.duration));
                    }
                }
                Hit::Volume => {
                    if rect.width > 1 {
                        self.crossfade_active = false;
                        let frac = ((x - rect.x) as f64 / (rect.width - 1) as f64).clamp(0.0, 1.0);
                        self.player.volume = (frac * 100.0).round();
                        self.player(PlayerCmd::SetVolume(self.player.volume));
                        if self.player.muted {
                            self.player(PlayerCmd::SetMute(false));
                        }
                    }
                }
                Hit::SearchInput => {
                    self.tab = Tab::Search;
                    self.search.editing = true;
                    self.search.cursor = self.search.input.chars().count();
                }
                Hit::SearchFilter(f) => {
                    self.search.filter = f;
                    if !self.search.input.trim().is_empty() {
                        self.submit_search();
                    }
                }
                Hit::SwitchAccount => self.open_accounts(),
                Hit::SleepTimer => self.open_sleep_timer(),
                Hit::AudioQuality => self.open_interlink(),
                Hit::NowPlayingArtist => {
                    if let Some(t) = self.current().cloned()
                        && !t.artists.trim().is_empty()
                    {
                        self.go_to_artist(t.artists, t.artist_id);
                    }
                }
                Hit::NowPlayingAlbum => {
                    if let Some(t) = self.current().cloned()
                        && let Some(album) = t.album
                    {
                        self.go_to_album(album, t.album_id, t.artists);
                    }
                }
                Hit::LyricLine(idx) => {
                    if let LyricsState::Found {
                        lyrics: Lyrics::Synced(lines),
                        ..
                    } = &self.lyrics
                        && let Some((ts, _)) = lines.get(idx)
                    {
                        self.player(PlayerCmd::SeekAbsolute(*ts));
                        self.lyrics_sel = None;
                        self.lyrics_scroll = None;
                    }
                }
                Hit::MenuItem(_)
                | Hit::ThemeItem(_)
                | Hit::AccountItem(_)
                | Hit::PlaylistPick(_)
                | Hit::Confirm(_)
                | Hit::SleepItem(_)
                | Hit::InterlinkItem(_)
                | Hit::EqBand(_)
                | Hit::EqToggle
                | Hit::EqPrevPreset
                | Hit::EqNextPreset
                | Hit::EqReset
                | Hit::StatsTab(_)
                | Hit::StatsItem(_) => {}
                Hit::LibSection(s) => {
                    self.library.section = s;
                    if self.logged_in || s == LibSection::Offline {
                        self.switch_library(s);
                    }
                }
                Hit::Auth => self.open_account_action(),
                Hit::AuthProfile(i) => self.select_auth_profile(i),
                Hit::AuthCustomBrowser => {
                    if !self.logged_in && self.library.auth.busy.is_none() && self.library.auth.waiting.is_none() {
                        self.library.auth.input = Some(AuthInput::CustomBrowser(String::new()));
                    }
                }
                Hit::AuthPasteCookie => {
                    if !self.logged_in && self.library.auth.busy.is_none() && self.library.auth.waiting.is_none() {
                        self.library.auth.input = Some(AuthInput::PasteCookie(String::new()));
                    }
                }
                Hit::AuthCheckNow => self.poll_auth_now(),
                Hit::AuthOpenBrowser => self.open_sign_in_link(),
                Hit::AuthCancel => self.library.auth.waiting = None,
                Hit::CloseError => {
                    self.flash = None;
                    self.dirty = true;
                }
                Hit::Suggestion(i) => {
                    if let Some(text) = self.search.visible_suggestions().get(i).map(|x| x.text.clone()) {
                        self.search.cursor = text.chars().count();
                        self.search.input = text;
                        self.search.editing = false;
                        self.search.suggest_sel = None;
                        self.submit_search();
                    }
                }
                Hit::List => {
                    if let Some((area, offset)) = self.hits.list {
                        let idx = offset + (y - area.y) as usize;
                        if self.tab == Tab::Search {
                            self.search.editing = false;
                        }
                        if self.is_header_at(idx) {
                            return;
                        }
                        if let Some((state, len)) = self.active_list()
                            && idx < len
                        {
                            let already = state.selected() == Some(idx);
                            state.select(Some(idx));
                            if already || double {
                                self.select(EnqueueMode::PlayNow);
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
pub mod tests_support {
    use super::*;

    pub fn list_view() -> ListView {
        ListView::new(
            View::Search {
                filter: SearchFilter::Songs,
                query: "q".into(),
            },
            1,
        )
    }
}

/// A fresh, private folder for one test App, so parallel tests never share
/// state files. Folders from earlier test runs (dead processes) are removed
/// the first time this runs, so nothing piles up in /tmp.
#[cfg(test)]
pub(crate) fn test_paths() -> Paths {
    use std::sync::Once;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static CLEAN: Once = Once::new();
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join("pova-tests");
    let me = std::process::id();
    CLEAN.call_once(|| {
        for e in std::fs::read_dir(&root).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let pid = name.split('-').next().and_then(|p| p.parse::<u32>().ok());
            if pid.is_some_and(|p| p != me && !std::path::Path::new(&format!("/proc/{p}")).exists()) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    });
    let dir = root.join(format!("{me}-{}", NEXT.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir_all(&dir).expect("test dir");
    Paths {
        config_dir: dir.clone(),
        cache_dir: dir.clone(),
        state_dir: dir.clone(),
        runtime_dir: dir,
    }
}

#[cfg(test)]
mod search_tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

    fn app() -> (App, UnboundedReceiver<YtmReq>) {
        let (p, _) = unbounded_channel();
        let (y, yrx) = unbounded_channel();
        let (l, _) = unbounded_channel();
        let (a, _) = unbounded_channel();
        let paths = test_paths();
        let (keymap, _) = Keymap::new(&Default::default());
        let app = App::new(
            Config::default(),
            paths,
            State::default(),
            keymap,
            Channels {
                player: p,
                ytm: y,
                lyrics: l,
                art: a,
                resolve: unbounded_channel().0,
                notify: unbounded_channel().0,
                discord: unbounded_channel().0,
                offline: unbounded_channel().0,
            },
            None,
            None,
        );
        (app, yrx)
    }

    fn key(app: &mut App, code: KeyCode) {
        app.on_terminal(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    fn sugg(text: &str) -> Suggestion {
        Suggestion {
            text: text.into(),
            history: false,
        }
    }

    #[test]
    fn typing_debounces_then_requests_suggestions() {
        let (mut app, mut rx) = app();
        app.dispatch(Action::TabSearch);
        for c in "call me".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        assert!(rx.try_recv().is_err(), "no request before the debounce");
        app.search.suggest_due = Some(Instant::now());
        app.tick();
        assert!(matches!(rx.try_recv(), Ok(YtmReq::Suggest { query }) if query == "call me"));
    }

    #[test]
    fn stale_suggestions_ignored_and_ghost_completion() {
        let (mut app, _rx) = app();
        app.dispatch(Action::TabSearch);
        for c in "Call Me".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        app.on_ytm(YtmEvent::Suggestions {
            query: "cal".into(),
            items: vec![sugg("calvin harris")],
        });
        assert!(app.search.visible_suggestions().is_empty());
        app.on_ytm(YtmEvent::Suggestions {
            query: "Call Me".into(),
            items: vec![sugg("call me when you're sober"), sugg("call me maybe")],
        });
        // Case-insensitive inline completion; → accepts it.
        assert_eq!(app.search.ghost(), Some(" when you're sober"));
        key(&mut app, KeyCode::Right);
        assert_eq!(app.search.input, "Call Me when you're sober");
    }

    #[test]
    fn down_enter_searches_selected_suggestion() {
        let (mut app, mut rx) = app();
        app.dispatch(Action::TabSearch);
        for c in "call me".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        app.on_ytm(YtmEvent::Suggestions {
            query: "call me".into(),
            items: vec![sugg("call me when you're sober"), sugg("call me maybe")],
        });
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Down);
        assert_eq!(app.search.suggest_sel, Some(1));
        key(&mut app, KeyCode::Enter);
        assert!(!app.search.editing);
        assert_eq!(app.search.input, "call me maybe");
        assert!(matches!(
            rx.try_recv(),
            Ok(YtmReq::Load { view: View::Search { query, .. }, .. }) if query == "call me maybe"
        ));
    }

    fn track(id: &str) -> Track {
        Track {
            video_id: id.into(),
            title: id.into(),
            artists: String::new(),
            artist_id: None,
            album: None,
            album_id: None,
            duration: None,
            thumbnail: None,
            liked: None,
        }
    }

    /// Sign in and answer the Liked-songs request with `liked_ids`.
    fn signed_in(liked_ids: &[&str]) -> (App, UnboundedReceiver<YtmReq>) {
        let (mut app, mut rx) = app();
        app.on_ytm(YtmEvent::Auth {
            logged_in: true,
            message: None,
        });
        let token = match rx.try_recv() {
            Ok(YtmReq::Load {
                token,
                view: View::LikedSongs,
            }) => token,
            other => panic!("expected liked-songs load, got {other:?}"),
        };
        assert!(!app.likes_loaded);
        let items = liked_ids.iter().map(|id| Item::Track(track(id))).collect();
        app.on_ytm(YtmEvent::Loaded {
            token,
            title: "Liked songs".into(),
            items,
        });
        assert!(app.likes_loaded);
        (app, rx)
    }

    #[test]
    fn dislike_rates_and_skips_and_needs_sign_in() {
        // Signed out: nothing is sent, and the error says what to do.
        let (mut app, mut rx) = app();
        app.queue.replace(vec![track("a"), track("b")], 0);
        app.dispatch(Action::Dislike);
        assert!(rx.try_recv().is_err());
        assert_eq!(app.queue.current, Some(0));
        assert!(
            app.flash
                .as_ref()
                .is_some_and(|f| f.error && f.text.contains("pova login"))
        );

        let (mut app, mut rx) = signed_in(&["a"]);
        app.queue.replace(vec![track("a"), track("b"), track("c")], 0);
        app.dispatch(Action::Dislike);
        assert!(matches!(rx.try_recv(), Ok(YtmReq::Dislike { video_id }) if video_id == "a"));
        assert_eq!(app.queue.current, Some(1), "skips to the next song");
        app.on_ytm(YtmEvent::Disliked { video_id: "a".into() });
        assert!(!is_liked(&app.likes, &track("a")), "a dislike replaces the like");
        assert_eq!(app.queue.get(0).unwrap().liked, Some(false));
    }

    #[test]
    fn clearing_the_queue_fetches_a_fresh_radio_near_the_end() {
        let (mut app, mut rx) = signed_in(&[]);
        app.queue.replace(vec![track("a")], 0);
        app.play_index(0); // last song starts: autoplay asks for a radio of "a"
        assert!(std::iter::from_fn(|| rx.try_recv().ok()).any(|r| matches!(r, YtmReq::Radio { .. })));
        app.on_ytm(YtmEvent::Radio {
            seed: "a".into(),
            tracks: vec![track("b"), track("c")],
        });
        assert_eq!(app.queue.len(), 3);

        app.dispatch(Action::ClearQueue);
        assert_eq!(app.queue.len(), 1, "only the playing song stays");
        assert!(
            app.flash
                .as_ref()
                .is_some_and(|f| f.text == "Queue cleared (autoplay on)")
        );
        // Mid-song: nothing yet, the cleared songs don't come straight back.
        app.on_player(PlayerEvent::Duration(200.0));
        app.on_player(PlayerEvent::TimePos(60.0));
        assert!(rx.try_recv().is_err());
        // Near the end: a fresh radio is fetched (this used to never happen).
        app.on_player(PlayerEvent::TimePos(180.0));
        assert!(matches!(rx.try_recv(), Ok(YtmReq::Radio { seed }) if seed == "a"));
        app.on_ytm(YtmEvent::Radio {
            seed: "a".into(),
            tracks: vec![track("d"), track("e")],
        });
        assert_eq!(
            app.queue.get(1).unwrap().video_id,
            "d",
            "fresh songs follow the current one"
        );
        assert_eq!(app.queue.current, Some(0), "the playing song isn't interrupted");
    }

    #[test]
    fn r_on_the_playing_song_replaces_only_whats_next() {
        let (mut app, mut rx) = signed_in(&[]);
        app.queue
            .replace(vec![track("p"), track("a"), track("b"), track("c")], 1);
        app.play_index(1);
        while rx.try_recv().is_ok() {}
        app.set_tab(Tab::Queue);
        app.queue_state.select(None);
        app.dispatch(Action::Radio);
        assert!(matches!(rx.try_recv(), Ok(YtmReq::Radio { seed }) if seed == "a"));
        app.on_ytm(YtmEvent::Radio {
            seed: "a".into(),
            tracks: vec![track("a"), track("x"), track("y")],
        });
        let ids: Vec<&str> = app.queue.entries.iter().map(|e| e.track.video_id.as_str()).collect();
        assert_eq!(
            ids,
            ["p", "a", "x", "y"],
            "b and c replaced; a keeps playing and p stays as history"
        );
        assert_eq!(app.queue.current, Some(1));
    }

    #[test]
    fn dislike_on_last_song_plays_autoplay_songs_when_they_arrive() {
        let (mut app, mut rx) = signed_in(&[]);
        app.queue.replace(vec![track("a")], 0);
        app.play_index(0); // requests the autoplay radio for "a"
        while rx.try_recv().is_ok() {}
        app.dispatch(Action::Dislike);
        assert_eq!(app.queue.current, Some(0), "nothing to skip to yet");
        app.on_ytm(YtmEvent::Radio {
            seed: "a".into(),
            tracks: vec![track("a"), track("b")],
        });
        assert_eq!(app.queue.current, Some(1), "skips once autoplay songs arrive");
    }

    #[test]
    fn dislike_on_single_song_repeat_all_does_not_replay_it() {
        let (mut app, _rx) = signed_in(&[]);
        app.cfg.autoplay = false;
        app.queue.replace(vec![track("a")], 0);
        app.queue.repeat = Repeat::All;
        app.dispatch(Action::Dislike);
        assert!(app.flash.as_ref().is_some_and(|f| f.text.contains("end of queue")));
    }

    #[test]
    fn already_liked_song_is_unliked_not_liked_again() {
        let (mut app, mut rx) = signed_in(&["a"]);
        // Search results carry no like status (liked: None).
        app.queue.replace(vec![track("a"), track("b")], 0);
        assert!(is_liked(&app.likes, app.current().unwrap()));
        app.dispatch(Action::Like);
        assert!(matches!(rx.try_recv(), Ok(YtmReq::Rate { video_id, liked: false }) if video_id == "a"));
        app.on_ytm(YtmEvent::Rated {
            video_id: "a".into(),
            liked: false,
        });
        assert!(!is_liked(&app.likes, app.current().unwrap()));
        // Not in Liked songs -> `f` likes it.
        app.queue.current = Some(1);
        app.dispatch(Action::Like);
        assert!(matches!(rx.try_recv(), Ok(YtmReq::Rate { video_id, liked: true }) if video_id == "b"));
    }

    #[test]
    fn like_waits_until_status_known() {
        let (mut app, mut rx) = app();
        app.on_ytm(YtmEvent::Auth {
            logged_in: true,
            message: None,
        });
        let token = match rx.try_recv() {
            Ok(YtmReq::Load { token, .. }) => token,
            other => panic!("{other:?}"),
        };
        app.queue.replace(vec![track("a")], 0);
        app.dispatch(Action::Like);
        assert!(rx.try_recv().is_err(), "must not guess while likes are loading");
        // A failed load must not block liking forever.
        app.on_ytm(YtmEvent::Failed {
            token: Some(token),
            message: "boom".into(),
        });
        assert!(app.likes_loaded);
        app.dispatch(Action::Like);
        assert!(matches!(rx.try_recv(), Ok(YtmReq::Rate { liked: true, .. })));
    }

    #[test]
    fn lists_with_like_status_teach_the_cache() {
        let (mut app, _rx) = signed_in(&[]);
        let mut t = track("x");
        t.liked = Some(true);
        app.on_ytm(YtmEvent::Loaded {
            token: 999,
            title: String::new(),
            items: vec![Item::Track(t)],
        });
        assert_eq!(app.likes.get("x"), Some(&true));
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

    struct Harness {
        app: App,
        player: UnboundedReceiver<PlayerCmd>,
        resolve: UnboundedReceiver<ResolveReq>,
    }

    fn harness() -> Harness {
        let (p, prx) = unbounded_channel();
        let (r, rrx) = unbounded_channel();
        let paths = test_paths();
        let (keymap, _) = Keymap::new(&Default::default());
        let channels = Channels {
            player: p,
            ytm: unbounded_channel().0,
            lyrics: unbounded_channel().0,
            art: unbounded_channel().0,
            resolve: r,
            notify: unbounded_channel().0,
            discord: unbounded_channel().0,
            offline: unbounded_channel().0,
        };
        let app = App::new(Config::default(), paths, State::default(), keymap, channels, None, None);
        Harness {
            app,
            player: prx,
            resolve: rrx,
        }
    }

    fn track(id: &str) -> Track {
        Track {
            video_id: id.into(),
            title: id.into(),
            artists: String::new(),
            artist_id: None,
            album: None,
            album_id: None,
            duration: None,
            thumbnail: None,
            liked: None,
        }
    }

    fn drain<T>(rx: &mut UnboundedReceiver<T>) -> Vec<T> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    fn direct(id: &str) -> String {
        format!("https://rr1.googlevideo.com/videoplayback?id={id}&expire=4102444800")
    }

    fn resolved(h: &mut Harness, id: &str) {
        let url = direct(id);
        let until = crate::resolve::expiry(&url, SystemTime::now());
        h.app.on_resolved(Resolved {
            video_id: id.into(),
            stream: Some((url, until)),
        });
    }

    #[test]
    fn playing_prefetches_next_two() {
        let mut h = harness();
        h.app
            .queue
            .replace(vec![track("a"), track("b"), track("c"), track("d")], 0);
        h.app.play_index(0);
        let ids: Vec<String> = drain(&mut h.resolve).into_iter().map(|r| r.video_id).collect();
        assert_eq!(ids, ["b", "c"]);
    }

    #[test]
    fn resolved_urls_are_used_for_play_and_next() {
        let mut h = harness();
        h.app.queue.replace(vec![track("a"), track("b")], 0);
        resolved(&mut h, "a");
        resolved(&mut h, "b");
        h.app.play_index(0);
        let cmds = drain(&mut h.player);
        assert!(
            cmds.iter().any(
                |c| matches!(c, PlayerCmd::Load { url, next: Some(n), .. } if *url == direct("a") && *n == direct("b"))
            ),
            "{cmds:?}"
        );
        // mpv reports the direct URL as its path; gapless advance must still map to "b".
        h.app.on_player(PlayerEvent::Path(Some(direct("a"))));
        assert_eq!(h.app.queue.current, Some(0));
        h.app.on_player(PlayerEvent::Path(Some(direct("b"))));
        assert_eq!(h.app.queue.current, Some(1));
    }

    #[test]
    fn late_resolution_upgrades_preloaded_next() {
        let mut h = harness();
        h.app.queue.replace(vec![track("a"), track("b")], 0);
        h.app.play_index(0);
        // mpv starts playing "a" (via the yt-dlp hook) and leaves idle.
        h.app.on_player(PlayerEvent::Idle(false));
        h.app.on_player(PlayerEvent::Path(Some(track("a").url())));
        drain(&mut h.player);
        resolved(&mut h, "b");
        let cmds = drain(&mut h.player);
        assert!(
            cmds.iter()
                .any(|c| matches!(c, PlayerCmd::SetNext(Some(u)) if *u == direct("b"))),
            "{cmds:?}"
        );
    }

    #[test]
    fn failed_direct_url_retries_once_via_ytdlp() {
        let mut h = harness();
        h.app.queue.replace(vec![track("a"), track("b")], 0);
        resolved(&mut h, "a");
        h.app.play_index(0);
        drain(&mut h.player);
        h.app.on_player(PlayerEvent::EndFile {
            reason: "error".into(),
            error: Some("403".into()),
        });
        let cmds = drain(&mut h.player);
        assert!(
            cmds.iter()
                .any(|c| matches!(c, PlayerCmd::Load { url, .. } if *url == track("a").url())),
            "{cmds:?}"
        );
        assert_eq!(h.app.queue.current, Some(0));
    }

    #[test]
    fn expired_streams_are_not_used() {
        let mut h = harness();
        h.app.queue.replace(vec![track("a")], 0);
        h.app.on_resolved(Resolved {
            video_id: "a".into(),
            stream: Some((direct("a"), SystemTime::now() - Duration::from_secs(1))),
        });
        h.app.play_index(0);
        let cmds = drain(&mut h.player);
        assert!(
            cmds.iter()
                .any(|c| matches!(c, PlayerCmd::Load { url, .. } if *url == track("a").url()))
        );
    }

    #[test]
    fn dwelling_on_a_queue_row_prefetches_it() {
        let mut h = harness();
        h.app
            .queue
            .replace(vec![track("a"), track("b"), track("c"), track("d"), track("e")], 0);
        h.app.tab = Tab::Queue;
        h.app.queue_state.select(Some(4));
        h.app.tick();
        assert!(drain(&mut h.resolve).is_empty(), "no prefetch before the dwell");
        h.app.dwell = Some(("e".into(), Instant::now() - Duration::from_secs(1)));
        h.app.tick();
        let ids: Vec<String> = drain(&mut h.resolve).into_iter().map(|r| r.video_id).collect();
        assert_eq!(ids, ["e"]);
        h.app.tick();
        assert!(drain(&mut h.resolve).is_empty(), "only once per selection");
    }

    #[test]
    fn menu_settings_toggle_in_place_and_persist() {
        let mut h = harness();
        h.app.dispatch(Action::Menu); // no list to go back in -> opens the menu
        assert!(matches!(h.app.overlay, Some(Overlay::Menu { .. })));
        let autoplay = MenuItem::ALL.iter().position(|i| *i == MenuItem::Autoplay).unwrap();
        if let Some(Overlay::Menu { state }) = h.app.overlay.as_mut() {
            state.select(Some(autoplay));
        }
        let before = h.app.cfg.autoplay;
        h.app
            .on_terminal(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert_eq!(h.app.cfg.autoplay, !before);
        assert!(
            matches!(h.app.overlay, Some(Overlay::Menu { .. })),
            "menu stays open for settings"
        );
        let saved = State::load(&h.app.paths.state_file());
        assert_eq!(saved.autoplay, Some(!before));
        // Themes entry opens the theme picker.
        if let Some(Overlay::Menu { state }) = h.app.overlay.as_mut() {
            state.select(Some(0));
        }
        h.app
            .on_terminal(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(matches!(h.app.overlay, Some(Overlay::Themes { .. })));
    }

    #[test]
    fn menu_toggles_loudness_live() {
        let mut h = harness();
        h.app.dispatch(Action::Menu);
        let at = MenuItem::ALL.iter().position(|i| *i == MenuItem::Loudness).unwrap();
        assert_eq!(MenuItem::ALL[at - 1], MenuItem::AlbumArt, "sits under album art");
        if let Some(Overlay::Menu { state }) = h.app.overlay.as_mut() {
            state.select(Some(at));
        }
        assert!(h.app.cfg.normalize_loudness);
        drain(&mut h.player);
        h.app
            .on_terminal(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(!h.app.cfg.normalize_loudness);
        assert!(matches!(h.app.overlay, Some(Overlay::Menu { .. })), "menu stays open");
        assert!(
            drain(&mut h.player)
                .iter()
                .any(|c| matches!(c, PlayerCmd::SetNormalize(false)))
        );
        assert_eq!(State::load(&h.app.paths.state_file()).normalize_loudness, Some(false));
    }

    #[test]
    fn song_change_notifies_once_but_not_on_paused_resume() {
        let (n, mut nrx) = unbounded_channel();
        let (keymap, _) = Keymap::new(&Default::default());
        let channels = Channels {
            player: unbounded_channel().0,
            ytm: unbounded_channel().0,
            lyrics: unbounded_channel().0,
            art: unbounded_channel().0,
            resolve: unbounded_channel().0,
            notify: n,
            discord: unbounded_channel().0,
            offline: unbounded_channel().0,
        };
        let cfg = Config {
            notifications: true,
            resume: false,
            ..Config::default()
        };
        let mut app = App::new(cfg, test_paths(), State::default(), keymap, channels, None, None);
        app.queue.replace(vec![track("a"), track("b"), track("c")], 0);
        app.load_index(1, Some(30.0), true);
        assert!(nrx.try_recv().is_err(), "resuming paused isn't a song change");
        app.play_index(0);
        let note = nrx.try_recv().expect("notification");
        assert_eq!(note.title, "a");
        assert!(note.art_url.is_some());
        app.on_track_changed();
        assert!(nrx.try_recv().is_err(), "same song, no second popup");
        app.cfg.notifications = false;
        app.play_index(2);
        assert!(nrx.try_recv().is_err());
    }

    #[test]
    fn unicode_case_folding_prefix_and_word_delete() {
        // Turkish dotted capital 'İ' (2 bytes) lowercases to "i\u{307}" (3 bytes).
        assert_eq!(case_insensitive_prefix_len("İstanbul", "i\u{307}st"), Some(4));
        assert_eq!(case_insensitive_prefix_len("Istanbul", "İ"), None);

        let mut h = harness();
        h.app.dispatch(Action::TabSearch);
        for c in "daft punk get lucky".chars() {
            h.app
                .on_terminal(Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        // Ctrl+W deletes the last word only.
        h.app
            .on_terminal(Event::Key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL)));
        assert_eq!(h.app.search.input, "daft punk get ");
        h.app
            .on_terminal(Event::Key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL)));
        assert_eq!(h.app.search.input, "daft punk ");
        // Ctrl+U clears before the cursor.
        h.app
            .on_terminal(Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)));
        assert_eq!(h.app.search.input, "");
    }

    #[test]
    fn error_skip_stops_instead_of_looping_forever() {
        let mut h = harness();
        h.app.queue.replace(vec![track("a")], 0);
        h.app.queue.repeat = Repeat::All;
        h.app.play_index(0);
        h.app.player.idle = true;
        drain(&mut h.player);
        // Repeat::All with 1 failing track must not re-load it forever.
        h.app.on_player(PlayerEvent::EndFile {
            reason: "error".into(),
            error: Some("403".into()),
        });
        assert!(drain(&mut h.player).is_empty());
    }

    #[test]
    fn removing_last_track_clears_stale_state() {
        let mut h = harness();
        h.app.queue.replace(vec![track("a")], 0);
        h.app.play_index(0);
        h.app.player.time = 42.0;
        h.app.player.duration = 200.0;
        h.app.art_key = Some("https://example.com/art.jpg".into());
        h.app.tab = Tab::Queue;
        h.app.queue_state.select(Some(0));
        drain(&mut h.player);

        h.app.dispatch(Action::Remove);
        assert!(h.app.queue.is_empty());
        assert!(matches!(h.app.lyrics, LyricsState::None));
        assert!(h.app.lyrics_requested.is_none());
        assert!(h.app.art_key.is_none());
        assert_eq!(h.app.player.time, 0.0);
        assert_eq!(h.app.player.duration, 0.0);
        assert!(drain(&mut h.player).iter().any(|c| matches!(c, PlayerCmd::Stop)));
    }

    #[test]
    fn menu_left_cycles_viz_backwards_and_click_reverts_theme_preview() {
        let mut h = harness();
        h.app.viz_mode = VizMode::Bars;
        h.app.dispatch(Action::Menu);
        let viz_idx = MenuItem::ALL.iter().position(|i| *i == MenuItem::VizMode).unwrap();
        if let Some(Overlay::Menu { state }) = h.app.overlay.as_mut() {
            state.select(Some(viz_idx));
        }
        h.app
            .on_terminal(Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)));
        assert_eq!(h.app.viz_mode, VizMode::Wave);

        // Theme preview dismissed by mouse click must revert to the original theme.
        let orig_name = h.app.theme.name.clone();
        h.app.overlay = Some(Overlay::Themes {
            names: vec![("Default".into(), None), ("Other".into(), None)],
            state: TableState::default(),
            original: h.app.theme.clone(),
        });
        h.app.theme.name = "Other".into();
        h.app.on_terminal(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(h.app.overlay.is_none());
        assert_eq!(h.app.theme.name, orig_name);
    }

    #[test]
    fn volume_click_and_scroll_and_header_click_ignored() {
        let mut h = harness();
        h.app.player.muted = true;
        h.app.hits.add(Rect::new(10, 5, 11, 1), Hit::Volume);
        // Click halfway along an 11-cell meter (x = 15 -> 5/10 = 50%).
        h.app.on_terminal(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 15,
            row: 5,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(h.app.player.volume, 50.0);
        let cmds = drain(&mut h.player);
        assert!(cmds.iter().any(|c| matches!(c, PlayerCmd::SetVolume(v) if *v == 50.0)));
        assert!(cmds.iter().any(|c| matches!(c, PlayerCmd::SetMute(false))));

        // Scroll up on volume meter increases volume by 5.
        h.app.on_terminal(Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 15,
            row: 5,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(h.app.player.volume, 55.0);

        // Clicking a Header item in Search must not select it.
        let mut lv = ListView::new(View::Playlist("PL1".into()), 1);
        lv.loading = false;
        lv.items = vec![
            Item::Header("Section".into()),
            Item::Track(track("a")),
            Item::Track(track("a")),
        ];
        lv.state.select(Some(2));
        h.app.tab = Tab::Search;
        h.app.search.stack.push(lv);
        h.app.hits.list = Some((Rect::new(0, 0, 40, 10), 0));
        h.app.hits.add(Rect::new(0, 0, 40, 10), Hit::List);
        h.app.on_terminal(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(h.app.search.stack[0].state.selected(), Some(2));

        // Playing the second duplicate track in a playlist must start at index 1, not 0.
        h.app.dispatch(Action::Select);
        assert_eq!(h.app.queue.current, Some(1));
    }

    #[test]
    fn missing_mpv_keeps_startup_error_and_does_not_spin_loading() {
        let mut h = harness();
        h.app
            .on_player(PlayerEvent::Error("failed to start mpv (is it installed?)".into()));
        h.app.on_player(PlayerEvent::Exited);
        assert!(
            h.app
                .flash
                .as_ref()
                .is_some_and(|f| f.text.contains("failed to start mpv"))
        );
        h.app.queue.replace(vec![track("a")], 0);
        h.app.play_index(0);
        assert!(h.app.load_started.is_none());
        assert!(
            h.app
                .flash
                .as_ref()
                .is_some_and(|f| f.text.contains("failed to start mpv"))
        );
    }

    #[test]
    fn remote_pause_on_idle_queue_does_not_start_playback() {
        let mut h = harness();
        h.app.queue.replace(vec![track("a")], 0);
        h.app.player.idle = true;
        h.app.player.paused = false;
        assert_eq!(h.app.on_control(crate::control::Command::Pause), "ok");
        assert!(drain(&mut h.player).is_empty(), "pause while idle must not load");
        assert_eq!(h.app.on_control(crate::control::Command::Play), "ok");
        assert!(
            drain(&mut h.player).iter().any(|c| matches!(c, PlayerCmd::Load { .. })),
            "play while idle should load the current track"
        );
    }

    #[test]
    fn status_line_avoids_placeholder_injection_and_reports_idle_paused() {
        let mut h = harness();
        let mut t = track("a");
        t.title = "Song {state} {volume}".into();
        t.artists = "Band {title}".into();
        h.app.queue.replace(vec![t], 0);
        h.app.player.idle = true;
        h.app.player.paused = false;
        let line = h.app.status_line("{state}: {artist} - {title}");
        assert_eq!(line, "paused: Band {title} - Song {state} {volume}");
    }

    #[test]
    fn clicking_error_box_closes_it_before_overlay_underneath() {
        let mut h = harness();
        h.app.dispatch(Action::Menu);
        h.app.flash("something broke", true);
        h.app.hits.add(Rect::new(2, 10, 30, 4), Hit::CloseError);
        h.app.on_terminal(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 11,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(h.app.flash.is_none(), "error box should close first");
        assert!(
            matches!(h.app.overlay, Some(Overlay::Menu { .. })),
            "overlay underneath should remain open"
        );
    }

    #[test]
    fn sign_out_clears_stale_likes() {
        let mut h = harness();
        h.app.logged_in = true;
        h.app.likes.insert("vid1".into(), true);
        h.app.likes_loaded = true;
        h.app.on_ytm(YtmEvent::Auth {
            logged_in: false,
            message: None,
        });
        assert!(h.app.likes.is_empty());
        assert!(!h.app.likes_loaded);
    }

    #[test]
    fn playlist_remove_drops_one_duplicate_and_delete_ignores_vl_prefix() {
        let mut h = harness();
        let mut lv = ListView::new(View::Playlist("VLPL1".into()), 1);
        lv.loading = false;
        lv.items = vec![Item::Track(track("dup")), Item::Track(track("dup"))];
        lv.state.select(Some(0));
        let mut root = ListView::new(View::LibraryPlaylists, 2);
        root.loading = false;
        root.items = vec![Item::Playlist(crate::model::PlaylistRef {
            id: "PL1".into(),
            title: "Mix".into(),
            subtitle: String::new(),
        })];
        h.app.library.stack = vec![root, lv];

        // Removing one occurrence of a duplicated track must leave the other.
        h.app.on_ytm(YtmEvent::PlaylistEdited {
            message: "Removed".into(),
            removed: Some(("PL1".into(), "dup".into())),
        });
        assert_eq!(h.app.library.stack[1].items.len(), 1);

        // Deleting with VL prefix must pop and remove even when list stored bare id.
        h.app.on_ytm(YtmEvent::PlaylistDeleted {
            playlist_id: "VLPL1".into(),
            title: "Mix".into(),
        });
        assert_eq!(h.app.library.stack.len(), 1);
        assert!(h.app.library.stack[0].items.is_empty());
    }

    #[test]
    fn add_to_playlist_highlights_first_row_and_accepts_paste() {
        let mut h = harness();
        h.app.logged_in = true;
        h.app.queue.replace(vec![track("a")], 0);
        h.app.dispatch(Action::AddToPlaylist);
        if let Some(Overlay::AddToPlaylist { state, naming, .. }) = h.app.overlay.as_mut() {
            assert_eq!(state.selected(), Some(0));
            *naming = Some(String::new());
        } else {
            panic!("expected AddToPlaylist overlay");
        }
        h.app.on_terminal(Event::Paste("Road\nTrip".into()));
        if let Some(Overlay::AddToPlaylist { naming, .. }) = &h.app.overlay {
            assert_eq!(naming.as_deref(), Some("RoadTrip"));
        }
    }
}

#[cfg(test)]
mod playlist_tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

    fn app() -> (App, UnboundedReceiver<YtmReq>) {
        let (y, yrx) = unbounded_channel();
        let paths = test_paths();
        let (keymap, _) = Keymap::new(&Default::default());
        let channels = Channels {
            player: unbounded_channel().0,
            ytm: y,
            lyrics: unbounded_channel().0,
            art: unbounded_channel().0,
            resolve: unbounded_channel().0,
            notify: unbounded_channel().0,
            discord: unbounded_channel().0,
            offline: unbounded_channel().0,
        };
        let cfg = Config {
            resume: false,
            ..Config::default()
        };
        let app = App::new(cfg, paths, State::default(), keymap, channels, None, None);
        (app, yrx)
    }

    fn track(id: &str) -> Track {
        Track {
            video_id: id.into(),
            title: format!("Song {id}"),
            artists: "Band".into(),
            artist_id: None,
            album: None,
            album_id: None,
            duration: Some(200.0),
            thumbnail: None,
            liked: None,
        }
    }

    fn key(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn screen(app: &mut App) -> String {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        term.draw(|f| {
            crate::ui::draw(f, app);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        buf.content().iter().map(|c| c.symbol()).collect()
    }

    fn reqs(rx: &mut UnboundedReceiver<YtmReq>) -> Vec<YtmReq> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    /// Open the picker on the playing track and answer its playlist load.
    fn open_picker(app: &mut App, rx: &mut UnboundedReceiver<YtmReq>) {
        app.logged_in = true;
        app.queue.replace(vec![track("a")], 0);
        app.tab = Tab::NowPlaying;
        key(app, KeyCode::Char('p'));
        let token = reqs(rx)
            .into_iter()
            .find_map(|r| match r {
                YtmReq::Load {
                    token,
                    view: View::LibraryPlaylists,
                } => Some(token),
                _ => None,
            })
            .expect("picker asks for the user's playlists");
        let pl = |id: &str, title: &str| {
            Item::Playlist(PlaylistRef {
                id: id.into(),
                title: title.into(),
                subtitle: "3 songs".into(),
            })
        };
        app.on_ytm(YtmEvent::Loaded {
            token,
            title: String::new(),
            items: vec![pl("LM", "Liked Music"), pl("PL1", "Road trip"), pl("PL2", "Gym")],
        });
    }

    #[test]
    fn picker_lists_own_playlists_and_adds() {
        let (mut app, mut rx) = app();
        open_picker(&mut app, &mut rx);
        let shown = screen(&mut app);
        assert!(shown.contains("add to playlist"));
        assert!(shown.contains("Road trip") && shown.contains("Gym"));
        assert!(!shown.contains("Liked Music"), "Liked Music is not a real playlist");
        key(&mut app, KeyCode::Down); // past "+ New playlist"
        key(&mut app, KeyCode::Enter);
        assert!(app.overlay.is_none());
        let sent = reqs(&mut rx);
        assert!(
            sent.iter()
                .any(|r| matches!(r, YtmReq::AddToPlaylist { playlist_id, video_id, .. }
                if playlist_id == "PL1" && video_id == "a")),
            "{sent:?}"
        );
    }

    #[test]
    fn picker_creates_a_new_playlist() {
        let (mut app, mut rx) = app();
        open_picker(&mut app, &mut rx);
        key(&mut app, KeyCode::Enter); // "+ New playlist"
        for c in "Night drive".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        assert!(screen(&mut app).contains("Night drive"));
        key(&mut app, KeyCode::Enter);
        let sent = reqs(&mut rx);
        assert!(
            sent.iter()
                .any(|r| matches!(r, YtmReq::CreatePlaylist { name, video_id, .. }
                if name == "Night drive" && video_id == "a")),
            "{sent:?}"
        );
    }

    #[test]
    fn signed_out_picker_explains_instead() {
        let (mut app, mut rx) = app();
        app.queue.replace(vec![track("a")], 0);
        key(&mut app, KeyCode::Char('p'));
        assert!(app.overlay.is_none());
        assert!(
            app.flash
                .as_ref()
                .is_some_and(|f| f.error && f.text.contains("pova login"))
        );
        assert!(reqs(&mut rx).is_empty());
    }

    #[test]
    fn library_sign_in_shows_link_and_profiles_and_loads_on_auth() {
        let (mut app, mut rx) = app();
        // Browser profiles are detected lazily, not at App::new().
        assert!(!app.library.auth.profiles_loaded);
        assert!(app.library.auth.profiles.is_empty());

        app.library.auth.profiles = vec![
            crate::auth::BrowserProfile {
                spec: "brave:Default".into(),
                label: "Brave — Personal".into(),
                default: true,
            },
            crate::auth::BrowserProfile {
                spec: "brave:Profile 2".into(),
                label: "Brave — Music".into(),
                default: false,
            },
        ];
        app.library.auth.profiles_loaded = true;
        app.library.auth.selected = 0;

        // Opening Library while signed out shows the link + detected profiles immediately.
        key(&mut app, KeyCode::Char('b'));
        assert_eq!(app.tab, Tab::Library);
        assert!(reqs(&mut rx).is_empty());
        let shown = screen(&mut app);
        assert!(shown.contains("https://music.youtube.com"), "{shown}");
        assert!(shown.contains("Brave — Personal"), "{shown}");
        assert!(shown.contains("Brave — Music"), "{shown}");

        // Move to the second Brave profile and press Enter.
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.library.auth.busy.as_deref(), Some("signing in with Brave — Music"));
        assert!(app.animating());
        let sent = reqs(&mut rx);
        assert!(
            sent.iter()
                .any(|r| matches!(r, YtmReq::ImportBrowser { spec, .. } if spec == "brave:Profile 2")),
            "{sent:?}"
        );

        // When auth succeeds, spinner clears and Library loads immediately.
        app.on_ytm(YtmEvent::Auth {
            logged_in: true,
            message: None,
        });
        assert!(app.library.auth.busy.is_none());
        assert!(!app.library.stack.is_empty());
        let sent = reqs(&mut rx);
        assert!(
            sent.iter().any(|r| matches!(
                r,
                YtmReq::Load {
                    view: View::LibraryPlaylists,
                    ..
                }
            )),
            "{sent:?}"
        );
    }

    #[test]
    fn library_sign_in_paste_cookie_is_masked_and_submits() {
        let (mut app, mut rx) = app();
        app.tab = Tab::Library;

        // Direct paste of a Cookie header switches straight to the masked paste box.
        app.on_terminal(Event::Paste("SAPISID=super_secret_token; SID=abc".into()));
        let shown = screen(&mut app);
        assert!(shown.contains("Paste YouTube Music cookie"), "{shown}");
        assert!(!shown.contains("super_secret_token"), "cookie must never be echoed");
        assert!(shown.contains("•"), "cookie should be masked");

        key(&mut app, KeyCode::Enter);
        let sent = reqs(&mut rx);
        assert!(
            sent.iter()
                .any(|r| matches!(r, YtmReq::SaveCookie { raw } if raw.contains("SAPISID=super_secret_token"))),
            "{sent:?}"
        );
    }

    #[test]
    fn library_sign_in_custom_browser_profile_submits_and_esc_returns() {
        let (mut app, mut rx) = app();
        app.tab = Tab::Library;
        key(&mut app, KeyCode::Char('p'));
        assert!(matches!(app.library.auth.input, Some(AuthInput::CustomBrowser(_))));
        key(&mut app, KeyCode::Esc);
        assert!(app.library.auth.input.is_none());

        key(&mut app, KeyCode::Char('p'));
        app.on_terminal(Event::Paste("chrome:Profile 2".into()));
        key(&mut app, KeyCode::Enter);
        let sent = reqs(&mut rx);
        assert!(
            sent.iter()
                .any(|r| matches!(r, YtmReq::ImportBrowser { spec, .. } if spec == "chrome:Profile 2")),
            "{sent:?}"
        );
    }

    #[test]
    fn library_sign_in_waits_for_profile_login_and_polls() {
        let (mut app, mut rx) = app();
        app.library.auth.profiles = vec![crate::auth::BrowserProfile {
            spec: "brave:Profile 2".into(),
            label: "Brave — Music".into(),
            default: true,
        }];
        app.library.auth.profiles_loaded = true;
        app.tab = Tab::Library;

        // Selecting a profile that isn't signed in yet opens the link & waits for that profile.
        key(&mut app, KeyCode::Enter);
        let _ = reqs(&mut rx);
        app.on_ytm(YtmEvent::AuthNeedsBrowser {
            spec: "brave:Profile 2".into(),
            label: "Brave — Music".into(),
            browser_opened: true,
        });
        assert!(app.library.auth.busy.is_none());
        let w = app.library.auth.waiting.as_ref().expect("waiting state");
        assert!(w.next_poll >= w.started + Duration::from_secs(9));
        assert!(app.animating());
        let shown = screen(&mut app);
        assert!(shown.contains("https://music.youtube.com"), "{shown}");
        assert!(shown.contains("Brave — Music"), "{shown}");
        assert!(shown.contains("Check now"), "{shown}");

        // Pressing Enter checks immediately; pending reply keeps waiting cleanly.
        key(&mut app, KeyCode::Enter);
        let sent = reqs(&mut rx);
        assert!(
            sent.iter()
                .any(|r| matches!(r, YtmReq::PollSignIn { spec, .. } if spec == "brave:Profile 2")),
            "{sent:?}"
        );
        app.on_ytm(YtmEvent::AuthPollPending);
        assert!(app.library.auth.waiting.is_some());
        assert!(app.flash.is_none(), "pending poll must not flash an error");

        // After 3 minutes, automatic polling stops, while "Check now" still works.
        if let Some(w) = app.library.auth.waiting.as_mut() {
            w.started = Instant::now() - AUTH_POLL_TIMEOUT - Duration::from_secs(1);
            w.next_poll = Instant::now() - Duration::from_secs(1);
        }
        assert!(!app.animating());
        app.tick();
        assert!(reqs(&mut rx).is_empty(), "must not auto-poll past 3 minutes");

        // Pressing 'o' reopens the link; Esc cancels waiting cleanly.
        key(&mut app, KeyCode::Char('o'));
        assert!(reqs(&mut rx).iter().any(|r| matches!(r, YtmReq::OpenSignInLink { .. })));
        key(&mut app, KeyCode::Esc);
        assert!(app.library.auth.waiting.is_none());
    }

    #[test]
    fn tab_cycling_does_not_trap_on_search() {
        let (mut app, _rx) = app();
        assert_eq!(app.tab, Tab::NowPlaying);
        key(&mut app, KeyCode::Tab);
        assert_eq!(app.tab, Tab::Queue);
        key(&mut app, KeyCode::Tab);
        assert_eq!(app.tab, Tab::Lyrics);
        key(&mut app, KeyCode::Tab);
        assert_eq!(app.tab, Tab::Search);
        assert!(
            !app.search.editing,
            "tabbing onto Search must not trap cursor in editing"
        );
        // Next Tab immediately advances to Library instead of hijacking to cycle filters.
        key(&mut app, KeyCode::Tab);
        assert_eq!(app.tab, Tab::Library);

        // Even if the user enters Search editing with '/' and hasn't typed anything yet,
        // Tab still exits editing and moves to Library smoothly.
        key(&mut app, KeyCode::Char('/'));
        assert_eq!(app.tab, Tab::Search);
        assert!(app.search.editing);
        key(&mut app, KeyCode::Tab);
        assert_eq!(app.tab, Tab::Library);
        assert!(!app.search.editing);
    }

    #[test]
    fn sign_out_from_account_action_asks_first() {
        let (mut app, mut rx) = app();
        app.logged_in = true;
        app.open_account_action();
        let shown = screen(&mut app);
        assert!(shown.contains("Yes, sign out"), "{shown}");
        key(&mut app, KeyCode::Char('y'));
        let sent = reqs(&mut rx);
        assert!(sent.iter().any(|r| matches!(r, YtmReq::SignOut)), "{sent:?}");
    }

    #[test]
    fn removing_from_own_playlist_updates_the_list() {
        let (mut app, mut rx) = app();
        app.logged_in = true;
        app.tab = Tab::Library;
        let mut lv = ListView::new(View::Playlist("PL1".into()), 7);
        lv.items = vec![Item::Track(track("a")), Item::Track(track("b"))];
        lv.state.select(Some(1));
        app.library.stack.push(lv);
        key(&mut app, KeyCode::Char('d'));
        let sent = reqs(&mut rx);
        assert!(
            sent.iter()
                .any(|r| matches!(r, YtmReq::RemoveFromPlaylist { playlist_id, video_id, .. }
                if playlist_id == "PL1" && video_id == "b")),
            "{sent:?}"
        );
        app.on_ytm(YtmEvent::PlaylistEdited {
            message: "Removed".into(),
            removed: Some(("PL1".into(), "b".into())),
        });
        let lv = app.library.stack.last().unwrap();
        assert_eq!(lv.items.len(), 1);
        assert_eq!(lv.state.selected(), Some(0));
    }

    fn library_with_playlists(app: &mut App) {
        app.logged_in = true;
        app.tab = Tab::Library;
        let mut lv = ListView::new(View::LibraryPlaylists, 3);
        lv.items = vec![
            Item::Playlist(PlaylistRef {
                id: "PL1".into(),
                title: "Road trip".into(),
                subtitle: String::new(),
            }),
            Item::Playlist(PlaylistRef {
                id: "PL2".into(),
                title: "Gym".into(),
                subtitle: String::new(),
            }),
        ];
        lv.state.select(Some(1));
        app.library.stack = vec![lv];
    }

    fn deletes(rx: &mut UnboundedReceiver<YtmReq>) -> Vec<String> {
        reqs(rx)
            .into_iter()
            .filter_map(|r| match r {
                YtmReq::DeletePlaylist { playlist_id, .. } => Some(playlist_id),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn delete_asks_first_and_defaults_to_no() {
        let (mut app, mut rx) = app();
        library_with_playlists(&mut app);
        key(&mut app, KeyCode::Char('D'));
        let shown = screen(&mut app);
        assert!(shown.contains("Delete the playlist “Gym”"), "{shown}");
        assert!(shown.contains("Yes, delete"));
        key(&mut app, KeyCode::Enter); // highlighted answer is No
        assert!(app.overlay.is_none());
        assert!(deletes(&mut rx).is_empty(), "Enter on the default must not delete");
        key(&mut app, KeyCode::Char('D'));
        key(&mut app, KeyCode::Esc);
        assert!(deletes(&mut rx).is_empty());
    }

    #[test]
    fn delete_confirmed_with_y_or_yes_button() {
        let (mut app, mut rx) = app();
        library_with_playlists(&mut app);
        key(&mut app, KeyCode::Char('D'));
        key(&mut app, KeyCode::Char('y'));
        assert_eq!(deletes(&mut rx), vec!["PL2".to_string()]);
        key(&mut app, KeyCode::Char('D'));
        key(&mut app, KeyCode::Left); // move to "Yes"
        key(&mut app, KeyCode::Enter);
        assert_eq!(deletes(&mut rx), vec!["PL2".to_string()]);
    }

    #[test]
    fn deleted_playlist_leaves_the_lists_and_closes() {
        let (mut app, _rx) = app();
        library_with_playlists(&mut app);
        app.library.stack.push(ListView::new(View::Playlist("PL2".into()), 9));
        app.on_ytm(YtmEvent::PlaylistDeleted {
            playlist_id: "PL2".into(),
            title: "Gym".into(),
        });
        assert_eq!(app.library.stack.len(), 1, "the open playlist view closes");
        let lv = &app.library.stack[0];
        assert_eq!(lv.items.len(), 1);
        assert_eq!(lv.state.selected(), Some(0));
    }

    #[test]
    fn liked_songs_cannot_be_deleted() {
        let (mut app, mut rx) = app();
        library_with_playlists(&mut app);
        app.library.stack.push(ListView::new(View::Playlist("LM".into()), 9));
        key(&mut app, KeyCode::Char('D'));
        assert!(app.overlay.is_none());
        assert!(deletes(&mut rx).is_empty());
    }
}

#[cfg(test)]
mod pro_features_tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

    struct Rig {
        app: App,
        player: UnboundedReceiver<PlayerCmd>,
        ytm: UnboundedReceiver<YtmReq>,
    }

    fn rig() -> Rig {
        let (p, prx) = unbounded_channel();
        let (y, yrx) = unbounded_channel();
        let paths = test_paths();
        let (keymap, _) = Keymap::new(&Default::default());
        let channels = Channels {
            player: p,
            ytm: y,
            lyrics: unbounded_channel().0,
            art: unbounded_channel().0,
            resolve: unbounded_channel().0,
            notify: unbounded_channel().0,
            discord: unbounded_channel().0,
            offline: unbounded_channel().0,
        };
        let cfg = Config {
            resume: false,
            ..Config::default()
        };
        let app = App::new(cfg, paths, State::default(), keymap, channels, None, None);
        Rig {
            app,
            player: prx,
            ytm: yrx,
        }
    }

    fn track(id: &str) -> Track {
        Track {
            video_id: id.into(),
            title: format!("Track {id}"),
            artists: "Seether".into(),
            artist_id: Some("UCseether".into()),
            album: Some("Disclaimer".into()),
            album_id: Some("MPREb_disc".into()),
            duration: Some(200.0),
            thumbnail: None,
            liked: None,
        }
    }

    fn key(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn drain<T>(rx: &mut UnboundedReceiver<T>) -> Vec<T> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    fn screen(app: &mut App) -> String {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 36)).unwrap();
        term.draw(|f| {
            crate::ui::draw(f, app);
        })
        .unwrap();
        term.backend().buffer().content().iter().map(|c| c.symbol()).collect()
    }

    #[test]
    fn sleep_timer_end_of_track_stops_without_advancing_and_cli_works() {
        let mut r = rig();
        r.app.queue.replace(vec![track("a"), track("b")], 0);
        r.app.play_index(0);
        r.app.player.idle = false;
        r.app.load_started = None;
        drain(&mut r.player);

        // Open Sleep Timer overlay with 'z' and select "End of current track" (row 0).
        key(&mut r.app, KeyCode::Char('z'));
        assert!(matches!(r.app.overlay, Some(Overlay::SleepTimer { .. })));
        let shown = screen(&mut r.app);
        assert!(shown.contains("sleep timer") && shown.contains("End of current track"));
        if let Some(Overlay::SleepTimer { state, .. }) = r.app.overlay.as_mut() {
            state.select(Some(0));
        }
        key(&mut r.app, KeyCode::Enter);
        assert!(r.app.sleep.is_end_of_track());
        assert_eq!(r.app.status_line("{sleep}"), "end of track");
        // mpv must clear preloaded next track so gapless playback doesn't advance.
        assert!(
            drain(&mut r.player)
                .iter()
                .any(|c| matches!(c, PlayerCmd::SetNext(None)))
        );

        // When mpv goes idle at track EOF, pova stops cleanly on track 0 instead of advancing to track 1.
        r.app.on_player(PlayerEvent::Idle(true));
        assert_eq!(r.app.queue.current, Some(0));
        assert!(r.app.player.paused);
        assert!(!r.app.sleep.is_active());

        // Remote CLI: set 15 min, adjust +5, status, off.
        assert!(r.app.on_control(crate::control::Command::Sleep("15".into())).starts_with("ok"));
        assert!(r.app.sleep.is_active());
        assert!(r.app.on_control(crate::control::Command::Sleep("+5".into())).starts_with("ok"));
        assert!(r.app.on_control(crate::control::Command::Sleep("off".into())).starts_with("ok"));
        assert!(!r.app.sleep.is_active());
    }

    #[test]
    fn equalizer_overlay_updates_mpv_filter_and_persists() {
        let mut r = rig();
        key(&mut r.app, KeyCode::Char('e'));
        assert!(matches!(r.app.overlay, Some(Overlay::Equalizer { band: 0 })));
        let shown = screen(&mut r.app);
        assert!(shown.contains("10-band equalizer") && shown.contains("Flat"));

        // Press 'p' to cycle to Bass Boost -> sends labeled @povaeq filter to mpv.
        key(&mut r.app, KeyCode::Char('p'));
        assert_eq!(r.app.cfg.eq.preset, EqPreset::BassBoost);
        assert!(r.app.cfg.eq.enabled);
        let cmds = drain(&mut r.player);
        assert!(cmds.iter().any(|c| matches!(c, PlayerCmd::SetEq(Some(f)) if f.starts_with("@povaeq:"))));
        assert_eq!(State::load(&r.app.paths.state_file()).eq.unwrap().preset, EqPreset::BassBoost);

        // Remote CLI `pova eq off` clears the filter.
        assert_eq!(r.app.on_control(crate::control::Command::Eq("off".into())), "ok eq off");
        assert!(drain(&mut r.player).iter().any(|c| matches!(c, PlayerCmd::SetEq(None))));
    }

    #[test]
    fn interlink_overlay_and_external_spotify_url_work() {
        let mut r = rig();
        r.app.queue.replace(vec![track("vid1")], 0);
        r.app.on_player(PlayerEvent::AudioCodec(Some("opus".into())));
        r.app.on_player(PlayerEvent::SampleRate(Some(48000)));
        r.app.on_player(PlayerEvent::AudioBitrate(Some(158_000)));
        assert_eq!(r.app.status_line("{quality}"), "MAX · OPUS 48kHz · 158k");

        // Open Interlink & Signal Path overlay with 'i'.
        key(&mut r.app, KeyCode::Char('i'));
        let shown = screen(&mut r.app);
        assert!(shown.contains("interlink & signal path"), "{shown}");
        assert!(shown.contains("OPUS"), "{shown}");
        assert!(shown.contains("Go to Artist") && shown.contains("Go to Album"));
        assert!(shown.contains("Copy Universal Link"));

        // Enter on row 0 ("Go to Artist") opens View::Artist("UCseether") directly.
        key(&mut r.app, KeyCode::Enter);
        assert!(r.app.overlay.is_none());
        assert_eq!(r.app.tab, Tab::Search);
        assert!(
            drain(&mut r.ytm)
                .iter()
                .any(|req| matches!(req, YtmReq::Load { view: View::Artist(id), .. } if id == "UCseether"))
        );

        // Remote `pova open` with a Spotify URL sends YtmReq::ResolveLink.
        let reply = r.app.on_control(crate::control::Command::Open(
            "https://open.spotify.com/track/4cOdK2wGLETKBW3PvgPWqT".into(),
        ));
        assert!(reply.starts_with("ok"));
        assert!(
            drain(&mut r.ytm)
                .iter()
                .any(|req| matches!(req, YtmReq::ResolveLink { .. }))
        );
    }

    #[test]
    fn ab_loop_speed_quality_and_interactive_synced_lyrics_seek() {
        let mut r = rig();
        r.app.queue.replace(vec![track("a"), track("b"), track("b")], 0);
        r.app.play_index(0);
        r.app.player.time = 20.0;
        r.app.player.duration = 200.0;
        drain(&mut r.player);

        // Set point A at 20s, point B at 40s, then passing 40s seeks back to 20s.
        key(&mut r.app, KeyCode::Char('B'));
        r.app.player.time = 40.0;
        key(&mut r.app, KeyCode::Char('B'));
        assert_eq!(r.app.ab_loop, AbLoop::Active { a: 20.0, b: 40.0 });
        r.app.on_player(PlayerEvent::TimePos(40.5));
        assert!(
            drain(&mut r.player)
                .iter()
                .any(|c| matches!(c, PlayerCmd::SeekAbsolute(t) if (*t - 20.0).abs() < 0.01))
        );

        // Playback speed stepping via ')' and remote command.
        key(&mut r.app, KeyCode::Char(')'));
        assert_eq!(r.app.cfg.playback_speed, 1.15);
        assert!(
            drain(&mut r.player)
                .iter()
                .any(|c| matches!(c, PlayerCmd::SetSpeed(s) if (*s - 1.15).abs() < 0.01))
        );

        // Quality tier switching via remote command sends SetYtdlFormat.
        assert_eq!(
            r.app.on_control(crate::control::Command::Quality("low".into())),
            "ok quality low (saver)"
        );
        assert!(
            drain(&mut r.player)
                .iter()
                .any(|c| matches!(c, PlayerCmd::SetYtdlFormat(f) if f.contains("abr<=64")))
        );

        // Interactive synced lyrics: Down selects next line, Enter seeks straight to its timestamp.
        r.app.tab = Tab::Lyrics;
        r.app.player.time = 5.0;
        r.app.lyrics = LyricsState::Found {
            lyrics: Lyrics::Synced(vec![
                (0.0, "Line 0".into()),
                (10.0, "Line 1".into()),
                (25.0, "Line 2".into()),
            ]),
            source: "LRCLIB",
        };
        key(&mut r.app, KeyCode::Down);
        assert_eq!(r.app.lyrics_sel, Some(1));
        key(&mut r.app, KeyCode::Down);
        assert_eq!(r.app.lyrics_sel, Some(2));
        key(&mut r.app, KeyCode::Enter);
        assert!(
            drain(&mut r.player)
                .iter()
                .any(|c| matches!(c, PlayerCmd::SeekAbsolute(t) if (*t - 25.0).abs() < 0.01))
        );
        assert_eq!(r.app.lyrics_sel, None);

        // Queue deduplication drops the second upcoming copy of "b".
        assert_eq!(r.app.queue.deduplicate_upcoming(), 1);
        assert_eq!(r.app.queue.len(), 2);
    }

    #[test]
    fn stats_discord_and_offline_cache_work_end_to_end() {
        let (p, mut prx) = unbounded_channel();
        let (d, mut drx) = unbounded_channel();
        let (o, mut orx) = unbounded_channel();
        let paths = test_paths();
        let (keymap, _) = Keymap::new(&Default::default());
        let channels = Channels {
            player: p,
            ytm: unbounded_channel().0,
            lyrics: unbounded_channel().0,
            art: unbounded_channel().0,
            resolve: unbounded_channel().0,
            notify: unbounded_channel().0,
            discord: d,
            offline: o,
        };
        let cfg = Config {
            resume: false,
            ..Config::default()
        };
        let mut app = App::new(cfg, paths, State::default(), keymap, channels, None, None);
        app.queue.replace(vec![track("off1")], 0);
        app.play_index(0);
        app.player.idle = false;
        app.player.paused = false;
        app.player.duration = 200.0;
        drain(&mut prx);

        // Listen for 26s in 2s steps -> counts a play in Wrapped Mini stats.
        for step in 1..=13 {
            app.on_player(PlayerEvent::TimePos(step as f64 * 2.0));
        }
        assert_eq!(app.stats.total_plays, 1);
        assert!(app.on_control(crate::control::Command::Stats).contains("1 plays"));

        // Open Wrapped Mini overlay with 'W' and verify rendering.
        key(&mut app, KeyCode::Char('W'));
        let shown = screen(&mut app);
        assert!(shown.contains("wrapped mini") && shown.contains("Track off1"), "{shown}");
        key(&mut app, KeyCode::Esc);

        // Toggle Discord Rich Presence via remote CLI.
        assert_eq!(app.on_control(crate::control::Command::Discord("on".into())), "ok discord on");
        let dcmds = drain(&mut drx);
        assert!(dcmds.iter().any(|c| matches!(c, PresenceCmd::SetEnabled(true))));
        assert!(dcmds.iter().any(|c| matches!(c, PresenceCmd::Update(Some(_)))));

        // Save track offline with 'O', complete download, and verify Library -> Offline works signed out.
        key(&mut app, KeyCode::Char('O'));
        assert!(matches!(orx.try_recv(), Ok(OfflineReq { track, .. }) if track.video_id == "off1"));
        let dir = app.paths.offline_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("off1.opus");
        std::fs::write(&file_path, b"local audio").unwrap();
        app.on_offline(OfflineEvent::Saved(crate::offline::OfflineEntry {
            track: track("off1"),
            file: "off1.opus".into(),
            bytes: 11,
            saved_at: 1_700_000_000,
        }));
        assert!(app.is_offline_cached("off1"));
        // Playing the cached track now loads the local file path directly (0 ms offline!).
        app.play_index(0);
        assert!(
            drain(&mut prx)
                .iter()
                .any(|c| matches!(c, PlayerCmd::Load { url, .. } if url.ends_with("off1.opus")))
        );
        // Switch Library to Offline while signed out: shows the saved track instead of sign-in screen.
        app.tab = Tab::Library;
        app.switch_library(LibSection::Offline);
        let shown = screen(&mut app);
        assert!(shown.contains("Track off1") && shown.contains("Offline"), "{shown}");
    }
}
