use std::collections::HashSet;

use chrono::{DateTime, Duration, Utc};
use cosmic::Element;
use cosmic::app::Task;
use cosmic::iced::widget::mouse_area;
use cosmic::iced::{Limits, Subscription, window::Id};
use cosmic::surface::{self, action::destroy_popup};
use cosmic::widget::{button, text};
use cosmic_config::CosmicConfigEntry;
use futures_util::SinkExt;
use tokio::signal::unix::{SignalKind, signal};

use cosmic_google_common::auth;
use cosmic_google_common::secrets::{self, Tokens};

use crate::calendar::{self, Event};
use crate::config::{APP_ID, Config, KEYRING_SERVICE};
use crate::ui;

const CALENDAR_URL: &str = "https://calendar.google.com";
const CALENDAR_ICON_SVG: &[u8] =
    include_bytes!("../data/icons/com.github.ragusa87.CosmicAppletGoogleAgenda.svg");

#[derive(Default)]
pub struct AppModel {
    pub core: cosmic::Core,
    pub config: Config,
    pub menu_popup: Option<Id>,
    pub info_popup: Option<Id>,
    pub tokens: Option<Tokens>,
    pub events: Vec<Event>,
    pub next: Option<Event>,
    pub idle_since: Option<DateTime<Utc>>,
    pub notified: HashSet<String>,
    pub stale: bool,
    /// Manual pause takeover. `None` follows the weekend auto-pause rule;
    /// `Some(true)`/`Some(false)` is an explicit user choice that overrides it
    /// (so "Resume" works even on a weekend, and "Pause" works midweek).
    pub paused_override: Option<bool>,
    /// Effective pause state seen by the previous `Tick`/`TogglePause`. Needed
    /// to detect the paused -> running edge: the fetch subscription is only
    /// recreated at that point and won't fire for a full `fetch_interval`, so
    /// the edge triggers an immediate refetch.
    pub last_paused: bool,
}

impl AppModel {
    /// The weekend auto-pause rule, ignoring any manual override.
    fn auto_paused(&self) -> bool {
        self.config.disable_during_weekend && is_weekend_local()
    }

    pub fn is_paused(&self) -> bool {
        self.paused_override.unwrap_or_else(|| self.auto_paused())
    }

    /// Drop a manual override once it agrees with the current auto-pause rule,
    /// so the applet returns to the configured behavior instead of pinning a
    /// stale choice forever (e.g. a weekend "Resume" leaking into every future
    /// weekend, or the "Pause on weekends" setting appearing ineffective).
    fn reconcile_pause_override(&mut self) {
        self.paused_override = reconciled_override(self.paused_override, self.auto_paused());
    }

    fn wedge_badge_canvas(&self, now: DateTime<Utc>, dot_size: f32) -> Element<'_, Message> {
        use cosmic::iced::{Color, Length};
        let busy = self
            .next
            .as_ref()
            .is_some_and(|ev| ev.start <= now && ev.end >= now);
        let badge_color = if busy {
            Color::from_rgb(0.91, 0.30, 0.24)
        } else {
            Color::from_rgb(0.30, 0.69, 0.31)
        };
        let remaining = if self.config.show_progress {
            self.next.as_ref().map_or(1.0, |ev| {
                if busy {
                    1.0 - meeting_progress(now, ev)
                } else {
                    let window_start = idle_window_start(self.idle_since, ev.start);
                    1.0 - free_progress(now, window_start, ev.start)
                }
            })
        } else {
            1.0
        };
        cosmic::iced::widget::canvas(WedgeBadge {
            color: badge_color,
            remaining,
        })
        .width(Length::Fixed(dot_size))
        .height(Length::Fixed(dot_size))
        .into()
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    LeftClick,
    OpenMenu,
    PopupClosed(Id),
    OpenCredentials,
    OpenUrl(String),

    Tick,
    Refetch,
    RefreshFromMenu,
    Fetched(Result<(Tokens, Vec<Event>), String>),
    TogglePause,

    UpdateConfig(Config),
    TokensLoaded(Option<Tokens>),

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

