use cosmic::applet;
use cosmic::iced::advanced::subscription::from_recipe;
use cosmic::iced::core::window as iced_core_window;
use cosmic::prelude::*;
use cosmic::surface::action::{app_popup, destroy_popup};
use cosmic_media_applet::manager::MediaSourceManager;
use cosmic_media_applet::message::AppMessage;
use cosmic_media_applet::mpris::{MediaEvent, PlaybackState, TrackInfo};
use std::hash::Hash;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tracing_subscriber::EnvFilter;

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
}

#[derive(Clone)]
pub enum Message {
    Previous,
    PlayPause,
    Next,
    ScanMusic,
    UpdateStats { album_count: usize, track_count: usize, last_scanned: u64 },
    MediaEvent(MediaEvent),
    ManagerReady(Arc<MediaSourceManager>),
    UpdateState {
        track: Option<TrackInfo>,
        state: PlaybackState,
    },
    PopupClosed(iced_core_window::Id),
    Surface(cosmic::surface::Action<Message>),
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
        let reserved = button_width * 4.0 + 4.0 * 6.0 + 2.0 * 8.0;
        let available = bounds_width - reserved;

        if available <= 0.0 {
            return 3;
        }

        let chars = (available / CHAR_PX) as usize;
        chars.clamp(3, 128)
    }
}

impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Message::Previous => f.debug_struct("Previous").finish(),
            Message::PlayPause => f.debug_struct("PlayPause").finish(),
            Message::Next => f.debug_struct("Next").finish(),
            Message::ScanMusic => f.debug_struct("ScanMusic").finish(),
            Message::UpdateStats { album_count, track_count, last_scanned } => f
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
        }
    }
}

#[derive(Debug, Clone)]
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
            Message::Previous => {
                if let Some(ref manager) = self.manager {
                    tracing::info!("Previous requested");
                    let m = manager.clone();
                    return cosmic::app::Task::perform(
                        async move { m.route(AppMessage::Previous).await },
                        |result| match result {
                            Ok(_) => {
                                tracing::info!("Previous command completed");
                                cosmic::Action::None
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "Previous command failed");
                                cosmic::Action::None
                            }
                        },
                    );
                }
            }
            Message::PlayPause => {
                if let Some(ref manager) = self.manager {
                    tracing::info!("PlayPause requested");
                    let m = manager.clone();
                    return cosmic::app::Task::perform(
                        async move { m.route(AppMessage::PlayPause).await },
                        |result| match result {
                            Ok(_) => {
                                tracing::info!("PlayPause command completed");
                                cosmic::Action::None
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "PlayPause command failed");
                                cosmic::Action::None
                            }
                        },
                    );
                }
            }
            Message::Next => {
                if let Some(ref manager) = self.manager {
                    tracing::info!("Next requested");
                    let m = manager.clone();
                    return cosmic::app::Task::perform(
                        async move { m.route(AppMessage::Next).await },
                        |result| match result {
                            Ok(_) => {
                                tracing::info!("Next command completed");
                                cosmic::Action::None
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "Next command failed");
                                cosmic::Action::None
                            }
                        },
                    );
                }
            }
            Message::ScanMusic => {
                if let Some(ref manager) = self.manager {
                    tracing::info!("ScanMusic requested");
                    let m = manager.clone();
                    return cosmic::app::Task::perform(
                        async move { m.route(AppMessage::ScanMusic).await },
                        |result| match result {
                            Ok(_) => {
                                tracing::info!("ScanMusic completed");
                                cosmic::Action::None
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "ScanMusic failed");
                                cosmic::Action::None
                            }
                        },
                    );
                }
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
            }
            Message::MediaEvent(event) => {
                tracing::debug!(?event, "Received media event");
                match event {
                    MediaEvent::StateChanged(state) => {
                        tracing::debug!(?state, "Received StateChanged from broadcast");
                        let state_clone = state.clone();
                        let (track, _) = match &self.manager {
                            Some(m) => m.cached_state(),
                            None => (self.current_track_info.clone(), state_clone),
                        };
                        self.apply_track_state(track, state);
                    }
                    MediaEvent::TrackChanged(track) => {
                        self.apply_track_state(Some(track), self.current_state.clone());
                    }
                    MediaEvent::SourceListChanged => {
                        tracing::info!("MPRIS source list changed; using cached state");
                        let (track, state) = match &self.manager {
                            Some(m) => m.cached_state(),
                            None => (None, PlaybackState::Stopped),
                        };
                        self.apply_track_state(track, state);
                    }
                    MediaEvent::StatsUpdated { track_count, album_count, last_scanned } => {
                        tracing::debug!(track_count, album_count, last_scanned, "Stats updated from broadcast");
                        self.stats_album_count = album_count;
                        self.stats_track_count = track_count;
                        self.stats_last_scanned = last_scanned;
                    }
                }
            }
            Message::UpdateStats { album_count, track_count, last_scanned } => {
                tracing::debug!(album_count, track_count, last_scanned, "UpdateStats message received");
                self.stats_album_count = album_count;
                self.stats_track_count = track_count;
                self.stats_last_scanned = last_scanned;
            }
            Message::UpdateState { track, state } => {
                tracing::debug!(?track, ?state, "Applied initial media state");
                self.apply_track_state(track, state);
            }
            Message::PopupClosed(id) => {
                if self.popup_id.as_ref() == Some(&id) {
                    self.popup_id = None;
                }
            }
            Message::Surface(action) => {
                return cosmic::task::message(cosmic::Action::Surface(action));
            }
        }
        cosmic::app::Task::none()
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
                                    None,
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
                        Some(Box::new(move |state: &MediaApplet| {
                            let stats_text = match state.stats_last_scanned {
                                0 => format!("Albums: {}  Tracks: {}", state.stats_album_count, state.stats_track_count),
                                ts => {
                                    let days_ago = (SystemTime::now()
                                        .duration_since(UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs()
                                        - ts)
                                        / 86400;
                                    let time_str = if days_ago == 0 { "today".to_string() } else { format!("{}d ago", days_ago) };
                                    format!("Albums: {}  Tracks: {} ({})", state.stats_album_count, state.stats_track_count, time_str)
                                }
                            };
                            let scan_btn = state
                                .core
                                .applet
                                .icon_button("view-refresh-symbolic")
                                .on_press(Message::ScanMusic);
                            let content = cosmic::widget::Row::new()
                                .push(cosmic::widget::text(stats_text))
                                .push(cosmic::widget::Column::new().width(cosmic::iced::Length::Fill).height(0))
                                .push(scan_btn)
                                .spacing(8)
                                .padding(8);
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
