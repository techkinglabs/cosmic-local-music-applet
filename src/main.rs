use cosmic::applet;
use cosmic::iced::advanced::subscription::from_recipe;
use cosmic::iced::core::window as iced_core_window;
use cosmic::prelude::*;
use cosmic::surface::action::{app_popup, destroy_popup};
use cosmic_media_applet::manager::MediaSourceManager;
use cosmic_media_applet::message::AppMessage;
use cosmic_media_applet::mpris::{MediaEvent, PlaybackState, TrackInfo};
use cosmic_media_applet::music_db::{AlbumInfo, MusicStatsDb, SortMode, TrackStat};
use std::hash::Hash;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone)]
pub struct PlaybackTime {
    pub position_ms: u64,
    pub duration_ms: u64,
}

pub struct MediaApplet {
    core: cosmic::Core,
    manager: Option<Arc<MediaSourceManager>>,
    current_track: String,
    current_track_info: Option<TrackInfo>,
    current_state: PlaybackState,
    popup_id: Option<iced_core_window::Id>,
    stats_album_count: usize,
    stats_track_count: usize,
    stats_last_scanned: u64,
    playback_time: PlaybackTime,
    current_volume: f32,
    albums: Vec<AlbumInfo>,
    tracks: Vec<TrackStat>,
    search_query: String,
    selected_album: Option<String>,
    show_favorites_only: bool,
    sort_mode: SortMode,
    view_mode: ViewMode,
    confirm_delete: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ViewMode {
    Albums,
    Tracks,
}

#[derive(Clone)]
pub enum Message {
    Previous,
    PlayPause,
    Next,
    Stop,
    ScanMusic,
    UpdateStats {
        album_count: usize,
        track_count: usize,
        last_scanned: u64,
    },
    MediaEvent(MediaEvent),
    ManagerReady(Arc<MediaSourceManager>),
    UpdateState {
        track: Option<TrackInfo>,
        state: PlaybackState,
    },
    PopupClosed(iced_core_window::Id),
    Surface(cosmic::surface::Action<Message>),
    PlayTrack(String),
    PlayAlbum(String),
    SetPosition(f32),
    SetVolume(f32),
    ToggleFavorite(String),
    SetPlaylist(Vec<TrackStat>),
    SearchChanged(String),
    SelectAlbum(Option<String>),
    ToggleFavoritesOnly,
    ToggleSortMode,
    ToggleViewMode,
    LoadAlbums,
    LoadTracks,
    UpdateAlbums(Vec<AlbumInfo>),
    UpdateTracks(Vec<TrackStat>),
    DeleteTrack(String),
    ConfirmDelete(Option<String>),
    CancelDelete,
}

impl MediaApplet {
    fn apply_track_state(&mut self, track: Option<TrackInfo>, state: PlaybackState) {
        self.current_track_info = track.clone();
        self.current_state = state;
        self.current_track = match track {
            Some(t) => {
                let track_display = t.to_string();
                tracing::debug!(track = %track_display, source = %t.source_id, "Updated track display");
                if track_display.is_empty() {
                    "No media".to_string()
                } else {
                    track_display
                }
            }
            None => "No media".to_string(),
        };
        tracing::debug!(state = ?self.current_state, "Updated playback display");
    }

    fn max_title_chars(&self) -> usize {
        const CHAR_PX: f32 = 7.0;
        let bounds_width = self
            .core
            .applet
            .suggested_bounds
            .as_ref()
            .filter(|bounds| bounds.width > 0.0)
            .map(|bounds| bounds.width)
            .unwrap_or(0.0);

        if bounds_width <= 0.0 {
            return usize::MAX;
        }

        let (icon, _) = self.core.applet.suggested_size(true);
        let button_width = icon as f32 * 1.5 + 8.0;
        let reserved = button_width * 5.0 + 4.0 * 8.0 + 2.0 * 8.0;
        let available = bounds_width - reserved;

        if available <= 0.0 {
            return 3;
        }

        let chars = (available / CHAR_PX) as usize;
        chars.clamp(3, 128)
    }

    fn format_time(ms: u64) -> String {
        let total_secs = ms / 1000;
        let minutes = total_secs / 60;
        let seconds = total_secs % 60;
        format!("{}:{:02}", minutes, seconds)
    }