        let mut app = AppModel {
            core,
            config: config.clone(),
            ..Default::default()
        };
        // Seed the edge detector with the real state, so a "Resume" click
        // before the first Tick (e.g. right after a weekend startup) still
        // counts as a paused -> running transition and refetches.
        app.last_paused = app.is_paused();

        let task = if config.is_configured() {
            let email = config.email.clone();
            cosmic::task::future(async move {
                let tokens = secrets::load(KEYRING_SERVICE, &email).await.ok();
                Message::TokensLoaded(tokens)
            })
        } else {
            Task::none()
        };

        (app, task)
    }

    fn on_close_requested(&self, id: Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    fn view(&self) -> Element<'_, Self::Message> {
        use cosmic::applet::cosmic_panel_config::PanelAnchor;
        use cosmic::iced::Length;
        use cosmic::iced::widget::Row;

        let is_horizontal = matches!(
            self.core.applet.anchor,
            PanelAnchor::Top | PanelAnchor::Bottom
        );

        let (icon_size, _) = self.core.applet.suggested_size(true);
        let (pad_major, pad_minor) = self.core.applet.suggested_padding(true);
        let icon_px = f32::from(icon_size);
        let label_size = (icon_px * 0.55).round();
        let dot_size = (icon_px * 0.6).round();
        let paused = self.is_paused();

        let icon = cosmic::widget::icon(
            cosmic::widget::icon::from_svg_bytes(CALENDAR_ICON_SVG.to_vec()).symbolic(paused),
        )
        .size(icon_size);

        let now = Utc::now();
        let extra = (dot_size / 2.0).round();
        let stack_px = icon_px + extra;

        let icon_area = cosmic::widget::container(icon)
            .width(Length::Fixed(stack_px))
            .height(Length::Fixed(stack_px))
            .align_x(cosmic::iced::alignment::Horizontal::Left)
            .align_y(cosmic::iced::alignment::Vertical::Top);

        let mut icon_with_badge = cosmic::iced::widget::Stack::new()
            .width(Length::Fixed(stack_px))
            .height(Length::Fixed(stack_px))
            .push(icon_area);
        if !paused {
            let badge = cosmic::widget::container(self.wedge_badge_canvas(now, dot_size))
                .width(Length::Fixed(stack_px))
                .height(Length::Fixed(stack_px))
                .align_x(cosmic::iced::alignment::Horizontal::Right)
                .align_y(cosmic::iced::alignment::Vertical::Bottom);
            icon_with_badge = icon_with_badge.push(badge);
        }

        let time_text = (!paused && self.config.show_time)
            .then(|| {
                self.next
                    .as_ref()
                    .map(|ev| label_widget(format_relative(now, ev.start), label_size, true))
            })
            .flatten();
        let title_widget = (!paused && self.config.show_title)
            .then(|| {
                self.next
                    .as_ref()
                    .map(|ev| label_widget(truncate_title(&ev.summary, 20), label_size, false))
            })
            .flatten();

        let mut row = Row::new()
            .align_y(cosmic::iced::Alignment::Center)
            .spacing(6)
            .push(icon_with_badge);
        if is_horizontal {
            if let Some(t) = time_text {
                row = row.push(t);
            }
            if let Some(t) = title_widget {
                row = row.push(t);
            }
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
        let display = cosmic::iced::time::every(self.config.display_tick()).map(|_| Message::Tick);
        let fetch = if self.is_paused() {
            Subscription::none()
        } else {
            cosmic::iced::time::every(self.config.fetch_interval()).map(|_| Message::Refetch)
        };
        let watch = self
            .core()
            .watch_config::<Config>(Self::APP_ID)
            .map(|update| Message::UpdateConfig(update.config));
        Subscription::batch([display, fetch, watch, sigusr2_subscription()])
    }

    #[allow(clippy::too_many_lines)]
    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::NoOp => {}

            Message::LeftClick => {
                // Drop the click when the menu popup is up. Wayland can deliver
                // the click event to the panel surface as the popup's grab is
                // being dismissed, which would otherwise re-open the info popup
                // when the user meant to interact with the menu.
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
                if self.menu_popup.as_ref() == Some(&id) {
                    self.menu_popup = None;
                }
                if self.info_popup.as_ref() == Some(&id) {
                    self.info_popup = None;
                }
            }

            Message::OpenUrl(url) => {
                let close_info = self
                    .info_popup
                    .take()
                    .map_or_else(Task::none, |id| dispatch_surface(destroy_popup(id)));
                let launch = cosmic::task::future(async move {
                    let _ = tokio::process::Command::new("xdg-open")
                        .arg(url)
                        .status()
                        .await;
                    Message::NoOp
                });
                return Task::batch([close_info, launch]);
            }

            Message::OpenCredentials => {
                let destroy_menu = self
                    .menu_popup
                    .take()
                    .map_or_else(Task::none, |id| dispatch_surface(destroy_popup(id)));

                let launch = cosmic::task::future(async {
                    match std::env::current_exe() {
                        Ok(path) => {
                            if let Err(e) = tokio::process::Command::new(path)
                                .arg("--show-settings")
                                .spawn()
                            {
                                tracing::warn!(error = %e, "failed to spawn settings binary");
                            }
                        }
                        Err(e) => tracing::warn!(error = %e, "current_exe() failed"),
                    }
                    Message::NoOp
                });

                return Task::batch([destroy_menu, launch]);
            }

            Message::Tick => {
                self.reconcile_pause_override();
                let paused = self.is_paused();
                let resumed = just_resumed(&mut self.last_paused, paused);
                if paused {
                    return Task::none();
                }
                let now = Utc::now();
                recompute_next(&mut self.events, &mut self.next, &mut self.idle_since, now);
                prune_notified(&self.events, &mut self.notified);
                let lead = if self.config.notify {
                    u64::from(self.config.notification_lead_secs)
                } else {
                    0
                };
                maybe_notify(self.next.as_ref(), &mut self.notified, lead, now);
                if resumed {
                    // The pause just ended (weekend rollover, or the setting was
                    // switched off): pull fresh events immediately instead of
                    // showing stale ones until the recreated subscription fires.
                    return cosmic::task::message(cosmic::Action::App(Message::Refetch));
                }
            }

            Message::RefreshFromMenu => {
                let destroy_menu = self
                    .menu_popup
                    .take()
                    .map_or_else(Task::none, |id| dispatch_surface(destroy_popup(id)));
                let refresh = cosmic::task::message(cosmic::Action::App(Message::Refetch));
                return Task::batch([destroy_menu, refresh]);
            }

            Message::TogglePause => {
                // Flip the *effective* state and pin it as a manual override, so
                // the toggle takes over from the weekend rule in either direction.
                self.paused_override = Some(!self.is_paused());
                // If the user toggled back to whatever the weekend rule already
                // wants, drop the override so we don't pin a stale choice.
                self.reconcile_pause_override();
                let destroy_menu = self
                    .menu_popup
                    .take()
                    .map_or_else(Task::none, |id| dispatch_surface(destroy_popup(id)));
                let paused = self.is_paused();
                let resume_fetch = if just_resumed(&mut self.last_paused, paused) {
                    cosmic::task::message(cosmic::Action::App(Message::Refetch))
                } else {
                    Task::none()
                };
                return Task::batch([destroy_menu, resume_fetch]);
            }

            Message::Refetch => {
                if self.is_paused() {
                    return Task::none();
                }
                let Some(tokens) = self.tokens.clone() else {
                    return Task::none();
                };
                if !self.config.is_configured() {
                    return Task::none();
                }
                let client_id = self.config.client_id.clone();
                let email = self.config.email.clone();
                return cosmic::task::future(async move {
                    let result = refresh_and_fetch(&client_id, &email, tokens)
                        .await
                        .map_err(|e| e.to_string());
                    Message::Fetched(result)
                });
            }

            Message::Fetched(Ok((tokens, events))) => {
                self.tokens = Some(tokens);
                self.events = events;
                self.stale = false;
                return cosmic::task::message(cosmic::Action::App(Message::Tick));
            }

            Message::Fetched(Err(e)) => {
                tracing::warn!(error = %e, "calendar fetch failed");
                self.stale = true;
            }

            Message::UpdateConfig(config) => {
                let email_changed = config.email != self.config.email;
                self.config = config;
                if email_changed && !self.config.email.is_empty() {
                    let email = self.config.email.clone();
                    return cosmic::task::future(async move {
                        let tokens = secrets::load(KEYRING_SERVICE, &email).await.ok();
                        Message::TokensLoaded(tokens)
                    });
                }
                if self.config.email.is_empty() {
                    self.tokens = None;
                    self.events.clear();
                    self.next = None;
                    self.notified.clear();
                }
            }

            Message::TokensLoaded(tokens) => {
                self.tokens = tokens;
                return cosmic::task::message(cosmic::Action::App(Message::Refetch));
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
                tracing::info!("SIGUSR2 received, forcing refetch");
                if sender.send(Message::Refetch).await.is_err() {
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
            let body = ui::menu_view(state.is_paused());
            Element::from(state.core.applet.popup_container(body)).map(cosmic::Action::App)
        })),
    );
    dispatch_surface(action)
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
                .max_width(360.0)
                .min_width(240.0)
                .min_height(80.0)
                .max_height(480.0);
            settings
        },
        Some(Box::new(|state: &AppModel| {
            let body = ui::event_info_view(&state.events, CALENDAR_URL);
            Element::from(state.core.applet.popup_container(body)).map(cosmic::Action::App)
        })),
    );
    dispatch_surface(action)
}

