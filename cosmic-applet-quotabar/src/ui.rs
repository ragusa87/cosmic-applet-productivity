use chrono::{DateTime, Local, NaiveDateTime, Timelike, Utc};
use cosmic::Element;
use cosmic::applet::menu_button;
use cosmic::iced::widget::{Row, canvas};
use cosmic::iced::{Alignment, Color, Length};
use cosmic::widget::{Column, container, text};
use cosmic_config::ConfigGet;

use crate::app::Message;
use crate::models::{ProviderSnapshot, RefreshError, ScopedLimit, SpendInfo, UsageWindow};

const ROW_WIDTH: f32 = 380.0;

pub fn menu_view<'a>() -> Element<'a, Message> {
    Column::new()
        .padding(4)
        .spacing(0)
        .push(menu_button(text::body("Refresh")).on_press(Message::RefreshFromMenu))
        .push(menu_button(text::body("Settings\u{2026}")).on_press(Message::OpenSettings))
        .into()
}

pub fn dashboard_view<'a>(
    snapshots: &'a [ProviderSnapshot],
    errors: &'a [RefreshError],
    refreshing: bool,
    last_refresh: Option<DateTime<Utc>>,
    ignore_credits_when_plan_used: bool,
    max_includes_scoped: bool,
) -> Element<'a, Message> {
    let header = Row::new()
        .align_y(Alignment::Center)
        .spacing(10)
        .push(text::title4("AI Quota"))
        .push(
            cosmic::widget::Space::new()
                .width(Length::Fill)
                .height(Length::Fixed(0.0)),
        )
        .push(refresh_button(refreshing));

    let mut col = Column::new().padding(12).spacing(10).push(header);

    if snapshots.is_empty() && errors.is_empty() {
        col = col.push(text::body(if refreshing {
            "Fetching first snapshot\u{2026}"
        } else {
            "No data yet"
        }));
    } else {
        for snapshot in snapshots {
            col = col.push(provider_card(
                snapshot,
                ignore_credits_when_plan_used,
                max_includes_scoped,
            ));
        }
        for err in errors {
            col = col.push(warning_banner(err));
        }
    }

    col = col.push(footer(last_refresh));
    col.into()
}

fn provider_card(
    snapshot: &ProviderSnapshot,
    ignore_credits_when_plan_used: bool,
    max_includes_scoped: bool,
) -> Element<'_, Message> {
    let now = chrono::Utc::now();
    let military = military_time();

    let mut header = Row::new()
        .align_y(Alignment::Center)
        .spacing(8)
        .push(text::body(snapshot.provider.display_name()).font(cosmic::font::bold()));
    if let Some(model) = snapshot.model.as_deref() {
        header = header.push(text::caption(model));
    }
    let header = header
        .push(
            cosmic::widget::Space::new()
                .width(Length::Fill)
                .height(Length::Fixed(0.0)),
        )
        .push(
            text::body(worst_badge(
                snapshot,
                ignore_credits_when_plan_used,
                max_includes_scoped,
            ))
            .font(cosmic::font::bold()),
        );

    let mut col = Column::new().padding(10).spacing(8).push(header);
    col = col.push(bar_row("DAILY", snapshot.short.as_ref(), now, military));
    col = col.push(bar_row("WEEKLY", snapshot.weekly.as_ref(), now, military));
    for limit in &snapshot.scoped {
        col = col.push(scoped_row(limit, now, military));
    }
    if let Some(spend) = snapshot.visible_spend(ignore_credits_when_plan_used) {
        col = col.push(spend_row(spend));
    }

    container(col).width(Length::Fill).padding(2).into()
}

// USD gets a `$` prefix; other currencies append their code (e.g. `38.70 EUR`).
fn format_money(value: f64, currency: &str) -> String {
    if currency == "USD" {
        format!("${value:.2}")
    } else {
        format!("{value:.2} {currency}")
    }
}

fn spend_label(spend: &SpendInfo) -> String {
    let used = format_money(spend.used, &spend.currency);
    match spend.limit {
        Some(limit) => format!("{used} / {}", format_money(limit, &spend.currency)),
        None => used,
    }
}

