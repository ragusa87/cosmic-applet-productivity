use cosmic_config::CosmicConfigEntry;
use cosmic_config_derive::CosmicConfigEntry;

use crate::models::Rule;

pub const APP_ID: &str = "com.github.ragusa87.CosmicAppletWindowRules";

#[derive(Debug, Clone, CosmicConfigEntry, Eq, PartialEq, Default)]
#[version = 1]
pub struct Config {
    pub rules: Vec<Rule>,
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
