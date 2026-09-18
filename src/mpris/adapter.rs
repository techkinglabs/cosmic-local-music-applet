use super::{MediaEvent, MediaSource, PlaybackState, TrackInfo};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use zbus::{Connection, Proxy};
use zvariant::Value;

const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
const MPRIS_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";

pub struct MprisAdapter {
    bus_name: String,
    display_name: String,
    source_id: String,
    connection: Arc<Connection>,
    event_sender: broadcast::Sender<MediaEvent>,
    state: Mutex<PlaybackState>,
    track: Mutex<Option<TrackInfo>>,
    _watch_handle: tokio::task::JoinHandle<()>,
}

impl MprisAdapter {
    pub async fn new(bus_name: String, connection: Arc<Connection>) -> anyhow::Result<Self> {
        let proxy = Proxy::new(&connection, bus_name.as_str(), MPRIS_PATH, MPRIS_INTERFACE).await?;
        let display_name = bus_name.strip_prefix("org.mpris.MediaPlayer2.").unwrap_or(&bus_name).to_string();
        let source_id = display_name.clone();
        let (event_sender, _) = broadcast::channel(32);

        let state = match proxy.get_property::<String>("PlaybackStatus").await {
            Ok(s) => match s.as_str() { "Playing" => PlaybackState::Playing, "Paused" => PlaybackState::Paused, _ => PlaybackState::Stopped },
            Err(_) => PlaybackState::Stopped,
        };
        let track = Self::fetch_track(&proxy, &source_id).await;

        let conn_clone = connection.clone();
        let sender_clone = event_sender.clone();
        let bus_name_clone = bus_name.clone();

        let _watch_handle = tokio::spawn(async move {
            let Ok(proxy) = Proxy::new(&conn_clone, bus_name_clone.as_str(), MPRIS_PATH, MPRIS_INTERFACE).await else { return };
            let Ok(mut stream) = proxy.receive_signal("PropertiesChanged").await else { return };
            while let Some(signal) = futures::StreamExt::next(&mut stream).await {
                let body = signal.body();
                let args: (String, HashMap<String, Value<'_>>, Vec<String>) = match body.deserialize() {
                    Ok(a) => a,
                    Err(_) => continue,
                };
                let (iface, changed, _) = args;
                if iface!= MPRIS_INTERFACE { continue; }
                if let Some(v) = changed.get("PlaybackStatus") {
                    if let Some(s) = Self::extract_string(v) {
                        let ps = match s.as_str() { "Playing" => PlaybackState::Playing, "Paused" => PlaybackState::Paused, _ => PlaybackState::Stopped };
                        let _ = sender_clone.send(MediaEvent::StateChanged(ps));
                    }
                }
                if let Some(v) = changed.get("Metadata") {
                    if let Value::Dict(dict) = v {
                        let mut map = HashMap::new();
                        for (k, vv) in dict.iter() {
                            if let Value::Str(ks) = k { map.insert(ks.to_string(), vv.clone()); }
                        }
                        if let Some(t) = Self::metadata_to_track(&map, &bus_name_clone) {
                            let _ = sender_clone.send(MediaEvent::TrackChanged(t));
                        }
                    }
                }
            }
        });

        Ok(Self { bus_name, display_name, source_id, connection, event_sender, state: Mutex::new(state), track: Mutex::new(track), _watch_handle })
    }

    fn extract_string(v: &Value<'_>) -> Option<String> {
        match v {
            Value::Str(s) => Some(s.to_string()),
            _ => None,
        }
    }

    async fn fetch_track(proxy: &Proxy<'_>, source_id: &str) -> Option<TrackInfo> {
        let meta: HashMap<String, Value<'_>> = proxy.get_property("Metadata").await.ok()?;
        Self::metadata_to_track(&meta, source_id)
    }

    fn metadata_to_track(metadata: &HashMap<String, Value<'_>>, source_id: &str) -> Option<TrackInfo> {
        let title = metadata.get("xesam:title").and_then(|v| Self::extract_string(v)).unwrap_or_default();
        let artist = metadata.get("xesam:artist").map(|v| match v {
            Value::Array(arr) => arr.iter().filter_map(|x| Self::extract_string(x)).collect::<Vec<_>>().join(", "),
            _ => Self::extract_string(v).unwrap_or_default(),
        }).unwrap_or_default();
        let album = metadata.get("xesam:album").and_then(|v| Self::extract_string(v));
        if title.is_empty() && artist.is_empty() { return None; }
        Some(TrackInfo { title, artist, album, source_id: source_id.to_string() })
    }
}

#[async_trait::async_trait]
impl MediaSource for MprisAdapter {
    fn id(&self) -> &str { &self.source_id }
    fn display_name(&self) -> &str { &self.display_name }
    async fn is_available(&self) -> bool {
        let proxy = match Proxy::new(&self.connection, "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus").await {
            Ok(p) => p,
            Err(_) => return false,
        };
        proxy.call::<&str, &str, String>("GetNameOwner", &self.bus_name.as_str()).await.ok().is_some()
    }
    async fn get_track(&self) -> Option<TrackInfo> {
        let proxy = Proxy::new(&self.connection, self.bus_name.as_str(), MPRIS_PATH, MPRIS_INTERFACE).await.ok()?;
        let mut t = Self::fetch_track(&proxy, &self.source_id).await.or_else(|| self.track.lock().unwrap().clone())?;
        t.source_id = self.source_id.clone(); Some(t)
    }
    async fn get_state(&self) -> PlaybackState {
        let Ok(proxy) = Proxy::new(&self.connection, self.bus_name.as_str(), MPRIS_PATH, MPRIS_INTERFACE).await else { return self.state.lock().unwrap().clone() };
        let Ok(s) = proxy.get_property::<String>("PlaybackStatus").await else { return self.state.lock().unwrap().clone() };
        let ps = match s.as_str() { "Playing" => PlaybackState::Playing, "Paused" => PlaybackState::Paused, _ => PlaybackState::Stopped };
        *self.state.lock().unwrap() = ps.clone(); ps
    }
    async fn play_pause(&self) -> anyhow::Result<()> { let p = Proxy::new(&self.connection, self.bus_name.as_str(), MPRIS_PATH, MPRIS_INTERFACE).await?; p.call::<&str, (), ()>("PlayPause", &()).await?; Ok(()) }
    async fn next(&self) -> anyhow::Result<()> { let p = Proxy::new(&self.connection, self.bus_name.as_str(), MPRIS_PATH, MPRIS_INTERFACE).await?; p.call::<&str, (), ()>("Next", &()).await?; Ok(()) }
    async fn previous(&self) -> anyhow::Result<()> { let p = Proxy::new(&self.connection, self.bus_name.as_str(), MPRIS_PATH, MPRIS_INTERFACE).await?; p.call::<&str, (), ()>("Previous", &()).await?; Ok(()) }
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<MediaEvent> { self.event_sender.subscribe() }
}