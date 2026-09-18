#![warn(missing_docs)]

use async_trait::async_trait;
use std::fmt;

pub use adapter::MprisAdapter;

/// D-Bus adapter for controlling a single MPRIS media source.
pub mod adapter;

/// Information about a media track.
#[derive(Debug, Clone)]
pub struct TrackInfo {
    /// Track title
    pub title: String,
    /// Track artist
    pub artist: String,
    /// Track album
    pub album: Option<String>,
    /// Unique source identifier (e.g., "chrome.instance123")
    pub source_id: String,
}

impl fmt::Display for TrackInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} - {}", self.artist, self.title)
    }
}

/// Playback state of a media source.
#[derive(Debug, Clone, PartialEq)]
pub enum PlaybackState {
    /// Track is currently playing
    Playing,
    /// Track is paused
    Paused,
    /// No track is playing
    Stopped,
}

/// Events emitted by media sources.
#[derive(Debug, Clone)]
pub enum MediaEvent {
    /// Track information changed
    TrackChanged(TrackInfo),
    /// Playback state changed
    StateChanged(PlaybackState),
    /// List of available sources changed
    SourceListChanged,
}

/// Trait for media sources that can be controlled via MPRIS2.
#[async_trait]
pub trait MediaSource: Send + Sync {
    /// Unique identifier for this source
    fn id(&self) -> &str;
    /// Display name (e.g., "Brave", "VLC")
    fn display_name(&self) -> &str;
    /// Check if this source is currently available
    async fn is_available(&self) -> bool;
    /// Get current track information
    async fn get_track(&self) -> Option<TrackInfo>;
    /// Get current playback state
    async fn get_state(&self) -> PlaybackState;
    /// Toggle play/pause
    async fn play_pause(&self) -> anyhow::Result<()>;
    /// Skip to next track
    async fn next(&self) -> anyhow::Result<()>;
    /// Go to previous track
    async fn previous(&self) -> anyhow::Result<()>;
    /// Subscribe to media events
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<MediaEvent>;
}
