use crate::error::AppError;
use crate::message::AppMessage;
use crate::mpris::{MediaEvent, MediaSource, PlaybackState, TrackInfo};
use crate::music_db::{music_folder, MusicStatsDb};
use crate::player::PlayerAdapter;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{RwLock, broadcast};
use tokio::time::timeout;
use zbus::{Connection, Proxy};

use super::mpris::MprisAdapter;

const DBUS_PATH: &str = "/org/freedesktop/DBus";
const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const MPRIS_CALL_TIMEOUT: Duration = Duration::from_millis(500);

pub struct MediaSourceManager {
    connection: Option<Arc<Connection>>,
    sources: Arc<RwLock<Vec<Arc<dyn MediaSource>>>>,
    active_source: Arc<RwLock<Option<Arc<dyn MediaSource>>>>,
    event_sender: broadcast::Sender<MediaEvent>,
    _scan_handle: Option<tokio::task::JoinHandle<()>>,
    _watch_handles: Arc<RwLock<Vec<tokio::task::JoinHandle<()>>>>,
    cached_track: Arc<RwLock<Option<TrackInfo>>>,
    cached_state: Arc<RwLock<PlaybackState>>,
    db_path: Option<PathBuf>,
    stats_album_count: Arc<RwLock<usize>>,
    stats_track_count: Arc<RwLock<usize>>,
    stats_last_scanned: Arc<RwLock<u64>>,
    player: Option<Arc<PlayerAdapter>>,
}

impl MediaSourceManager {
    pub fn new_empty() -> Self {
        let (tx, _) = broadcast::channel(64);
        Self {
            connection: None,
            sources: Arc::new(RwLock::new(Vec::new())),
            active_source: Arc::new(RwLock::new(None)),
            event_sender: tx,
            _scan_handle: None,
            _watch_handles: Arc::new(RwLock::new(Vec::new())),
            cached_track: Arc::new(RwLock::new(None)),
            cached_state: Arc::new(RwLock::new(PlaybackState::Stopped)),
            db_path: None,
            stats_album_count: Arc::new(RwLock::new(0)),
            stats_track_count: Arc::new(RwLock::new(0)),
            stats_last_scanned: Arc::new(RwLock::new(0)),
            player: None,
        }
    }

