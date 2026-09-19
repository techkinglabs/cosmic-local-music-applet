use cosmic::applet;
use cosmic::iced::advanced::subscription::from_recipe;
use cosmic::iced::Length;
use cosmic::prelude::*;
use cosmic_media_applet::manager::MediaSourceManager;
use cosmic_media_applet::message::AppMessage;
use cosmic_media_applet::mpris::{MediaEvent, PlaybackState, TrackInfo};
use std::hash::Hash;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing_subscriber::EnvFilter;

const MAX_TITLE_LENGTH: usize = 30;

pub struct MediaApplet {
    core: cosmic::Core,
    manager: Option<Arc<MediaSourceManager>>,
    current_track: String,
    play_icon: &'static str,
    current_track_info: Option<TrackInfo>,
    current_state: PlaybackState,
}

#[derive(Clone)]
pub enum Message {
    Previous,
    PlayPause,
    Next,
    MediaEvent(MediaEvent),
    ManagerReady(Arc<MediaSourceManager>),
    UpdateState {
        track: Option<TrackInfo>,
        state: PlaybackState,
    },
}

impl MediaApplet {
    fn apply_track_state(&mut self, track: Option<TrackInfo>, state: PlaybackState) {
        self.current_track_info = track.clone();
        self.current_state = state.clone();
        if let Some(t) = track {
            let title = t.to_string();
            let display = if title.chars().count() > MAX_TITLE_LENGTH {
                title
                    .chars()
                    .take(MAX_TITLE_LENGTH)
                    .collect::<String>()
                    + "..."
            } else {
                title
            };
            self.current_track = display;
            tracing::debug!(track = %self.current_track, source = %t.source_id, "Updated track display");
        } else {
            self.current_track = "No media".to_string();
        }
        self.play_icon = match state {
            PlaybackState::Playing => "⏸",
            PlaybackState::Paused => "▶",
            PlaybackState::Stopped => "▶",
        };
        tracing::debug!(state = ?self.current_state, icon = %self.play_icon, "Updated playback display");
    }
}

impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Message::Previous => f.debug_struct("Previous").finish(),
            Message::PlayPause => f.debug_struct("PlayPause").finish(),
            Message::Next => f.debug_struct("Next").finish(),
            Message::MediaEvent(e) => f.debug_struct("MediaEvent").field("event", e).finish(),
            Message::ManagerReady(_) => f.debug_struct("ManagerReady").finish(),
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
            match rx.recv().await {
                Ok(event) => Some((Message::MediaEvent(event), rx)),
                Err(_) => None,
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

    fn init(core: cosmic::Core, _flags: Self::Flags) -> (Self, cosmic::app::Task<Self::Message>) {
        (
            Self {
                core,
                manager: None,
                 current_track: "No media".to_string(),
                 play_icon: "▶",
                 current_track_info: None,
                 current_state: PlaybackState::Stopped,
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
            Message::ManagerReady(manager) => {
                self.manager = Some(manager.clone());
                tracing::info!("MediaSourceManager ready; reading initial track and state");
                let (track, state) = manager.cached_state();
                tracing::debug!(?track, ?state, "Using cached initial media state");
                self.apply_track_state(track, state);
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
                }
            }
            Message::UpdateState { track, state } => {
                tracing::debug!(?track, ?state, "Applied initial media state");
                self.apply_track_state(track, state);
            }
        }
        cosmic::app::Task::none()
    }

    fn view(&self) -> Element<'_, Self::Message> {
        let previous_btn = cosmic::widget::button::text("⏮")
            .on_press(Message::Previous)
            .padding(4);
        let play_pause_btn = cosmic::widget::button::text(self.play_icon)
            .on_press(Message::PlayPause)
            .padding(4);
        let next_btn = cosmic::widget::button::text("⏭")
            .on_press(Message::Next)
            .padding(4);
        let title_widget = cosmic::widget::text(&self.current_track)
            .size(14)
            .width(Length::FillPortion(1));

        cosmic::widget::Row::new()
            .push(previous_btn)
            .push(play_pause_btn)
            .push(next_btn)
            .push(title_widget)
            .spacing(6)
            .padding([4, 8])
        .into()
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
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/home/techking/cosmic-media-applet.log");
    match log_file {
        Ok(file) => {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(file)
                .with_target(false)
                .try_init();
        }
        Err(e) => {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_target(false)
                .try_init();
            eprintln!("Failed to open log file: {e}");
        }
    }
}

fn main() -> cosmic::iced::Result {
    init_tracing();
    tracing::info!("Media applet starting");
    applet::run::<MediaApplet>(())
}