fn spend_row(spend: &SpendInfo) -> Element<'_, Message> {
    let bar = canvas(BarProgram {
        used_percent: spend.percent,
    })
    .width(Length::Fill)
    .height(Length::Fixed(10.0));

    Row::new()
        .align_y(Alignment::Center)
        .spacing(10)
        .width(Length::Fixed(ROW_WIDTH))
        .push(text::caption("CREDITS").width(Length::Fixed(56.0)))
        .push(bar)
        .push(
            text::caption(spend_label(spend))
                .width(Length::Fixed(120.0))
                .align_x(cosmic::iced::alignment::Horizontal::Right),
        )
        .into()
}

fn worst_badge(
    snapshot: &ProviderSnapshot,
    ignore_credits_when_plan_used: bool,
    max_includes_scoped: bool,
) -> String {
    snapshot
        .worst_used(ignore_credits_when_plan_used, max_includes_scoped)
        .map_or_else(|| "—".to_owned(), |w| format!("{}%", round_pct(w)))
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn round_pct(v: f64) -> i64 {
    v.clamp(-1_000.0, 1_000.0).round() as i64
}

fn bar_row<'a>(
    label: &'a str,
    window: Option<&'a UsageWindow>,
    now: DateTime<Utc>,
    military: bool,
) -> Element<'a, Message> {
    let pct_text = window.map_or_else(
        || "—".to_owned(),
        |w| format!("{}%", round_pct(w.used_percent)),
    );
    let reset_text = window
        .and_then(|w| w.resets_at)
        .map(|r| time_reset_label(r, now, military))
        .unwrap_or_default();

    let used = window.map_or(0.0, |w| w.used_percent);
    let bar = canvas(BarProgram { used_percent: used })
        .width(Length::Fill)
        .height(Length::Fixed(10.0));

    Row::new()
        .align_y(Alignment::Center)
        .spacing(10)
        .width(Length::Fixed(ROW_WIDTH))
        .push(text::caption(label).width(Length::Fixed(56.0)))
        .push(bar)
        .push(
            text::caption(pct_text)
                .width(Length::Fixed(44.0))
                .align_x(cosmic::iced::alignment::Horizontal::Right),
        )
        .push(
            text::caption(reset_text)
                .width(Length::Fixed(72.0))
                .align_x(cosmic::iced::alignment::Horizontal::Right),
        )
        .into()
}

// A per-model/per-surface limit bar. Same layout as `bar_row`, but the label
// comes from the limit itself and the data is always present.
fn scoped_row<'a>(
    limit: &'a ScopedLimit,
    now: DateTime<Utc>,
    military: bool,
) -> Element<'a, Message> {
    let pct_text = format!("{}%", round_pct(limit.used_percent));
    let reset_text = limit
        .resets_at
        .map(|r| time_reset_label(r, now, military))
        .unwrap_or_default();

    let bar = canvas(BarProgram {
        used_percent: limit.used_percent,
    })
    .width(Length::Fill)
    .height(Length::Fixed(10.0));

    Row::new()
        .align_y(Alignment::Center)
        .spacing(10)
        .width(Length::Fixed(ROW_WIDTH))
        .push(text::caption(limit.label.as_str()).width(Length::Fixed(56.0)))
        .push(bar)
        .push(
            text::caption(pct_text)
                .width(Length::Fixed(44.0))
                .align_x(cosmic::iced::alignment::Horizontal::Right),
        )
        .push(
            text::caption(reset_text)
                .width(Length::Fixed(72.0))
                .align_x(cosmic::iced::alignment::Horizontal::Right),
        )
        .into()
}

fn warning_banner(err: &RefreshError) -> Element<'_, Message> {
    let line = format!("{}: {}", err.provider.display_name(), err.message);
    text::caption(line).into()
}