    fn route_command<F, Fut>(&mut self, f: F) -> cosmic::app::Task<Message>
    where
        F: FnOnce(Arc<MediaSourceManager>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        if let Some(ref manager) = self.manager {
            let m = manager.clone();
            return cosmic::app::Task::perform(
                async move { f(m).await },
                |result| match result {
                    Ok(_) => cosmic::Action::None,
                    Err(e) => {
                        tracing::warn!(error = %e, "Command failed");
                        cosmic::Action::None
                    }
                },
            );
        }
        cosmic::app::Task::none()
    }
}

impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Message::Previous => f.debug_struct("Previous").finish(),
            Message::PlayPause => f.debug_struct("PlayPause").finish(),
            Message::Next => f.debug_struct("Next").finish(),
            Message::Stop => f.debug_struct("Stop").finish(),
            Message::ScanMusic => f.debug_struct("ScanMusic").finish(),
            Message::UpdateStats {
                album_count,
                track_count,
                last_scanned,
            } => f
                .debug_struct("UpdateStats")
                .field("album_count", album_count)
                .field("track_count", track_count)
                .field("last_scanned", last_scanned)
                .finish(),
            Message::MediaEvent(e) => f.debug_struct("MediaEvent").field("event", e).finish(),
            Message::ManagerReady(_) => f.debug_struct("ManagerReady").finish(),
            Message::PopupClosed(id) => f
                .debug_struct("PopupClosed")
                .field("id", id)
                .finish(),
            Message::Surface(a) => f
                .debug_struct("Surface")
                .field("action", &format_args!("{:?}", a))
                .finish(),
            Message::UpdateState { track, state } => f
                .debug_struct("UpdateState")
                .field("track", track)
                .field("state", state)
                .finish(),
            Message::PlayTrack(path) => f
                .debug_struct("PlayTrack")
                .field("path", &path)
                .finish(),
            Message::PlayAlbum(album) => f
                .debug_struct("PlayAlbum")
                .field("album", &album)
                .finish(),
            Message::SetPosition(pos) => f
                .debug_struct("SetPosition")
                .field("position", pos)
                .finish(),
            Message::SetVolume(vol) => f
                .debug_struct("SetVolume")
                .field("volume", vol)
                .finish(),
            Message::ToggleFavorite(path) => f
                .debug_struct("ToggleFavorite")
                .field("path", &path)
                .finish(),
            Message::SetPlaylist(tracks) => f
                .debug_struct("SetPlaylist")
                .field("count", &tracks.len())
                .finish(),
            Message::SearchChanged(query) => f
                .debug_struct("SearchChanged")
                .field("query", &query)
                .finish(),
            Message::SelectAlbum(album) => f
                .debug_struct("SelectAlbum")
                .field("album", album)
                .finish(),
            Message::ToggleFavoritesOnly => f.debug_struct("ToggleFavoritesOnly").finish(),
            Message::ToggleSortMode => f.debug_struct("ToggleSortMode").finish(),
            Message::ToggleViewMode => f.debug_struct("ToggleViewMode").finish(),
            Message::LoadAlbums => f.debug_struct("LoadAlbums").finish(),
            Message::LoadTracks => f.debug_struct("LoadTracks").finish(),
            Message::UpdateAlbums(albums) => f
                .debug_struct("UpdateAlbums")
                .field("count", &albums.len())
                .finish(),
            Message::UpdateTracks(tracks) => f
                .debug_struct("UpdateTracks")
                .field("count", &tracks.len())
                .finish(),
            Message::DeleteTrack(path) => f
                .debug_struct("DeleteTrack")
                .field("path", &path)
                .finish(),
            Message::ConfirmDelete(path) => f
                .debug_struct("ConfirmDelete")
                .field("path", &path)
                .finish(),
            Message::CancelDelete => f.debug_struct("CancelDelete").finish(),
        }
    }
}

#[derive(Clone)]
struct BroadcastSubscription {
    sender: broadcast::Sender<MediaEvent>,
}

impl cosmic::iced::advanced::subscription::Recipe for BroadcastSubscription {
    type Output = Message;

    fn hash(&self, state: &mut cosmic::iced::advanced::subscription::Hasher) {
        std::any::TypeId::of::<Self>().hash(state);
    }

    fn stream(
        self: Box<Self>,
        _input: cosmic::iced::advanced::subscription::EventStream,
    ) -> futures::stream::BoxStream<'static, Self::Output> {
        let receiver = self.sender.subscribe();
        Box::pin(futures::stream::unfold(receiver, |mut rx| async move {
            loop {
                match rx.recv().await {
                    Ok(event) => return Some((Message::MediaEvent(event), rx)),
                    Err(RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "Media event subscriber lagged behind; continuing");
                        continue;
                    }
                    Err(RecvError::Closed) => return None,
                }
            }
        }))
    }
}

impl cosmic::Application for MediaApplet {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = "com.system76.CosmicMediaApplet";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn on_close_requested(&self, id: iced_core_window::Id) -> Option<Self::Message> {
        if self.popup_id.as_ref() == Some(&id) {
            Some(Message::PopupClosed(id))
        } else {
            None
        }
    }

