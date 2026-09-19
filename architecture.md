
### Cosmic Media Applet Architecture - v0.2 (MPRIS-only)

**Overview**
A lightweight COSMIC desktop applet that provides universal media controls. It does not play music itself. It detects and controls any MPRIS2-compatible source currently running on the system. This covers Brave Browser (including YouTube, Jellyfin Web, SoundCloud), Jellyfin Media Player, VLC, Spotify, and others.

Core principle: On Linux, the browser IS the player. No browser extension is needed for v0.1.

**Requirements**
1. Integrate with COSMIC applet system (Rust, libcosmic git)
2. Panel UI: Previous / Play-Pause / Next button (icon_button) + track title label
3. Auto-detect active media source
4. Controls must work across all sources (route command to active source)
5. No local file playback in this version

**System Architecture**

```
┌─────────────────────────────────────────────────────┐
│ COSMIC Shell │
│ ┌─────────────────────────────────────────────────┐ │
│ │ Media Applet (UI Layer) │ │
│ │ - Panel widget: [<<] [▶/⏸] [>>] [ Title... ] (symbolic icons, no popup) │ │
│ │ - Dynamic title length via suggested_bounds │ │
│ │ - State: Option<TrackInfo>, PlaybackState │ │
│ │ - Subscribes to MediaSourceManager events │ │
│ └──────────────────┬──────────────────────────────┘ │
│ │ │
│ ┌──────────────────▼──────────────────────────────┐ │
│ │ MediaSourceManager │ │
│ │ - Discovers MPRIS players via D-Bus │ │
│ │ - select_active() on startup + reselect on events │ │
│ │ - Routes UI commands to active source │ │
│ │ - Broadcasts MediaEvent to UI │ │
│ │ - Forwards per-adapter subscriptions │ │
│ └──────────────────┬──────────────────────────────┘ │
│ │ │
│ ┌──────────────────▼──────────────────────────────┐ │
│ │ MprisAdapter (Per-source adapter) │ │
│ │ - Uses zbus directly (no mpris crate) │ │
│ │ - Wraps org.mpris.MediaPlayer2.* on D-Bus │ │
│ │ - Listens to PropertiesChanged signals │ │
│ │ - broadcast::channel for events │ │
│ │ Covers: Brave, YouTube-in-Brave, │ │
│ │ Jellyfin-in-Brave, VLC, Spotify, etc. │ │
│ └─────────────────────────────────────────────────┘
└─────────────────────────────────────────────────────┘
```

**Component Breakdown**

**1. COSMIC Applet (UI Layer)**
* Built from cosmic-applet-template pattern
* Layout: `icon_button` with symbolic icons in panel (no popup window)
  * `media-skip-backward-symbolic`, `media-playback-pause-symbolic` / `media-playback-start-symbolic`, `media-skip-forward-symbolic`
  * Title text widget shown/hidden dynamically based on `self.core.applet.suggested_bounds`
* State cached in struct: `current_track`, `current_track_info`, `current_state`
* Logic: UI is dumb. It renders state from Manager and sends `Message::Prev/PlayPause/Next`.

**2. Media Source Manager**
* Owns: `Vec<Arc<dyn MediaSource>>`
* Responsibilities:
  * `scan()`: call `ListNames` via D-Bus every rescan + on `NameOwnerChanged` signal
  * `do_scan`: creates adapters, evaluates cached states, selects active, forwards subscriptions
  * `reselect_active()`: re-evaluates which source is active based on cached states; called on `StateChanged` events from any adapter
  * `route(cmd)`: forwards command to `active_source`
  * Event bus: `tokio::sync::broadcast` channel for `MediaEvent::TrackChanged | StateChanged | SourceListChanged`
  * Per-adapter subscription forwarding: each adapter's `subscribe()` receiver is forwarded to the manager's event channel

**3. Source Adapter - MprisAdapter**

All adapters must implement this trait:

```rust
use async_trait::async_trait;

#[derive(Clone, Debug)]
pub struct TrackInfo {
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub source_id: String, // e.g., "chrome.instance123"
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
* Direct `zbus` Proxy creation for D-Bus calls (no mpris crate)
* `get_track()`: reads `Metadata` property via Proxy -> `TrackInfo`
* `subscribe()`: returns receiver of adapter's internal broadcast channel
* Internal watch task: uses `zbus::fdo::PropertiesProxy` + `receive_properties_changed()` to listen to `PropertiesChanged` signals (belonging to `org.freedesktop.DBus.Properties`, not the MPRIS Player interface), updates cached state/track, emits events
* No polling loop. Event-driven via D-Bus.

**Data Flow**

1. Applet starts -> Manager starts `scan()` task
2. D-Bus detects `org.mpris.MediaPlayer2.chrome.instance_...` (YouTube in Brave)
3. Manager creates `MprisAdapter` for it
4. Adapter's internal watch task uses `PropertiesProxy::receive_properties_changed()` to listen to `PropertiesChanged` signals and emits `TrackChanged`/`StateChanged`
5. Manager's forwarding task relays adapter events to UI via manager's broadcast channel, updating `cached_track`/`cached_state` and calling `reselect_active_inner()` on each event
6. `StateChanged` events trigger `reselect_active()` to re-evaluate active source
7. Adapter emits `TrackChanged` -> Manager updates cache, forwards to UI -> title label updates
8. User clicks Next -> UI sends `Message::Next` -> Manager calls `active.next().await` -> D-Bus call `org.mpris.MediaPlayer2.Player.Next` -> Brave skips video
9. Brave emits `PropertiesChanged` -> adapter watch task updates cache -> emits event -> forward to UI

**Dependencies (Rust)**

```toml
[dependencies]
libcosmic = { git = "https://github.com/pop-os/libcosmic.git", features = ["applet", "tokio", "winit", "wayland"] }
zbus = "5.19"
zvariant = "5"
tokio = { version = "1", features = ["full"] }
async-trait = "0.1"
anyhow = "1.0"
tracing = "0.1"
thiserror = "1.0"
futures = "0.3"
```

No `reqwest`, no `tokio-tungstenite`, no WebSocket for v0.1.

**Build & Run**

```bash
cargo build --release
cp target/release/cosmic-media-applet ~/.local/bin/
cp com.system76.CosmicMediaApplet.desktop ~/.local/share/cosmic/applets/
pkill cosmic-panel
```

**Future Enhancements (v0.2+)**

1. `JellyfinAdapter`: Direct API control for remote Jellyfin server when not open in browser. Uses `reqwest` + Jellyfin WebSocket for Now Playing.
2. Album art via `mpris:artUrl`
3. Progress bar / seek
4. Source priority settings in applet settings UI
