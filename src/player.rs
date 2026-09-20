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

struct Minimp3Frame {
    samples: Vec<i16>,
}

pub struct Minimp3Source {
    frames: Arc<Mutex<Vec<Minimp3Frame>>>,
    current_frame: Arc<Mutex<usize>>,
    sample_idx: Arc<Mutex<usize>>,
    sample_rate: u32,
    channels: u16,
    total_samples: usize,
    duration_ms: Option<u64>,
}

impl Iterator for Minimp3Source {
    type Item = i16;

    fn next(&mut self) -> Option<i16> {
        let mut frame_idx = self.current_frame.lock().unwrap();
        let mut idx = self.sample_idx.lock().unwrap();
        let frames = self.frames.lock().unwrap();

        loop {
            if *frame_idx >= frames.len() {
                *idx = 0;
                return None;
            }

            let frame = &frames[*frame_idx];
            if *idx < frame.samples.len() {
                let sample = frame.samples[*idx];
                *idx += 1;
                return Some(sample);
            } else {
                *frame_idx += 1;
                *idx = 0;
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(self.total_samples))
    }
}

impl Source for Minimp3Source {
    fn current_frame_len(&self) -> Option<usize> {
        let frames = self.frames.lock().unwrap();
        if frames.is_empty() {
            Some(0)
        } else {
            let frame_idx = *self.current_frame.lock().unwrap();
            if frame_idx < frames.len() {
                let idx = *self.sample_idx.lock().unwrap();
                Some(frames[frame_idx].samples.len() - idx.min(frames[frame_idx].samples.len()))
            } else {
                Some(0)
            }
        }
    }

    fn channels(&self) -> u16 {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn total_duration(&self) -> Option<std::time::Duration> {
        self.duration_ms.map(|ms| std::time::Duration::from_millis(ms))
    }

    fn try_seek(&mut self, pos: std::time::Duration) -> Result<(), rodio::source::SeekError> {
        let target_sample = (pos.as_secs_f32() * self.sample_rate as f32 * self.channels as f32) as usize;
        let mut current = 0usize;
        let mut found_frame = 0usize;
        let mut found_idx = 0usize;
        let frames = self.frames.lock().unwrap();
        for (i, f) in frames.iter().enumerate() {
            if current + f.samples.len() >= target_sample {
                found_frame = i;
                found_idx = target_sample - current;
                break;
            }
            current += f.samples.len();
        }
        drop(frames);
        *self.current_frame.lock().unwrap() = found_frame;
        *self.sample_idx.lock().unwrap() = found_idx;
        Ok(())
    }
}

fn load_with_minimp3(path: &str) -> Result<Minimp3Source> {
    let file = std::fs::File::open(path)
        .map_err(|e| anyhow::anyhow!("Failed to open file: {}", e))?;
    let mut decoder = minimp3::Decoder::new(std::io::BufReader::new(file));

    let mut frames: Vec<Minimp3Frame> = Vec::new();
    let mut sample_rate = 44100u32;
    let mut channels = 2u16;
    let mut total_samples = 0usize;

    while let Ok(frame) = decoder.next_frame() {
        sample_rate = frame.sample_rate as u32;
        channels = frame.channels as u16;
        total_samples += frame.data.len();
        frames.push(Minimp3Frame { samples: frame.data });
    }

    if frames.is_empty() {
        return Err(anyhow::anyhow!("Minimp3 decoder produced no frames"));
    }

    let duration_ms = (total_samples as f64 / sample_rate as f64 / channels as f64 * 1000.0) as u64;

    Ok(Minimp3Source {
        frames: Arc::new(Mutex::new(frames)),
        current_frame: Arc::new(Mutex::new(0)),
        sample_idx: Arc::new(Mutex::new(0)),
        sample_rate,
        channels,
        total_samples,
        duration_ms: Some(duration_ms),
    })
}

fn create_decoder(path: &str, skip: Option<std::time::Duration>) -> Result<Box<dyn Source<Item = i16> + Send>> {
    let file = std::fs::File::open(path)
        .map_err(|e| anyhow::anyhow!("Failed to open file: {}", e))?;
    match Decoder::new(std::io::BufReader::new(file)) {
        Ok(source) => {
            if let Some(dur) = skip {
                Ok(Box::new(source.skip_duration(dur)))
            } else {
                Ok(Box::new(source))
            }
        }
        Err(e) => {
            tracing::warn!(path = %path, error = %e, "Rodio symphonia decoder failed; falling back to minimp3");
            let mut source = load_with_minimp3(path)
                .map_err(|e2| anyhow::anyhow!("Both symphonia and minimp3 decoders failed: symphonia: {}, minimp3: {}", e, e2))?;
            if let Some(dur) = skip {
                source.try_seek(dur).ok();
            }
            Ok(Box::new(source))
        }
    }
}

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
        let path_owned = path.to_string();

        let playlist = self.playlist.lock().unwrap().clone();
        if let Some(pos) = playlist.iter().position(|t| t.path == path_owned) {
            *self.playlist_index.lock().unwrap() = Some(pos);
        }

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

        let source = tokio::task::spawn_blocking({
            let path_owned = path.to_string();
            move || create_decoder(&path_owned, None)
        })
        .await
        .map_err(|e| anyhow::anyhow!("Decoder thread failed: {}", e))??;

        let source_duration_ms = source.total_duration()
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let db_duration_ms = track_stat.as_ref().map(|s| s.duration_ms).unwrap_or(0);
        let duration_ms = if db_duration_ms > 0 { db_duration_ms } else { source_duration_ms };

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
            let sink_guard = self.sink.lock().unwrap();
            if let Some(s) = sink_guard.as_ref() {
                s.stop();
                s.clear();
            }
        }

