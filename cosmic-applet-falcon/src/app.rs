use cosmic::Element;
use cosmic::app::Task;
use cosmic::iced::widget::mouse_area;
use cosmic::iced::{Limits, Subscription, window::Id};
use cosmic::surface::{self, action::LiveSettings, action::destroy_popup};
use cosmic::widget::button;
use futures_util::{SinkExt, StreamExt};
use tokio::signal::unix::{SignalKind, signal};

use crate::systemd::{self, ActiveState, Snapshot, UnitAction};
use crate::ui;

const APP_ID: &str = "com.github.ragusa87.CosmicAppletFalcon";
const FALCON_ICON_SVG: &[u8] =
    include_bytes!("../data/icons/com.github.ragusa87.CosmicAppletFalcon.svg");

#[derive(Default)]
pub struct AppModel {
    pub core: cosmic::Core,
    pub snapshot: Option<Snapshot>,
    pub status_error: Option<String>,
    pub action_error: Option<String>,
    pub pending: Option<UnitAction>,
    pub popup: Option<Id>,
}

#[derive(Debug, Clone)]
pub enum Message {
    TogglePopup,
    PopupClosed(Id),

    StatusUpdate(Result<Snapshot, String>),
    RunAction(UnitAction),
    ActionDone(Option<String>),
    ForceRefresh,
}

impl AppModel {
    fn active_state(&self) -> Option<ActiveState> {
        self.snapshot.as_ref().map(|s| s.active_state)
    }

    pub fn is_running(&self) -> bool {
        self.active_state().is_some_and(ActiveState::is_running)
    }
}

impl cosmic::Application for AppModel {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;

    const APP_ID: &'static str = APP_ID;

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(core: cosmic::Core, _flags: Self::Flags) -> (Self, Task<Self::Message>) {
        let app = AppModel {
            core,
            ..Default::default()
        };
        (app, Task::none())
    }

    fn on_close_requested(&self, id: Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    fn view(&self) -> Element<'_, Self::Message> {
        use cosmic::applet::cosmic_panel_config::PanelAnchor;
        use cosmic::iced::{Color, Length};

        let is_horizontal = matches!(
            self.core.applet.anchor,
            PanelAnchor::Top | PanelAnchor::Bottom
        );

        let (icon_size, _) = self.core.applet.suggested_size(true);
        let (pad_major, pad_minor) = self.core.applet.suggested_padding(true);
        let icon_px = f32::from(icon_size);

        // Colored falcon while the sensor runs, monochrome/grey when it doesn't.
        let icon = cosmic::widget::icon(
            cosmic::widget::icon::from_svg_bytes(FALCON_ICON_SVG.to_vec())
                .symbolic(!self.is_running()),
        )
        .size(icon_size);

        let badge_label = match self.active_state() {
            Some(ActiveState::Failed) => Some("!"),
            Some(s) if s.is_transitional() => Some("\u{2022}"),
            _ => None,
        };

        let badge_height = (icon_px * 0.7).round();
        let badge_text_size = (icon_px * 0.46).round();
        let badge_pad_h = (icon_px * 0.22).round();
        let badge_pad_v = (icon_px * 0.06).round();
        let badge_radius = badge_height / 2.0;
        let badge_color = match self.active_state() {
            Some(ActiveState::Failed) => Color::from_rgb(0.75, 0.11, 0.15),
            _ => Color::from_rgb(0.85, 0.55, 0.05),
        };

        let extra = badge_radius.round();
        let stack_px = icon_px + extra;

        let icon_area = cosmic::widget::container(icon)
            .width(Length::Fixed(stack_px))
            .height(Length::Fixed(stack_px))
            .align_x(cosmic::iced::alignment::Horizontal::Left)
            .align_y(cosmic::iced::alignment::Vertical::Top);

        let stacked: Element<'_, Self::Message> = if let Some(label) = badge_label {
            let badge_text = cosmic::widget::text(label)
                .size(badge_text_size)
                .class(Color::WHITE)
                .font(cosmic::font::bold());

            let badge_pill = cosmic::widget::container(badge_text)
                .padding([badge_pad_v, badge_pad_h])
                .height(Length::Fixed(badge_height))
                .align_x(cosmic::iced::alignment::Horizontal::Center)
                .align_y(cosmic::iced::alignment::Vertical::Center)
                .style(
                    move |_theme: &cosmic::Theme| cosmic::iced::widget::container::Style {
                        background: Some(cosmic::iced::Background::Color(badge_color)),
                        border: cosmic::iced::Border {
                            radius: cosmic::iced::border::Radius::from(badge_radius),
                            ..Default::default()
                        },
                        text_color: Some(Color::WHITE),
                        ..Default::default()
                    },
                );

            let badge_area = cosmic::widget::container(badge_pill)
                .width(Length::Fixed(stack_px))
                .height(Length::Fixed(stack_px))
                .align_x(cosmic::iced::alignment::Horizontal::Right)
                .align_y(cosmic::iced::alignment::Vertical::Bottom);

            cosmic::iced::widget::Stack::new()
                .width(Length::Fixed(stack_px))
                .height(Length::Fixed(stack_px))
                .push(icon_area)
                .push(badge_area)
                .into()
        } else {
            icon_area.into()
        };

