use crate::mpris::{MediaEvent, MediaSource, PlaybackState, TrackInfo};
use crate::music_db::{music_folder, MusicStatsDb, TrackStat};
use anyhow::Result;
use async_trait::async_trait;
use rodio::{Decoder, OutputStream, Sink, Source};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub const LOCAL_PLAYER_ID: &str = "local-player";

struct SendableOutputStream(OutputStream);

unsafe impl Send for SendableOutputStream {}
unsafe impl Sync for SendableOutputStream {}

pub struct PlayerAdapter {
    event_sender: tokio::sync::broadcast::Sender<MediaEvent>,
    sink: Arc<Mutex<Option<Sink>>>,
    _stream: Arc<Mutex<Option<SendableOutputStream>>>,
    _stream_handle: Arc<Mutex<Option<rodio::OutputStreamHandle>>>,
    current_track: Arc<Mutex<Option<TrackInfo>>>,
    state: Arc<Mutex<PlaybackState>>,
    position_start: Arc<Mutex<Option<std::time::Instant>>>,
    current_position_ms: Arc<Mutex<u64>>,
    volume: Arc<Mutex<f32>>,
    music_folder: PathBuf,
    playlist: Arc<Mutex<Vec<TrackStat>>>,
    playlist_index: Arc<Mutex<Option<usize>>>,
}

impl PlayerAdapter {
    pub fn new() -> Result<Self> {
        let music_folder = music_folder();
        let (event_sender, _) = tokio::sync::broadcast::channel(32);

        let (stream, stream_handle) = OutputStream::try_default()?;
        let sink = Sink::try_new(&stream_handle)
            .map_err(|e| anyhow::anyhow!("Failed to create sink: {}", e))?;
        sink.set_volume(0.5);

        tracing::info!("PlayerAdapter created with music folder: {:?}", music_folder);

        Ok(Self {
            event_sender,
            sink: Arc::new(Mutex::new(Some(sink))),
            _stream: Arc::new(Mutex::new(Some(SendableOutputStream(stream)))),
            _stream_handle: Arc::new(Mutex::new(Some(stream_handle))),
            current_track: Arc::new(Mutex::new(None)),
            state: Arc::new(Mutex::new(PlaybackState::Stopped)),
            position_start: Arc::new(Mutex::new(None)),
            current_position_ms: Arc::new(Mutex::new(0)),
            volume: Arc::new(Mutex::new(0.5)),
            music_folder,
            playlist: Arc::new(Mutex::new(Vec::new())),
            playlist_index: Arc::new(Mutex::new(None)),
        })
    }