fn footer<'a>(last_refresh: Option<DateTime<Utc>>) -> Element<'a, Message> {
    let s = last_refresh.map_or_else(
        || "Not yet refreshed".to_owned(),
        |t| {
            let age_secs = chrono::Utc::now().signed_duration_since(t).num_seconds();
            format!("Updated {} ago", short_duration(age_secs.max(0)))
        },
    );
    text::caption(s).into()
}

fn refresh_button<'a>(refreshing: bool) -> Element<'a, Message> {
    let label = if refreshing { "\u{2026}" } else { "\u{21bb}" };
    cosmic::widget::button::standard(label)
        .on_press(Message::Refresh)
        .into()
}

/// COSMIC's system-wide clock preference (24-hour vs 12-hour), read from the
/// time applet's config so we match the rest of the desktop. Defaults to
/// 12-hour — the same default COSMIC itself uses — when the key is absent.
fn military_time() -> bool {
    cosmic_config::Config::new("com.system76.CosmicAppletTime", 1)
        .and_then(|cfg| cfg.get::<bool>("military_time"))
        .unwrap_or(false)
}

/// Human-friendly label for when a quota window resets.
///
/// Within 12h we show a relative countdown ("in 6h"), which is the most
/// intuitive framing when the reset is imminent. Beyond that a countdown like
/// "in 34h" is hard to map onto a real moment, so we switch to an absolute
/// local weekday + time ("Mon 10am" or "Mon 13h", per the system clock format).
fn time_reset_label(resets_at: DateTime<Utc>, now: DateTime<Utc>, military: bool) -> String {
    let seconds = resets_at.signed_duration_since(now).num_seconds();
    if seconds < 12 * 60 * 60 {
        format!("in {}", short_duration(seconds))
    } else {
        absolute_reset(resets_at.with_timezone(&Local).naive_local(), military)
    }
}

/// Format a local datetime as a short weekday + time. With `military` we use a
/// 24-hour clock ("Mon 13h" / "Mon 13:30"), zero-padded so midnight reads as
/// "Mon 00h" rather than the confusing "Mon 0h"; otherwise a 12-hour clock with
/// a meridiem ("Mon 10am" / "Mon 10:30am"). Kept pure (no timezone or config
/// lookup) so it is easy to test.
fn absolute_reset(local: NaiveDateTime, military: bool) -> String {
    let weekday = local.format("%a");
    let minute = local.minute();
    if military {
        let hour = local.hour();
        if minute == 0 {
            format!("{weekday} {hour:02}h")
        } else {
            format!("{weekday} {hour:02}:{minute:02}")
        }
    } else {
        let (hour12, meridiem) = match local.hour() {
            0 => (12, "am"),
            h @ 1..=11 => (h, "am"),
            12 => (12, "pm"),
            h => (h - 12, "pm"),
        };
        if minute == 0 {
            format!("{weekday} {hour12}{meridiem}")
        } else {
            format!("{weekday} {hour12}:{minute:02}{meridiem}")
        }
    }
}

fn short_duration(seconds: i64) -> String {
    let s = seconds.max(0);
    let total_minutes = s / 60;
    if total_minutes < 60 {
        return format!("{total_minutes}m");
    }
    let hours = total_minutes / 60;
    if hours < 48 {
        let mins = total_minutes % 60;
        if mins == 0 {
            return format!("{hours}h");
        }
        return format!("{hours}h{mins}m");
    }
    let days = hours / 24;
    let rem = hours % 24;
    if rem == 0 {
        format!("{days}d")
    } else {
        format!("{days}d{rem}h")
    }
}

struct BarProgram {
    used_percent: f64,
}

impl canvas::Program<Message, cosmic::Theme> for BarProgram {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &cosmic::iced::Renderer,
        _theme: &cosmic::Theme,
        bounds: cosmic::iced::Rectangle,
        _cursor: cosmic::iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        use cosmic::iced::widget::canvas::{Frame, Path};
        use cosmic::iced::{Point, Size};

        let mut frame = Frame::new(renderer, bounds.size());
        let track = Color::from_rgb(0.12, 0.13, 0.16);
        frame.fill(
            &Path::rectangle(Point::ORIGIN, Size::new(bounds.width, bounds.height)),
            track,
        );

