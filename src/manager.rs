use crate::error::AppError;
use crate::mpris::{MediaEvent, MediaSource, PlaybackState, TrackInfo};
use crate::message::AppMessage;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
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
        }
    }

    pub async fn new() -> anyhow::Result<Self> {
        let connection = Arc::new(Connection::session().await?);
        let (event_sender, _) = broadcast::channel(64);
        let sources: Arc<RwLock<Vec<Arc<dyn MediaSource>>>> = Arc::new(RwLock::new(Vec::new()));
        let active_source: Arc<RwLock<Option<Arc<dyn MediaSource>>>> = Arc::new(RwLock::new(None));
        let watch_handles: Arc<RwLock<Vec<tokio::task::JoinHandle<()>>>> =
            Arc::new(RwLock::new(Vec::new()));

        let conn_clone = connection.clone();
        let sources_clone = sources.clone();
        let active_clone = active_source.clone();
        let sender_clone = event_sender.clone();
        let watch_handles_clone = watch_handles.clone();

        let _scan_handle = tokio::spawn(async move {
            Self::do_scan(
                &conn_clone,
                &sources_clone,
                &active_clone,
                &sender_clone,
                &watch_handles_clone,
            )
            .await;

            let dbus_proxy = Proxy::new(&conn_clone, "org.freedesktop.DBus", DBUS_PATH, "org.freedesktop.DBus").await.ok();
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
            .await {
                if let Ok(result) = signal.body().deserialize::<(String, String, String)>() {
                    let (name, _old, _new) = result;
                    if name.starts_with(MPRIS_PREFIX) {
                        tracing::debug!(name=%name, "MPRIS name changed, rescanning");
                        Self::do_scan(
                            &conn_clone,
                            &sources_clone,
                            &active_clone,
                            &sender_clone,
                            &watch_handles_clone,
                        )
                        .await;
                    }
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
        })
    }

    async fn do_scan(
        conn: &Arc<Connection>,
        sources_lock: &Arc<RwLock<Vec<Arc<dyn MediaSource>>>>,
        active_lock: &Arc<RwLock<Option<Arc<dyn MediaSource>>>>,
        sender: &broadcast::Sender<MediaEvent>,
        watch_handles: &Arc<RwLock<Vec<tokio::task::JoinHandle<()>>>>,
    ) {
        let names = Self::list_mpris_names_static(conn).await;
        tracing::debug!(count = names.len(), "Found MPRIS players: {:?}", names);

        let mut new_sources: Vec<Arc<dyn MediaSource>> = Vec::new();
        for bus_name in names {
            match MprisAdapter::new(bus_name.clone(), conn.clone()).await {
                Ok(adapter) => new_sources.push(Arc::new(adapter) as Arc<dyn MediaSource>),
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
            match state {
                PlaybackState::Playing if playing_idx.is_none() => playing_idx = Some(i),
                PlaybackState::Paused if paused_idx.is_none() => paused_idx = Some(i),
                _ => {}
            }
            if playing_idx.is_some() { break; }
        }

        let new_active = playing_idx.or(paused_idx).and_then(|idx| new_sources.get(idx).cloned());

        *sources_lock.write().await = new_sources;
        *active_lock.write().await = new_active.clone();

        let _ = sender.send(MediaEvent::SourceListChanged);

        if let Some(_active) = new_active {
            if let Some(idx) = playing_idx.or(paused_idx) {
                let state = cached_states.get(idx).cloned().unwrap_or(PlaybackState::Stopped);
                let _ = sender.send(MediaEvent::StateChanged(state));
                if let Some(track) = cached_tracks.get(idx).cloned().flatten() {
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
        *old_handles = handles;
    }

    async fn list_mpris_names_static(conn: &Connection) -> Vec<String> {
        let proxy = match Proxy::new(conn, "org.freedesktop.DBus", DBUS_PATH, "org.freedesktop.DBus").await {
            Ok(p) => p,
            Err(_) => return Vec::new(),
        };
        let names: Vec<String> = match proxy.call("ListNames", &()).await {
            Ok(n) => n,
            Err(_) => return Vec::new(),
        };
        names.into_iter().filter(|n| n.starts_with(MPRIS_PREFIX)).collect()
    }

    pub async fn scan(&self) {
        if let Some(conn) = self.connection.as_ref() {
            let sources = self.sources.clone();
            let active = self.active_source.clone();
            let sender = self.event_sender.clone();
            let watch_handles = self._watch_handles.clone();
            Self::do_scan(conn, &sources, &active, &sender, &watch_handles).await;
        }
    }

    pub async fn reselect_active(&self) {
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
            if playing_idx.is_some() { break; }
        }

        let new_active = playing_idx.or(paused_idx).and_then(|idx| sources.get(idx).cloned());
        let current_active = self.active_source.read().await.clone();

        let new_active_id = new_active.as_ref().map(|s| s.id());
        let current_active_id = current_active.as_ref().map(|s| s.id());

        if new_active_id != current_active_id {
            *self.active_source.write().await = new_active.clone();
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
        match cmd {
            AppMessage::Previous => source.previous().await,
            AppMessage::PlayPause => source.play_pause().await,
            AppMessage::Next => source.next().await,
        }
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
        self.active_source.read().await.as_ref().map(|s| s.display_name().to_string()).unwrap_or_default()
    }

    pub async fn active_track(&self) -> Option<TrackInfo> {
        let active = self.active_source.read().await.clone();
        match active {
            Some(s) => s.get_track().await,
            None => None,
        }
    }

    pub async fn active_state(&self) -> PlaybackState {
        let active = self.active_source.read().await.clone();
        match active {
            Some(s) => s.get_state().await,
            None => PlaybackState::Stopped,
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