        let (horizontal_padding, vertical_padding) = if is_horizontal {
            (pad_major, pad_minor)
        } else {
            (pad_minor, pad_major)
        };

        let btn = button::custom(stacked)
            .padding([vertical_padding, horizontal_padding])
            .on_press(Message::TogglePopup)
            .class(cosmic::theme::Button::AppletIcon);

        let interactive = mouse_area(btn).on_right_press(Message::TogglePopup);

        self.core.applet.autosize_window(interactive).into()
    }

    fn view_window(&self, _id: Id) -> Element<'_, Self::Message> {
        cosmic::widget::container(cosmic::widget::text("")).into()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        let status = Subscription::run(|| systemd::stream().map(Message::StatusUpdate));
        Subscription::batch([status, sigusr2_subscription()])
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::TogglePopup => {
                if let Some(id) = self.popup.take() {
                    return dispatch_surface(destroy_popup(id));
                }
                let new_id = Id::unique();
                self.popup = Some(new_id);
                return open_popup(new_id);
            }

            Message::PopupClosed(id) => {
                if self.popup.as_ref() == Some(&id) {
                    self.popup = None;
                }
            }

            Message::StatusUpdate(Ok(snapshot)) => {
                self.snapshot = Some(snapshot);
                self.status_error = None;
            }

            Message::StatusUpdate(Err(e)) => {
                self.status_error = Some(e);
            }

            Message::RunAction(action) => {
                if self.pending.is_some() {
                    return Task::none();
                }
                self.pending = Some(action);
                self.action_error = None;
                return cosmic::task::future(async move {
                    Message::ActionDone(systemd::run_action(action).await.err())
                });
            }

            Message::ActionDone(error) => {
                self.pending = None;
                self.action_error = error;
                systemd::REFRESH_NOTIFY.notify_one();
            }

            Message::ForceRefresh => {
                systemd::REFRESH_NOTIFY.notify_one();
            }
        }
        Task::none()
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}

fn dispatch_surface(a: surface::Action) -> Task<Message> {
    cosmic::task::message(cosmic::Action::Cosmic(cosmic::app::Action::Surface(a)))
}

fn sigusr2_stream() -> impl cosmic::iced::futures::Stream<Item = Message> {
    cosmic::iced::stream::channel(
        4,
        |mut sender: cosmic::iced::futures::channel::mpsc::Sender<Message>| async move {
            let mut sig = match signal(SignalKind::user_defined2()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "failed to install SIGUSR2 handler");
                    return;
                }
            };
            while sig.recv().await.is_some() {
                if sender.send(Message::ForceRefresh).await.is_err() {
                    break;
                }
            }
        },
    )
}

fn sigusr2_subscription() -> Subscription<Message> {
    Subscription::run(sigusr2_stream)
}

fn open_popup(new_id: Id) -> Task<Message> {
    let action = surface::action::app_popup::<AppModel>(
        |_| LiveSettings::default(),
        move |state: &mut AppModel| {
            let parent = state.core.main_window_id().unwrap_or(Id::NONE);
            let mut settings = state
                .core
                .applet
                .get_popup_settings(parent, new_id, None, None, None);
            settings.grab = true;
            settings.positioner.size_limits = Limits::NONE
                .max_width(360.0)
                .min_width(260.0)
                .min_height(60.0)
                .max_height(400.0);
            settings
        },
        Some(Box::new(|state: &AppModel| {
            let body = ui::popup_view(state);
            Element::from(state.core.applet.popup_container(body)).map(cosmic::Action::App)
        })),
    );
    dispatch_surface(action)
}