        let (new_sink, stream_handle) = {
            let sh_opt = self._stream_handle.lock().unwrap().clone();
            let stream_handle = if let Some(sh) = sh_opt {
                sh
            } else {
                let (stream, sh) = OutputStream::try_default()?;
                *self._stream.lock().unwrap() = Some(SendableOutputStream(stream));
                sh
            };
            let new_sink = Sink::try_new(&stream_handle)
                .map_err(|e| anyhow::anyhow!("Failed to create sink: {}", e))?;
            new_sink.set_volume(volume);
            new_sink.append(source);
            (new_sink, stream_handle)
        };

        {
            let mut sink_guard = self.sink.lock().unwrap();
            *sink_guard = Some(new_sink);
            let mut sh_guard = self._stream_handle.lock().unwrap();
            *sh_guard = Some(stream_handle);
        }

        {
            let mut track_guard = self.current_track.lock().unwrap();
            *track_guard = Some(track_info.clone());
        }
        *self.state.lock().unwrap() = PlaybackState::Playing;
        *self.position_start.lock().unwrap() = Some(std::time::Instant::now());

        if let Some(stat) = &track_stat {
            if let Ok(db) = MusicStatsDb::new() {
                let _ = db.save_playback_state(Some(&stat.path), 0, "playing");
            }
        }
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

    pub async fn get_albums_sorted(&self, sort_mode: crate::music_db::SortMode) -> Result<Vec<crate::music_db::AlbumInfo>> {
        let db = MusicStatsDb::new()?;
        db.get_albums_sorted(sort_mode)
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

    pub async fn all_tracks_sorted(&self, sort_mode: crate::music_db::SortMode) -> Result<Vec<TrackStat>> {
        let db = MusicStatsDb::new()?;
        db.get_tracks_sorted(sort_mode)
    }

    pub async fn set_playlist(&self, tracks: Vec<TrackStat>) {
        *self.playlist.lock().unwrap() = tracks;
        *self.playlist_index.lock().unwrap() = None;
    }

    pub async fn play_track_at(&self, idx: usize) -> Result<()> {
        let playlist = self.playlist.lock().unwrap().clone();
        if let Some(track) = playlist.get(idx).cloned() {
            *self.playlist_index.lock().unwrap() = Some(idx);
            self.play_track(&track.path).await?;
        }
        Ok(())
    }

    pub fn music_folder(&self) -> &std::path::Path {
        &self.music_folder
    }

    pub async fn resume(&self) -> Result<()> {
        let db = MusicStatsDb::new()?;
        if let Some((path_opt, pos_ms, state_str)) = db.get_playback_state() {
            if let Some(path) = path_opt {
                if state_str == "playing" || state_str == "paused" {
                    if let Some(dur) = self.get_track_duration(&path).await {
                        if pos_ms > 0 && pos_ms < dur {
                            self.set_position(pos_ms).await?;
                            return Ok(());
                        }
                    }
                    self.play_track(&path).await?;
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    async fn get_track_duration(&self, path: &str) -> Option<u64> {
        let db = MusicStatsDb::new().ok()?;
        db.get_track_by_path(path).ok().flatten().map(|t| t.duration_ms)
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
        let has_sink = {
            let sink_guard = self.sink.lock().unwrap();
            sink_guard.is_some()
        };
        if has_sink {
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
        } else {
            self.resume().await?;
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

        let track = self.current_track.lock().unwrap().clone();
        if let Some(ref t) = track {
            if let Some(ref path) = t.file_path {
                if let Ok(db) = MusicStatsDb::new() {
                    let _ = db.save_playback_state(Some(path), 0, "stopped");
                }
            }
        }

        let _ = self.event_sender.send(MediaEvent::StateChanged(PlaybackState::Stopped));
        Ok(())
    }

    async fn set_position(&self, position_ms: u64) -> Result<()> {
        let track = self.current_track.lock().unwrap().clone();
        if let Some(ref t) = track {
            if let Some(ref path) = t.file_path {
                let skip = std::time::Duration::from_millis(position_ms);

                {
                    let sink_guard = self.sink.lock().unwrap();
                    if let Some(s) = sink_guard.as_ref() {
                        s.stop();
                        s.clear();
                    }
                }

                let seeked_source = tokio::task::spawn_blocking({
                    let path_owned = path.to_string();
                    move || create_decoder(&path_owned, Some(skip))
                })
                .await
                .map_err(|e| anyhow::anyhow!("Decoder thread failed: {}", e))??;

                let volume = *self.volume.lock().unwrap();
                let stream_handle = {
                    let sh_guard = self._stream_handle.lock().unwrap();
                    sh_guard.clone()
                };

                if let Some(sh) = stream_handle {
                    let new_sink = Sink::try_new(&sh)
                        .map_err(|e| anyhow::anyhow!("Failed to create sink: {}", e))?;
                    new_sink.set_volume(volume);
                    new_sink.append(seeked_source);
                    new_sink.play();
                    *self.sink.lock().unwrap() = Some(new_sink);
                } else {
                    let (_stream, stream_handle) = OutputStream::try_default()?;
                    let new_sink = Sink::try_new(&stream_handle)
                        .map_err(|e| anyhow::anyhow!("Failed to create sink: {}", e))?;
                    new_sink.set_volume(volume);
                    new_sink.append(seeked_source);
                    new_sink.play();
                    *self._stream.lock().unwrap() = Some(SendableOutputStream(_stream));
                    *self._stream_handle.lock().unwrap() = Some(stream_handle);
                    *self.sink.lock().unwrap() = Some(new_sink);
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
