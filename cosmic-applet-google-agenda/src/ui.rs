use cosmic::Element;
use cosmic::applet::menu_button;
use cosmic::iced::{Alignment, Length};
use cosmic::widget::{
    Column, Row, button, dropdown, scrollable, settings, text, text_input, toggler,
};

use crate::app::Message;
use crate::calendar::Event;

/// Selectable notification lead times (seconds), paired with `LEAD_LABELS` by
/// index. Exposed so the settings binary can map a dropdown selection back to
/// `notification_lead_secs`.
pub const LEAD_PRESETS_SECS: [u32; 5] = [60, 300, 600, 900, 1800];
const LEAD_LABELS: [&str; 5] = [
    "1 minute before",
    "5 minutes before",
    "10 minutes before",
    "15 minutes before",
    "30 minutes before",
];

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

pub fn menu_view<'a>() -> Element<'a, Message> {
    Column::new()
        .padding(4)
        .spacing(0)
        .push(menu_button(text::body("Refresh")).on_press(Message::RefreshFromMenu))
        .push(menu_button(text::body("Settings\u{2026}")).on_press(Message::OpenCredentials))
        .into()
}

/// How many of the following events (after the current/next one) to list in
/// the popup.
const UPCOMING_SHOWN: usize = 4;

pub fn event_info_view<'a>(events: &'a [Event], calendar_url: &str) -> Element<'a, Message> {
    let Some(next) = events.first() else {
        return Column::new()
            .padding([8, 16])
            .width(Length::Fill)
            .push(text::body("No upcoming events"))
            .into();
    };

    let mut header = Column::new()
        .padding([8, 16])
        .spacing(4)
        .width(Length::Fill)
        .push(text::title4(next.summary.clone()))
        .push(text::body(format_event_when(next)));
    if let Some(loc) = next.location.as_deref() {
        header = header.push(text::body(format!("\u{1f4cd} {loc}")));
    }

    let (label, url) = match next.meet_url.as_deref() {
        Some(u) => ("Open in Google Meet\u{2026}", u.to_owned()),
        None => ("Open calendar\u{2026}", calendar_url.to_owned()),
    };

    let mut col = Column::new()
        .padding([8, 0])
        .spacing(4)
        .width(Length::Fill)
        .push(header)
        .push(menu_button(text::body(label)).on_press(Message::OpenUrl(url)));

    for ev in events.iter().skip(1).take(UPCOMING_SHOWN) {
        col = col.push(upcoming_row(ev));
    }
    col.into()
}

fn upcoming_row(ev: &Event) -> Element<'_, Message> {
    let start = ev.start.with_timezone(&chrono::Local);
    Row::new()
        .padding([2, 16])
        .spacing(8)
        .width(Length::Fill)
        .push(text::caption(start.format("%a %H:%M").to_string()).width(Length::Fixed(80.0)))
        .push(text::body(truncate(&ev.summary, 28)))
        .into()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('\u{2026}');
        out
    }
}

fn format_event_when(ev: &Event) -> String {
    let start = ev.start.with_timezone(&chrono::Local);
    let end = ev.end.with_timezone(&chrono::Local);
    if start.date_naive() == end.date_naive() {
        format!(
            "{}\n{} \u{2013} {}",
            start.format("%A, %B %-d, %Y"),
            start.format("%H:%M"),
            end.format("%H:%M"),
        )
    } else {
        format!(
            "{}\n\u{2192} {}",
            start.format("%a, %b %-d, %Y %H:%M"),
            end.format("%a, %b %-d, %Y %H:%M"),
        )
    }
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
    pub on_toggle_notify: fn(bool) -> M,
    pub on_lead_change: fn(usize) -> M,
    pub on_try_notify: M,
    pub authorize: M,
    pub cancel: M,
}

#[allow(clippy::fn_params_excessive_bools, clippy::too_many_arguments)]
pub fn settings_view<'a, M: Clone + 'static>(
    form: &'a CredentialsForm,
    show_title: bool,
    show_time: bool,
    show_progress: bool,
    notify: bool,
    notification_lead_secs: u32,
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

    let mut notifications_section = settings::section()
        .title("Notifications")
        .add(settings::item(
            "Enable meeting notifications",
            toggler(notify).on_toggle(handlers.on_toggle_notify),
        ));
    if notify {
        let selected = LEAD_PRESETS_SECS
            .iter()
            .position(|&s| s == notification_lead_secs);
        notifications_section = notifications_section.add(settings::item(
            "Notify before start",
            dropdown(&LEAD_LABELS, selected, handlers.on_lead_change),
        ));
        notifications_section = notifications_section.add(settings::item(
            "Preview",
            button::standard("Try notification").on_press(handlers.on_try_notify.clone()),
        ));
    }

    let content = Column::new()
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
        .push(notifications_section);

    scrollable(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}
