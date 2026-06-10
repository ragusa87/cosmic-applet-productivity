use std::sync::Arc;
use std::time::Duration;

use cosmic::Element;
use cosmic::app::Task;
use cosmic::iced::widget::mouse_area;
use cosmic::iced::{Limits, Subscription, window::Id};
use cosmic::surface::{self, action::destroy_popup};
use cosmic::widget::button;
use futures_util::SinkExt;
use tokio::signal::unix::{SignalKind, signal};

use crate::models::{Provider, ProviderSnapshot, RefreshError};
use crate::{anthropic, openai, ui};

const APP_ID: &str = "com.github.ragusa87.CosmicAppletQuotaBar";
const ICON_SVG: &[u8] =
    include_bytes!("../data/icons/com.github.ragusa87.CosmicAppletQuotaBar.svg");
const REFRESH_INTERVAL: Duration = Duration::from_mins(5);

pub struct AppModel {
    pub core: cosmic::Core,
    pub client: Arc<reqwest::Client>,
    pub snapshots: Vec<ProviderSnapshot>,
    pub errors: Vec<RefreshError>,
    pub refreshing: bool,
    pub last_refresh: Option<chrono::DateTime<chrono::Utc>>,
    pub info_popup: Option<Id>,
    pub menu_popup: Option<Id>,
}

#[derive(Debug, Clone)]
pub enum Message {
    LeftClick,
    OpenMenu,
    PopupClosed(Id),
    Refresh,
    RefreshFromMenu,
    Refreshed {
        snapshots: Vec<ProviderSnapshot>,
        errors: Vec<RefreshError>,
    },
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
        let client = anthropic::http_client().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "falling back to default reqwest client");
            reqwest::Client::new()
        });
        let app = AppModel {
            core,
            client: Arc::new(client),
            snapshots: Vec::new(),
            errors: Vec::new(),
            refreshing: false,
            last_refresh: None,
            info_popup: None,
            menu_popup: None,
        };
        let kick = cosmic::task::message(cosmic::Action::App(Message::Refresh));
        (app, kick)
    }

    fn on_close_requested(&self, id: Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    fn view(&self) -> Element<'_, Self::Message> {
        use cosmic::applet::cosmic_panel_config::PanelAnchor;
        use cosmic::iced::widget::Row;
        use cosmic::iced::Length;

        let is_horizontal = matches!(
            self.core.applet.anchor,
            PanelAnchor::Top | PanelAnchor::Bottom
        );

        let (icon_size, _) = self.core.applet.suggested_size(true);
        let (pad_major, pad_minor) = self.core.applet.suggested_padding(true);
        let icon_px = f32::from(icon_size);
        let label_size = (icon_px * 0.55).round();

        let icon = cosmic::widget::icon(cosmic::widget::icon::from_svg_bytes(ICON_SVG.to_vec()))
            .size(icon_size);

        let worst = worst_used_percent(&self.snapshots);
        let label_text = worst.map(|w| format!("{}%", round_pct(w)));

        let mut row = Row::new()
            .align_y(cosmic::iced::Alignment::Center)
            .spacing(6)
            .push(icon);
        if is_horizontal
            && let Some(text_str) = label_text
        {
            row = row.push(
                cosmic::widget::text(text_str)
                    .size(label_size)
                    .height(Length::Fixed(icon_px))
                    .align_y(cosmic::iced::alignment::Vertical::Center),
            );
        }

        let content: Element<'_, Self::Message> = row.into();

        let (horizontal_padding, vertical_padding) = if is_horizontal {
            (pad_major, pad_minor)
        } else {
            (pad_minor, pad_major)
        };

        let btn = button::custom(content)
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
        let tick = cosmic::iced::time::every(REFRESH_INTERVAL).map(|_| Message::Refresh);
        Subscription::batch([tick, sigusr2_subscription()])
    }

    #[allow(clippy::too_many_lines)]
    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::LeftClick => {
                if self.menu_popup.is_some() {
                    return Task::none();
                }
                if let Some(id) = self.info_popup.take() {
                    return dispatch_surface(destroy_popup(id));
                }
                let new_id = Id::unique();
                self.info_popup = Some(new_id);
                return open_info_popup(new_id);
            }

            Message::OpenMenu => {
                if let Some(id) = self.menu_popup.take() {
                    return dispatch_surface(destroy_popup(id));
                }
                let close_info = self
                    .info_popup
                    .take()
                    .map_or_else(Task::none, |id| dispatch_surface(destroy_popup(id)));
                let new_id = Id::unique();
                self.menu_popup = Some(new_id);
                return Task::batch([close_info, open_menu_popup(new_id)]);
            }

            Message::PopupClosed(id) => {
                if self.info_popup.as_ref() == Some(&id) {
                    self.info_popup = None;
                }
                if self.menu_popup.as_ref() == Some(&id) {
                    self.menu_popup = None;
                }
            }

            Message::Refresh => {
                if self.refreshing {
                    return Task::none();
                }
                self.refreshing = true;
                let client = self.client.clone();
                return cosmic::task::future(async move {
                    let (snapshots, errors) = refresh_all(&client).await;
                    Message::Refreshed { snapshots, errors }
                });
            }

            Message::RefreshFromMenu => {
                let destroy_menu = self
                    .menu_popup
                    .take()
                    .map_or_else(Task::none, |id| dispatch_surface(destroy_popup(id)));
                let refresh = cosmic::task::message(cosmic::Action::App(Message::Refresh));
                return Task::batch([destroy_menu, refresh]);
            }

            Message::Refreshed { snapshots, errors } => {
                self.refreshing = false;
                self.snapshots = snapshots;
                self.errors = errors;
                self.last_refresh = Some(chrono::Utc::now());
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
                tracing::info!("SIGUSR2 received, forcing refresh");
                if sender.send(Message::Refresh).await.is_err() {
                    break;
                }
            }
        },
    )
}

