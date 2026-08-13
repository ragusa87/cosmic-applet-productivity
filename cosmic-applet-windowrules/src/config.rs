use cosmic_config::CosmicConfigEntry;
use cosmic_config_derive::CosmicConfigEntry;

use crate::models::Rule;

pub const APP_ID: &str = "com.github.ragusa87.CosmicAppletWindowRules";

#[derive(Debug, Clone, CosmicConfigEntry, Eq, PartialEq)]
#[version = 1]
pub struct Config {
    pub rules: Vec<Rule>,
    /// Experimental: cap how many windows live on each workspace. While
    /// enabled, the rules above are not evaluated for new windows.
    pub cap_enabled: bool,
    /// Maximum windows per workspace when the cap is enabled (>= 1).
    pub cap_max_windows: u32,
    /// Only place new windows: never reposition existing windows to enforce
    /// the cap (manual arrangements are respected); empty-workspace gaps are
    /// still compacted away, moving whole groups without splitting them.
    pub cap_only_place_new: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            rules: Vec::new(),
            cap_enabled: false,
            cap_max_windows: 1,
            cap_only_place_new: false,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        cosmic_config::Config::new(APP_ID, Self::VERSION)
            .map(|ctx| match Self::get_entry(&ctx) {
                Ok(c) => c,
                Err((_e, c)) => c,
            })
            .unwrap_or_default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let ctx = cosmic_config::Config::new(APP_ID, Self::VERSION)
            .map_err(|e| anyhow::anyhow!("cosmic-config init: {e}"))?;
        self.write_entry(&ctx)
            .map_err(|e| anyhow::anyhow!("cosmic-config write: {e}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn default_cap_is_off_with_max_one() {
        let c = Config::default();
        assert!(!c.cap_enabled);
        assert_eq!(c.cap_max_windows, 1);
        assert!(!c.cap_only_place_new);
    }
}