async fn refresh_and_fetch(
    client_id: &str,
    email: &str,
    tokens: Tokens,
) -> anyhow::Result<(Tokens, Vec<Event>)> {
    let tokens = if tokens.is_access_token_fresh() {
        tokens
    } else {
        let new = auth::refresh(client_id, &tokens).await?;
        if let Err(e) = secrets::save(KEYRING_SERVICE, email, &new).await {
            tracing::warn!(error = %e, "failed to persist refreshed tokens");
        }
        new
    };
    let events = calendar::upcoming_events(&tokens.access_token).await?;
    Ok((tokens, events))
}

/// Drop a manual pause override once it matches what the auto-pause rule would
/// decide on its own; otherwise keep the explicit user choice.
fn reconciled_override(override_: Option<bool>, auto_paused: bool) -> Option<bool> {
    match override_ {
        Some(v) if v == auto_paused => None,
        other => other,
    }
}

/// Record the current effective pause state into `last_paused` and report
/// whether this is the paused -> running edge — the only transition that
/// warrants an immediate refetch.
fn just_resumed(last_paused: &mut bool, paused: bool) -> bool {
    std::mem::replace(last_paused, paused) && !paused
}

fn is_weekend_local() -> bool {
    use chrono::Datelike;
    matches!(
        chrono::Local::now().weekday(),
        chrono::Weekday::Sat | chrono::Weekday::Sun
    )
}

