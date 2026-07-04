use cosmic_config::CosmicConfigEntry;
use cosmic_config_derive::CosmicConfigEntry;

pub const APP_ID: &str = "com.github.ragusa87.CosmicAppletQuotaBar";

/// Non-secret alert settings. Editable from the Settings window
/// (right-click -> Settings…, or `--show-settings`) or by hand under
/// `~/.config/com.github.ragusa87.CosmicAppletQuotaBar/v1/`. The applet's
/// config watcher applies changes live.
#[derive(Debug, Clone, CosmicConfigEntry, Eq, PartialEq)]
#[version = 1]
pub struct Config {
    /// Fire a desktop notification when a usage window crosses the threshold.
    pub alert_enabled: bool,
    /// Percentage (0–100) at or above which the alert fires.
    pub alert_threshold_pct: u8,
    /// Hide the pay-as-you-go credit row while the plan still has headroom.
    /// The row reappears once the daily or weekly window reaches 100%.
    pub ignore_credits_when_plan_used: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            alert_enabled: true,
            alert_threshold_pct: 90,
            ignore_credits_when_plan_used: false,
        }
    }
}
