use cosmic_config::CosmicConfigEntry;
use cosmic_config_derive::CosmicConfigEntry;

pub const APP_ID: &str = "com.github.ragusa87.CosmicAppletTaxi";

#[derive(Debug, Clone, CosmicConfigEntry, Eq, PartialEq)]
#[version = 1]
pub struct Config {
    pub cutover_hour: u8,
    pub merge_gap_minutes: u32,
    pub round_min_minutes: u32,
    pub taxi_command: String,
    pub taxirc_path: String,
    /// Master switch for pausing timers on screen lock / suspend. When off,
    /// lock/unlock does nothing and the per-timer opt-out is hidden.
    pub enable_autopause: bool,
    /// Show seconds (`HH:MM:SS`) instead of just `HH:MM` in the panel label of
    /// the running timer.
    pub show_seconds: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cutover_hour: 4,
            merge_gap_minutes: 5,
            round_min_minutes: 15,
            taxi_command: "uv run --with taxi,taxi-zebra taxi".to_owned(),
            taxirc_path: String::new(),
            enable_autopause: true,
            show_seconds: true,
        }
    }
}

impl Config {
    pub fn cutover_hour(&self) -> u8 {
        self.cutover_hour.min(23)
    }

    pub fn merge_gap(&self) -> chrono::Duration {
        chrono::Duration::minutes(i64::from(self.merge_gap_minutes))
    }

    pub fn taxi_argv(&self) -> Vec<String> {
        shell_words::split(&self.taxi_command).unwrap_or_default()
    }
}