    pub async fn play_track(&self, path: &str) -> Result<()> {
        let path_buf = PathBuf::from(path);

        let track_stat = tokio::task::spawn_blocking({
            let db_path = MusicStatsDb::db_path();
            let path_owned = path.to_string();
            move || {
                let conn = rusqlite::Connection::open(db_path).ok()?;
                conn.query_row(
                    "SELECT path, title, artist, album, duration_ms, play_count, is_favorite, last_played
                     FROM tracks WHERE path = ?1",
                    rusqlite::params![path_owned],
                    |row| {
                        Ok(TrackStat {
                            path: row.get::<_, String>(0)?,
                            title: row.get::<_, String>(1)?,
                            artist: row.get::<_, String>(2)?,
                            album: row.get::<_, String>(3)?,
                            duration_ms: row.get::<_, i64>(4)? as u64,
                            play_count: row.get::<_, i64>(5)? as u64,
                            is_favorite: row.get::<_, i64>(6)? != 0,
                            last_played: row.get::<_, i64>(7)? as u64,
                        })
                    },
                ).ok()
            }
        })
        .await
        .ok()
        .flatten();

        if let Some(stat) = &track_stat {
            if let Ok(db) = MusicStatsDb::new() {
                let _ = db.increment_play_count(&stat.path);
            }
        }

        let file = std::fs::File::open(&path_buf)
            .map_err(|e| anyhow::anyhow!("Failed to open file: {}", e))?;
        let source = Decoder::new(std::io::BufReader::new(file))
            .map_err(|e| anyhow::anyhow!("Failed to decode audio: {}", e))?;

        let duration_ms = track_stat.as_ref().map(|s| s.duration_ms).unwrap_or(0);

        let track_info = TrackInfo {
            title: track_stat.as_ref().map(|s| s.title.clone()).unwrap_or_else(|| {
                path_buf.file_stem().and_then(|s| s.to_str()).unwrap_or("Unknown").to_string()
            }),
            artist: track_stat.as_ref().map(|s| s.artist.clone()).unwrap_or_default(),
            album: Some(track_stat.as_ref().map(|s| s.album.clone()).unwrap_or_default()),
            source_id: LOCAL_PLAYER_ID.to_string(),
            duration_ms: if duration_ms > 0 { Some(duration_ms) } else { None },
            file_path: Some(path.to_string()),
        };

        let volume = *self.volume.lock().unwrap();

        {
            let sink_guard = self.sink.lock().unwrap().take();
            if let Some(s) = sink_guard {
                s.stop();
            }
        }

        let (new_sink, wrapped_stream, stream_handle) = {
            let (stream, stream_handle) = OutputStream::try_default()?;
            let new_sink = Sink::try_new(&stream_handle)
                .map_err(|e| anyhow::anyhow!("Failed to create sink: {}", e))?;
            new_sink.set_volume(volume);
            new_sink.append(source);
            (new_sink, SendableOutputStream(stream), stream_handle)
        };

        {
            let mut sink_guard = self.sink.lock().unwrap();
            *sink_guard = Some(new_sink);
            let mut stream_guard = self._stream.lock().unwrap();
            *stream_guard = Some(wrapped_stream);
            let mut sh_guard = self._stream_handle.lock().unwrap();
            *sh_guard = Some(stream_handle);
        }

        {
            let mut track_guard = self.current_track.lock().unwrap();
            *track_guard = Some(track_info.clone());
        }
        *self.state.lock().unwrap() = PlaybackState::Playing;
        *self.position_start.lock().unwrap() = Some(std::time::Instant::now());
        {
            let mut idx_guard = self.playlist_index.lock().unwrap();
            if let Some(stat) = &track_stat {
                let playlist = self.playlist.lock().unwrap().clone();
                if let Some(pos) = playlist.iter().position(|t| t.path == stat.path) {
                    *idx_guard = Some(pos);
                }
            }
        }

        *self.current_position_ms.lock().unwrap() = 0;

        let _ = self.event_sender.send(MediaEvent::TrackChanged(track_info));
        let _ = self.event_sender.send(MediaEvent::StateChanged(PlaybackState::Playing));
        if duration_ms > 0 {
            let _ = self.event_sender.send(MediaEvent::PlaybackPosition {
                position_ms: 0,
                duration_ms,
            });
        }

        self.start_position_task();

        Ok(())
    }