fn recompute_next(
    events: &mut Vec<Event>,
    next: &mut Option<Event>,
    idle_since: &mut Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) {
    if let Some(latest_end) = events.iter().filter(|e| e.end < now).map(|e| e.end).max()
        && idle_since.is_none_or(|prev| latest_end > prev)
    {
        *idle_since = Some(latest_end);
    }
    events.retain(|e| e.end >= now);
    *next = events.iter().find(|e| e.end >= now).cloned();
}

fn prune_notified(events: &[Event], notified: &mut HashSet<String>) {
    let live: HashSet<&str> = events.iter().map(|e| e.id.as_str()).collect();
    notified.retain(|id| live.contains(id.as_str()));
}

struct Notice {
    summary: String,
    body: String,
}

fn decide_notify(
    next: Option<&Event>,
    notified: &mut HashSet<String>,
    lead_secs: u64,
    now: DateTime<Utc>,
) -> Option<Notice> {
    if lead_secs == 0 {
        return None;
    }
    let ev = next?;
    if notified.contains(&ev.id) {
        return None;
    }
    let delta = (ev.start - now).num_seconds();
    let lead = i64::try_from(lead_secs).unwrap_or(i64::MAX);
    if delta < 0 || delta > lead {
        return None;
    }
    notified.insert(ev.id.clone());
    Some(Notice {
        summary: format!("Meeting in {} min", delta.div_euclid(60).max(0)),
        body: format!(
            "{} \u{2014} {}",
            ev.summary,
            ev.start.with_timezone(&chrono::Local).format("%H:%M")
        ),
    })
}

