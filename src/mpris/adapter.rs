use super::{MediaEvent, MediaSource, PlaybackState, TrackInfo};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use zbus::fdo::PropertiesProxy;
use zbus::{Connection, Proxy};
use zvariant::Value;

const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
const MPRIS_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";

/// Adapter that wraps an MPRIS2 player and implements the `MediaSource` trait.
pub struct MprisAdapter {
    bus_name: String,
    display_name: String,
    source_id: String,
    connection: Arc<Connection>,
    event_sender: broadcast::Sender<MediaEvent>,
    state: Arc<Mutex<PlaybackState>>,
    track: Arc<Mutex<Option<TrackInfo>>>,
    _watch_handle: tokio::task::JoinHandle<()>,
}

impl MprisAdapter {
    /// Creates a new adapter for the given MPRIS bus name.
    pub async fn new(bus_name: String, connection: Arc<Connection>) -> anyhow::Result<Self> {
        tracing::debug!(bus_name = %bus_name, "Creating MPRIS adapter");
        let proxy = Proxy::new(&connection, bus_name.as_str(), MPRIS_PATH, MPRIS_INTERFACE).await?;
        let display_name = bus_name
            .strip_prefix("org.mpris.MediaPlayer2.")
            .unwrap_or(&bus_name)
            .to_string();
        let source_id = display_name.clone();
        let (event_sender, _) = broadcast::channel(32);

        let state = Arc::new(Mutex::new(match proxy.get_property::<String>("PlaybackStatus").await {
            Ok(s) => match s.as_str() {
                "Playing" => PlaybackState::Playing,
                "Paused" => PlaybackState::Paused,
                _ => PlaybackState::Stopped,
            },
            Err(e) => {
                tracing::warn!(bus_name = %bus_name, error = %e, "Failed to read initial PlaybackStatus");
                PlaybackState::Stopped
            }
        }));
        let track = Arc::new(Mutex::new(Self::fetch_track(&proxy, &source_id).await));
        {
            let state_guard = state.lock().unwrap_or_else(|e| e.into_inner());
            let track_guard = track.lock().unwrap_or_else(|e| e.into_inner());
            tracing::info!(bus_name = %bus_name, state = ?*state_guard, has_track = track_guard.is_some(), "Initialized MPRIS adapter");
        }

        let conn_clone = connection.clone();
        let sender_clone = event_sender.clone();
        let bus_name_clone = bus_name.clone();
        let track_clone = track.clone();
        let state_clone = state.clone();

        let _watch_handle = tokio::spawn(async move {
            let props_proxy = match PropertiesProxy::new(
                &conn_clone,
                bus_name_clone.as_str(),
                MPRIS_PATH,
            )
            .await
            {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(bus_name = %bus_name_clone, error = %e, "Failed to create PropertiesProxy");
                    return;
                }
            };
            let mut stream = match props_proxy.receive_properties_changed().await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(bus_name = %bus_name_clone, error = %e, "Failed to subscribe to PropertiesChanged");
                    return;
                }
            };
            tracing::info!(bus_name = %bus_name_clone, "Watching MPRIS PropertiesChanged signals");
            while let Some(signal) = futures::StreamExt::next(&mut stream).await {
                let args = match signal.args() {
                    Ok(a) => a,
                    Err(e) => {
                        tracing::warn!(bus_name = %bus_name_clone, error = %e, "Failed to parse PropertiesChanged signal args");
                        continue;
                    }
                };
                let iface = args.interface_name();
                if iface.as_str() != MPRIS_INTERFACE {
                    continue;
                }
                let changed = args.changed_properties();
                tracing::debug!(bus_name = %bus_name_clone, changed = ?changed.keys().collect::<Vec<_>>(), "MPRIS properties changed");
                for key in changed.keys() {
                    tracing::debug!(bus_name = %bus_name_clone, key = ?key, "Property key");
                }
                if let Some(v) = changed.get("PlaybackStatus") {
                    if let Some(s) = Self::extract_string(v) {
                        let ps = match s.as_str() {
                            "Playing" => PlaybackState::Playing,
                            "Paused" => PlaybackState::Paused,
                            _ => PlaybackState::Stopped,
                        };
                        tracing::info!(bus_name = %bus_name_clone, ?ps, "MPRIS playback state changed");
                        *state_clone.lock().unwrap_or_else(|e| e.into_inner()) = ps.clone();
                        let _ = sender_clone.send(MediaEvent::StateChanged(ps));
                    }
                }
                if let Some(v) = changed.get("Metadata") {
                    if let Value::Dict(dict) = v {
                        let mut map = HashMap::new();
                        for (k, vv) in dict.iter() {
                            if let Value::Str(ks) = k {
                                map.insert(ks.to_string(), vv.clone());
                            }
                        }
                        if let Some(t) = Self::metadata_to_track(&map, &bus_name_clone) {
                            tracing::info!(bus_name = %bus_name_clone, title = %t.title, artist = %t.artist, "MPRIS metadata changed");
                            *track_clone.lock().unwrap_or_else(|e| e.into_inner()) = Some(t.clone());
                            let _ = sender_clone.send(MediaEvent::TrackChanged(t));
                        }
                    } else {
                        tracing::debug!(bus_name = %bus_name_clone, "Metadata is not a Value::Dict");
                    }
                }
            }
            tracing::debug!(bus_name = %bus_name_clone, "MPRIS signal stream ended");
        });

        Ok(Self {
            bus_name,
            display_name,
            source_id,
            connection,
            event_sender,
            state,
            track,
            _watch_handle,
        })
    }

    fn extract_string(v: &Value<'_>) -> Option<String> {
        match v {
            Value::Str(s) => Some(s.to_string()),
            Value::Value(inner) => Self::extract_string(inner),
            _ => None,
        }
    }

    async fn fetch_track(proxy: &Proxy<'_>, source_id: &str) -> Option<TrackInfo> {
        let meta: HashMap<String, Value<'_>> = proxy.get_property("Metadata").await.ok()?;
        Self::metadata_to_track(&meta, source_id)
    }

    fn metadata_to_track(
        metadata: &HashMap<String, Value<'_>>,
        source_id: &str,
    ) -> Option<TrackInfo> {
        let title_val = metadata.get("xesam:title");
        let title = title_val
            .and_then(|v| Self::extract_string(v))
            .unwrap_or_default();
        let artist = metadata
            .get("xesam:artist")
            .and_then(|v| {
                let inner = match v {
                    Value::Value(iv) => iv,
                    other => other,
                };
                match inner {
                    Value::Array(arr) => Some(
                        arr.iter()
                            .filter_map(|x| Self::extract_string(x))
                            .collect::<Vec<_>>()
                            .join(", "),
                    ),
                    _ => Self::extract_string(inner),
                }
            })
            .unwrap_or_default();
        let album = metadata
            .get("xesam:album")
            .and_then(|v| Self::extract_string(v));
        if title.is_empty() && artist.is_empty() {
            return None;
        }
        Some(TrackInfo {
            title,
            artist,
            album,
            source_id: source_id.to_string(),
        })
    }
}