    fn init(core: cosmic::Core, _flags: Self::Flags) -> (Self, cosmic::app::Task<Self::Message>) {
        (
            Self {
                core,
                manager: None,
                current_track: "No media".to_string(),
                current_track_info: None,
                current_state: PlaybackState::Stopped,
                popup_id: None,
                stats_album_count: 0,
                stats_track_count: 0,
                stats_last_scanned: 0,
                playback_time: PlaybackTime {
                    position_ms: 0,
                    duration_ms: 0,
                },
                current_volume: 0.5,
                albums: Vec::new(),
                tracks: Vec::new(),
                search_query: String::new(),
                selected_album: None,
                show_favorites_only: false,
                sort_mode: SortMode::PlayCountDesc,
                view_mode: ViewMode::Albums,
                confirm_delete: None,
            },
            cosmic::app::Task::perform(
                async {
                    tracing::info!("Initializing MediaSourceManager");
                    MediaSourceManager::new().await
                },
                |result| match result {
                    Ok(manager) => {
                        tracing::info!("MediaSourceManager initialized");
                        cosmic::Action::App(Message::ManagerReady(Arc::new(manager)))
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "Failed to initialize MediaSourceManager");
                        cosmic::Action::App(Message::ManagerReady(Arc::new(
                            MediaSourceManager::new_empty(),
                        )))
                    }
                },
            ),
        )
    }

    fn update(&mut self, message: Self::Message) -> cosmic::app::Task<Self::Message> {
        match message {
            Message::Previous => self.route_command(|m| async move { m.route(AppMessage::Previous).await }),
            Message::PlayPause => self.route_command(|m| async move { m.route(AppMessage::PlayPause).await }),
            Message::Next => self.route_command(|m| async move { m.route(AppMessage::Next).await }),
            Message::Stop => self.route_command(|m| async move { m.route(AppMessage::Stop).await }),
            Message::ScanMusic => {
                if let Some(ref manager) = self.manager {
                    let m = manager.clone();
                    return cosmic::app::Task::perform(
                        async move { m.route(AppMessage::ScanMusic).await },
                        |result| match result {
                            Ok(_) => cosmic::Action::App(Message::LoadAlbums),
                            Err(e) => {
                                tracing::warn!(error = %e, "Scan failed");
                                cosmic::Action::None
                            }
                        },
                    );
                }
                cosmic::app::Task::none()
            }
            Message::PlayTrack(path) => {
                self.route_command(move |m| async move { m.route(AppMessage::PlayTrack(path)).await })
            }
            Message::SetPosition(pos) => {
                let duration = self.playback_time.duration_ms;
                let pos_ms = (pos * duration as f32) as u64;
                self.route_command(move |m| async move { m.route(AppMessage::SetPosition(pos_ms)).await })
            }
            Message::SetVolume(vol) => {
                self.current_volume = vol.clamp(0.0, 1.0);
                let clamped = self.current_volume;
                self.route_command(move |m| async move { m.route(AppMessage::SetVolume(clamped)).await })
            }
            Message::ToggleFavorite(path) => {
                self.route_command(move |m| async move { m.route(AppMessage::ToggleFavorite(path)).await })
            }
            Message::SetPlaylist(tracks) => {
                self.route_command(move |m| async move { m.route(AppMessage::SetPlaylist(tracks)).await })
            }
            Message::ToggleSortMode => {
                self.sort_mode = match self.sort_mode {
                    SortMode::NameAsc => SortMode::NameDesc,
                    SortMode::NameDesc => SortMode::PlayCountAsc,
                    SortMode::PlayCountAsc => SortMode::PlayCountDesc,
                    SortMode::PlayCountDesc => SortMode::NameAsc,
                };
                if self.view_mode == ViewMode::Tracks && self.selected_album.is_none() {
                    cosmic::app::Task::perform(async move {}, |_| cosmic::Action::App(Message::LoadTracks))
                } else {
                    cosmic::app::Task::perform(async move {}, |_| cosmic::Action::App(Message::LoadAlbums))
                }
            }
            Message::ToggleViewMode => {
                self.view_mode = match self.view_mode {
                    ViewMode::Albums => ViewMode::Tracks,
                    ViewMode::Tracks => ViewMode::Albums,
                };
                self.selected_album = None;
                if self.view_mode == ViewMode::Tracks {
                    cosmic::app::Task::perform(async move {}, |_| cosmic::Action::App(Message::LoadTracks))
                } else {
                    cosmic::app::Task::perform(async move {}, |_| cosmic::Action::App(Message::LoadAlbums))
                }
            }
            Message::SearchChanged(query) => {
                self.search_query = query;
                if let Some(ref manager) = self.manager {
                    let m = manager.clone();
                    let q = self.search_query.clone();
                    return cosmic::app::Task::perform(
                        async move { m.search_tracks(&q).await },
                        |result| match result {
                            Ok(tracks) => {
                                let t = tracks;
                                cosmic::Action::App(Message::UpdateTracks(t))
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "Search failed");
                                cosmic::Action::None
                            }
                        },
                    );
                }
                cosmic::app::Task::none()
            }
            Message::SelectAlbum(album) => {
                self.selected_album = album.clone();
                if let Some(ref manager) = self.manager {
                    let m = manager.clone();
                    return cosmic::app::Task::perform(
                        async move {
                            match &album {
                                Some(a) => m.get_album_tracks(a).await,
                                None => m.all_tracks().await,
                            }
                        },
                        |result| match result {
                            Ok(tracks) => cosmic::Action::App(Message::UpdateTracks(tracks)),
                            Err(e) => {
                                tracing::warn!(error = %e, "Failed to load album tracks");
                                cosmic::Action::None
                            }
                        },
                    );
                }
                cosmic::app::Task::none()
            }
            Message::PlayAlbum(album) => {
                if let Some(ref manager) = self.manager {
                    let m = manager.clone();
                    let alb = album.clone();
                    return cosmic::app::Task::perform(
                        async move {
                            let tracks = m.get_album_tracks(&alb).await?;
                            m.route(AppMessage::SetPlaylist(tracks.clone())).await?;
                            if let Some(first) = tracks.first() {
                                m.route(AppMessage::PlayTrack(first.path.clone())).await?;
                            }
                            Ok::<(), anyhow::Error>(())
                        },
                        |result| match result {
                            Ok(_) => cosmic::Action::None,
                            Err(e) => {
                                tracing::warn!(error = %e, "PlayAlbum failed");
                                cosmic::Action::None
                            }
                        },
                    );
                }
                cosmic::app::Task::none()
            }
            Message::ToggleFavoritesOnly => {
                self.show_favorites_only = !self.show_favorites_only;
                if self.view_mode == ViewMode::Tracks && self.selected_album.is_none() {
                    cosmic::app::Task::perform(async move {}, |_| cosmic::Action::App(Message::LoadTracks))
                } else if self.view_mode == ViewMode::Albums {
                    cosmic::app::Task::perform(async move {}, |_| cosmic::Action::App(Message::LoadAlbums))
                } else {
                    cosmic::app::Task::none()
                }
            }
            Message::LoadAlbums => {
                if let Some(ref manager) = self.manager {
                    let m = manager.clone();
                    let sort_mode = self.sort_mode;
                    return cosmic::app::Task::perform(
                        async move { m.get_albums_sorted(sort_mode).await },
                        |result| match result {
                            Ok(albums) => cosmic::Action::App(Message::UpdateAlbums(albums)),
                            Err(e) => {
                                tracing::warn!(error = %e, "Failed to load albums");
                                cosmic::Action::None
                            }
                        },
                    );
                }
                cosmic::app::Task::none()
            }
            Message::LoadTracks => {
                if let Some(ref manager) = self.manager {
                    let m = manager.clone();
                    let sort_mode = self.sort_mode;
                    return cosmic::app::Task::perform(
                        async move { m.all_tracks_sorted(sort_mode).await },
                        |result| match result {
                            Ok(tracks) => cosmic::Action::App(Message::UpdateTracks(tracks)),
                            Err(e) => {
                                tracing::warn!(error = %e, "Failed to load tracks");
                                cosmic::Action::None
                            }
                        },
                    );
                }
                cosmic::app::Task::none()
            }
            Message::DeleteTrack(path) => {
                self.confirm_delete = Some(path);
                cosmic::app::Task::none()
            }
            Message::ConfirmDelete(path_opt) => {
                if let Some(ref do_delete) = path_opt {
                    let do_delete = do_delete.clone();
                    if let Some(ref manager) = self.manager {
                        let m = manager.clone();
                        return cosmic::app::Task::perform(
                            async move {
                                if let Some(_player) = m.player() {
                                    let db = MusicStatsDb::new()?;
                                    db.delete_track(&do_delete)?;
                                }
                                std::fs::remove_file(&do_delete).map_err(|e| {
                                    anyhow::anyhow!("Failed to delete file: {}", e)
                                })?;
                                Ok::<(), anyhow::Error>(())
                            },
                            |result| match result {
                                Ok(_) => cosmic::Action::App(Message::LoadAlbums),
                                Err(e) => {
                                    tracing::warn!(error = %e, "Delete failed");
                                    cosmic::Action::None
                                }
                            },
                        );
                    }
                }
                self.confirm_delete = None;
                cosmic::app::Task::none()
            }
            Message::CancelDelete => {
                self.confirm_delete = None;
                cosmic::app::Task::none()
            }
            Message::ManagerReady(manager) => {
                self.manager = Some(manager.clone());
                tracing::info!("MediaSourceManager ready; reading initial track and state");
                let (track, state) = manager.cached_state();
                tracing::debug!(?track, ?state, "Using cached initial media state");
                self.apply_track_state(track, state);
                let (albums, count, ts) = manager.cached_stats();
                tracing::debug!(albums, count, ts, "Using cached initial stats");
                self.stats_album_count = albums;
                self.stats_track_count = count;
                self.stats_last_scanned = ts;

                return cosmic::app::Task::perform(
                    async move { manager.get_albums_sorted(SortMode::PlayCountDesc).await },
                    |result| match result {
                        Ok(albums) => cosmic::Action::App(Message::UpdateAlbums(albums)),
                        Err(e) => {
                            tracing::warn!(error = %e, "Failed to load albums initially");
                            cosmic::Action::None
                        }
                    },
                );
            }
            Message::MediaEvent(event) => {
                tracing::debug!(?event, "Received media event");
                match event {
                    MediaEvent::StateChanged(state) => {
                        tracing::debug!(?state, "Received StateChanged from broadcast");
                        let (track, _) = match &self.manager {
                            Some(m) => m.cached_state(),
                            None => (self.current_track_info.clone(), state),
                        };
                        self.apply_track_state(track, state);
                    }
                    MediaEvent::TrackChanged(track) => {
                        self.apply_track_state(Some(track), self.current_state);
                    }
                    MediaEvent::SourceListChanged => {
                        tracing::info!("Media source list changed; using cached state");
                        let (track, state) = match &self.manager {
                            Some(m) => m.cached_state(),
                            None => (None, PlaybackState::Stopped),
                        };
                        self.apply_track_state(track, state);
                    }
                    MediaEvent::StatsUpdated {
                        track_count,
                        album_count,
                        last_scanned,
                    } => {
                        tracing::debug!(track_count, album_count, last_scanned, "Stats updated from broadcast");
                        self.stats_album_count = album_count;
                        self.stats_track_count = track_count;
                        self.stats_last_scanned = last_scanned;
                    }
                    MediaEvent::PlaybackPosition {
                        position_ms,
                        duration_ms,
                    } => {
                        self.playback_time = PlaybackTime {
                            position_ms,
                            duration_ms,
                        };
                    }
                    MediaEvent::VolumeChanged(vol) => {
                        self.current_volume = vol;
                    }
                }
                cosmic::app::Task::none()
            }
            Message::UpdateStats {
                album_count,
                track_count,
                last_scanned,
            } => {
                tracing::debug!(album_count, track_count, last_scanned, "UpdateStats message received");
                self.stats_album_count = album_count;
                self.stats_track_count = track_count;
                self.stats_last_scanned = last_scanned;
                cosmic::app::Task::none()
            }
            Message::UpdateState { track, state } => {
                tracing::debug!(?track, ?state, "Applied initial media state");
                self.apply_track_state(track, state);
                cosmic::app::Task::none()
            }
            Message::UpdateAlbums(albums) => {
                self.albums = albums;
                cosmic::app::Task::none()
            }
            Message::UpdateTracks(tracks) => {
                self.tracks = tracks.clone();
                if !tracks.is_empty() {
                    let task = cosmic::app::Task::done(cosmic::Action::App(
                        Message::SetPlaylist(tracks),
                    ));
                    return task;
                }
                cosmic::app::Task::none()
            }
            Message::PopupClosed(id) => {
                if self.popup_id.as_ref() == Some(&id) {
                    self.popup_id = None;
                }
                cosmic::app::Task::none()
            }
            Message::Surface(action) => {
                cosmic::task::message(cosmic::Action::Surface(action))
            }
        }
    }

    fn view(&self) -> Element<'_, Self::Message> {
        let play_icon = match self.current_state {
            PlaybackState::Playing => "media-playback-pause-symbolic",
            PlaybackState::Paused | PlaybackState::Stopped => "media-playback-start-symbolic",
        };
        let previous_btn = self
            .core
            .applet
            .icon_button("media-skip-backward-symbolic")
            .on_press(Message::Previous);
        let play_pause_btn = self
            .core
            .applet
            .icon_button(play_icon)
            .on_press(Message::PlayPause);
        let next_btn = self
            .core
            .applet
            .icon_button("media-skip-forward-symbolic")
            .on_press(Message::Next);
        let stop_btn = self
            .core
            .applet
            .icon_button("media-playback-stop-symbolic")
            .on_press(Message::Stop);

        let have_popup = self.popup_id;
        let popup_btn = self
            .core
            .applet
            .icon_button("open-menu-symbolic")
            .on_press_with_rectangle(move |offset, bounds| {
                if let Some(id) = have_popup {
                    Message::Surface(destroy_popup(id))
                } else {
                    Message::Surface(app_popup::<MediaApplet>(
                        |_| cosmic::surface::action::LiveSettings::default(),
                        move |state: &mut MediaApplet| {
                            let new_id = iced_core_window::Id::unique();
                            state.popup_id = Some(new_id);
                            let mut popup_settings = state
                                .core
                                .applet
                                .get_popup_settings(
                                    state.core.main_window_id().unwrap(),
                                    new_id,
                                    Some((560, 480)),
                                    None,
                                    None,
                                );
                            popup_settings.positioner.anchor_rect =
                                cosmic::iced::Rectangle {
                                    x: (bounds.x - offset.x) as i32,
                                    y: (bounds.y - offset.y) as i32,
                                    width: bounds.width as i32,
                                    height: bounds.height as i32,
                                };
                            popup_settings
                        },
                        Some(Box::new(|state: &MediaApplet| {
                            let content = media_player_view(state);
                            Element::from(state.core.applet.popup_container(content))
                                .map(cosmic::Action::App)
                        })),
                    ))
                }
            });

        let title_text = {
            let max_chars = self.max_title_chars();
            self.current_track.chars().take(max_chars).collect::<String>()
        };

        let title = cosmic::widget::text(title_text)
            .size(14)
            .ellipsize(cosmic::iced::core::text::Ellipsize::End(
                cosmic::iced::core::text::EllipsizeHeightLimit::Lines(1),
            ));

        let row = cosmic::widget::Row::new()
            .push(popup_btn)
            .push(previous_btn)
            .push(play_pause_btn)
            .push(next_btn)
            .push(stop_btn)
            .push(title)
            .spacing(6)
            .padding([4, 8])
            .align_y(cosmic::iced::alignment::Vertical::Center);

        self.core.applet.autosize_window(row).into()
    }

    fn subscription(&self) -> cosmic::iced::Subscription<Self::Message> {
        match &self.manager {
            Some(manager) => from_recipe(BroadcastSubscription {
                sender: manager.event_sender(),
            }),
            None => cosmic::iced::Subscription::none(),
        }
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}