    fn start_position_task(&self) {
        let sender = self.event_sender.clone();
        let sink_clone = self.sink.clone();
        let position_start_clone = self.position_start.clone();
        let current_position_clone = self.current_position_ms.clone();
        let current_track_clone = self.current_track.clone();
        let state_clone = self.state.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(250));
            loop {
                interval.tick().await;

                let track = current_track_clone.lock().unwrap().clone();
                let pos_start = *position_start_clone.lock().unwrap();
                let state = *state_clone.lock().unwrap();

                let sink_stopped = {
                    let sink_guard = sink_clone.lock().unwrap();
                    sink_guard.as_ref().map(|s| s.empty()).unwrap_or(true)
                };

                if sink_stopped && state == PlaybackState::Playing {
                    *state_clone.lock().unwrap() = PlaybackState::Stopped;
                    let _ = sender.send(MediaEvent::StateChanged(PlaybackState::Stopped));
                    *position_start_clone.lock().unwrap() = None;
                    break;
                }

                if state == PlaybackState::Playing {
                    if let Some(instant) = pos_start {
                        let elapsed = instant.elapsed().as_millis() as u64;
                        *current_position_clone.lock().unwrap() = elapsed;
                        if let Some(ref t) = track {
                            if let Some(dur) = t.duration_ms {
                                let _ = sender.send(MediaEvent::PlaybackPosition {
                                    position_ms: elapsed,
                                    duration_ms: dur,
                                });
                                if elapsed >= dur {
                                    *state_clone.lock().unwrap() = PlaybackState::Stopped;
                                    let _ = sender.send(MediaEvent::StateChanged(PlaybackState::Stopped));
                                    *position_start_clone.lock().unwrap() = None;
                                    break;
                                }
                            }
                        }
                    }
                } else if let Some(ref t) = track {
                    if let Some(dur) = t.duration_ms {
                        let stored_pos = *current_position_clone.lock().unwrap();
                        let _ = sender.send(MediaEvent::PlaybackPosition {
                            position_ms: stored_pos,
                            duration_ms: dur,
                        });
                    }
                }
            }
        });
    }

    pub async fn load_playlist(&self) -> Result<Vec<TrackStat>> {
        let db = MusicStatsDb::new()?;
        db.get_tracks_sorted_by_play_count()
    }

    pub async fn search(&self, query: &str) -> Result<Vec<TrackStat>> {
        let db = MusicStatsDb::new()?;
        db.search_tracks(query)
    }

    pub async fn get_albums(&self) -> Result<Vec<crate::music_db::AlbumInfo>> {
        let db = MusicStatsDb::new()?;
        db.get_albums()
    }

    pub async fn get_album_tracks(&self, album: &str) -> Result<Vec<TrackStat>> {
        let db = MusicStatsDb::new()?;
        db.get_tracks_in_album(album)
    }

    pub async fn get_stats(&self) -> Result<(usize, usize)> {
        let db = MusicStatsDb::new()?;
        let (tracks, albums, _) = db.get_stats();
        Ok((tracks, albums))
    }

    pub async fn toggle_favorite(&self, path: &str) -> Result<()> {
        let db = MusicStatsDb::new()?;
        db.toggle_favorite(path)
    }

    pub async fn all_tracks(&self) -> Result<Vec<TrackStat>> {
        let db = MusicStatsDb::new()?;
        db.get_tracks_sorted_by_play_count()
    }

    pub fn music_folder(&self) -> &std::path::Path {
        &self.music_folder
    }
}

impl Default for PlayerAdapter {
    fn default() -> Self {
        Self::new().expect("Failed to initialize PlayerAdapter")
    }
}

#[async_trait]
impl MediaSource for PlayerAdapter {
    fn id(&self) -> &str {
        LOCAL_PLAYER_ID
    }

    fn display_name(&self) -> &str {
        "Local Music"
    }

    async fn is_available(&self) -> bool {
        self.music_folder.exists()
    }

    async fn get_track(&self) -> Option<TrackInfo> {
        self.current_track.lock().unwrap().clone()
    }

    async fn get_state(&self) -> PlaybackState {
        *self.state.lock().unwrap()
    }

    async fn play_pause(&self) -> Result<()> {
        let sink_guard = self.sink.lock().unwrap();
        if let Some(s) = sink_guard.as_ref() {
            let is_paused = s.is_paused();
            if is_paused {
                s.play();
                *self.state.lock().unwrap() = PlaybackState::Playing;
                *self.position_start.lock().unwrap() = Some(std::time::Instant::now());
                let _ = self.event_sender.send(MediaEvent::StateChanged(PlaybackState::Playing));
            } else {
                s.pause();
                *self.state.lock().unwrap() = PlaybackState::Paused;
                let _ = self.event_sender.send(MediaEvent::StateChanged(PlaybackState::Paused));
            }
        }
        Ok(())
    }

    async fn next(&self) -> Result<()> {
        let (playlist, idx) = {
            let playlist = self.playlist.lock().unwrap().clone();
            let idx = *self.playlist_index.lock().unwrap();
            (playlist, idx)
        };

        if let (Some(idx), false) = (idx, playlist.is_empty()) {
            let next_idx = (idx + 1).min(playlist.len() - 1);
            if next_idx != idx {
                let path = playlist[next_idx].path.clone();
                self.play_track(&path).await?;
            }
        }
        Ok(())
    }

