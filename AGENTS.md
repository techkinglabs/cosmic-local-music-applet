# AGENTS.md - cosmic-media-applet

## Goal
MPRIS-only applet, no local playback. Brave/YouTube IS the player.

## Stack
libcosmic 1.0 (git), tokio full, zbus 5.19, no mpris crate

## Architecture
- MediaSource trait in src/mpris/mod.rs
- MprisAdapter wraps zbus Proxy per source
- MediaSourceManager: scan() + select_active() are async, no block_on
- UI: controls directly in panel via icon_button with symbolic icons, no popup
- Subscriptions: adapter events forwarded via broadcast channel + reselect_active() on StateChanged
- PropertiesChanged: listened via zbus::fdo::PropertiesProxy + receive_properties_changed()

## Critical rules for AI
1. NEVER use futures::executor::block_on in async code
2. MPRIS xesam:artist is Array<String>, not String
3. zbus must be 5.x to match libcosmic
4. No polling, use PropertiesChanged signal via PropertiesProxy
5. No reqwest/websocket in v0.1
6. view() = icon_button in panel (no popup window)
7. Use chars().take(N) for string slicing (NOT byte slicing)
8. AppError is used in manager::route()
9. Title length is dynamic — use self.core.applet.suggested_bounds to compute max chars

## How to run
cargo build --release
cp target/release/cosmic-media-applet ~/.local/bin/
cp com.system76.CosmicMediaApplet.desktop ~/.local/share/cosmic/applets/
pkill cosmic-panel

## Known issues for future work
- Proxy caching in adapter (currently creates new Proxy per call)
- do_scan rebuilds all adapters on every NameOwnerChanged; diff-based incremental update would reduce churn
