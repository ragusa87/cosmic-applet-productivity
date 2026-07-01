use cosmic::Element;
use cosmic::applet::menu_button;
use cosmic::iced::{Alignment, Length};
use cosmic::widget::{Column, Row, button, scrollable, settings, text, text_input, toggler};

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
    pub fn is_complete(&self) -> bool {
        !self.email.is_empty() && !self.client_id.is_empty() && !self.client_secret.is_empty()
    }
}

/// `manual_paused` drives the Pause/Resume label (what the menu toggle
/// controls); `effective_paused` (manual OR weekend auto-pause) decides whether
/// the Refresh item is shown, since refreshing does nothing while paused.
pub fn menu_view<'a>(manual_paused: bool, effective_paused: bool) -> Element<'a, Message> {
    let pause_label = if manual_paused { "Resume" } else { "Pause" };
    let mut col = Column::new()
        .padding(4)
        .spacing(0)
        .push(menu_button(text::body(pause_label)).on_press(Message::TogglePause));
    if !effective_paused {
        col = col.push(menu_button(text::body("Refresh")).on_press(Message::RefreshFromMenu));
    }
    col.push(menu_button(text::body("Settings\u{2026}")).on_press(Message::OpenCredentials))
        .into()
}

/// Builders for the messages emitted by the credentials form. The form widget
/// is shared between the panel applet and the standalone settings binary;
/// they have different `Message` enums, so callers pass closures that build
/// their own variants from the form events.
pub struct CredentialsHandlers<M: Clone> {
    pub on_email: fn(String) -> M,
    pub on_client_id: fn(String) -> M,
    pub on_client_secret: fn(String) -> M,
    pub on_toggle_notify: fn(bool) -> M,
    pub on_toggle_auto_pause_weekend: fn(bool) -> M,
    pub authorize: M,
    pub cancel: M,
}

pub fn credentials_view<'a, M: Clone + 'static>(
    form: &'a CredentialsForm,
    notify: bool,
    auto_pause_weekend: bool,
    status: &'a Status,
    authorizing: bool,
    handlers: &CredentialsHandlers<M>,
) -> Element<'a, M> {
    let header = text::title4("Gmail settings");

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

    let mut cancel = button::standard("Cancel");
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
         Scope: gmail.metadata.",
    );

    let notifications_section = settings::section()
        .title("Notifications")
        .add(settings::item(
            "Notify on new mail",
            toggler(notify).on_toggle(handlers.on_toggle_notify),
        ));

    let pause_section = settings::section().title("Pause").add(settings::item(
        "Auto-pause on weekend",
        toggler(auto_pause_weekend).on_toggle(handlers.on_toggle_auto_pause_weekend),
    ));

    let content = Column::new()
        .padding(12)
        .spacing(10)
        .width(Length::Fill)
        .push(header)
        .push(text::body("Google credentials"))
        .push(email_field)
        .push(id_field)
        .push(secret_field)
        .push(actions)
        .push(hint)
        .push(notifications_section)
        .push(pause_section);

    scrollable(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}
