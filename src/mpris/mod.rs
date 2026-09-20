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
    /// Unique source identifier (e.g., "chrome.instance123", "local-player")
    pub source_id: String,
    /// Track duration in milliseconds
    pub duration_ms: Option<u64>,
    /// Absolute file path for local tracks (None for MPRIS sources)
    pub file_path: Option<String>,
}

impl TrackInfo {
    /// Creates a new `TrackInfo` with default values
    pub fn new(title: String, artist: String, album: Option<String>, source_id: String) -> Self {
        Self {
            title,
            artist,
            album,
            source_id,
            duration_ms: None,
            file_path: None,
        }
    }
}

impl Default for TrackInfo {
    fn default() -> Self {
        Self {
            title: String::new(),
            artist: String::new(),
            album: None,
            source_id: String::new(),
            duration_ms: None,
            file_path: None,
        }
    }
}

impl fmt::Display for TrackInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let has_artist = !self.artist.is_empty();
        let has_title = !self.title.is_empty();
        match (has_artist, has_title) {
            (true, true) => write!(f, "{} \u{2014} {}", self.artist, self.title),
            (true, false) => write!(f, "{}", self.artist),
            (false, true) => write!(f, "{}", self.title),
            (false, false) => Ok(()),
        }
    }
}

/// Playback state of a media source.
#[derive(Debug, Clone, Copy, PartialEq)]
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
    /// Music folder stats updated
    StatsUpdated {
        /// Number of tracks found
        track_count: usize,
        /// Number of albums (directories) found
        album_count: usize,
        /// Unix timestamp of last scan
        last_scanned: u64,
    },
    /// Playback position update (position_ms, duration_ms)
    PlaybackPosition {
        /// Current position in milliseconds
        position_ms: u64,
        /// Total duration in milliseconds
        duration_ms: u64,
    },
    /// Volume changed (0.0 to 1.0)
    VolumeChanged(f32),
}

/// Trait for media sources that can be controlled via MPRIS2 or local playback.
#[async_trait]
pub trait MediaSource: Send + Sync {
    /// Unique identifier for this source
    fn id(&self) -> &str;
    /// Display name (e.g., "Brave", "VLC", "Local Music")
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
    /// Stop playback
    async fn stop(&self) -> anyhow::Result<()>;
    /// Set playback position (milliseconds)
    async fn set_position(&self, position_ms: u64) -> anyhow::Result<()>;
    /// Set volume (0.0 to 1.0)
    async fn set_volume(&self, volume: f32) -> anyhow::Result<()>;
    /// Get current playback position (milliseconds)
    async fn get_position(&self) -> Option<u64>;
    /// Get current volume (0.0 to 1.0)
    async fn get_volume(&self) -> Option<f32>;
    /// Subscribe to media events
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<MediaEvent>;
}