#[async_trait::async_trait]
impl MediaSource for MprisAdapter {
    fn id(&self) -> &str {
        &self.source_id
    }
    fn display_name(&self) -> &str {
        &self.display_name
    }
    async fn is_available(&self) -> bool {
        let proxy = match Proxy::new(
            &self.connection,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .await
        {
            Ok(p) => p,
            Err(e) => {
                tracing::debug!(bus_name = %self.bus_name, error = %e, "MPRIS availability check failed");
                return false;
            }
        };
        let available = proxy
            .call::<&str, &str, String>("GetNameOwner", &self.bus_name.as_str())
            .await
            .ok()
            .is_some();
        tracing::debug!(bus_name = %self.bus_name, available, "Checked MPRIS availability");
        available
    }
    async fn get_track(&self) -> Option<TrackInfo> {
        tracing::debug!(bus_name = %self.bus_name, "get_track: creating proxy");
        let proxy = Proxy::new(
            &self.connection,
            self.bus_name.as_str(),
            MPRIS_PATH,
            MPRIS_INTERFACE,
        )
        .await
        .ok()?;
        tracing::debug!(bus_name = %self.bus_name, "get_track: proxy created, fetching track");
        let mut t = Self::fetch_track(&proxy, &self.source_id)
            .await
            .or_else(|| {
                tracing::debug!(bus_name = %self.bus_name, "get_track: fetch failed, using cached track");
                self.track.lock().unwrap_or_else(|e| e.into_inner()).clone()
            })?;
        t.source_id = self.source_id.clone();
        tracing::debug!(bus_name = %self.bus_name, title = %t.title, "get_track: returning track");
        Some(t)
    }
    async fn get_state(&self) -> PlaybackState {
        tracing::debug!(bus_name = %self.bus_name, "get_state: creating proxy");
        let Ok(proxy) = Proxy::new(
            &self.connection,
            self.bus_name.as_str(),
            MPRIS_PATH,
            MPRIS_INTERFACE,
        )
        .await
        else {
            tracing::debug!(bus_name = %self.bus_name, "get_state: proxy creation failed, using cached");
            return self.state.lock().unwrap_or_else(|e| e.into_inner()).clone();
        };
        tracing::debug!(bus_name = %self.bus_name, "get_state: proxy created, fetching state");
        let Ok(s) = proxy.get_property::<String>("PlaybackStatus").await else {
            return self.state.lock().unwrap_or_else(|e| e.into_inner()).clone();
        };
        let ps = match s.as_str() {
            "Playing" => PlaybackState::Playing,
            "Paused" => PlaybackState::Paused,
            _ => PlaybackState::Stopped,
        };
         *self.state.lock().unwrap_or_else(|e| e.into_inner()) = ps.clone();
        ps
    }

    async fn play_pause(&self) -> anyhow::Result<()> {
        let p = Proxy::new(
            &self.connection,
            self.bus_name.as_str(),
            MPRIS_PATH,
            MPRIS_INTERFACE,
        )
        .await?;
        p.call::<&str, (), ()>("PlayPause", &()).await?;
        Ok(())
    }
    async fn next(&self) -> anyhow::Result<()> {
        let p = Proxy::new(
            &self.connection,
            self.bus_name.as_str(),
            MPRIS_PATH,
            MPRIS_INTERFACE,
        )
        .await?;
        p.call::<&str, (), ()>("Next", &()).await?;
        Ok(())
    }
    async fn previous(&self) -> anyhow::Result<()> {
        let p = Proxy::new(
            &self.connection,
            self.bus_name.as_str(),
            MPRIS_PATH,
            MPRIS_INTERFACE,
        )
        .await?;
        p.call::<&str, (), ()>("Previous", &()).await?;
        Ok(())
    }
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<MediaEvent> {
        self.event_sender.subscribe()
    }
}

impl Drop for MprisAdapter {
    fn drop(&mut self) {
        self._watch_handle.abort();
        tracing::debug!(bus_name = %self.bus_name, "Aborted MPRIS PropertiesChanged watch task");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_value_str(s: &str) -> Value<'_> {
        Value::Str(zvariant::Str::from(s))
    }

