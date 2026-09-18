use cosmic_media_applet::mpris::{MediaEvent, PlaybackState, TrackInfo};
use cosmic_media_applet::manager::MediaSourceManager;
use cosmic_media_applet::message::AppMessage;
use cosmic::applet;
use cosmic::prelude::*;
use cosmic::iced::platform_specific::shell::wayland::commands::popup::{destroy_popup, get_popup};
use cosmic::iced::advanced::subscription::from_recipe;
use cosmic::iced::window;
use std::hash::Hash;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing;

const MAX_TITLE_LENGTH: usize = 30;

pub struct MediaApplet {
    core: cosmic::Core,
    manager: Option<Arc<MediaSourceManager>>,
    popup: Option<window::Id>,
    current_track: String,
    full_title: String,
    play_icon: &'static str,
    current_track_info: Option<TrackInfo>,
    current_state: PlaybackState,
}

#[derive(Clone)]
pub enum Message {
    Previous,
    PlayPause,
    Next,
    TogglePopup,
    PopupClosed(window::Id),
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
            self.full_title = t.to_string();
            let display = if self.full_title.chars().count() > MAX_TITLE_LENGTH {
                self.full_title.chars().take(MAX_TITLE_LENGTH).collect::<String>() + "..."
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
            Message::TogglePopup => f.debug_struct("TogglePopup").finish(),
            Message::PopupClosed(id) => f.debug_struct("PopupClosed").field("id", id).finish(),
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
                popup: None,
                current_track_info: None,
                current_state: PlaybackState::Stopped,
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

    fn on_close_requested(&self, id: window::Id) -> Option<Self::Message> {
        Some(Message::PopupClosed(id))
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
                    let m = manager.clone();
                    return cosmic::app::Task::perform(
                        async move { m.route(AppMessage::PlayPause).await },
                        |result| match result {
                            Ok(_) => cosmic::Action::None,
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
                    let m = manager.clone();
                    return cosmic::app::Task::perform(
                        async move { m.route(AppMessage::Next).await },
                        |result| match result {
                            Ok(_) => cosmic::Action::None,
                            Err(e) => {
                                tracing::warn!(error = %e, "Next command failed");
                                cosmic::Action::None
                            }
                        },
                    );
                }
            }
            Message::TogglePopup => {
                if let Some(id) = self.popup.take() {
                    return destroy_popup(id);
                } else {
                    let new_id = window::Id::unique();
                    self.popup.replace(new_id);
                    let mut popup_settings = self.core.applet.get_popup_settings(
                        self.core.main_window_id().unwrap(),
                        new_id,
                        None,
                        None,
                        None,
                    );
                    popup_settings.positioner.size_limits = cosmic::iced::Limits::NONE
                        .max_width(372.0)
                        .min_width(300.0)
                        .min_height(200.0)
                        .max_height(1080.0);
                    return get_popup(popup_settings);
                }
            }
            Message::PopupClosed(id) => {
                if self.popup.as_ref() == Some(&id) {
                    self.popup = None;
                }
            }
            Message::ManagerReady(manager) => {
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
                        self.apply_track_state(self.current_track_info.clone(), state);
                        if let Some(ref m) = self.manager {
                            let m2 = m.clone();
                            return cosmic::app::Task::perform(
                                async move { m2.reselect_active().await },
                                |_| cosmic::Action::None,
                            );
                        }
                    }
                    MediaEvent::TrackChanged(track) => {
                        self.apply_track_state(Some(track), self.current_state.clone());
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
        cosmic::widget::Row::new()
            .push(
                self.core
                    .applet
                    .icon_button("multimedia-player-symbolic")
                    .on_press(Message::TogglePopup),
            )
            .push(cosmic::widget::text("Media").size(14))
            .spacing(4)
            .padding([6, 10])
            .into()
    }

    fn view_window(&self, _id: window::Id) -> Element<'_, Self::Message> {
        let previous_btn = cosmic::widget::button::text("<<").on_press(Message::Previous);
        let play_pause_btn = cosmic::widget::button::text(self.play_icon).on_press(Message::PlayPause);
        let next_btn = cosmic::widget::button::text(">>").on_press(Message::Next);

        let title_widget = cosmic::widget::text(&self.current_track).size(14);

        self.core.applet.popup_container(
            cosmic::widget::Row::new()
                .push(previous_btn)
                .push(play_pause_btn)
                .push(next_btn)
                .push(title_widget)
                .spacing(6)
                .padding(10)
        )
        .into()
    }

    fn subscription(&self) -> cosmic::iced::Subscription<Self::Message> {
        match &self.manager {
            Some(manager) => {
                from_recipe(BroadcastSubscription {
                    sender: manager.event_sender(),
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
