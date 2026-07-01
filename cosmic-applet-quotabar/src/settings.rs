use cosmic::Element;
use cosmic::app::Task;
use cosmic::iced::{self, Length, Size};
use cosmic::widget::{Column, button, dropdown, scrollable, settings, text, toggler};
use cosmic_config::CosmicConfigEntry;

use crate::config::{APP_ID, Config};

/// Selectable alert thresholds (percent), paired with `THRESHOLD_LABELS` by
/// index.
pub const THRESHOLD_PRESETS: [u8; 8] = [50, 60, 70, 75, 80, 85, 90, 95];
const THRESHOLD_LABELS: [&str; 8] = ["50%", "60%", "70%", "75%", "80%", "85%", "90%", "95%"];

pub fn run() -> iced::Result {
    // Both modes ship in the same binary, so `pkill -USR2 cosmic-applet-quotabar`
    // would also reach this process. SIGUSR2's default action is to terminate;
    // ignore it here so an external "refresh the applet" signal doesn't kill an
    // open settings window.
    // SAFETY: signal(2) with SIG_IGN is async-signal-safe and has no preconditions.
    unsafe {
        libc::signal(libc::SIGUSR2, libc::SIG_IGN);
    }

    let settings = cosmic::app::Settings::default().size(Size::new(460.0, 320.0));
    cosmic::app::run::<SettingsApp>(settings, ())
}

#[derive(Default)]
pub struct SettingsApp {
    core: cosmic::Core,
    config: Config,
}

#[derive(Debug, Clone)]
pub enum Msg {
    ToggleEnabled(bool),
    SetThresholdIdx(usize),
    Close,
}

impl cosmic::Application for SettingsApp {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Msg;

    const APP_ID: &'static str = APP_ID;

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(core: cosmic::Core, _flags: Self::Flags) -> (Self, Task<Self::Message>) {
        let config = cosmic_config::Config::new(APP_ID, Config::VERSION)
            .map(|ctx| match Config::get_entry(&ctx) {
                Ok(c) => c,
                Err((_errors, c)) => c,
            })
            .unwrap_or_default();
        (Self { core, config }, Task::none())
    }

    fn view(&self) -> Element<'_, Self::Message> {
        let header = text::title4("AI Quota settings");

        let mut alerts = settings::section().title("Alerts").add(settings::item(
            "Notify me on quota threshold",
            toggler(self.config.alert_enabled).on_toggle(Msg::ToggleEnabled),
        ));
        if self.config.alert_enabled {
            let selected = THRESHOLD_PRESETS
                .iter()
                .position(|&p| p == self.config.alert_threshold_pct);
            alerts = alerts.add(settings::item(
                "Alert threshold",
                dropdown(&THRESHOLD_LABELS, selected, Msg::SetThresholdIdx),
            ));
        }

        let hint = text::caption(
            "Fires once when any usage window (daily or weekly, per provider) crosses \
             the threshold, and re-arms once it drops back below.",
        );

        let content = Column::new()
            .padding(12)
            .spacing(10)
            .width(Length::Fill)
            .push(header)
            .push(alerts)
            .push(hint)
            .push(button::standard("Close").on_press(Msg::Close));

        scrollable(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Msg::ToggleEnabled(v) => {
                self.config.alert_enabled = v;
                persist_config(&self.config);
            }
            Msg::SetThresholdIdx(idx) => {
                if let Some(&pct) = THRESHOLD_PRESETS.get(idx) {
                    self.config.alert_threshold_pct = pct;
                    persist_config(&self.config);
                }
            }
            Msg::Close => return cosmic::iced::exit(),
        }
        Task::none()
    }
}

fn persist_config(config: &Config) {
    if let Ok(ctx) = cosmic_config::Config::new(APP_ID, Config::VERSION)
        && let Err(why) = config.write_entry(&ctx)
    {
        tracing::warn!(?why, "failed writing config entry");
    }
}