    async fn previous(&self) -> Result<()> {
        let (playlist, idx) = {
            let playlist = self.playlist.lock().unwrap().clone();
            let idx = *self.playlist_index.lock().unwrap();
            (playlist, idx)
        };

        if let Some(idx) = idx {
            let target_idx = if idx == 0 { idx } else { idx - 1 };
            if let Some(track) = playlist.get(target_idx) {
                let path = track.path.clone();
                self.play_track(&path).await?;
            }
        }
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        {
            let sink_guard = self.sink.lock().unwrap();
            if let Some(s) = sink_guard.as_ref() {
                s.stop();
            }
        }
        *self.state.lock().unwrap() = PlaybackState::Stopped;
        *self.position_start.lock().unwrap() = None;
        *self.current_position_ms.lock().unwrap() = 0;
        let _ = self.event_sender.send(MediaEvent::StateChanged(PlaybackState::Stopped));
        Ok(())
    }

    async fn set_position(&self, position_ms: u64) -> Result<()> {
        let track = self.current_track.lock().unwrap().clone();
        if let Some(ref t) = track {
            if let Some(ref path) = t.file_path {
                let file = std::fs::File::open(path)?;
                let new_source = Decoder::new(std::io::BufReader::new(file))?;

                let skip = std::time::Duration::from_millis(position_ms);
                let seeked_source = new_source.skip_duration(skip);

                let stream_handle_opt = self._stream_handle.lock().unwrap().take();
                let volume = *self.volume.lock().unwrap();

                let (new_sink, wrapped_stream, new_stream_handle) = {
                    let (new_stream, new_stream_handle) = if let Some(sh) = stream_handle_opt {
                        (None, sh)
                    } else {
                        let (s, sh) = OutputStream::try_default()?;
                        (Some(SendableOutputStream(s)), sh)
                    };
                    let new_sink = Sink::try_new(&new_stream_handle)
                        .map_err(|e| anyhow::anyhow!("Failed to create sink: {}", e))?;
                    new_sink.set_volume(volume);
                    new_sink.append(seeked_source);
                    new_sink.play();
                    (new_sink, new_stream, new_stream_handle)
                };

                {
                    let mut sink_guard = self.sink.lock().unwrap();
                    *sink_guard = Some(new_sink);
                }
                {
                    let mut stream_guard = self._stream.lock().unwrap();
                    *stream_guard = wrapped_stream;
                }
                {
                    let mut sh_guard = self._stream_handle.lock().unwrap();
                    *sh_guard = Some(new_stream_handle);
                }

                *self.position_start.lock().unwrap() = Some(std::time::Instant::now() - skip);
                *self.current_position_ms.lock().unwrap() = position_ms;
                *self.state.lock().unwrap() = PlaybackState::Playing;

                let _ = self.event_sender.send(MediaEvent::StateChanged(PlaybackState::Playing));
            }
        }
        Ok(())
    }

    async fn set_volume(&self, volume: f32) -> Result<()> {
        let clamped = volume.clamp(0.0, 1.0);
        *self.volume.lock().unwrap() = clamped;
        let sink_guard = self.sink.lock().unwrap();
        if let Some(s) = sink_guard.as_ref() {
            s.set_volume(clamped);
        }
        let _ = self.event_sender.send(MediaEvent::VolumeChanged(clamped));
        Ok(())
    }

    async fn get_position(&self) -> Option<u64> {
        let state = *self.state.lock().unwrap();
        if state == PlaybackState::Playing {
            let pos_start = *self.position_start.lock().unwrap();
            if let Some(instant) = pos_start {
                let elapsed = instant.elapsed().as_millis() as u64;
                *self.current_position_ms.lock().unwrap() = elapsed;
                Some(elapsed)
            } else {
                Some(*self.current_position_ms.lock().unwrap())
            }
        } else {
            Some(*self.current_position_ms.lock().unwrap())
        }
    }

    async fn get_volume(&self) -> Option<f32> {
        Some(*self.volume.lock().unwrap())
    }

    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<MediaEvent> {
        self.event_sender.subscribe()
    }
}
