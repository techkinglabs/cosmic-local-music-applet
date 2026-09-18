# AGENTS.md - cosmic-media-applet

## Goal
MPRIS-only applet, no local playback. Brave/YouTube IS the player.

## Stack
libcosmic 1.0 (git), tokio full, zbus 5.19, no mpris crate

## Architecture
- MediaSource trait in src/mpris/mod.rs
- MprisAdapter only for v0.1
- MediaSourceManager: scan() + select_active() are async, no block_on
- UI: dumb, renders Option<TrackInfo> + PlaybackState

## Critical rules for AI
1. NEVER use futures::executor::block_on in async code
2. MPRIS xesam:artist is Array<String>, not String
3. zbus must be 5.x to match libcosmic
4. No polling, use PropertiesChanged signal
5. No reqwest/websocket in v0.1

## How to run
cargo build --release
cp target/release/cosmic-media-applet ~/.local/bin/
cp com.system76.CosmicMediaApplet.desktop ~/.local/share/cosmic/applets/
pkill cosmic-panel

## Known bugs to fix
- select_active sync -> async
- metadata parsing Array
- subscription() is none