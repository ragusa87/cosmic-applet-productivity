use cosmic_config::CosmicConfigEntry;
use cosmic_config_derive::CosmicConfigEntry;

pub const APP_ID: &str = "com.github.ragusa87.CosmicAppletSlack";

#[derive(Debug, Clone, Default, CosmicConfigEntry, Eq, PartialEq)]
#[version = 1]
pub struct Config {
    pub disable_during_weekend: bool,
}
