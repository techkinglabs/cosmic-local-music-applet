### Cosmic Media Applet Architecture - v0.3 (MPRIS + Local Playback)

**Overview**
A COSMIC desktop applet that provides universal media controls via MPRIS2 AND a full local music player inside the popup. The panel shows compact media controls (Previous / Play-Pause / Next + dynamic title). Clicking the hamburger/menu icon opens a popup with a complete music player — albums, search, favorites, play count sorting, and media controls. Local playback is handled by a Rodio-based `PlayerAdapter` that implements the same `MediaSource` trait as `MprisAdapter`, so the manager and UI treat both sources uniformly.

**Requirements**
1. Integrate with COSMIC applet system (Rust, libcosmic git)
2. Panel UI: Previous / Play-Pause / Next button + track title label (icon_button, symbolic icons)
3. Auto-detect active media source (MPRIS or local player)
4. Controls must work across all sources (route command to active source)
5. Popup player: local music folder playback with media controls, albums, search, favorites, play count
6. Play count increments when local tracks are played; sortable by play count descending
7. Favorites persisted in SQLite DB (`is_favorite` column)

**System Architecture**

```
┌───────────────────────────────────────────────────────────────┐
│ COSMIC Shell                                                   │
│ ┌───────────────────────────────────────────────────────────┐ │
│ │ Media Applet (UI Layer)                                    │ │
│ │                                                            │ │
│ │  Panel (always visible):                                   │ │
│ │  [☰] [<<] [▶/⏸] [>>] [ Track Title... ]                   │ │
│ │   └─ hamburger opens app_popup                              │ │
│ │                                                            │ │
│ │  Popup (app_popup action):                                 │ │
│ │  ┌─────────────────────────────────────────────────────┐   │ │
│ │  │ Media Controls: [<<] [▶/⏸] [>>] [■]               │   │ │
│ │  │ Track Info: Title, Artist, Album                    │   │ │
│ │  │ Progress Bar + Duration (slider)                    │   │ │
│ │  │ Volume Slider                                       │   │ │
│ │  │ ─────────────────────────────────────────────────── │   │ │
│ │  │ Albums View: grid of album dirs with cover placeholder│   │ │
│ │  │ Track Listing: songs in album (title, artist, dur)  │   │ │
│ │  │ Search: text input → filter tracks                  │   │ │
│ │  │ Favorites: ★ toggle per track (persisted in SQLite) │   │ │
│ │  │ Play Count: sortable desc (secondary: by title)     │   │ │
│ │  │ Now Playing: highlight active track                 │   │ │
│ │  └─────────────────────────────────────────────────────┘   │ │
│ │                                                            │ │
│ │ - State: Option<TrackInfo>, PlaybackState                 │ │
│ │ - Subscribes to MediaSourceManager events via broadcast    │ │
│ └──────────────────┬────────────────────────────────────────┘ │
│                   │                                           │
│ ┌─────────────────▼────────────────────────────────────────┐ │
│ │ MediaSourceManager (src/manager.rs)                      │ │
│ │                                                          │ │
│ │  Owned: Arc<RwLock<Vec<Arc<dyn MediaSource>>>>           │ │
│ │  Owned: Arc<RwLock<Option<Arc<dyn MediaSource>>>> active │ │
│ │                                                          │ │
│ │  - scan(): discover MPRIS + start local player           │ │
│ │    • MPRIS: ListNames via D-Bus                          │ │
│ │    • Local: spawn_blocking filesystem traversal         │ │
│ │  - do_scan: build adapters, eval cached states,          │ │
│ │    select active, forward subscriptions                   │ │
│ │  - select_active() / reselect_active(): async, no block  │ │
│ │    - priority: Playing > Paused > Stopped                │ │
│ │    - local player participates as a MediaSource          │ │
│ │  - route(cmd): forwards command to active source         │ │
│ │    • AppMessage::ScanMusic → scan_music() (local)        │ │
│ │    • AppMessage::Previous/PlayPause/Next                 │ │
│ │  - Event bus: tokio::sync::broadcast for MediaEvent      │ │
│ │    • TrackChanged | StateChanged | SourceListChanged     │ │
│ │    • StatsUpdated (track/album count, last_scanned)      │ │
│ │    • PlaybackPosition (for progress bar updates)         │ │
│ │    • VolumeChanged                                      │ │
│ │                                                          │ │
│ │  - Per-adapter subscription forwarding: each adapter's   │ │
│ │    subscribe() receiver relayed to manager's broadcast   │ │
│ │                                                          │ │
│ │  - D-Bus NameOwnerChanged → triggers do_scan rescan     │ │
│ └─────────────────┬────────────────────────────────────────┘ │
│                   │                                           │
│  ┌────────────────┴────────────────────────┐                │
│  │                                           │                │
│  ▼                                           ▼                │
│ ┌────────────────────────────────────┐ ┌────────────────────┐ │
│ │ MprisAdapter (src/mpris/adapter.rs)│ │ PlayerAdapter      │ │
│ │                                    │ │ (src/player.rs)    │ │
│ │ - zbus Proxy per MPRIS source      │ │                    │ │
│ │ - Wraps org.mpris.MediaPlayer2.*   │ │ - Rodio-based      │ │
│ │ - PropertiesChanged via            │ │   playback engine  │ │
│ │   PropertiesProxy                  │ │ - Implements       │ │
│ │ - broadcast::channel for events    │ │   MediaSource      │ │
│ │                                    │ │ - Audio files from │ │
│ │ Sources: Brave, YouTube in Brave,  │ │   music folder     │ │
│ │   Jellyfin Web, VLC, Spotify, etc  │ │ - Own TrackInfo    │ │
│ │                                    │ │   (source_id=      │ │
│ │                                    │ │    "local-player") │ │
│ │                                    │ │ - Emits:           │ │
│ │                                    │ │   TrackChanged,    │ │
│ │                                    │ │   StateChanged,    │ │
│ │                                    │ │   PlaybackPosition,│ │
│ │                                    │ │   VolumeChanged    │ │
│ └────────────────────────────────────┘ └────────────────────┘ │
│                                                               │
│ ┌───────────────────────────────────────────────────────────┐ │
│ │ MusicStatsDb (src/music_db.rs)                            │ │
│ │ - rusqlite with bundled feature                          │ │
│ │ - SQLite DB at XDG_DATA_HOME/cosmic-media-applet/         │ │
│ │ - Tables:                                                  │ │
│ │   • music_stats: global scan metadata (count, albums, ts) │ │
│ │   • tracks: per-track stats (play_count, is_favorite)     │ │
│ │ - scan_music_folder(): spawn_blocking traversal           │ │
│ │   • Recursive dir walk, filter by extension               │ │
│ │   • Extensions: mp3, flac, ogg, wav, m4a, opus, wma      │ │
│ │ - get_tracks(): query with optional filters + sorting     │ │
│ │ - toggle_favorite(): flip is_favorite column              │ │
│ │ - increment_play_count(): called on local track start     │ │
│ └───────────────────────────────────────────────────────────┘ │
└───────────────────────────────────────────────────────────────┘
```