fn maybe_notify(
    next: Option<&Event>,
    notified: &mut HashSet<String>,
    lead_secs: u64,
    now: DateTime<Utc>,
) {
    let Some(notice) = decide_notify(next, notified, lead_secs, now) else {
        return;
    };
    cosmic_google_common::notify::show(&notice.summary, &notice.body, APP_ID);
}

struct WedgeBadge {
    color: cosmic::iced::Color,
    remaining: f32,
}

impl cosmic::iced::widget::canvas::Program<Message, cosmic::Theme> for WedgeBadge {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &cosmic::iced::Renderer,
        _theme: &cosmic::Theme,
        bounds: cosmic::iced::Rectangle,
        _cursor: cosmic::iced::mouse::Cursor,
    ) -> Vec<cosmic::iced::widget::canvas::Geometry> {
        use cosmic::iced::Point;
        use cosmic::iced::widget::canvas::{Frame, Path};
        use std::f32::consts::PI;

        const SEGMENTS: u16 = 64;

        let mut frame = Frame::new(renderer, bounds.size());
        if self.remaining <= 0.0 {
            return vec![frame.into_geometry()];
        }
        let center = frame.center();
        let radius = bounds.width.min(bounds.height) / 2.0;
        let path = if self.remaining >= 1.0 {
            Path::circle(center, radius)
        } else {
            // The *missing* slice grows clockwise from 12; the filled wedge
            // starts at the boundary that has just moved past 12 and sweeps
            // clockwise the long way back to 12.
            let elapsed = 1.0 - self.remaining;
            let start = -PI / 2.0 + elapsed * 2.0 * PI;
            let end = 3.0 * PI / 2.0;
            // `Builder::arc()` implicitly does a `move_to` to the arc's start
            // point, which would break the sub-path and turn the fill into
            // a circular segment instead of a pie wedge. Tessellate the arc
            // by hand so center, both radii and the arc are one sub-path.
            Path::new(|builder| {
                builder.move_to(center);
                for i in 0..=SEGMENTS {
                    let t = f32::from(i) / f32::from(SEGMENTS);
                    let angle = start + (end - start) * t;
                    builder.line_to(Point::new(
                        center.x + radius * angle.cos(),
                        center.y + radius * angle.sin(),
                    ));
                }
                builder.close();
            })
        };
        frame.fill(&path, self.color);
        vec![frame.into_geometry()]
    }
}

fn meeting_progress(now: DateTime<Utc>, ev: &Event) -> f32 {
    let total = (ev.end - ev.start).num_seconds().max(1);
    let elapsed = (now - ev.start).num_seconds().clamp(0, total);
    #[allow(clippy::cast_precision_loss)]
    let frac = elapsed as f32 / total as f32;
    frac.clamp(0.0, 1.0)
}

// Cap on the free-time window the icon visualises. Beyond this, the pie stays
// full — the wedge is meant to be a short-term countdown to the next event,
// not a multi-hour bar that's almost-empty all day.
const FREE_WINDOW_CAP_HOURS: i64 = 1;

fn idle_window_start(
    idle_since: Option<DateTime<Utc>>,
    next_start: DateTime<Utc>,
) -> DateTime<Utc> {
    let cap_start = next_start - Duration::hours(FREE_WINDOW_CAP_HOURS);
    idle_since.map_or(cap_start, |d| d.max(cap_start))
}

fn free_progress(now: DateTime<Utc>, idle_since: DateTime<Utc>, next_start: DateTime<Utc>) -> f32 {
    let total = (next_start - idle_since).num_seconds().max(1);
    let elapsed = (now - idle_since).num_seconds().clamp(0, total);
    #[allow(clippy::cast_precision_loss)]
    let frac = elapsed as f32 / total as f32;
    frac.clamp(0.0, 1.0)
}

fn label_widget(s: String, size: f32, bold: bool) -> Element<'static, Message> {
    use cosmic::iced::Color;
    let mut t = text(s).size(size).class(Color::WHITE);
    if bold {
        t = t.font(cosmic::font::bold());
    }
    t.into()
}

