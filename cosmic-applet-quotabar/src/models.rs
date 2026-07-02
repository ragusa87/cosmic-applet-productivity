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

/// Pay-as-you-go usage-credit spend for post-plan ("extra usage") consumption.
#[derive(Debug, Clone)]
pub struct SpendInfo {
    pub used: f64,
    pub limit: Option<f64>,
    pub percent: f64,
    pub currency: String,
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct ProviderSnapshot {
    pub provider: Provider,
    pub short: Option<UsageWindow>,
    pub weekly: Option<UsageWindow>,
    pub spend: Option<SpendInfo>,
}

impl ProviderSnapshot {
    pub fn worst_used(&self) -> Option<f64> {
        [
            self.short.as_ref().map(|w| w.used_percent),
            self.weekly.as_ref().map(|w| w.used_percent),
            self.spend.as_ref().filter(|s| s.enabled).map(|s| s.percent),
        ]
        .into_iter()
        .flatten()
        .fold(None, |acc, x| Some(acc.map_or(x, |a: f64| a.max(x))))
    }
}

#[derive(Debug, Clone)]
pub struct RefreshError {
    pub provider: Provider,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(used_percent: f64) -> UsageWindow {
        UsageWindow {
            used_percent,
            resets_at: None,
        }
    }

    fn spend(percent: f64, enabled: bool) -> SpendInfo {
        SpendInfo {
            used: 0.0,
            limit: None,
            percent,
            currency: "USD".to_owned(),
            enabled,
        }
    }

    fn snapshot(
        short: Option<UsageWindow>,
        weekly: Option<UsageWindow>,
        spend: Option<SpendInfo>,
    ) -> ProviderSnapshot {
        ProviderSnapshot {
            provider: Provider::Anthropic,
            short,
            weekly,
            spend,
        }
    }

    #[test]
    fn worst_used_is_none_without_windows_or_spend() {
        assert_eq!(snapshot(None, None, None).worst_used(), None);
    }

    #[test]
    fn worst_used_takes_max_of_windows() {
        let snap = snapshot(Some(window(30.0)), Some(window(70.0)), None);
        assert_eq!(snap.worst_used(), Some(70.0));
    }

    #[test]
    fn worst_used_includes_enabled_spend() {
        let snap = snapshot(Some(window(30.0)), None, Some(spend(90.0, true)));
        assert_eq!(snap.worst_used(), Some(90.0));
    }

    #[test]
    fn worst_used_ignores_disabled_spend() {
        let snap = snapshot(Some(window(30.0)), None, Some(spend(90.0, false)));
        assert_eq!(snap.worst_used(), Some(30.0));
    }

    #[test]
    fn worst_used_reflects_spend_only_snapshot() {
        let snap = snapshot(None, None, Some(spend(45.0, true)));
        assert_eq!(snap.worst_used(), Some(45.0));
    }
}
