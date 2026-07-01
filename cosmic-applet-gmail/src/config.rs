use cosmic_config::CosmicConfigEntry;
use cosmic_config_derive::CosmicConfigEntry;

pub const APP_ID: &str = "com.github.ragusa87.CosmicAppletGmail";

/// Service string under which OAuth tokens are stored in the freedesktop
/// Secret Service. Distinct from `APP_ID` for backwards-compat with tokens
/// that existing installs already wrote under this key.
pub const KEYRING_SERVICE: &str = "com.github.ragusa87.CosmicAppletGmail:tokens";

#[derive(Debug, Clone, CosmicConfigEntry, Eq, PartialEq)]
#[version = 1]
pub struct Config {
    pub email: String,
    pub client_id: String,
    pub poll_interval_secs: u32,
    /// Fire a desktop notification when the unread count rises.
    pub notify: bool,
    /// Manual pause: no polling, no notifications, icon greyed out.
    pub paused: bool,
    /// Automatically pause on Saturday/Sunday.
    pub auto_pause_weekend: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            email: String::new(),
            client_id: String::new(),
            poll_interval_secs: 60,
            notify: true,
            paused: false,
            auto_pause_weekend: false,
        }
    }
}

impl Config {
    pub fn poll_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(u64::from(self.poll_interval_secs.max(15)))
    }

    pub fn is_configured(&self) -> bool {
        !self.email.is_empty() && !self.client_id.is_empty()
    }

    /// Effective pause state: the manual `paused` flag, or the weekend
    /// auto-pause when today is Saturday/Sunday. When paused the applet skips
    /// polling and notifications and greys its icon.
    pub fn is_paused(&self) -> bool {
        use chrono::Datelike;
        self.paused
            || (self.auto_pause_weekend
                && matches!(
                    chrono::Local::now().weekday(),
                    chrono::Weekday::Sat | chrono::Weekday::Sun
                ))
    }
}
