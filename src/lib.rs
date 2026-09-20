//! COSMIC local music applet for universal media controls via MPRIS2.
//!
//! Provides a panel applet that controls media playback through D-Bus
//! (MPRIS2 protocol) and also supports local music folder playback
//! within the popup window via a Rodio-based player.

pub use self::message::AppMessage;
pub use self::mpris::{MediaEvent, MediaSource, MprisAdapter, PlaybackState, TrackInfo};
pub use self::music_db::{AlbumInfo, MusicStats, MusicStatsDb, TrackStat};
pub use self::player::PlayerAdapter;

/// Media source trait for controlling playback.
pub mod error;
/// MPRIS media source manager for discovery and control.
pub mod manager;
/// Application message types.
pub mod message;
/// MPRIS adapter for D-Bus communication.
pub mod mpris;
/// SQLite-backed music stats database.
pub mod music_db;
/// Local music player (Rodio-based).
pub mod player;