fn media_player_view(state: &MediaApplet) -> Element<'_, Message> {
    let play_icon = match state.current_state {
        PlaybackState::Playing => "media-playback-pause-symbolic",
        PlaybackState::Paused | PlaybackState::Stopped => "media-playback-start-symbolic",
    };

    let prev_btn = state
        .core
        .applet
        .icon_button("media-skip-backward-symbolic")
        .on_press(Message::Previous);
    let play_pause_btn = state
        .core
        .applet
        .icon_button(play_icon)
        .on_press(Message::PlayPause);
    let next_btn = state
        .core
        .applet
        .icon_button("media-skip-forward-symbolic")
        .on_press(Message::Next);
    let stop_btn = state
        .core
        .applet
        .icon_button("media-playback-stop-symbolic")
        .on_press(Message::Stop);

    let position_ratio = if state.playback_time.duration_ms > 0 {
        state.playback_time.position_ms as f32 / state.playback_time.duration_ms as f32
    } else {
        0.0
    };

    let progress_slider = cosmic::widget::slider(
        0.0..=1.0,
        position_ratio,
        Message::SetPosition,
    )
    .step(0.01_f32)
    .width(cosmic::iced::Length::Fill);

    let progress_label = cosmic::widget::text(format!(
        "{} / {}",
        MediaApplet::format_time(state.playback_time.position_ms),
        MediaApplet::format_time(state.playback_time.duration_ms)
    ));

    let volume_slider = cosmic::widget::slider(
        0.0..=1.0,
        state.current_volume,
        Message::SetVolume,
    )
    .width(cosmic::iced::Length::Fixed(100.0))
    .step(0.01_f32);

    let track_title = cosmic::widget::text(
        state.current_track_info.as_ref().map(|t| t.title.clone()).unwrap_or_default(),
    )
    .size(16);

    let track_artist = cosmic::widget::text(
        state.current_track_info.as_ref().map(|t| t.artist.clone()).unwrap_or_default(),
    )
    .size(13);

    let track_album = cosmic::widget::text(
        state.current_track_info.as_ref().and_then(|t| t.album.clone()).unwrap_or_default(),
    )
    .size(12);

    let trash_btn = state
        .core
        .applet
        .icon_button("user-trash-symbolic")
        .on_press(Message::DeleteTrack(
            state.current_track_info.as_ref()
                .and_then(|t| t.file_path.clone())
                .unwrap_or_default(),
        ));

    let controls_row = cosmic::widget::Row::new()
        .push(prev_btn)
        .push(play_pause_btn)
        .push(next_btn)
        .push(stop_btn)
        .push(volume_slider)
        .push(trash_btn)
        .spacing(12)
        .align_y(cosmic::iced::alignment::Vertical::Center);

    let progress_row = cosmic::widget::Row::new()
        .push(progress_slider)
        .push(progress_label)
        .spacing(8);

    let media_controls = cosmic::widget::Column::new()
        .push(track_title)
        .push(track_artist)
        .push(track_album)
        .push(controls_row)
        .push(progress_row)
        .spacing(8)
        .padding(8);

    let search_input = cosmic::widget::text_input("Search tracks...", &state.search_query)
        .on_input(Message::SearchChanged)
        .padding(8);

    let favorites_btn = state
        .core
        .applet
        .icon_button(if state.show_favorites_only {
            "starred-symbolic"
        } else {
            "star-symbolic"
        })
        .on_press(Message::ToggleFavoritesOnly);

    let scan_btn = state
        .core
        .applet
        .icon_button("view-refresh-symbolic")
        .on_press(Message::ScanMusic);

    let sort_icon = match state.sort_mode {
        SortMode::NameAsc => "view-sort-ascending-symbolic",
        SortMode::NameDesc => "view-sort-descending-symbolic",
        SortMode::PlayCountAsc => "view-sort-ascending-symbolic",
        SortMode::PlayCountDesc => "view-sort-descending-symbolic",
    };
    let sort_btn = state
        .core
        .applet
        .icon_button(sort_icon)
        .on_press(Message::ToggleSortMode);

    let view_icon = match state.view_mode {
        ViewMode::Albums => "view-dual-symbolic",
        ViewMode::Tracks => "view-list-symbolic",
    };
    let view_btn = state
        .core
        .applet
        .icon_button(view_icon)
        .on_press(Message::ToggleViewMode);

    let header_row = cosmic::widget::Row::new()
        .push(search_input)
        .push(favorites_btn)
        .push(scan_btn)
        .push(sort_btn)
        .push(view_btn)
        .spacing(8)
        .padding(8);

    let content: cosmic::widget::Column<'_, Message, cosmic::Theme> = if let Some(ref album) = state.selected_album {
        cosmic::widget::Column::new()
            .push(media_controls)
            .push(header_row)
            .push(track_listing_view(state, album))
            .into()
     } else if state.view_mode == ViewMode::Tracks {
        cosmic::widget::Column::new()
            .push(media_controls)
            .push(header_row)
            .push(tracks_view(state))
            .into()
    } else {
        cosmic::widget::Column::new()
            .push(media_controls)
            .push(header_row)
            .push(albums_view(state))
            .into()
    };

    let final_content = if let Some(ref path_to_delete) = state.confirm_delete {
        let title = state
            .current_track_info
            .as_ref()
            .map(|t| t.to_string())
            .unwrap_or_else(|| path_to_delete.clone());
        let dialog = cosmic::widget::Container::new(
            cosmic::widget::Column::new()
                .push(cosmic::widget::text("Delete this track?").size(16))
                .push(cosmic::widget::text(title.clone()).size(12))
                .push(
                    cosmic::widget::Row::new()
                        .push(
                            state
                                .core
                                .applet
                                .icon_button("dialog-ok-symbolic")
                                .on_press(Message::ConfirmDelete(Some(path_to_delete.clone()))),
                        )
                        .push(
                            state
                                .core
                                .applet
                                .icon_button("dialog-cancel-symbolic")
                                .on_press(Message::CancelDelete),
                        )
                        .spacing(12),
                )
                .spacing(8)
                .padding(12),
        )
        .padding(12);

        cosmic::widget::Container::new(dialog)
            .width(cosmic::iced::Length::Fill)
            .height(cosmic::iced::Length::Fill)
            .center(cosmic::iced::Length::Fill)
    } else {
        cosmic::widget::Container::new(content)
            .width(cosmic::iced::Length::Fill)
            .height(cosmic::iced::Length::Fill)
    };

    Element::from(cosmic::widget::scrollable(final_content).width(cosmic::iced::Length::Fill))
}