    fn make_value_array_str(items: Vec<&str>) -> Value<'_> {
        let arr: Vec<Value<'_>> = items.iter().map(|s| make_value_str(s)).collect();
        Value::Array(zvariant::Array::from(arr))
    }

    #[test]
    fn extract_string_from_value() {
        let v = make_value_str("hello");
        assert_eq!(MprisAdapter::extract_string(&v), Some("hello".to_string()));
    }

    #[test]
    fn extract_string_from_nested_value() {
        let inner = make_value_str("nested");
        let v = Value::Value(Box::new(inner));
        assert_eq!(MprisAdapter::extract_string(&v), Some("nested".to_string()));
    }

    #[test]
    fn extract_string_from_non_string() {
        let v = Value::Bool(true);
        assert_eq!(MprisAdapter::extract_string(&v), None);
    }

    #[test]
    fn metadata_to_track_full() {
        let mut metadata = HashMap::new();
        metadata.insert("xesam:title".to_string(), make_value_str("Bohemian Rhapsody"));
        metadata.insert("xesam:artist".to_string(), make_value_array_str(vec!["Queen"]));
        metadata.insert("xesam:album".to_string(), make_value_str("A Night at the Opera"));

        let track = MprisAdapter::metadata_to_track(&metadata, "test_source").unwrap();
        assert_eq!(track.title, "Bohemian Rhapsody");
        assert_eq!(track.artist, "Queen");
        assert_eq!(track.album, Some("A Night at the Opera".to_string()));
        assert_eq!(track.source_id, "test_source");
    }

    #[test]
    fn metadata_to_track_artist_array_multiple() {
        let mut metadata = HashMap::new();
        metadata.insert("xesam:title".to_string(), make_value_str("Under Pressure"));
        metadata.insert(
            "xesam:artist".to_string(),
            make_value_array_str(vec!["Queen", "David Bowie"]),
        );

        let track = MprisAdapter::metadata_to_track(&metadata, "test_source").unwrap();
        assert_eq!(track.artist, "Queen, David Bowie");
    }

    #[test]
    fn metadata_to_track_artist_as_variant_array() {
        let artist_arr = make_value_array_str(vec!["Radiohead"]);
        let wrapped = Value::Value(Box::new(artist_arr));
        let mut metadata = HashMap::new();
        metadata.insert("xesam:title".to_string(), make_value_str("OK Computer"));
        metadata.insert("xesam:artist".to_string(), wrapped);

        let track = MprisAdapter::metadata_to_track(&metadata, "test_source").unwrap();
        assert_eq!(track.artist, "Radiohead");
    }

    #[test]
    fn metadata_to_track_title_as_variant_string() {
        let title_str = make_value_str("Paranoid Android");
        let wrapped = Value::Value(Box::new(title_str));
        let mut metadata = HashMap::new();
        metadata.insert("xesam:title".to_string(), wrapped);

        let track = MprisAdapter::metadata_to_track(&metadata, "test_source").unwrap();
        assert_eq!(track.title, "Paranoid Android");
    }

    #[test]
    fn metadata_to_track_no_metadata() {
        let metadata: HashMap<String, Value<'_>> = HashMap::new();
        let track = MprisAdapter::metadata_to_track(&metadata, "test_source");
        assert!(track.is_none());
    }

    #[test]
    fn metadata_to_track_empty_title_and_artist() {
        let mut metadata = HashMap::new();
        metadata.insert("xesam:title".to_string(), make_value_str(""));
        metadata.insert("xesam:artist".to_string(), make_value_array_str(vec![]));

        let track = MprisAdapter::metadata_to_track(&metadata, "test_source");
        assert!(track.is_none());
    }
}
