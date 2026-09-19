use crate::error::AppError;
use crate::message::AppMessage;
use crate::mpris::{MediaEvent, MediaSource, PlaybackState, TrackInfo};
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast};
use zbus::{Connection, Proxy};

use super::mpris::MprisAdapter;

const DBUS_PATH: &str = "/org/freedesktop/DBus";
const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";

pub struct MediaSourceManager {
    connection: Option<Arc<Connection>>,
    sources: Arc<RwLock<Vec<Arc<dyn MediaSource>>>>,
    active_source: Arc<RwLock<Option<Arc<dyn MediaSource>>>>,
    event_sender: broadcast::Sender<MediaEvent>,
    _scan_handle: Option<tokio::task::JoinHandle<()>>,
    _watch_handles: Arc<RwLock<Vec<tokio::task::JoinHandle<()>>>>,
    cached_track: Arc<RwLock<Option<TrackInfo>>>,
    cached_state: Arc<RwLock<PlaybackState>>,
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
        let cached_state: Arc<RwLock<PlaybackState>> = Arc::new(RwLock::new(PlaybackState::Stopped));

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

        Ok(Self {
            connection: Some(connection),
            sources,
            active_source,
            event_sender,
            _scan_handle: Some(_scan_handle),
            _watch_handles: watch_handles,
            cached_track,
            cached_state,
        })
    }

    async fn do_scan(
        conn: &Arc<Connection>,
        sources_lock: &Arc<RwLock<Vec<Arc<dyn MediaSource>>>>,
        active_lock: &Arc<RwLock<Option<Arc<dyn MediaSource>>>>,
        sender: &broadcast::Sender<MediaEvent>,
        watch_handles: &Arc<RwLock<Vec<tokio::task::JoinHandle<()>>>>,
        cached_track: &RwLock<Option<TrackInfo>>,
        cached_state: &RwLock<PlaybackState>,
    ) {
        tracing::debug!("Scanning session D-Bus for MPRIS players");
        let names = Self::list_mpris_names_static(conn).await;
        tracing::info!(count = names.len(), players = ?names, "MPRIS discovery result");

        let mut new_sources: Vec<Arc<dyn MediaSource>> = Vec::new();
        for bus_name in names {
            match MprisAdapter::new(bus_name.clone(), conn.clone()).await {
                Ok(adapter) => {
                    tracing::info!(bus_name = %bus_name, display_name = %adapter.display_name(), "Created MPRIS adapter");
                    new_sources.push(Arc::new(adapter) as Arc<dyn MediaSource>);
                }
                Err(e) => tracing::warn!(bus_name=%bus_name, error=%e, "Failed to create adapter"),
            }
        }

        let mut playing_idx: Option<usize> = None;
        let mut paused_idx: Option<usize> = None;
        let mut cached_states: Vec<PlaybackState> = Vec::with_capacity(new_sources.len());
        let mut cached_tracks: Vec<Option<TrackInfo>> = Vec::with_capacity(new_sources.len());

        for (i, src) in new_sources.iter().enumerate() {
            let state = src.get_state().await;
            let track = src.get_track().await;
            cached_states.push(state.clone());
            cached_tracks.push(track.clone());
            tracing::debug!(index = i, source = %src.display_name(), ?state, has_track = track.is_some(), "Cached MPRIS source state");
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
            .and_then(|idx| new_sources.get(idx).cloned());
        tracing::info!(active = ?new_active.as_ref().map(|source| source.display_name()), "Selected active MPRIS source");

        *sources_lock.write().await = new_sources;
        *active_lock.write().await = new_active.clone();

        let _ = sender.send(MediaEvent::SourceListChanged);

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
        }

        let mut handles = Vec::new();
        let sources: Vec<Arc<dyn MediaSource>> = sources_lock.read().await.clone();
        for src in sources.iter().cloned() {
            let sender_clone = sender.clone();
            let handle = tokio::spawn(async move {
                let mut rx = src.subscribe();
                while let Ok(event) = rx.recv().await {
                    let _ = sender_clone.send(event);
                }
            });
            handles.push(handle);
        }

        let mut old_handles = watch_handles.write().await;
        for handle in old_handles.drain(..) {
            handle.abort();
        }
        tracing::debug!(count = handles.len(), "Installed MPRIS event forwarders");
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
            Self::do_scan(conn, &sources, &active, &sender, &watch_handles, &cached_track, &cached_state).await;
        } else {
            tracing::warn!("Manual MPRIS scan skipped; manager has no D-Bus connection");
        }
    }

    pub async fn reselect_active(&self) {
        tracing::debug!("Re-evaluating active MPRIS source");
        let sources = self.sources.read().await.clone();
        let mut playing_idx: Option<usize> = None;
        let mut paused_idx: Option<usize> = None;

        for (i, src) in sources.iter().enumerate() {
            let state = src.get_state().await;
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
        let current_active = self.active_source.read().await.clone();

        let new_active_id = new_active.as_ref().map(|s| s.id());
        let current_active_id = current_active.as_ref().map(|s| s.id());
        tracing::debug!(current = ?current_active_id, candidate = ?new_active_id, "Active source comparison");

        if new_active_id != current_active_id {
            *self.active_source.write().await = new_active.clone();
            tracing::info!(source = ?new_active.as_ref().map(|source| source.display_name()), "Active MPRIS source changed");
            if let Some(src) = new_active {
                let state = src.get_state().await;
                let _ = self.event_sender.send(MediaEvent::StateChanged(state));
                let track = src.get_track().await;
                if let Some(t) = track {
                    let _ = self.event_sender.send(MediaEvent::TrackChanged(t));
                }
            }
        }
    }

    pub async fn route(&self, cmd: AppMessage) -> anyhow::Result<()> {
        let active = self.active_source.read().await.clone();
        let source = active.ok_or_else(|| AppError::NoActiveSource)?;
        tracing::info!(source = %source.display_name(), command = ?cmd, "Routing media command");
        let result = match cmd {
            AppMessage::Previous => source.previous().await,
            AppMessage::PlayPause => source.play_pause().await,
            AppMessage::Next => source.next().await,
        };
        match &result {
            Ok(()) => {
                tracing::debug!(source = %source.display_name(), command = ?cmd, "Media command accepted")
            }
            Err(e) => {
                tracing::warn!(source = %source.display_name(), command = ?cmd, error = %e, "Media command rejected")
            }
        }
        result
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
        let state = self.cached_state.try_read().ok().map(|g| g.clone()).unwrap_or(PlaybackState::Stopped);
        tracing::debug!(track = ?track, state = ?state, "Read cached state");
        (track, state)
    }
}

impl Drop for MediaSourceManager {
    fn drop(&mut self) {
        if let Some(handle) = self._scan_handle.take() {
            handle.abort();
        }
    }
}
