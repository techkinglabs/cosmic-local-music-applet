# AGENTS.md - cosmic-media-applet

## Goal
COSMIC desktop applet with a popup music player. Supports MPRIS external sources (Brave/YouTube as player) AND local music folder playback within the popup.

## Stack
libcosmic 1.0 (git), tokio full, zbus 5.19, no mpris crate, rusqlite 0.29 for stats DB

## Architecture
- **Local playback**: Rodio-based player in `src/player.rs`, controlled via `PlayerAdapter` implementing `MediaSource`
- **MPRIS external sources**: `MprisAdapter` wraps zbus Proxy per source (`src/mpris/adapter.rs`)
- **MediaSource trait** in `src/mpris/mod.rs` — unified trait for both local and MPRIS sources
- **MediaSourceManager** (`src/manager.rs`): `scan()` + `select_active()` are async, no block_on
- **MusicStatsDb** (`src/music_db.rs`): SQLite-backed track stats (play count, favorites, scan metadata)
- **UI**: popup window (`app_popup`) containing full media player UI — album list, search, favorites, play count sorting, media controls
- **Subscriptions**: adapter events forwarded via broadcast channel + `reselect_active()` on `StateChanged`
- **PropertiesChanged**: listened via `zbus::fdo::PropertiesProxy` + `receive_properties_changed()`

## Critical rules for AI
1. NEVER use `futures::executor::block_on` in async code — use `tokio::task::spawn_blocking` for sync work
2. MPRIS `xesam:artist` is `Array<String>`, not `String`
3. zbus must be 5.x to match libcosmic
4. No polling — use `PropertiesChanged` signal via `PropertiesProxy` for MPRIS; local player uses broadcast channels
5. No reqwest/websocket in v0.1
6. `view()` renders `icon_button`s in panel; popup is opened via `app_popup` action — popup contains the full media player UI
7. Use `chars().take(N)` for string slicing (NOT byte slicing)
8. `AppError` is used in `manager::route()`
9. Title length is dynamic — use `self.core.applet.suggested_bounds` to compute max chars
10. Local file scanning uses `spawn_blocking` for filesystem traversal (sync I/O)
11. Music stats DB uses rusqlite with `bundled` feature — no external sqlite3 dependency

## Popup Player Feature
The popup (opened by clicking the hamburger/menu icon in the panel) contains:
- **Media controls**: Previous / Play-Pause / Next / Stop buttons with symbolic icons
- **Track info**: Title, artist, album display
- **Progress bar**: Position slider with duration display
- **Volume slider**
- **Albums view**: Grid or list of album directories from music folder, showing cover art placeholder
- **Track listing**: Songs within selected album, showing title, artist, duration
- **Search**: Text input to filter tracks by title/artist/album across all scanned music
- **Favorites**: Star toggle on tracks — persisted in SQLite DB (`is_favorite` column)
- **Play count**: Tracks increment play count when played; sortable by play count (descending)
- **Smart sorting**: Default sort by play count descending, with secondary sort by title
- **Now Playing**: Highlights currently playing track across albums/search views

## How to run
```bash
cargo build --release
./install.sh
# Or manually:
cp target/release/cosmic-media-applet ~/.local/bin/
cp com.system76.CosmicMediaApplet.desktop ~/.local/share/cosmic/applets/
pkill cosmic-panel
```

## Known issues for future work
- Proxy caching in `MprisAdapter` (currently creates new Proxy per call)
- `do_scan` rebuilds all adapters on every `NameOwnerChanged`; diff-based incremental update would reduce churn
- Local player seeks via `rodio::Source::skip_duration` — exact positioning needs refinement
- Album art extraction from local audio files (metadata tags) not yet implemented
- Playback queue / shuffle / repeat modes not yet implemented
