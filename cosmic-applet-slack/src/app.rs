use cosmic::Element;
use cosmic::app::Task;
use cosmic::iced::widget::mouse_area;
use cosmic::iced::{Limits, Subscription, window::Id};
use cosmic::surface::{self, action::destroy_popup};
use cosmic::widget::button;
use cosmic_config::CosmicConfigEntry;
use futures_util::{SinkExt, StreamExt};
use tokio::signal::unix::{SignalKind, signal};

use crate::config::{APP_ID, Config};
use crate::slack::{self, SlackEvent, Unread};
use crate::ui;

const SLACK_ICON_SVG: &[u8] =
    include_bytes!("../data/icons/com.github.ragusa87.CosmicAppletSlack.svg");
const SLACK_URI: &str = "slack:";

#[derive(Default)]
pub struct AppModel {
    pub core: cosmic::Core,
    pub config: Config,
    pub unread: Unread,
    pub slack_running: bool,
    pub menu_popup: Option<Id>,
    pub paused: bool,
}

impl AppModel {
    pub fn is_paused(&self) -> bool {
        self.paused || (self.config.disable_during_weekend && is_weekend_local())
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    LeftClick,
    OpenMenu,
    PopupClosed(Id),

    SlackEvent(SlackEvent),
    ForceRefresh,
    RefreshFromMenu,
    TogglePause,
    ToggleDisableDuringWeekend,

    UpdateConfig(Config),

    NoOp,
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
        let config = cosmic_config::Config::new(Self::APP_ID, Config::VERSION)
            .map(|ctx| match Config::get_entry(&ctx) {
                Ok(c) => c,
                Err((_errors, c)) => c,
            })
            .unwrap_or_default();

        let app = AppModel {
            core,
            config,
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
        let paused = self.is_paused();

        let icon = cosmic::widget::icon(
            cosmic::widget::icon::from_svg_bytes(SLACK_ICON_SVG.to_vec()).symbolic(paused),
        )
        .size(icon_size);

        let badge_label = if self.slack_running && !paused {
            match self.unread {
                Unread::None => None,
                Unread::Indicator => Some("\u{2022}".to_owned()),
                Unread::Count(n) => Some(n.to_string()),
            }
        } else {
            None
        };

        let badge_height = (icon_px * 0.7).round();
        let badge_text_size = (icon_px * 0.46).round();
        let badge_pad_h = (icon_px * 0.22).round();
        let badge_pad_v = (icon_px * 0.06).round();
        let badge_radius = badge_height / 2.0;
        let badge_color = Color::from_rgb(0.29, 0.07, 0.34);

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
            .on_press(Message::LeftClick)
            .class(cosmic::theme::Button::AppletIcon);

        let interactive = mouse_area(btn).on_right_press(Message::OpenMenu);

        self.core.applet.autosize_window(interactive).into()
    }

    fn view_window(&self, _id: Id) -> Element<'_, Self::Message> {
        cosmic::widget::container(cosmic::widget::text("")).into()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        // Keep the subscription alive while paused: `SlackEvent` handlers
        // early-return when `is_paused()`, and the inner rescan timer keeps
        // ticking so weekend auto-pause re-evaluates the wall clock and
        // resumes automatically on Monday without user interaction.
        let slack = Subscription::run(|| slack::stream().map(Message::SlackEvent));
        let watch = self
            .core()
            .watch_config::<Config>(Self::APP_ID)
            .map(|update| Message::UpdateConfig(update.config));
        Subscription::batch([slack, watch, sigusr2_subscription()])
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::NoOp => {}

            Message::LeftClick => {
                if self.menu_popup.is_some() {
                    return Task::none();
                }
                return cosmic::task::future(async {
                    let _ = tokio::process::Command::new("xdg-open")
                        .arg(SLACK_URI)
                        .status()
                        .await;
                    Message::NoOp
                });
            }

            Message::OpenMenu => {
                if let Some(id) = self.menu_popup.take() {
                    return dispatch_surface(destroy_popup(id));
                }
                let new_id = Id::unique();
                self.menu_popup = Some(new_id);
                return open_menu_popup(new_id);
            }

            Message::PopupClosed(id) => {
                if self.menu_popup.as_ref() == Some(&id) {
                    self.menu_popup = None;
                }
            }

            Message::SlackEvent(SlackEvent::Unread(u)) => {
                if self.is_paused() {
                    return Task::none();
                }
                self.slack_running = true;
                self.unread = u;
            }

            Message::SlackEvent(SlackEvent::Gone) => {
                self.slack_running = false;
                self.unread = Unread::None;
            }

            Message::ForceRefresh => {
                slack::REFRESH_NOTIFY.notify_one();
            }

            Message::RefreshFromMenu => {
                let destroy_menu = self
                    .menu_popup
                    .take()
                    .map_or_else(Task::none, |id| dispatch_surface(destroy_popup(id)));
                let refresh = cosmic::task::message(cosmic::Action::App(Message::ForceRefresh));
                return Task::batch([destroy_menu, refresh]);
            }

            Message::TogglePause => {
                self.paused = !self.paused;
                let destroy_menu = self
                    .menu_popup
                    .take()
                    .map_or_else(Task::none, |id| dispatch_surface(destroy_popup(id)));
                let resume = if self.is_paused() {
                    Task::none()
                } else {
                    cosmic::task::message(cosmic::Action::App(Message::ForceRefresh))
                };
                return Task::batch([destroy_menu, resume]);
            }

            Message::ToggleDisableDuringWeekend => {
                self.config.disable_during_weekend = !self.config.disable_during_weekend;
                persist_config(&self.config);
                let destroy_menu = self
                    .menu_popup
                    .take()
                    .map_or_else(Task::none, |id| dispatch_surface(destroy_popup(id)));
                return destroy_menu;
            }

            Message::UpdateConfig(config) => {
                self.config = config;
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

fn is_weekend_local() -> bool {
    use chrono::Datelike;
    matches!(
        chrono::Local::now().weekday(),
        chrono::Weekday::Sat | chrono::Weekday::Sun
    )
}

fn persist_config(config: &Config) {
    match cosmic_config::Config::new(APP_ID, Config::VERSION) {
        Ok(ctx) => {
            if let Err(why) = config.write_entry(&ctx) {
                tracing::warn!(?why, "failed writing config entry");
            }
        }
        Err(why) => tracing::warn!(?why, "failed opening cosmic-config"),
    }
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

fn open_menu_popup(new_id: Id) -> Task<Message> {
    let action = surface::action::app_popup::<AppModel>(
        move |state: &mut AppModel| {
            let parent = state.core.main_window_id().unwrap_or(Id::NONE);
            let mut settings = state
                .core
                .applet
                .get_popup_settings(parent, new_id, None, None, None);
            settings.grab = true;
            settings.positioner.size_limits = Limits::NONE
                .max_width(280.0)
                .min_width(180.0)
                .min_height(40.0)
                .max_height(200.0);
            settings
        },
        Some(Box::new(|state: &AppModel| {
            let body = ui::menu_view(
                state.paused,
                state.is_paused(),
                state.config.disable_during_weekend,
            );
            Element::from(state.core.applet.popup_container(body)).map(cosmic::Action::App)
        })),
    );
    dispatch_surface(action)
}