fn sigusr2_subscription() -> Subscription<Message> {
    Subscription::run(sigusr2_stream)
}

async fn refresh_all(
    client: &reqwest::Client,
) -> (Vec<ProviderSnapshot>, Vec<RefreshError>) {
    let (anth, oai) =
        tokio::join!(anthropic::fetch_snapshot(client), openai::fetch_snapshot(client));

    let mut snapshots = Vec::new();
    let mut errors = Vec::new();
    match anth {
        Ok(s) => snapshots.push(s),
        Err(e) => {
            tracing::warn!(error = %e, "Anthropic snapshot failed");
            errors.push(RefreshError {
                provider: Provider::Anthropic,
                message: e.to_string(),
            });
        }
    }
    match oai {
        Ok(s) => snapshots.push(s),
        Err(e) => {
            tracing::warn!(error = %e, "OpenAI snapshot failed");
            errors.push(RefreshError {
                provider: Provider::OpenAi,
                message: e.to_string(),
            });
        }
    }
    (snapshots, errors)
}

fn worst_used_percent(snapshots: &[ProviderSnapshot]) -> Option<f64> {
    snapshots
        .iter()
        .filter_map(ProviderSnapshot::worst_used)
        .fold(None, |acc, x| Some(acc.map_or(x, |a: f64| a.max(x))))
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn round_pct(v: f64) -> i64 {
    v.clamp(-1_000.0, 1_000.0).round() as i64
}

fn open_info_popup(new_id: Id) -> Task<Message> {
    let action = surface::action::app_popup::<AppModel>(
        move |state: &mut AppModel| {
            let parent = state.core.main_window_id().unwrap_or(Id::NONE);
            let mut settings = state
                .core
                .applet
                .get_popup_settings(parent, new_id, None, None, None);
            settings.grab = true;
            settings.positioner.size_limits = Limits::NONE
                .max_width(420.0)
                .min_width(320.0)
                .min_height(120.0)
                .max_height(480.0);
            settings
        },
        Some(Box::new(|state: &AppModel| {
            let body = ui::dashboard_view(
                &state.snapshots,
                &state.errors,
                state.refreshing,
                state.last_refresh,
            );
            Element::from(state.core.applet.popup_container(body)).map(cosmic::Action::App)
        })),
    );
    dispatch_surface(action)
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
            let body = ui::menu_view();
            Element::from(state.core.applet.popup_container(body)).map(cosmic::Action::App)
        })),
    );
    dispatch_surface(action)
}