fn format_relative(now: DateTime<Utc>, start: DateTime<Utc>) -> String {
    let delta_secs = (start - now).num_seconds();
    if delta_secs <= 0 {
        return format!(
            "Since {}",
            start.with_timezone(&chrono::Local).format("%H:%M")
        );
    }
    let minutes = delta_secs / 60;
    if minutes < 60 {
        return format!("In {minutes}m");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("In {hours}h");
    }
    let days = hours / 24;
    format!("In {days}d")
}

fn truncate_title(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('\u{2026}');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn ts(h: i64, m: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 12, 12, 0, 0).unwrap()
            + Duration::hours(h)
            + Duration::minutes(m)
    }

    fn ev(id: &str, start_h: i64, end_h: i64) -> Event {
        Event {
            id: id.to_owned(),
            summary: id.to_owned(),
            start: ts(start_h, 0),
            end: ts(end_h, 0),
            meet_url: None,
            location: None,
        }
    }

    #[test]
    fn reconcile_keeps_override_that_fights_auto_rule() {
        // Weekend auto-pause is on; user hits "Resume" -> Some(false) while the
        // rule wants paused. The explicit choice must survive the weekend.
        assert_eq!(reconciled_override(Some(false), true), Some(false));
        // Midweek manual pause -> Some(true) while the rule wants running.
        assert_eq!(reconciled_override(Some(true), false), Some(true));
    }

    #[test]
    fn reconcile_drops_override_that_agrees_with_auto_rule() {
        // Weekend ends: the "Resume" override now matches the rule, so drop it
        // and let future weekends auto-pause again.
        assert_eq!(reconciled_override(Some(false), false), None);
        // Weekend starts while a manual pause is active: the rule already wants
        // paused, so the override becomes redundant.
        assert_eq!(reconciled_override(Some(true), true), None);
    }

    #[test]
    fn reconcile_leaves_unset_override_untouched() {
        assert_eq!(reconciled_override(None, true), None);
        assert_eq!(reconciled_override(None, false), None);
    }

    #[test]
    fn just_resumed_fires_only_on_paused_to_running_edge() {
        let mut last_paused = false;
        // Steady running: no refetch.
        assert!(!just_resumed(&mut last_paused, false));
        // Entering pause (weekend starts): no refetch, but the state is recorded.
        assert!(!just_resumed(&mut last_paused, true));
        assert!(last_paused);
        // Steady paused: no refetch.
        assert!(!just_resumed(&mut last_paused, true));
        // Weekend ends: this is the edge that must refetch, exactly once.
        assert!(just_resumed(&mut last_paused, false));
        assert!(!just_resumed(&mut last_paused, false));
    }

    #[test]
    fn format_relative_buckets() {
        let now = ts(0, 0);
        assert!(format_relative(now, now).starts_with("Since "));
        assert!(format_relative(now, now - Duration::minutes(5)).starts_with("Since "));
        assert_eq!(format_relative(now, now + Duration::minutes(12)), "In 12m");
        assert_eq!(format_relative(now, now + Duration::hours(2)), "In 2h");
        assert_eq!(format_relative(now, now + Duration::days(3)), "In 3d");
    }

    #[test]
    fn recompute_next_picks_first_unfinished_event() {
        let mut events = vec![
            ev("past", -2, -1),
            ev("now-running", -1, 1),
            ev("later", 2, 3),
        ];
        let mut next = None;
        let mut idle_since = None;
        recompute_next(&mut events, &mut next, &mut idle_since, ts(0, 0));
        assert_eq!(next.as_ref().map(|e| e.id.as_str()), Some("now-running"));
        assert_eq!(events.len(), 2);
        assert_eq!(idle_since, Some(ts(-1, 0)));
    }

    #[test]
    fn recompute_next_keeps_latest_idle_since() {
        // First tick captures the end of a meeting that just ended.
        let mut events = vec![ev("a", -2, -1), ev("b", 1, 2)];
        let mut next = None;
        let mut idle_since = None;
        recompute_next(&mut events, &mut next, &mut idle_since, ts(0, 0));
        assert_eq!(idle_since, Some(ts(-1, 0)));

        // A later tick after `b` ends bumps `idle_since` forward.
        recompute_next(&mut events, &mut next, &mut idle_since, ts(3, 0));
        assert_eq!(idle_since, Some(ts(2, 0)));
        assert!(next.is_none());
    }

    #[test]
    fn decide_notify_fires_within_lead_window_once() {
        let now = ts(0, 0);
        let mut events = [ev("e1", 0, 1)];
        events[0].start = now + Duration::minutes(3);
        let mut notified = HashSet::new();
        assert!(decide_notify(Some(&events[0]), &mut notified, 300, now).is_some());
        assert!(notified.contains("e1"));
        assert!(decide_notify(Some(&events[0]), &mut notified, 300, now).is_none());
    }

    #[test]
    fn decide_notify_skips_when_lead_zero() {
        let now = ts(0, 0);
        let mut events = [ev("e1", 0, 1)];
        events[0].start = now + Duration::minutes(3);
        let mut notified = HashSet::new();
        assert!(decide_notify(Some(&events[0]), &mut notified, 0, now).is_none());
        assert!(notified.is_empty());
    }

    #[test]
    fn decide_notify_skips_outside_lead_window() {
        let now = ts(0, 0);
        let mut events = [ev("future", 0, 1)];
        events[0].start = now + Duration::minutes(30);
        let mut notified = HashSet::new();
        assert!(decide_notify(Some(&events[0]), &mut notified, 300, now).is_none());
        assert!(notified.is_empty());
    }

    #[test]
    fn prune_notified_drops_stale_ids() {
        let events = vec![ev("a", 0, 1)];
        let mut notified: HashSet<String> = HashSet::from(["a".to_owned(), "b".to_owned()]);
        prune_notified(&events, &mut notified);
        assert!(notified.contains("a"));
        assert!(!notified.contains("b"));
    }

    #[test]
    fn meeting_progress_buckets() {
        let start = ts(0, 0);
        let end = ts(1, 0);
        let event = Event {
            id: "e".to_owned(),
            summary: "e".to_owned(),
            start,
            end,
            meet_url: None,
            location: None,
        };
        assert!((meeting_progress(start, &event) - 0.0).abs() < 1e-6);
        assert!((meeting_progress(start + Duration::minutes(30), &event) - 0.5).abs() < 1e-6);
        assert!((meeting_progress(end, &event) - 1.0).abs() < 1e-6);
        // Out-of-range times are clamped.
        assert!((meeting_progress(start - Duration::minutes(5), &event) - 0.0).abs() < 1e-6);
        assert!((meeting_progress(end + Duration::minutes(5), &event) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn free_progress_buckets() {
        let start = ts(0, 0);
        let next = ts(1, 0);
        assert!((free_progress(start, start, next) - 0.0).abs() < 1e-6);
        assert!((free_progress(start + Duration::minutes(30), start, next) - 0.5).abs() < 1e-6);
        assert!((free_progress(next, start, next) - 1.0).abs() < 1e-6);
        // Out-of-range times are clamped.
        assert!((free_progress(start - Duration::minutes(5), start, next) - 0.0).abs() < 1e-6);
        assert!((free_progress(next + Duration::minutes(5), start, next) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn idle_window_start_caps_long_or_missing_idle() {
        let next = ts(0, 0);
        let cap = next - Duration::hours(FREE_WINDOW_CAP_HOURS);

        // No prior meeting end recorded: window starts cap-hours before next.
        assert_eq!(idle_window_start(None, next), cap);

        // Stale idle from yesterday: still capped.
        assert_eq!(
            idle_window_start(Some(next - Duration::hours(24)), next),
            cap
        );

        // Recent idle inside the cap window wins (shorter, accurate interval).
        let recent = next - Duration::minutes(15);
        assert_eq!(idle_window_start(Some(recent), next), recent);
    }

    #[test]
    fn truncate_title_appends_ellipsis() {
        assert_eq!(truncate_title("hi", 20), "hi");
        assert_eq!(
            truncate_title("0123456789abcdefghij", 20),
            "0123456789abcdefghij"
        );
        assert_eq!(
            truncate_title("0123456789abcdefghijK", 20),
            "0123456789abcdefghi\u{2026}"
        );
    }
}