        let pct = self.used_percent.clamp(0.0, 100.0) / 100.0;
        #[allow(clippy::cast_possible_truncation)]
        let pct_f32 = pct as f32;
        let fill_width = bounds.width * pct_f32;
        if fill_width > 0.0 {
            let color = bar_color(self.used_percent);
            frame.fill(
                &Path::rectangle(Point::ORIGIN, Size::new(fill_width, bounds.height)),
                color,
            );
        }
        vec![frame.into_geometry()]
    }
}

fn bar_color(used_percent: f64) -> Color {
    if used_percent >= 90.0 {
        Color::from_rgb(0.94, 0.27, 0.27)
    } else if used_percent >= 75.0 {
        Color::from_rgb(0.98, 0.45, 0.09)
    } else if used_percent >= 50.0 {
        Color::from_rgb(0.96, 0.62, 0.04)
    } else {
        Color::from_rgb(0.13, 0.77, 0.37)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_money_usd_prefixes_dollar() {
        assert_eq!(format_money(38.77, "USD"), "$38.77");
        assert_eq!(format_money(80.0, "USD"), "$80.00"); // rounds to 2 dp
    }

    #[test]
    fn format_money_other_currency_appends_code() {
        assert_eq!(format_money(38.7, "EUR"), "38.70 EUR");
    }

    #[test]
    fn spend_label_shows_used_and_limit() {
        let spend = SpendInfo {
            used: 38.77,
            limit: Some(80.0),
            percent: 48.0,
            currency: "USD".to_owned(),
            enabled: true,
        };
        assert_eq!(spend_label(&spend), "$38.77 / $80.00");
    }

    #[test]
    fn spend_label_used_only_without_limit() {
        let spend = SpendInfo {
            used: 38.77,
            limit: None,
            percent: 0.0,
            currency: "USD".to_owned(),
            enabled: true,
        };
        assert_eq!(spend_label(&spend), "$38.77");
    }

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn naive(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    #[test]
    fn reset_label_uses_relative_within_12h() {
        let now = dt("2026-08-04T10:00:00Z");
        assert_eq!(
            time_reset_label(dt("2026-08-04T16:00:00Z"), now, true),
            "in 6h"
        );
        assert_eq!(
            time_reset_label(dt("2026-08-04T10:30:00Z"), now, true),
            "in 30m"
        );
    }

    #[test]
    fn reset_label_switches_to_absolute_beyond_12h() {
        let now = dt("2026-08-04T10:00:00Z");
        // 18h ahead is no longer "in 18h"; it becomes a weekday + time.
        let label = time_reset_label(dt("2026-08-05T04:00:00Z"), now, true);
        assert!(!label.starts_with("in "), "got {label}");
    }

    #[test]
    fn absolute_reset_24h_format() {
        // 2026-08-04 is a Tuesday.
        assert_eq!(
            absolute_reset(naive("2026-08-04 13:00:00"), true),
            "Tue 13h"
        );
        assert_eq!(
            absolute_reset(naive("2026-08-04 13:30:00"), true),
            "Tue 13:30"
        );
        assert_eq!(
            absolute_reset(naive("2026-08-04 00:00:00"), true),
            "Tue 00h"
        );
        assert_eq!(
            absolute_reset(naive("2026-08-04 09:05:00"), true),
            "Tue 09:05"
        );
    }

    #[test]
    fn absolute_reset_12h_format() {
        assert_eq!(
            absolute_reset(naive("2026-08-04 10:00:00"), false),
            "Tue 10am"
        );
        assert_eq!(
            absolute_reset(naive("2026-08-04 22:30:00"), false),
            "Tue 10:30pm"
        );
        assert_eq!(
            absolute_reset(naive("2026-08-04 00:00:00"), false),
            "Tue 12am"
        );
        assert_eq!(
            absolute_reset(naive("2026-08-04 12:15:00"), false),
            "Tue 12:15pm"
        );
    }
}
