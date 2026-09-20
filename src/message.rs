/// Commands sent from the UI to the manager.
#[derive(Debug, Clone)]
pub enum AppMessage {
    /// Go to previous track
    Previous,
    /// Toggle play/pause
    PlayPause,
    /// Skip to next track
    Next,
    /// Stop playback
    Stop,
    /// Set playback position (milliseconds)
    SetPosition(u64),
    /// Set volume (0.0 to 1.0)
    SetVolume(f32),
    /// Scan music folder and update stats
    ScanMusic,
    /// Play a specific track from the popup (file path)
    PlayTrack(String),
    /// Toggle favorite for a track
    ToggleFavorite(String),
}
