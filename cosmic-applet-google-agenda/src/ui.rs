use cosmic::Element;
use cosmic::applet::menu_button;
use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::{Alignment, Length};
use cosmic::widget::{
    Column, Row, button, container, dropdown, scrollable, settings, text, text_input, toggler,
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

/// Symbolic bell glyph shown on the meeting overlay. Single-path `currentColor`
/// SVG so libcosmic recolors it to the active theme rather than a fixed brand
/// color — keeps the overlay sober and theme-native.
const OVERLAY_ICON_SVG: &[u8] = include_bytes!("../data/icons/meeting-overlay-symbolic.svg");

/// Content shown on the full-screen meeting overlay. Kept as plain strings so
/// the same view serves both a real reminder (built from the upcoming event via
/// [`OverlayContent::from_event`]) and the `--test-overlay` dry run, which has
/// no calendar event to draw from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayContent {
    pub title: String,
    pub countdown: String,
    pub time: Option<String>,
}

impl OverlayContent {
    /// Build the overlay copy from the upcoming event and the current time.
    pub fn from_event(ev: &Event, now: chrono::DateTime<chrono::Utc>) -> Self {
        let mins = (ev.start - now).num_minutes();
        let countdown = if mins <= 0 {
            "Starting now".to_owned()
        } else if mins == 1 {
            "Starting in 1 minute".to_owned()
        } else {
            format!("Starting in {mins} minutes")
        };
        let start = ev.start.with_timezone(&chrono::Local);
        let end = ev.end.with_timezone(&chrono::Local);
        Self {
            title: ev.summary.clone(),
            countdown,
            time: Some(format!(
                "{} \u{2013} {}",
                start.format("%H:%M"),
                end.format("%H:%M")
            )),
        }
    }

    /// Placeholder content for the `--test-overlay` CLI flag.
    pub fn test() -> Self {
        Self {
            title: "Team standup".to_owned(),
            countdown: "Starting in 5 minutes".to_owned(),
            time: Some("10:30 \u{2013} 10:45".to_owned()),
        }
    }
}

/// Full-screen reminder rendered on a layer-shell overlay when a meeting is
/// about to start. `None` is tolerated so the view renders harmlessly during
/// the brief window between dismissing the overlay and the surface being torn
/// down.
pub fn meeting_overlay_view(content: Option<&OverlayContent>) -> Element<'_, Message> {
    let Some(content) = content else {
        return container(text::body("")).into();
    };

    let icon = cosmic::widget::icon(
        cosmic::widget::icon::from_svg_bytes(OVERLAY_ICON_SVG.to_vec()).symbolic(true),
    )
    .size(56)
    .class(cosmic::theme::Svg::Custom(std::rc::Rc::new(|theme| {
        cosmic::widget::svg::Style {
            color: Some(theme.cosmic().accent_color().into()),
        }
    })));

    let mut details = Column::new()
        .align_x(Alignment::Center)
        .spacing(6)
        .push(text::title1(content.title.clone()))
        .push(text::title3(content.countdown.clone()).class(cosmic::theme::Text::Accent));
    if let Some(time) = content.time.as_deref() {
        details = details.push(text::body(time.to_owned()));
    }

    let actions = Row::new()
        .spacing(12)
        .push(button::standard("Snooze 1 min").on_press(Message::SnoozeOverlay))
        .push(button::suggested("Dismiss").on_press(Message::DismissOverlay));

    let card = Column::new()
        .align_x(Alignment::Center)
        .spacing(28)
        .push(icon)
        .push(details)
        .push(actions);

    let framed = container(card.padding([44, 64]))
        .class(cosmic::theme::Container::Card)
        .max_width(560.0);

    container(framed)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .class(cosmic::theme::Container::WindowBackground)
        .into()
}

pub fn menu_view<'a>(effective_paused: bool) -> Element<'a, Message> {
    let pause_label = if effective_paused { "Resume" } else { "Pause" };
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
    pub on_toggle_show_meeting_overlay: fn(bool) -> M,
    pub on_toggle_disable_during_weekend: fn(bool) -> M,
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
    show_meeting_overlay: bool,
    notification_lead_secs: u32,
    disable_during_weekend: bool,
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
        ))
        .add(settings::item_row(vec![
            Column::new()
                .spacing(2)
                .width(Length::Fill)
                .push(text::body("Show meeting overlay"))
                .push(text::caption(
                    "Full-screen reminder when a meeting is about to start",
                ))
                .into(),
            toggler(show_meeting_overlay)
                .on_toggle(handlers.on_toggle_show_meeting_overlay)
                .into(),
        ]));
    // The lead time drives both the desktop notification and the overlay, so
    // expose it whenever either is enabled.
    if notify || show_meeting_overlay {
        let selected = LEAD_PRESETS_SECS
            .iter()
            .position(|&s| s == notification_lead_secs);
        notifications_section = notifications_section.add(settings::item(
            "Notify before start",
            dropdown(&LEAD_LABELS, selected, handlers.on_lead_change),
        ));
    }
    if notify {
        notifications_section = notifications_section.add(settings::item(
            "Preview",
            button::standard("Try notification").on_press(handlers.on_try_notify.clone()),
        ));
    }

    let behavior_section = settings::section().title("Behavior").add(settings::item(
        "Pause on weekends",
        toggler(disable_during_weekend).on_toggle(handlers.on_toggle_disable_during_weekend),
    ));

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
        .push(notifications_section)
        .push(behavior_section);

    scrollable(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone, Utc};

    fn ev(start: chrono::DateTime<Utc>, end: chrono::DateTime<Utc>) -> Event {
        Event {
            id: "e1".to_owned(),
            summary: "Team standup".to_owned(),
            start,
            end,
            meet_url: None,
            location: None,
        }
    }

    #[test]
    fn overlay_countdown_pluralizes_and_rounds_down() {
        let now = Utc.with_ymd_and_hms(2026, 5, 12, 9, 55, 30).unwrap();
        // 4m30s out rounds down to "4 minutes".
        let start = Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap();
        let content = OverlayContent::from_event(&ev(start, start + Duration::minutes(30)), now);
        assert_eq!(content.title, "Team standup");
        assert_eq!(content.countdown, "Starting in 4 minutes");
    }

    #[test]
    fn overlay_countdown_singular_minute() {
        let now = Utc.with_ymd_and_hms(2026, 5, 12, 9, 59, 0).unwrap();
        let start = Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap();
        let content = OverlayContent::from_event(&ev(start, start + Duration::minutes(30)), now);
        assert_eq!(content.countdown, "Starting in 1 minute");
    }

    #[test]
    fn overlay_countdown_now_when_started() {
        let start = Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap();
        let now = start; // exactly at start
        let content = OverlayContent::from_event(&ev(start, start + Duration::minutes(30)), now);
        assert_eq!(content.countdown, "Starting now");
        assert!(content.time.is_some());
    }
}