    pub async fn new() -> anyhow::Result<Self> {
        tracing::info!("Connecting to the session D-Bus");
        let connection = Arc::new(Connection::session().await?);
        let (event_sender, _) = broadcast::channel(64);
        let sources: Arc<RwLock<Vec<Arc<dyn MediaSource>>>> = Arc::new(RwLock::new(Vec::new()));
        let active_source: Arc<RwLock<Option<Arc<dyn MediaSource>>>> = Arc::new(RwLock::new(None));
        let watch_handles: Arc<RwLock<Vec<tokio::task::JoinHandle<()>>>> =
            Arc::new(RwLock::new(Vec::new()));
        let cached_track: Arc<RwLock<Option<TrackInfo>>> = Arc::new(RwLock::new(None));
        let cached_state: Arc<RwLock<PlaybackState>> =
            Arc::new(RwLock::new(PlaybackState::Stopped));

        let player = match PlayerAdapter::new() {
            Ok(p) => {
                tracing::info!("Local PlayerAdapter created");
                Some(Arc::new(p))
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to create PlayerAdapter; local playback unavailable");
                None
            }
        };

        {
            let mut sources_guard = sources.write().await;
            if let Some(ref player) = player {
                sources_guard.push(player.clone() as Arc<dyn MediaSource>);
            }
        }

        if let Some(player) = player.as_ref() {
            let player = player.clone();
            let player_id = player.id().to_string();
            let sender_clone = event_sender.clone();
            let cached_track_fwd = cached_track.clone();
            let cached_state_fwd = cached_state.clone();
            let active_fwd = active_source.clone();
            let sources_fwd = sources.clone();
            let sender_for_task = event_sender.clone();

            let handle = tokio::spawn(async move {
                let mut rx = player.subscribe();
                loop {
                    match rx.recv().await {
                        Ok(event) => {
                            match &event {
                                MediaEvent::StateChanged(state) => {
                                    *cached_state_fwd.write().await = state.clone();
                                    Self::reselect_active_inner(
                                        &sources_fwd,
                                        &active_fwd,
                                        &cached_state_fwd,
                                        &cached_track_fwd,
                                        sender_for_task.clone(),
                                        Some(&player_id),
                                    )
                                    .await;
                                }
                                MediaEvent::TrackChanged(track) => {
                                    *cached_track_fwd.write().await = Some(track.clone());
                                    Self::reselect_active_inner(
                                        &sources_fwd,
                                        &active_fwd,
                                        &cached_state_fwd,
                                        &cached_track_fwd,
                                        sender_for_task.clone(),
                                        Some(&player_id),
                                    )
                                    .await;
                                }
                                MediaEvent::PlaybackPosition { .. } => {},
                                MediaEvent::VolumeChanged(_) => {},
                                MediaEvent::StatsUpdated { .. } => {},
                                MediaEvent::SourceListChanged => {},
                                MediaEvent::TrackFinished => {
                                    let player = player.clone();
                                    tokio::spawn(async move {
                                        if let Err(e) = player.next().await {
                                            tracing::warn!(error = %e, "Failed to auto-advance to next track");
                                        }
                                    });
                                }
                            }
                            let _ = sender_clone.send(event);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            tracing::warn!(
                                source = %player_id,
                                skipped,
                                "Player event forwarder lagged; continuing"
                            );
                            continue;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
            watch_handles.write().await.push(handle);
        }

        let conn_clone = connection.clone();
        let sources_clone = sources.clone();
        let active_clone = active_source.clone();
        let sender_clone = event_sender.clone();
        let watch_handles_clone = watch_handles.clone();
        let cached_track_clone = cached_track.clone();
        let cached_state_clone = cached_state.clone();

        let _scan_handle = tokio::spawn(async move {
            tracing::info!("Starting MPRIS discovery and signal watch");
            Self::do_scan(
                &conn_clone,
                &sources_clone,
                &active_clone,
                &sender_clone,
                &watch_handles_clone,
                &cached_track_clone,
                &cached_state_clone,
            )
            .await;

            let dbus_proxy = match Proxy::new(
                &conn_clone,
                "org.freedesktop.DBus",
                DBUS_PATH,
                "org.freedesktop.DBus",
            )
            .await
            {
                Ok(proxy) => Some(proxy),
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to create D-Bus proxy for NameOwnerChanged");
                    None
                }
            };
            let mut name_stream = if let Some(proxy) = dbus_proxy {
                proxy.receive_signal("NameOwnerChanged").await.ok()
            } else {
                None
            };

            while let Some(signal) = async {
                if let Some(ref mut stream) = name_stream {
                    futures::StreamExt::next(stream).await
                } else {
                    futures::future::pending().await
                }
            }
            .await
            {
                match signal.body().deserialize::<(String, String, String)>() {
                    Ok((name, _old, _new)) => {
                        if name.starts_with(MPRIS_PREFIX) {
                            tracing::debug!(name=%name, "MPRIS name changed, rescanning");
                            Self::do_scan(
                                &conn_clone,
                                &sources_clone,
                                &active_clone,
                                &sender_clone,
                                &watch_handles_clone,
                                &cached_track_clone,
                                &cached_state_clone,
                            )
                            .await;
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "Failed to parse NameOwnerChanged signal"),
                }
            }
        });

        let db_path = MusicStatsDb::db_path();
        let (initial_count, initial_albums, initial_ts) = tokio::task::spawn_blocking(move || {
            MusicStatsDb::new()
                .map(|db| db.get_stats())
                .unwrap_or((0, 0, 0))
        })
        .await
        .unwrap_or((0, 0, 0));

        let stats_album_count: Arc<RwLock<usize>> = Arc::new(RwLock::new(initial_albums));
        let stats_track_count: Arc<RwLock<usize>> = Arc::new(RwLock::new(initial_count));
        let stats_last_scanned: Arc<RwLock<u64>> = Arc::new(RwLock::new(initial_ts));

        Ok(Self {
            connection: Some(connection),
            sources,
            active_source,
            event_sender,
            _scan_handle: Some(_scan_handle),
            _watch_handles: watch_handles,
            cached_track,
            cached_state,
            db_path: Some(db_path),
            stats_album_count,
            stats_track_count,
            stats_last_scanned,
            player,
        })
    }

    async fn do_scan(
        conn: &Arc<Connection>,
        sources_lock: &Arc<RwLock<Vec<Arc<dyn MediaSource>>>>,
        active_lock: &Arc<RwLock<Option<Arc<dyn MediaSource>>>>,
        sender: &broadcast::Sender<MediaEvent>,
        watch_handles: &Arc<RwLock<Vec<tokio::task::JoinHandle<()>>>>,
        cached_track: &Arc<RwLock<Option<TrackInfo>>>,
        cached_state: &Arc<RwLock<PlaybackState>>,
    ) {
        tracing::debug!("Scanning session D-Bus for MPRIS players");
        let names = Self::list_mpris_names_static(conn).await;
        tracing::info!(count = names.len(), players = ?names, "MPRIS discovery result");

        {
            let mut sources_guard = sources_lock.write().await;

            // Bus-name-derived ids of players that are currently live on the bus.
            let live_ids: std::collections::HashSet<String> = names
                .iter()
                .map(|n| n.strip_prefix(MPRIS_PREFIX).unwrap_or(n).to_string())
                .collect();

            // Drop any MPRIS source whose player is no longer present.
            // Never drop the local player — it's not part of `names` at all.
            sources_guard.retain(|s| {
                s.id() == crate::player::LOCAL_PLAYER_ID || live_ids.contains(s.id())
            });

            let existing_ids: std::collections::HashSet<String> =
                sources_guard.iter().map(|s| s.id().to_string()).collect();
            for bus_name in &names {
                let candidate_id = bus_name.strip_prefix(MPRIS_PREFIX).unwrap_or(bus_name);
                if !existing_ids.contains(candidate_id) {
                    match MprisAdapter::new(bus_name.clone(), conn.clone()).await {
                        Ok(adapter) => {
                            tracing::info!(bus_name = %bus_name, display_name = %adapter.display_name(), "Created MPRIS adapter");
                            sources_guard.push(Arc::new(adapter) as Arc<dyn MediaSource>);
                        }
                        Err(e) => tracing::warn!(bus_name=%bus_name, error=%e, "Failed to create adapter"),
                    }
                }
            }
        }

        let mut playing_idx: Option<usize> = None;
        let mut paused_idx: Option<usize> = None;
        let mut cached_states: Vec<PlaybackState> = Vec::new();
        let mut cached_tracks: Vec<Option<TrackInfo>> = Vec::new();

        let sources_snapshot = sources_lock.read().await.clone();
        for (i, src) in sources_snapshot.iter().enumerate() {
            let state = match timeout(MPRIS_CALL_TIMEOUT, src.get_state()).await {
                Ok(s) => s,
                Err(_) => {
                    tracing::warn!(source = %src.display_name(), "get_state() timed out");
                    PlaybackState::Stopped
                }
            };
            let track = match timeout(MPRIS_CALL_TIMEOUT, src.get_track()).await {
                Ok(t) => t,
                Err(_) => {
                    tracing::warn!(source = %src.display_name(), "get_track() timed out");
                    None
                }
            };
            cached_states.push(state.clone());
            cached_tracks.push(track.clone());
            tracing::debug!(index = i, source = %src.display_name(), ?state, has_track = track.is_some(), "Cached source state");
            match state {
                PlaybackState::Playing if playing_idx.is_none() => playing_idx = Some(i),
                PlaybackState::Paused if paused_idx.is_none() => paused_idx = Some(i),
                _ => {}
            }
            if playing_idx.is_some() {
                break;
            }
        }

        let new_active = playing_idx
            .or(paused_idx)
            .and_then(|idx| sources_snapshot.get(idx).cloned());
        tracing::info!(active = ?new_active.as_ref().map(|source| source.display_name()), "Selected active source");

        *active_lock.write().await = new_active.clone();

        if let Some(_active) = new_active {
            if let Some(idx) = playing_idx.or(paused_idx) {
                let state = cached_states
                    .get(idx)
                    .cloned()
                    .unwrap_or(PlaybackState::Stopped);
                *cached_state.write().await = state.clone();
                let _ = sender.send(MediaEvent::StateChanged(state));
                if let Some(track) = cached_tracks.get(idx).cloned().flatten() {
                    *cached_track.write().await = Some(track.clone());
                    let _ = sender.send(MediaEvent::TrackChanged(track));
                }
            }
        } else {
            *cached_state.write().await = PlaybackState::Stopped;
            let _ = sender.send(MediaEvent::StateChanged(PlaybackState::Stopped));
            *cached_track.write().await = None;
        }

        let _ = sender.send(MediaEvent::SourceListChanged);

        let mut old_handles = watch_handles.write().await;
        for handle in old_handles.drain(..) {
            handle.abort();
        }
        tracing::debug!(count = sources_snapshot.len(), "Installed source event forwarders");
        let mut handles = Vec::new();
        for src in sources_snapshot.iter().cloned() {
            let sender_clone = sender.clone();
            let src_id = src.id().to_string();
            let cached_track_fwd = cached_track.clone();
            let cached_state_fwd = cached_state.clone();
            let active_fwd = active_lock.clone();
            let sources_fwd = sources_lock.clone();
            let sender_for_task = sender.clone();
            let handle = tokio::spawn(async move {
                let mut rx = src.subscribe();
                loop {
                    match rx.recv().await {
                        Ok(event) => {
                            match &event {
                                MediaEvent::StateChanged(state) => {
                                    *cached_state_fwd.write().await = state.clone();
                                    let current_active = active_fwd.read().await.clone();
                                    let active_id = current_active.as_ref().map(|s| s.id());
                                    if active_id.is_none() || active_id == Some(src_id.as_str()) {
                                        *active_fwd.write().await = Some(src.clone());
                                    }
                                    Self::reselect_active_inner(
                                        &sources_fwd,
                                        &active_fwd,
                                        &cached_state_fwd,
                                        &cached_track_fwd,
                                        sender_for_task.clone(),
                                        Some(&src_id),
                                    )
                                    .await;
                                }
                                MediaEvent::TrackChanged(track) => {
                                    *cached_track_fwd.write().await = Some(track.clone());
                                    Self::reselect_active_inner(
                                        &sources_fwd,
                                        &active_fwd,
                                        &cached_state_fwd,
                                        &cached_track_fwd,
                                        sender_for_task.clone(),
                                        Some(&src_id),
                                    )
                                    .await;
                                }
                                MediaEvent::SourceListChanged => {},
                                MediaEvent::StatsUpdated { .. } => {},
                                MediaEvent::PlaybackPosition { .. } => {},
                                MediaEvent::VolumeChanged(_) => {},
                                MediaEvent::TrackFinished => {},
                            }
                            let _ = sender_clone.send(event);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            tracing::warn!(
                                source = %src_id,
                                skipped,
                                "Source event forwarder lagged; continuing"
                            );
                            continue;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
            handles.push(handle);
        }
        *old_handles = handles;
    }

    async fn list_mpris_names_static(conn: &Connection) -> Vec<String> {
        let proxy = match Proxy::new(
            conn,
            "org.freedesktop.DBus",
            DBUS_PATH,
            "org.freedesktop.DBus",
        )
        .await
        {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to create D-Bus proxy for ListNames");
                return Vec::new();
            }
        };
        let names: Vec<String> = match proxy.call("ListNames", &()).await {
            Ok(names) => names,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to list session D-Bus names");
                Vec::new()
            }
        };
        names
            .into_iter()
            .filter(|name| name.starts_with(MPRIS_PREFIX))
            .collect()
    }

    pub async fn scan(&self) {
        if let Some(conn) = self.connection.as_ref() {
            tracing::info!("Manual MPRIS scan requested");
            let sources = self.sources.clone();
            let active = self.active_source.clone();
            let sender = self.event_sender.clone();
            let watch_handles = self._watch_handles.clone();
            let cached_track = self.cached_track.clone();
            let cached_state = self.cached_state.clone();
            Self::do_scan(
                conn,
                &sources,
                &active,
                &sender,
                &watch_handles,
                &cached_track,
                &cached_state,
            )
            .await;
        } else {
            tracing::warn!("Manual MPRIS scan skipped; manager has no D-Bus connection");
        }
    }

    async fn reselect_active_inner(
        sources_lock: &Arc<RwLock<Vec<Arc<dyn MediaSource>>>>,
        active_lock: &Arc<RwLock<Option<Arc<dyn MediaSource>>>>,
        cached_state: &Arc<RwLock<PlaybackState>>,
        cached_track: &Arc<RwLock<Option<TrackInfo>>>,
        sender: broadcast::Sender<MediaEvent>,
        priority_source_id: Option<&str>,
    ) {
        let sources = sources_lock.read().await.clone();
        let mut playing_idx: Option<usize> = None;
        let mut paused_idx: Option<usize> = None;

        for (i, src) in sources.iter().enumerate() {
            let state = match timeout(MPRIS_CALL_TIMEOUT, src.get_state()).await {
                Ok(s) => s,
                Err(_) => {
                    tracing::warn!(source = %src.display_name(), "get_state() timed out in reselect_active_inner");
                    PlaybackState::Stopped
                }
            };
            match state {
                PlaybackState::Playing if playing_idx.is_none() => playing_idx = Some(i),
                PlaybackState::Paused if paused_idx.is_none() => paused_idx = Some(i),
                _ => {}
            }
            if playing_idx.is_some() {
                break;
            }
        }

        let new_active = playing_idx
            .or(paused_idx)
            .and_then(|idx| sources.get(idx).cloned());
        let current_active = active_lock.read().await.clone();

        let new_active = if new_active.is_none() {
            let mut found: Option<Arc<dyn MediaSource>> = None;
            if let Some(priority_id) = priority_source_id {
                found = sources.iter().find(|s| s.id() == priority_id).cloned();
            }
            found
                .or_else(|| sources.iter().find(|s| s.id() == crate::player::LOCAL_PLAYER_ID).cloned())
                .or_else(|| sources.first().cloned())
        } else {
            new_active
        };

        let new_active_id = new_active.as_ref().map(|s| s.id());
        let current_active_id = current_active.as_ref().map(|s| s.id());
        tracing::debug!(current = ?current_active_id, candidate = ?new_active_id, "Active source comparison");

        if new_active_id != current_active_id {
            *active_lock.write().await = new_active.clone();
            tracing::info!(source = ?new_active.as_ref().map(|source| source.display_name()), "Active source changed");
            if let Some(src) = new_active {
                let state = match timeout(MPRIS_CALL_TIMEOUT, src.get_state()).await {
                    Ok(s) => s,
                    Err(_) => {
                        tracing::warn!(source = %src.display_name(), "get_state() timed out");
                        PlaybackState::Stopped
                    }
                };
                *cached_state.write().await = state.clone();
                let _ = sender.send(MediaEvent::StateChanged(state));
                let track = match timeout(MPRIS_CALL_TIMEOUT, src.get_track()).await {
                    Ok(t) => t,
                    Err(_) => {
                        tracing::warn!(source = %src.display_name(), "get_track() timed out");
                        None
                    }
                };
                if let Some(t) = track {
                    *cached_track.write().await = Some(t.clone());
                    let _ = sender.send(MediaEvent::TrackChanged(t));
                }
            }
        }
    }

    pub async fn reselect_active(&self) {
        tracing::debug!("Re-evaluating active source");
        Self::reselect_active_inner(
            &self.sources,
            &self.active_source,
            &self.cached_state,
            &self.cached_track,
            self.event_sender.clone(),
            None,
        )
        .await;
    }

    pub async fn route(&self, cmd: AppMessage) -> anyhow::Result<()> {
        match cmd {
            AppMessage::ScanMusic => {
                tracing::info!("ScanMusic requested");
                self.scan_music().await
            }
            AppMessage::PlayTrack(path) => {
                if let Some(ref player) = self.player {
                    tracing::info!(path = %path, "Playing local track");
                    player.play_track(&path).await
                } else {
                    tracing::warn!("No local player available for PlayTrack");
                    Err(AppError::Mpris("Local player not available".to_string()).into())
                }
            }
            AppMessage::SetPlaylist(tracks) => {
                if let Some(ref player) = self.player {
                    player.set_playlist(tracks).await;
                }
                Ok(())
            }
            AppMessage::ToggleFavorite(path) => {
                if let Some(ref player) = self.player {
                    player.toggle_favorite(&path).await
                } else {
                    let db = MusicStatsDb::new()?;
                    db.toggle_favorite(&path)
                }
            }
            AppMessage::Previous => {
                let active = self.active_source.read().await.clone();
                let source = active.ok_or_else(|| AppError::NoActiveSource)?;
                tracing::info!(source = %source.display_name(), command = ?cmd, "Routing media command");
                let result = source.previous().await;
                self.log_command_result(&result, &source, &cmd);
                result
            }
            AppMessage::PlayPause => {
                let active = self.active_source.read().await.clone();
                let source = active.ok_or_else(|| AppError::NoActiveSource)?;
                tracing::info!(source = %source.display_name(), command = ?cmd, "Routing media command");
                let result = source.play_pause().await;
                self.log_command_result(&result, &source, &cmd);
                result
            }
            AppMessage::Next => {
                let active = self.active_source.read().await.clone();
                let source = active.ok_or_else(|| AppError::NoActiveSource)?;
                tracing::info!(source = %source.display_name(), command = ?cmd, "Routing media command");
                let result = source.next().await;
                self.log_command_result(&result, &source, &cmd);
                result
            }
            AppMessage::Stop => {
                let active = self.active_source.read().await.clone();
                let source = active.ok_or_else(|| AppError::NoActiveSource)?;
                tracing::info!(source = %source.display_name(), command = ?cmd, "Routing media command");
                let result = source.stop().await;
                self.log_command_result(&result, &source, &cmd);
                result
            }
            AppMessage::SetPosition(pos_ms) => {
                let active = self.active_source.read().await.clone();
                let source = active.ok_or_else(|| AppError::NoActiveSource)?;
                tracing::info!(source = %source.display_name(), pos_ms, "Routing set position command");
                let result = source.set_position(pos_ms).await;
                self.log_command_result(&result, &source, &cmd);
                result
            }
            AppMessage::SetVolume(vol) => {
                let active = self.active_source.read().await.clone();
                let source = active.ok_or_else(|| AppError::NoActiveSource)?;
                tracing::info!(source = %source.display_name(), volume = vol, "Routing set volume command");
                let result = source.set_volume(vol).await;
                self.log_command_result(&result, &source, &cmd);
                result
            }
        }
    }

    fn log_command_result(&self, result: &anyhow::Result<()>, source: &Arc<dyn MediaSource>, cmd: &AppMessage) {
        match result {
            Ok(()) => tracing::debug!(source = %source.display_name(), command = ?cmd, "Media command accepted"),
            Err(e) => tracing::warn!(source = %source.display_name(), command = ?cmd, error = %e, "Media command rejected"),
        }
    }

    pub async fn scan_music(&self) -> anyhow::Result<()> {
        tracing::info!("Starting music folder scan");
        let music_dir = music_folder();
        let _db_path = self
            .db_path
            .clone()
            .ok_or_else(|| AppError::Scan("Database not initialized".to_string()))?;

        let (track_count, album_count) = tokio::task::spawn_blocking(move || {
            MusicStatsDb::scan_music_folder(music_dir.as_path())
        })
        .await
        .map_err(|e| AppError::Scan(format!("scan task failed: {e}")))?;

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        *self.stats_album_count.write().await = album_count;
        *self.stats_track_count.write().await = track_count;
        *self.stats_last_scanned.write().await = timestamp;

        tokio::task::spawn_blocking(move || {
            MusicStatsDb::new()
                .and_then(|db| db.set_stats(track_count, album_count, timestamp))
        })
        .await
        .map_err(|e| AppError::Scan(format!("set_stats task failed: {e}")))??;

        let _ = self.event_sender.send(MediaEvent::StatsUpdated {
            track_count,
            album_count,
            last_scanned: timestamp,
        });
        tracing::info!(track_count, album_count, last_scanned = timestamp, "Music scan complete");
        Ok(())
    }

    pub async fn get_stats(&self) -> (usize, usize, u64) {
        let albums = *self.stats_album_count.read().await;
        let count = *self.stats_track_count.read().await;
        let ts = *self.stats_last_scanned.read().await;
        (albums, count, ts)
    }

    pub fn cached_stats(&self) -> (usize, usize, u64) {
        let albums = match self.stats_album_count.try_read() {
            Ok(g) => *g,
            Err(_) => 0,
        };
        let count = match self.stats_track_count.try_read() {
            Ok(g) => *g,
            Err(_) => 0,
        };
        let ts = match self.stats_last_scanned.try_read() {
            Ok(g) => *g,
            Err(_) => 0,
        };
        (albums, count, ts)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<MediaEvent> {
        self.event_sender.subscribe()
    }

    pub fn event_sender(&self) -> broadcast::Sender<MediaEvent> {
        self.event_sender.clone()
    }

    pub async fn sources_snapshot(&self) -> Vec<Arc<dyn MediaSource>> {
        self.sources.read().await.clone()
    }

    pub async fn active_source_cloned(&self) -> Option<Arc<dyn MediaSource>> {
        self.active_source.read().await.clone()
    }

    pub async fn active_display_name(&self) -> String {
        self.active_source
            .read()
            .await
            .as_ref()
            .map(|s| s.display_name().to_string())
            .unwrap_or_default()
    }

    pub async fn active_track(&self) -> Option<TrackInfo> {
        let active = self.active_source.read().await.clone();
        match active {
            Some(s) => {
                let track = s.get_track().await;
                tracing::debug!(source = %s.display_name(), has_track = track.is_some(), "Read active track");
                track
            }
            None => {
                tracing::warn!("No active source when reading track");
                None
            }
        }
    }

    pub async fn active_state(&self) -> PlaybackState {
        let active = self.active_source.read().await.clone();
        match active {
            Some(s) => {
                let state = s.get_state().await;
                tracing::debug!(source = %s.display_name(), ?state, "Read active playback state");
                state
            }
            None => {
                tracing::warn!("No active source when reading state");
                PlaybackState::Stopped
            }
        }
    }

    pub fn cached_state(&self) -> (Option<TrackInfo>, PlaybackState) {
        let track = self.cached_track.try_read().ok().and_then(|g| g.clone());
        let state = self
            .cached_state
            .try_read()
            .ok()
            .map(|g| g.clone())
            .unwrap_or(PlaybackState::Stopped);
        tracing::debug!(track = ?track, state = ?state, "Read cached state");
        (track, state)
    }

    pub fn player(&self) -> Option<Arc<PlayerAdapter>> {
        self.player.clone()
    }

    pub async fn all_tracks(&self) -> anyhow::Result<Vec<crate::music_db::TrackStat>> {
        self.all_tracks_sorted(crate::music_db::SortMode::PlayCountDesc).await
    }

    pub async fn all_tracks_sorted(&self, sort_mode: crate::music_db::SortMode) -> anyhow::Result<Vec<crate::music_db::TrackStat>> {
        if let Some(ref player) = self.player {
            player.all_tracks_sorted(sort_mode).await
        } else {
            let db = MusicStatsDb::new()?;
            db.get_tracks_sorted(sort_mode)
        }
    }

    pub async fn search_tracks(&self, query: &str) -> anyhow::Result<Vec<crate::music_db::TrackStat>> {
        if let Some(ref player) = self.player {
            player.search(query).await
        } else {
            let db = MusicStatsDb::new()?;
            db.search_tracks(query)
        }
    }

    pub async fn get_albums(&self) -> anyhow::Result<Vec<crate::music_db::AlbumInfo>> {
        self.get_albums_sorted(crate::music_db::SortMode::NameAsc).await
    }

    pub async fn get_albums_sorted(&self, sort_mode: crate::music_db::SortMode) -> anyhow::Result<Vec<crate::music_db::AlbumInfo>> {
        if let Some(ref player) = self.player {
            player.get_albums_sorted(sort_mode).await
        } else {
            let db = MusicStatsDb::new()?;
            db.get_albums_sorted(sort_mode)
        }
    }

    pub async fn get_album_tracks(&self, album: &str) -> anyhow::Result<Vec<crate::music_db::TrackStat>> {
        if let Some(ref player) = self.player {
            player.get_album_tracks(album).await
        } else {
            let db = MusicStatsDb::new()?;
            db.get_tracks_in_album(album)
        }
    }
}

impl Drop for MediaSourceManager {
    fn drop(&mut self) {
        if let Some(handle) = self._scan_handle.take() {
            handle.abort();
        }
    }
}
