use cosmic::Element;
use cosmic::applet::menu_button;
use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::{Alignment, Length};
use cosmic::widget::{
    Column, Row, button, container, dropdown, scrollable, settings, text, text_input, toggler,
};

use crate::app::Message;
use crate::calendar::Event;
use crate::config::Config;

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
    /// Start of the event this overlay reminds about, kept so the countdown can
    /// be recomputed when the overlay is snoozed and re-shown. `None` for the
    /// `--test-overlay` placeholder, which has no real event.
    pub start: Option<chrono::DateTime<chrono::Utc>>,
}

/// Format the "Starting in …" line for an event `mins` minutes away.
fn format_countdown(mins: i64) -> String {
    if mins <= 0 {
        "Starting now".to_owned()
    } else if mins == 1 {
        "Starting in 1 minute".to_owned()
    } else {
        format!("Starting in {mins} minutes")
    }
}

impl OverlayContent {
    /// Build the overlay copy from the upcoming event and the current time.
    pub fn from_event(ev: &Event, now: chrono::DateTime<chrono::Utc>) -> Self {
        let mins = (ev.start - now).num_minutes();
        let start_local = ev.start.with_timezone(&chrono::Local);
        let end = ev.end.with_timezone(&chrono::Local);
        Self {
            title: ev.summary.clone(),
            countdown: format_countdown(mins),
            time: Some(format!(
                "{} \u{2013} {}",
                start_local.format("%H:%M"),
                end.format("%H:%M")
            )),
            start: Some(ev.start),
        }
    }

    /// Recompute the countdown line against `now`. Called when a snoozed overlay
    /// is re-shown so it reflects the time actually remaining, not the time when
    /// it was first raised. No-op for content without a backing event.
    pub fn refresh(&mut self, now: chrono::DateTime<chrono::Utc>) {
        if let Some(start) = self.start {
            self.countdown = format_countdown((start - now).num_minutes());
        }
    }

    /// Placeholder content for the `--test-overlay` CLI flag.
    pub fn test() -> Self {
        Self {
            title: "Team standup".to_owned(),
            countdown: "Starting in 5 minutes".to_owned(),
            time: Some("10:30 \u{2013} 10:45".to_owned()),
            start: None,
        }
    }
}

/// Full-screen reminder rendered on a layer-shell overlay when a meeting is
/// about to start. `None` is tolerated so the view renders harmlessly during
/// the brief window between dismissing the overlay and the surface being torn
/// down.
pub fn meeting_overlay_view<'a, M: Clone + 'static>(
    content: Option<&OverlayContent>,
    on_snooze: M,
    on_dismiss: M,
) -> Element<'a, M> {
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
        .push(button::standard("Snooze 1 min").on_press(on_snooze))
        .push(button::suggested("Dismiss").on_press(on_dismiss));

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

/// Layer-shell settings for the meeting overlay: a top-most surface anchored to
/// every edge so it fills the output, drawn above panels (`exclusive_zone = -1`).
/// Shared so both the applet and the settings preview open an identical surface.
pub fn overlay_layer_settings(
    id: cosmic::iced::window::Id,
) -> cosmic::iced::runtime::platform_specific::wayland::layer_surface::SctkLayerSurfaceSettings {
    use cosmic::cctk::sctk::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer};
    use cosmic::iced::runtime::platform_specific::wayland::layer_surface::SctkLayerSurfaceSettings;

    SctkLayerSurfaceSettings {
        id,
        layer: Layer::Overlay,
        keyboard_interactivity: KeyboardInteractivity::OnDemand,
        anchor: Anchor::all(),
        namespace: "meeting-overlay".to_owned(),
        size: Some((None, None)),
        exclusive_zone: -1,
        ..Default::default()
    }
}

