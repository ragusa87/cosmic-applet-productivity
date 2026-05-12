use cosmic::Element;
use cosmic::applet::menu_button;
use cosmic::iced::{Alignment, Length};
use cosmic::widget::{Column, Row, button, settings, text, text_input, toggler};

use crate::app::Message;

#[derive(Debug, Clone, Default)]
pub enum Status {
    #[default]
    Idle,
    Authorizing,
    Saved,
    Error(String),
}

#[derive(Debug, Clone, Default)]
pub struct CredentialsForm {
    pub email: String,
    pub client_id: String,
    pub client_secret: String,
}

impl CredentialsForm {
    pub fn fill_ids_from_env(&mut self) {
        if self.client_id.is_empty()
            && let Ok(v) = std::env::var("AGENDA_PANEL_CLIENT_ID")
        {
            self.client_id = v;
        }
    }

    pub fn fill_secret_from_env(&mut self) {
        if self.client_secret.is_empty()
            && let Ok(v) = std::env::var("AGENDA_PANEL_CLIENT_SECRET")
        {
            self.client_secret = v;
        }
    }

    pub fn is_complete(&self) -> bool {
        !self.email.is_empty() && !self.client_id.is_empty() && !self.client_secret.is_empty()
    }
}

pub fn menu_view<'a>() -> Element<'a, Message> {
    Column::new()
        .padding(4)
        .spacing(0)
        .push(menu_button(text::body("Settings\u{2026}")).on_press(Message::OpenCredentials))
        .into()
}

/// Builders for the messages emitted by the settings form. The form widget
/// is shared between the panel applet and the standalone settings binary;
/// they have different `Message` enums, so callers pass closures that build
/// their own variants from the form events.
pub struct SettingsHandlers<M: Clone> {
    pub on_email: fn(String) -> M,
    pub on_client_id: fn(String) -> M,
    pub on_client_secret: fn(String) -> M,
    pub on_toggle_show_title: fn(bool) -> M,
    pub on_toggle_show_time: fn(bool) -> M,
    pub on_toggle_show_progress: fn(bool) -> M,
    pub authorize: M,
    pub cancel: M,
}

#[allow(clippy::fn_params_excessive_bools)]
pub fn settings_view<'a, M: Clone + 'static>(
    form: &'a CredentialsForm,
    show_title: bool,
    show_time: bool,
    show_progress: bool,
    status: &'a Status,
    authorizing: bool,
    handlers: &SettingsHandlers<M>,
) -> Element<'a, M> {
    let header = text::title4("Settings");

    let email_field = text_input("user@gmail.com", &form.email)
        .label("Email")
        .on_input(handlers.on_email);

    let id_field = text_input("…apps.googleusercontent.com", &form.client_id)
        .label("OAuth client ID")
        .on_input(handlers.on_client_id);

    let secret_field = text_input("GOCSPX-…", &form.client_secret)
        .label("OAuth client secret")
        .password()
        .on_input(handlers.on_client_secret);

    let mut authorize = button::suggested("Authorize with Google");
    if form.is_complete() && !authorizing {
        authorize = authorize.on_press(handlers.authorize.clone());
    }

    let mut cancel = button::standard("Close");
    if !authorizing {
        cancel = cancel.on_press(handlers.cancel.clone());
    }

    let status_line: Element<'a, M> = match status {
        Status::Idle => text::caption("").into(),
        Status::Authorizing => text::caption("Waiting for browser…").into(),
        Status::Saved => text::caption("✔ Saved").into(),
        Status::Error(e) => text::caption(format!("✗ {e}")).into(),
    };

    let actions = Row::new()
        .align_y(Alignment::Center)
        .spacing(8)
        .push(cancel)
        .push(authorize)
        .push(status_line);

    let hint = text::caption(
        "Create an OAuth desktop client in Google Cloud Console (see README). \
         Scope: calendar.events.readonly.",
    );

    let display_section = settings::section()
        .title("Display")
        .add(settings::item(
            "Show event time next to icon",
            toggler(show_time).on_toggle(handlers.on_toggle_show_time),
        ))
        .add(settings::item(
            "Show event title next to countdown",
            toggler(show_title).on_toggle(handlers.on_toggle_show_title),
        ))
        .add(settings::item(
            "Show meeting progress on icon",
            toggler(show_progress).on_toggle(handlers.on_toggle_show_progress),
        ));

    Column::new()
        .padding(12)
        .spacing(10)
        .width(Length::Fill)
        .push(header)
        .push(text::body("Google Calendar credentials"))
        .push(email_field)
        .push(id_field)
        .push(secret_field)
        .push(actions)
        .push(hint)
        .push(display_section)
        .into()
}