fn albums_view(state: &MediaApplet) -> Element<'_, Message> {
    if state.albums.is_empty() {
        return Element::from(
            cosmic::widget::Column::new()
                .push(cosmic::widget::text("No albums found. Scan your music folder to get started.").size(12))
                .padding(16),
        );
    }

     let album_rows: Vec<Element<'_, Message>> = state
        .albums
        .iter()
        .map(|album_info| {
            let album_name = album_info.album.clone();
            let play_all_btn = state
                .core
                .applet
                .icon_button("media-playback-start-symbolic")
                .on_press(Message::PlayAlbum(album_name));
            let btn = state
                .core
                .applet
                .icon_button("folder-symbolic")
                .on_press(Message::SelectAlbum(Some(album_info.album.clone())));
            let label = cosmic::widget::text(format!(
                "{} — {} ({} tracks)",
                album_info.album,
                album_info.artist,
                album_info.track_count
            ))
            .size(12);
            cosmic::widget::Row::new()
                .push(play_all_btn)
                .push(btn)
                .push(label)
                .spacing(8)
                .into()
        })
        .collect();

    let mut col = cosmic::widget::Column::new()
        .push(cosmic::widget::text("Albums").size(14))
        .spacing(4);
    for row in album_rows {
        col = col.push(row);
    }
    Element::from(col)
}