/// Live surface settings for the meeting overlay. Left at the defaults
/// (`corners: None`) on purpose: the overlay fills the whole output and must
/// have square corners, but pinning an explicit radius here forces libcosmic to
/// re-issue a `get_corner_radius_layer` request on every theme/configure event,
/// and on the COSMIC 1.8 corner-radius manager protocol the second request per
/// surface raises `corner_radius_exists` (error 0) in a tight loop. Square
/// corners are instead achieved by clearing `Auto::System` on the applet core
/// (see `AppModel::init`), which makes libcosmic skip corner handling for this
/// layer surface entirely.
pub fn overlay_live_settings() -> cosmic::surface::action::LiveSettings {
    cosmic::surface::action::LiveSettings::default()
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
    pub on_toggle_only_accepted: fn(bool) -> M,
    pub on_lead_change: fn(usize) -> M,
    pub on_try_notify: M,
    pub on_test_overlay: M,
    pub authorize: M,
    pub cancel: M,
}

/// The "Notifications" settings group. The lead-time dropdown and the
/// notification/overlay previews are conditionally shown, so this is split out
/// to keep `settings_view` readable.
fn notifications_section<'a, M: Clone + 'static>(
    notify: bool,
    show_meeting_overlay: bool,
    notification_lead_secs: u32,
    handlers: &SettingsHandlers<M>,
) -> Element<'a, M> {
    let mut section = settings::section()
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
        section = section.add(settings::item(
            "Notify before start",
            dropdown(&LEAD_LABELS, selected, handlers.on_lead_change),
        ));
    }
    if notify {
        section = section.add(settings::item(
            "Preview",
            button::standard("Try notification").on_press(handlers.on_try_notify.clone()),
        ));
    }
    if show_meeting_overlay {
        section = section.add(settings::item(
            "Preview overlay",
            button::standard("Test notification overlay")
                .on_press(handlers.on_test_overlay.clone()),
        ));
    }
    section.into()
}

pub fn settings_view<'a, M: Clone + 'static>(
    form: &'a CredentialsForm,
    config: &Config,
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
            toggler(config.show_time).on_toggle(handlers.on_toggle_show_time),
        ))
        .add(settings::item(
            "Show event title next to countdown",
            toggler(config.show_title).on_toggle(handlers.on_toggle_show_title),
        ))
        .add(settings::item(
            "Show meeting progress on icon",
            toggler(config.show_progress).on_toggle(handlers.on_toggle_show_progress),
        ));

    let notifications = notifications_section(
        config.notify,
        config.show_meeting_overlay,
        config.notification_lead_secs,
        handlers,
    );

    let behavior_section = settings::section()
        .title("Behavior")
        .add(settings::item(
            "Pause on weekends",
            toggler(config.disable_during_weekend)
                .on_toggle(handlers.on_toggle_disable_during_weekend),
        ))
        .add(settings::item_row(vec![
            Column::new()
                .spacing(2)
                .width(Length::Fill)
                .push(text::body("Only accepted events"))
                .push(text::caption(
                    "Ignore invitations you haven't accepted — no listing, reminder or overlay",
                ))
                .into(),
            toggler(config.only_accepted_events)
                .on_toggle(handlers.on_toggle_only_accepted)
                .into(),
        ]));

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
        .push(notifications)
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

    #[test]
    fn refresh_recomputes_countdown_after_snooze() {
        let start = Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap();
        // Raised 4 minutes out.
        let raised = Utc.with_ymd_and_hms(2026, 5, 12, 9, 56, 0).unwrap();
        let mut content =
            OverlayContent::from_event(&ev(start, start + Duration::minutes(30)), raised);
        assert_eq!(content.countdown, "Starting in 4 minutes");
        // Snoozed a minute; re-shown 1 minute closer.
        content.refresh(raised + Duration::minutes(1));
        assert_eq!(content.countdown, "Starting in 3 minutes");
    }

    #[test]
    fn refresh_noop_without_event() {
        let now = Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap();
        let mut content = OverlayContent::test();
        content.refresh(now);
        // Placeholder content has no backing event, so it is left untouched.
        assert_eq!(content.countdown, "Starting in 5 minutes");
    }
}
