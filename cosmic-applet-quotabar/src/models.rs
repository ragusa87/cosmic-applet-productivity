use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    Anthropic,
    OpenAi,
}

impl Provider {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Anthropic => "Anthropic",
            Self::OpenAi => "OpenAI",
        }
    }
}

#[derive(Debug, Clone)]
pub struct UsageWindow {
    pub used_percent: f64,
    pub resets_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct ProviderSnapshot {
    pub provider: Provider,
    pub short: Option<UsageWindow>,
    pub weekly: Option<UsageWindow>,
}

impl ProviderSnapshot {
    pub fn worst_used(&self) -> Option<f64> {
        match (
            self.short.as_ref().map(|w| w.used_percent),
            self.weekly.as_ref().map(|w| w.used_percent),
        ) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(a), None) | (None, Some(a)) => Some(a),
            (None, None) => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RefreshError {
    pub provider: Provider,
    pub message: String,
}
