use cosmic_media_applet::mpris::{MediaEvent, PlaybackState, TrackInfo};
use cosmic_media_applet::manager::MediaSourceManager;
use cosmic_media_applet::message::AppMessage;
use cosmic::applet;
use cosmic::prelude::*;
use cosmic::iced::advanced::subscription::{EventStream, Hasher, Recipe, from_recipe};
use std::hash::Hash;
use std::sync::Arc;
use tokio::sync::broadcast;
const MAX_TITLE_LENGTH: usize = 30;

pub struct MediaApplet {
    core: cosmic::Core,
    manager: Option<Arc<MediaSourceManager>>,
    current_track: String,
    full_title: String,
    play_icon: &'static str,
    event_sender: Option<broadcast::Sender<MediaEvent>>,
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
        if let Some(t) = track {
            self.full_title = t.to_string();
            let display = if self.full_title.len() > MAX_TITLE_LENGTH {
                format!("{}...", &self.full_title[..MAX_TITLE_LENGTH])
            } else {
                self.full_title.clone()
            };
            self.current_track = display;
        }
        self.play_icon = match state {
            PlaybackState::Playing => "⏸",
            PlaybackState::Paused => "▶",
            PlaybackState::Stopped => "▶",
        };
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

impl Recipe for BroadcastSubscription {
    type Output = Message;

    fn hash(&self, state: &mut Hasher) {
        std::any::TypeId::of::<Self>().hash(state);
    }

    fn stream(
        self: Box<Self>,
        _input: EventStream,
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

    fn init(
        core: cosmic::Core,
        _flags: Self::Flags,
    ) -> (Self, cosmic::app::Task<Self::Message>) {
        (
            Self {
                core,
                manager: None,
                current_track: "No media".to_string(),
                full_title: String::new(),
                play_icon: "▶",
                event_sender: None,
            },
            cosmic::app::Task::perform(
                async { MediaSourceManager::new().await },
                |result| match result {
                    Ok(manager) => cosmic::Action::App(Message::ManagerReady(Arc::new(manager))),
                    Err(e) => {
                        tracing::error!(error = %e, "Failed to initialize MediaSourceManager");
                        cosmic::Action::App(Message::ManagerReady(Arc::new(MediaSourceManager::new_empty())))
                    }
                },
            ),
        )
    }

    fn update(
        &mut self,
        message: Self::Message,
    ) -> cosmic::app::Task<Self::Message> {
        match message {
            Message::Previous => {
                if let Some(ref manager) = self.manager {
                    let m = manager.clone();
                    return cosmic::app::Task::perform(
                        async move { m.route(AppMessage::Previous).await },
                        |result| match result {
                            Ok(_) => cosmic::Action::None,
                            Err(_) => cosmic::Action::App(Message::MediaEvent(MediaEvent::StateChanged(PlaybackState::Stopped))),
                        },
                    );
                }
            }
            Message::PlayPause => {
                if let Some(ref manager) = self.manager {
                    let m = manager.clone();
                    return cosmic::app::Task::perform(
                        async move { m.route(AppMessage::PlayPause).await },
                        |result| match result {
                            Ok(_) => cosmic::Action::None,
                            Err(_) => cosmic::Action::App(Message::MediaEvent(MediaEvent::StateChanged(PlaybackState::Stopped))),
                        },
                    );
                }
            }
            Message::Next => {
                if let Some(ref manager) = self.manager {
                    let m = manager.clone();
                    return cosmic::app::Task::perform(
                        async move { m.route(AppMessage::Next).await },
                        |result| match result {
                            Ok(_) => cosmic::Action::None,
                            Err(_) => cosmic::Action::App(Message::MediaEvent(MediaEvent::StateChanged(PlaybackState::Stopped))),
                        },
                    );
                }
            }
            Message::ManagerReady(manager) => {
                self.event_sender = Some(manager.event_sender());
                self.manager = Some(manager.clone());
                let m = manager.clone();
                return cosmic::app::Task::perform(
                    async move {
                        (m.active_track().await, m.active_state().await)
                    },
                    |(track, state)| cosmic::Action::App(Message::UpdateState { track, state }),
                );
            }
            Message::MediaEvent(event) => {
                match event {
                    MediaEvent::StateChanged(state) => {
                        self.play_icon = match state {
                            PlaybackState::Playing => "⏸",
                            PlaybackState::Paused => "▶",
                            PlaybackState::Stopped => "▶",
                        };
                    }
                    MediaEvent::TrackChanged(track) => {
                        self.full_title = track.to_string();
                        let display = if self.full_title.len() > MAX_TITLE_LENGTH {
                            format!("{}...", &self.full_title[..MAX_TITLE_LENGTH])
                        } else {
                            self.full_title.clone()
                        };
                        self.current_track = display;
                    }
                    MediaEvent::SourceListChanged => {}
                }
            }
            Message::UpdateState { track, state } => {
                self.apply_track_state(track, state);
            }
        }
        cosmic::app::Task::none()
    }

    fn view(&self) -> Element<'_, Self::Message> {
        let previous_btn = cosmic::widget::button::text("<<").on_press(Message::Previous);
        let play_pause_btn = cosmic::widget::button::text(self.play_icon).on_press(Message::PlayPause);
        let next_btn = cosmic::widget::button::text(">>").on_press(Message::Next);

        let title_widget = cosmic::widget::text(&self.current_track).size(12);

        self.core.applet.popup_container(
            cosmic::widget::Row::new()
                .push(previous_btn)
                .push(play_pause_btn)
                .push(next_btn)
                .push(title_widget)
                .spacing(6)
        )
        .into()
    }

    fn subscription(&self) -> cosmic::iced::Subscription<Self::Message> {
        match &self.event_sender {
            Some(sender) => {
                from_recipe(BroadcastSubscription {
                    sender: sender.clone(),
                })
            }
            None => cosmic::iced::Subscription::none(),
        }
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}

fn main() -> cosmic::iced::Result {
    applet::run::<MediaApplet>(())
}
