/// Commands sent from the UI to the manager.
#[derive(Debug, Clone)]
pub enum AppMessage {
    /// Go to previous track
    Previous,
    /// Toggle play/pause
    PlayPause,
    /// Skip to next track
    Next,
    /// Scan music folder and update stats
    ScanMusic,
}
