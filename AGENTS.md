# AGENTS.md - cosmic-media-applet

## Goal
MPRIS-only applet, no local playback. Brave/YouTube IS the player.

## Stack
libcosmic 1.0 (git), tokio full, zbus 5.19, no mpris crate

## Architecture
- MediaSource trait in src/mpris/mod.rs
- MprisAdapter wraps zbus Proxy per source
- MediaSourceManager: scan() + select_active() are async, no block_on
- UI: popup pattern: icon_button in panel toggles view_window popup
- Subscriptions: adapter events forwarded via broadcast channel + reselect_active() on StateChanged

## Critical rules for AI
1. NEVER use futures::executor::block_on in async code
2. MPRIS xesam:artist is Array<String>, not String
3. zbus must be 5.x to match libcosmic
4. No polling, use PropertiesChanged signal
5. No reqwest/websocket in v0.1
6. view() = icon_button, view_window() = popup content
7. Use chars().take(N) for string slicing (NOT byte slicing)
8. AppError is used in manager::route()

## How to run
cargo build --release
cp target/release/cosmic-media-applet ~/.local/bin/
cp com.system76.CosmicMediaApplet.desktop ~/.local/share/cosmic/applets/
pkill cosmic-panel

## Known issues for future work
- Proxy caching in adapter (currently creates new Proxy per call)
- Watch task handles not explicitly aborted on Drop (process exit handles cleanup)
