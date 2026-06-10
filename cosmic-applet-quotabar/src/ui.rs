use chrono::{DateTime, Utc};
use cosmic::Element;
use cosmic::applet::menu_button;
use cosmic::iced::widget::{Row, canvas};
use cosmic::iced::{Alignment, Color, Length};
use cosmic::widget::{Column, container, text};

use crate::app::Message;
use crate::models::{ProviderSnapshot, RefreshError, UsageWindow};

const ROW_WIDTH: f32 = 380.0;

pub fn menu_view<'a>() -> Element<'a, Message> {
    Column::new()
        .padding(4)
        .spacing(0)
        .push(menu_button(text::body("Refresh")).on_press(Message::RefreshFromMenu))
        .into()
}

pub fn dashboard_view<'a>(
    snapshots: &'a [ProviderSnapshot],
    errors: &'a [RefreshError],
    refreshing: bool,
    last_refresh: Option<DateTime<Utc>>,
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
            col = col.push(provider_card(snapshot));
        }
        for err in errors {
            col = col.push(warning_banner(err));
        }
    }

    col = col.push(footer(last_refresh));
    col.into()
}

fn provider_card(snapshot: &ProviderSnapshot) -> Element<'_, Message> {
    let now = chrono::Utc::now();

    let header = Row::new()
        .align_y(Alignment::Center)
        .spacing(8)
        .push(text::body(snapshot.provider.display_name()).font(cosmic::font::bold()))
        .push(
            cosmic::widget::Space::new()
                .width(Length::Fill)
                .height(Length::Fixed(0.0)),
        )
        .push(text::body(worst_badge(snapshot)).font(cosmic::font::bold()));

    let mut col = Column::new().padding(10).spacing(8).push(header);
    col = col.push(bar_row("DAILY", snapshot.short.as_ref(), now));
    col = col.push(bar_row("WEEKLY", snapshot.weekly.as_ref(), now));

    container(col).width(Length::Fill).padding(2).into()
}

fn worst_badge(snapshot: &ProviderSnapshot) -> String {
    snapshot
        .worst_used()
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
) -> Element<'a, Message> {
    let pct_text = window.map_or_else(
        || "—".to_owned(),
        |w| format!("{}%", round_pct(w.used_percent)),
    );
    let reset_text = window
        .and_then(|w| w.resets_at)
        .map(|r| {
            format!(
                "in {}",
                short_duration(r.signed_duration_since(now).num_seconds())
            )
        })
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