fn track_listing_view<'a>(state: &'a MediaApplet, album: &'a str) -> Element<'a, Message> {
    let back_btn = state
        .core
        .applet
        .icon_button("go-previous-symbolic")
        .on_press(Message::SelectAlbum(None));

    let mut rows: Vec<Element<'_, Message>> = Vec::new();
    for track in &state.tracks {
        let fav_icon = if track.is_favorite {
            "starred-symbolic"
        } else {
            "star-symbolic"
        };
        let fav_btn = state
            .core
            .applet
            .icon_button(fav_icon)
            .on_press(Message::ToggleFavorite(track.path.clone()));
        let play_btn = state
            .core
            .applet
            .icon_button("media-playback-start-symbolic")
            .on_press(Message::PlayTrack(track.path.clone()));
        let duration = MediaApplet::format_time(track.duration_ms);

        rows.push(
            cosmic::widget::Row::new()
                .push(play_btn)
                .push(fav_btn)
                .push(cosmic::widget::text(&track.title))
                .push(cosmic::widget::text(format!(" — {} (plays: {})", track.artist, track.play_count)))
                .push(cosmic::widget::text(duration))
                .spacing(8)
                .into()
        );
    }

    if rows.is_empty() {
        rows.push(cosmic::widget::text("No tracks found").size(12).into());
    }

     let mut col = cosmic::widget::Column::new()
        .push(
            cosmic::widget::Row::new()
                .push(back_btn)
                .push(cosmic::widget::text(album).size(14))
                .push(
                    state
                        .core
                        .applet
                        .icon_button("media-playback-start-symbolic")
                        .on_press(Message::PlayAlbum(album.to_string()))
                )
                .spacing(8)
                .align_y(cosmic::iced::alignment::Vertical::Center)
        )
        .spacing(4);
    for row in rows {
        col = col.push(row);
    }
    Element::from(col)
}

