
### Cosmic Media Applet Architecture - v0.2 (MPRIS-only)

**Overview**
A lightweight COSMIC desktop applet that provides universal media controls. It does not play music itself. It detects and controls any MPRIS2-compatible source currently running on the system. This covers Brave Browser (including YouTube, Jellyfin Web, SoundCloud), Jellyfin Media Player, VLC, Spotify, and others.

Core principle: On Linux, the browser IS the player. No browser extension is needed for v0.1.

**Requirements**
1. Integrate with COSMIC applet system (Rust, libcosmic)
2. Panel UI: Previous / Play-Pause / Next buttons + track title label
3. Auto-detect active media source
4. Controls must work across all sources (route command to active source)
5. No local file playback in this version

**System Architecture**

```
┌─────────────────────────────────────────────────────┐
│ COSMIC Shell │
│ ┌─────────────────────────────────────────────────┐ │
│ │ Media Applet (UI Layer) │ │
│ │ - Panel widget: [<<] [▶/⏸] [>>] [ Title... ] │ │
│ │ - State: Option<TrackInfo>, PlaybackState │ │
│ │ - Subscribes to MediaSourceManager events │ │
│ └──────────────────┬──────────────────────────────┘ │
│ │ │
│ ┌──────────────────▼──────────────────────────────┐ │
│ │ MediaSourceManager │ │
│ │ - Discovers MPRIS players via D-Bus │ │
│ │ - Selects active source (last-playing wins) │ │
│ │ - Routes UI commands to active source │ │
│ │ - Broadcasts MediaEvent to UI │ │
│ └──────────────────┬──────────────────────────────┘ │
│ │ │
│ ┌──────────────────▼──────────────────────────────┐ │
│ │ MprisAdapter (Single adapter for v0.1) │ │
│ │ - Uses mpris crate / zbus │ │
│ │ - Wraps org.mpris.MediaPlayer2.* on D-Bus │ │
│ │ - Listens to PropertiesChanged signals │ │
│ │ Covers: Brave, YouTube-in-Brave, │ │
│ │ Jellyfin-in-Brave, VLC, Spotify, etc. │ │
│ └─────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────┘
```

**Component Breakdown**

**1. COSMIC Applet (UI Layer)**
* Built from `cosmic-applet-template`
* Layout: horizontal `Row` in panel.
* Elements:
  * `previous_btn`, `play_pause_btn`, `next_btn` (cosmic::widget::button)
  * `title_label` (scrolling text, max 30 chars, tooltip with full title)
* Logic: UI is dumb. It only renders state from Manager and sends `Message::Prev/PlayPause/Next`.

**2. Media Source Manager**
* Owns: `Vec<Arc<dyn MediaSource>>`
* Responsibilities:
  * `scan()`: call `PlayerFinder::find_all()` every 3s + on D-Bus NameOwnerChanged
  * `select_active()`: priority rule: 1) Any `Playing` -> most recent `Playing` 2) else most recent `Paused` 3) else None
  * `route(cmd)`: forwards command to `active_source`
  * Event bus: `tokio::sync::broadcast` channel for `MediaEvent::TrackChanged | StateChanged | SourceChanged`

**3. Source Adapter - MprisAdapter**

All adapters must implement this trait:

```rust
use async_trait::async_trait;

#[derive(Clone, Debug)]
pub struct TrackInfo {
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub source_id: String, // e.g. "chrome.instance123"
}

#[derive(Clone, Debug, PartialEq)]
pub enum PlaybackState { Playing, Paused, Stopped }

#[derive(Clone, Debug)]
pub enum MediaEvent {
    TrackChanged(TrackInfo),
    StateChanged(PlaybackState),
    SourceListChanged,
}

#[async_trait]
pub trait MediaSource: Send + Sync {
    fn id(&self) -> &str;
    fn display_name(&self) -> &str; // "Brave", "VLC"
    async fn is_available(&self) -> bool;
    async fn get_track(&self) -> Option<TrackInfo>;
    async fn get_state(&self) -> PlaybackState;

    async fn play_pause(&self) -> anyhow::Result<()>;
    async fn next(&self) -> anyhow::Result<()>;
    async fn previous(&self) -> anyhow::Result<()>;

    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<MediaEvent>;
}
```

Implementation notes for `MprisAdapter`:
* Wrap `mpris::Player`
* `get_track()`: map `player.get_metadata()` -> `TrackInfo`
* `subscribe()`: use `zbus` to watch `org.freedesktop.DBus.Properties.PropertiesChanged` on `org.mpris.MediaPlayer2.Player`
* No polling loop. Event-driven via D-Bus.

**Data Flow**

1. Applet starts -> Manager starts `scan()` task
2. D-Bus detects `org.mpris.MediaPlayer2.chrome.instance_...` (YouTube in Brave)
3. Manager creates `MprisAdapter` for it, subscribes to its events
4. Adapter emits `TrackChanged` -> Manager emits to UI -> title label updates
5. User clicks Next -> UI sends `Message::Next` -> Manager calls `active.next().await` -> D-Bus call `org.mpris.MediaPlayer2.Player.Next` -> Brave skips video
6. Brave emits `PropertiesChanged` -> loop restarts

**Dependencies (Rust)**

```toml
[dependencies]
libcosmic = "0.4"
cosmic = { version = "0.4", features = ["applet"] }
mpris = "2.0"
zbus = "4"
tokio = { version = "1", features = ["full"] }
async-trait = "0.1"
anyhow = "1.0"
```

No `reqwest`, no `tokio-tungstenite`, no WebSocket for v0.1.

**Build & Run**

```bash
cargo build --release
just install
# add in: Settings -> Desktop -> Panel -> Applets -> Media Capture
```

**Future Enhancements (v0.2+)**

1. `JellyfinAdapter`: Direct API control for remote Jellyfin server when not open in browser. Uses `reqwest` + Jellyfin WebSocket for Now Playing.
2. Album art via `mpris:artUrl`
3. Progress bar / seek
4. Source priority settings in applet settings UI