**Component Breakdown**

**1. COSMIC Applet (UI Layer)**
* Built from cosmic-applet-template pattern (libcosmic 1.0 git)
* Panel (`view()`): horizontal `Row` of `icon_button`s
  * `open-menu-symbolic` (hamburger → opens `app_popup`)
  * `media-skip-backward-symbolic` (Previous)
  * `media-playback-pause-symbolic` / `media-playback-start-symbolic` (PlayPause, icon depends on state)
  * `media-playback-stop-symbolic` (Stop)
  * `media-skip-forward-symbolic` (Next)
  * Dynamic title text via `self.core.applet.suggested_bounds` → `chars().take(max_chars)` truncation
* Popup (`app_popup`): full media player UI
  * Media controls row: Previous, PlayPause, Next, Stop
  * Track info column: title, artist, album
  * Progress slider + position/duration labels
  * Volume slider
  * Albums view: grid/list of album directories with cover placeholder
  * Track listing for selected album: title, artist, duration, favorite star, play count badge
  * Search input: filters tracks across all albums by title/artist/album
  * Sort toggle: play count desc (default), title asc (secondary)
  * Now Playing highlight on active track
* State in struct: `current_track: String`, `current_track_info: Option<TrackInfo>`, `current_state: PlaybackState`, `stats_album_count`, `stats_track_count`, `stats_last_scanned`
* Logic: UI is dumb. Renders state from Manager, sends `Message::Prev/PlayPause/Next/Stop/ScanMusic` to Manager via `route()`.