fn tracks_view(state: &MediaApplet) -> Element<'_, Message> {
    if state.tracks.is_empty() {
        return Element::from(
            cosmic::widget::Column::new()
                .push(cosmic::widget::text("No tracks found. Switch to Albums view or scan your music folder.").size(12))
                .padding(16),
        );
    }

    let sort_label = match state.sort_mode {
        SortMode::NameAsc => "Name A-Z",
        SortMode::NameDesc => "Name Z-A",
        SortMode::PlayCountAsc => "Play count asc",
        SortMode::PlayCountDesc => "Play count desc",
    };

    let mut rows: Vec<Element<'_, Message>> = Vec::new();
    for track in &state.tracks {
        let is_current = state.current_track_info.as_ref()
            .map(|t| {
                t.file_path.as_deref() == Some(&track.path)
            })
            .unwrap_or(false);

        let fav_icon = if track.is_favorite {
            "starred-symbolic"
        } else {
            "star-symbolic"
        };
        let fav_btn = state
            .core
            .applet
            .icon_button(fav_icon)
            .on_press(Message::ToggleFavorite(track.path.clone()));
        let play_btn = state
            .core
            .applet
            .icon_button("media-playback-start-symbolic")
            .on_press(Message::PlayTrack(track.path.clone()));
        let duration = MediaApplet::format_time(track.duration_ms);

        rows.push(
            cosmic::widget::Row::new()
                .push(play_btn)
                .push(fav_btn)
                .push(cosmic::widget::text(&track.title))
                .push(cosmic::widget::text(format!(" — {} (plays: {})", track.artist, track.play_count)))
                .push(cosmic::widget::text(duration))
                .push(cosmic::widget::text(if is_current { " ▶" } else { "" }))
                .spacing(8)
                .into()
        );
    }

    let mut col = cosmic::widget::Column::new()
        .push(
            cosmic::widget::Row::new()
                .push(cosmic::widget::text("All Tracks").size(14))
                .push(cosmic::widget::text(format!("  (sorted by {})", sort_label)).size(11))
                .spacing(4)
        )
        .spacing(4);
    for row in rows {
        col = col.push(row);
    }
    Element::from(col)
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("cosmic_media_applet=info"));
    let log_path = std::env::var("HOME")
        .ok()
        .map(|h| format!("{h}/.local/state/cosmic-media-applet.log"));
    let file_result = log_path.as_ref().and_then(|p| {
        if let Some(parent) = std::path::Path::new(p).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .ok()
    });
    match file_result {
        Some(file) => {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(file)
                .with_target(false)
                .try_init();
        }
        None => {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_target(false)
                .try_init();
            eprintln!("Falling back to stdout/stderr logging (catch via journalctl --user)");
        }
    }
}

fn main() -> cosmic::iced::Result {
    init_tracing();
    tracing::info!("Media applet starting");
    applet::run::<MediaApplet>(())
}