**2. Media Source Manager**
* Owns: `Vec<Arc<dyn MediaSource>>` (both MPRIS and local PlayerAdapter)
* Responsibilities:
  * `scan()` / `do_scan()`:
    * MPRIS discovery via D-Bus `ListNames` or `NameOwnerChanged` signal
    * Local player: always present (singleton `PlayerAdapter`), registered as a source
    * Creates adapters, evaluates cached states, selects active, forwards subscriptions
  * `reselect_active()` / `reselect_active_inner()`: async, no block_on
    * Selection priority: any `Playing` source wins; else first `Paused`; else `Stopped`
    * Local player participates like any other `MediaSource`
  * `route(cmd: AppMessage)`:
    * `AppMessage::Previous` / `PlayPause` / `Next` → `active.next().await` etc.
    * `AppMessage::ScanMusic` → `scan_music().await` (local folder traversal + SQLite update)
    * `AppError::NoActiveSource` if no active source
  * Event bus: `tokio::sync::broadcast` channel for `MediaEvent`
    * `TrackChanged(TrackInfo)`, `StateChanged(PlaybackState)`
    * `SourceListChanged`
    * `StatsUpdated { track_count, album_count, last_scanned }`
    * `PlaybackPosition { position_ms, duration_ms }` (for progress bar)
    * `VolumeChanged(f32)`
  * Per-adapter subscription forwarding: each adapter's `subscribe()` receiver is relayed to the manager's broadcast channel via spawned forwarding tasks
  * `select_active()` is called on every `StateChanged` event to re-evaluate active source
* DB-backed stats: `MusicStatsDb` for local track metadata (play count, favorites, scan metadata)

**3. Source Adapters**

**3a. MprisAdapter** (`src/mpris/adapter.rs`)
* Direct `zbus` Proxy creation (no mpris crate)
* `get_track()`: reads `Metadata` property via Proxy → `TrackInfo`
  * `xesam:title` → `title` (String)
  * `xesam:artist` → `artist` (Array<String>, joined with ", ")
  * `xesam:album` → `album` (Option<String>)
* `subscribe()`: returns receiver of adapter's internal broadcast channel
* Internal watch task: uses `zbus::fdo::PropertiesProxy` + `receive_properties_changed()` to listen to `PropertiesChanged` signals on `org.freedesktop.DBus.Properties`, filters for `org.mpris.MediaPlayer2.Player` interface, updates cached state/track, emits events
* No polling loop. Event-driven via D-Bus.
* Sources: Brave, YouTube-in-Brave, Jellyfin-in-Brave, VLC, Spotify, etc.

**3b. PlayerAdapter** (`src/player.rs`)
* Rodio-based local playback engine
* Implements `MediaSource` trait (same as `MprisAdapter`)
  * `id()` → `"local-player"`
  * `display_name()` → `"Local Music"`
  * `source_id` in `TrackInfo` → `"local-player"`
* Playback:
  * Decodes audio file via `rodio::source::Decoder`
  * Plays via `rodio::Sink`
  * On track start: calls `MusicStatsDb::increment_play_count()`
  * Position tracking: periodic `PlaybackPosition` events via broadcast
* Play state:
  * `play_pause()`: toggle Play/Pause on `Sink`
  * `next()` / `previous()`: advance within playlist (queue index)
  * `is_available()` → `true` (always available if music folder exists)
* Event model:
  * Uses broadcast channel (no D-Bus) — aligns with AGENTS.md rule
  * Emits `TrackChanged`, `StateChanged`, `PlaybackPosition`, `VolumeChanged`
* File scanning integration:
  * `scan_music()` in manager triggers `spawn_blocking` traversal
  * Results stored in SQLite + returned as `StatsUpdated`

**4. MediaSource Trait** (`src/mpris/mod.rs`)

```rust
#[derive(Clone, Debug)]
pub struct TrackInfo {
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub source_id: String,
    pub duration_ms: Option<u64>,  // added for progress bar
}

#[derive(Clone, Debug, PartialEq)]
pub enum PlaybackState { Playing, Paused, Stopped }

#[derive(Clone, Debug)]
pub enum MediaEvent {
    TrackChanged(TrackInfo),
    StateChanged(PlaybackState),
    SourceListChanged,
    StatsUpdated { track_count: usize, album_count: usize, last_scanned: u64 },
    PlaybackPosition { position_ms: u64, duration_ms: u64 },
    VolumeChanged(f32),
}

#[async_trait]
pub trait MediaSource: Send + Sync {
    fn id(&self) -> &str;
    fn display_name(&self) -> &str;
    async fn is_available(&self) -> bool;
    async fn get_track(&self) -> Option<TrackInfo>;
    async fn get_state(&self) -> PlaybackState;
    async fn play_pause(&self) -> anyhow::Result<()>;
    async fn next(&self) -> anyhow::Result<()>;
    async fn previous(&self) -> anyhow::Result<()>;
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<MediaEvent>;
}
```

**5. MusicStatsDb** (`src/music_db.rs`)
* rusqlite with `bundled` feature — no external sqlite3 dependency
* DB path: `XDG_DATA_HOME/cosmic-media-applet/music_stats.db` or `$HOME/.local/share/cosmic-media-applet/music_stats.db`
* Tables:
  * `music_stats`: global scan metadata (id=1, track_count, album_count, last_scanned)
  * `tracks`: per-file track stats
    * `path` (TEXT, PK — absolute file path),
    * `title` (TEXT), `artist` (TEXT), `album` (TEXT),
    * `duration_ms` (INTEGER),
    * `play_count` (INTEGER, default 0),
    * `is_favorite` (INTEGER 0/1, default 0),
    * `last_played` (INTEGER, unix timestamp)
* Methods:
  * `scan_music_folder(dir)`: `spawn_blocking`, recursive traversal, writes tracks to DB, returns `(track_count, album_count)`
  * `get_tracks_sorted_by_play_count()`: default sort: play_count DESC, title ASC
  * `search_tracks(query)`: filter by title/artist/album (LIKE, case-insensitive)
  * `get_albums()`: list distinct album directories
  * `get_tracks_in_album(album)`: tracks within a specific album dir
  * `toggle_favorite(path)`: flip `is_favorite`
  * `increment_play_count(path)`: `play_count += 1`, `last_played = now()`
  * `set_stats(track_count, album_count, timestamp)`: update global row
  * `get_stats()`: read global row

**Data Flow**

1. Applet starts → `init()` creates `MediaSourceManager::new().await`
2. Manager:
   a. Connects to session D-Bus (for MPRIS discovery)
   b. Creates singleton `PlayerAdapter` (local playback) — registered as a `MediaSource`
   c. Spawns `do_scan()` task for initial MPRIS discovery
   d. Loads cached stats from SQLite via `spawn_blocking`
   e. Spawns `NameOwnerChanged` signal listener → `do_scan` on MPRIS name changes
   f. For each MPRIS source: creates `MprisAdapter`, spawns event-forwarding task
3. Applet receives `ManagerReady` → reads `cached_state()` for initial track/state → renders panel
4. Panel shows dynamic title via `chars().take(max_title_chars())`
5. User clicks hamburger → `app_popup` opens popup with full media player UI
6. Popup renders: media controls, track info, progress bar, volume, albums, search, favorites, play count sorting
7. User clicks PlayPause → `Message::PlayPause` → `manager.route(AppMessage::PlayPause)` → `active_source.play_pause().await`
   * If active is `PlayerAdapter`: Rodio `Sink` toggle
   * If active is `MprisAdapter`: D-Bus `PlayPause` call
8. Local track starts playing → `PlayerAdapter` emits `TrackChanged` → Manager forwards via broadcast → UI updates
   * Manager calls `MusicStatsDb::increment_play_count()` via `spawn_blocking`
9. MPRIS source emits `PropertiesChanged` → adapter watch task → `TrackChanged`/`StateChanged` → Manager forwards → UI updates
10. `StateChanged` triggers `reselect_active()` → if local player now playing and no MPRIS playing, local becomes active
11. User scans music → `Message::ScanMusic` → `manager.scan_music()` → `spawn_blocking` filesystem traversal → SQLite write → `StatsUpdated` event → popup stats refresh
12. User toggles favorite → DB update → UI reflects ★ instantly
13. User searches → filter `tracks` table by title/artist/album → results sorted by play count desc

**Dependencies (Rust)**

```toml
[dependencies]
libcosmic = { git = "https://github.com/pop-os/libcosmic.git", rev = "87ab8179e1bd9880239c340855ae8862034bd0e8", features = ["applet", "tokio", "winit", "wayland"] }
zbus = "5.19"
zvariant = "5"
tokio = { version = "1", features = ["full"] }
async-trait = "0.1"
anyhow = "1.0"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
thiserror = "1.0"
futures = "0.3"
rusqlite = { version = "0.29", features = ["bundled"] }
# Local playback (to be added):
rodio = "0.20"
symphonia = { version = "0.5", features = ["mp3", "flac", "ogg", "wav", "m4a", "aiff"] }
```

No `reqwest`, no `tokio-tungstenite`, no WebSocket for v0.1.

**Build & Run**

```bash
cargo build --release
./install.sh
# Or manually:
cp target/release/cosmic-media-applet ~/.local/bin/
cp com.system76.CosmicMediaApplet.desktop ~/.local/share/cosmic/applets/
pkill cosmic-panel
```

**Known issues for future work**
- Proxy caching in `MprisAdapter` (currently creates new Proxy per call)
- `do_scan` rebuilds all adapters on every `NameOwnerChanged`; diff-based incremental update would reduce churn
- Local player seeks via `rodio::Source::skip_duration` — exact positioning needs refinement
- Album art extraction from local audio file metadata tags not yet implemented
- Playback queue / shuffle / repeat modes not yet implemented
