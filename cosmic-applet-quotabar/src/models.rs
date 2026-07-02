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
    /// True once the plan itself is exhausted, i.e. the daily or weekly window
    /// has reached 100%. Used to decide when post-plan credit spend becomes
    /// relevant.
    pub fn plan_maxed(&self) -> bool {
        self.short.as_ref().is_some_and(|w| w.used_percent >= 100.0)
            || self
                .weekly
                .as_ref()
                .is_some_and(|w| w.used_percent >= 100.0)
    }

    /// The credit spend to render, or `None` when it should be hidden.
    ///
    /// Spend must be present and enabled. When `ignore_credits_when_plan_used`
    /// is set, it is additionally hidden until the plan is maxed
    /// (see [`Self::plan_maxed`]).
    pub fn visible_spend(&self, ignore_credits_when_plan_used: bool) -> Option<&SpendInfo> {
        let spend = self.spend.as_ref().filter(|s| s.enabled)?;
        if ignore_credits_when_plan_used && !self.plan_maxed() {
            return None;
        }
        Some(spend)
    }

    /// Highest usage percentage across the windows, and the credit spend when
    /// it is visible. Credits are folded in exactly when [`Self::visible_spend`]
    /// would show them, so the badge stays consistent with the credit row.
    pub fn worst_used(&self, ignore_credits_when_plan_used: bool) -> Option<f64> {
        [
            self.short.as_ref().map(|w| w.used_percent),
            self.weekly.as_ref().map(|w| w.used_percent),
            self.visible_spend(ignore_credits_when_plan_used)
                .map(|s| s.percent),
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
        assert_eq!(snapshot(None, None, None).worst_used(false), None);
    }

    #[test]
    fn worst_used_takes_max_of_windows() {
        let snap = snapshot(Some(window(30.0)), Some(window(70.0)), None);
        assert_eq!(snap.worst_used(false), Some(70.0));
    }

    #[test]
    fn worst_used_includes_enabled_spend() {
        let snap = snapshot(Some(window(30.0)), None, Some(spend(90.0, true)));
        assert_eq!(snap.worst_used(false), Some(90.0));
    }

    #[test]
    fn worst_used_ignores_disabled_spend() {
        let snap = snapshot(Some(window(30.0)), None, Some(spend(90.0, false)));
        assert_eq!(snap.worst_used(false), Some(30.0));
    }

    #[test]
    fn worst_used_reflects_spend_only_snapshot() {
        let snap = snapshot(None, None, Some(spend(45.0, true)));
        assert_eq!(snap.worst_used(false), Some(45.0));
    }

    #[test]
    fn worst_used_ignores_credits_while_plan_has_headroom() {
        // Setting on, plan not maxed: the high credit percent must not leak
        // into the badge — only daily/weekly count.
        let snap = snapshot(
            Some(window(30.0)),
            Some(window(70.0)),
            Some(spend(90.0, true)),
        );
        assert_eq!(snap.worst_used(true), Some(70.0));
    }

    #[test]
    fn worst_used_includes_credits_once_plan_maxed() {
        let snap = snapshot(
            Some(window(100.0)),
            Some(window(70.0)),
            Some(spend(120.0, true)),
        );
        assert_eq!(snap.worst_used(true), Some(120.0));
    }

    #[test]
    fn worst_used_spend_only_hidden_while_setting_on() {
        // No plan windows at all → never maxed → credits stay out of the badge.
        let snap = snapshot(None, None, Some(spend(45.0, true)));
        assert_eq!(snap.worst_used(true), None);
    }

    #[test]
    fn visible_spend_shown_when_setting_off() {
        let snap = snapshot(
            Some(window(30.0)),
            Some(window(40.0)),
            Some(spend(20.0, true)),
        );
        assert!(snap.visible_spend(false).is_some());
    }

    #[test]
    fn visible_spend_hidden_when_disabled_regardless_of_setting() {
        let snap = snapshot(Some(window(100.0)), None, Some(spend(20.0, false)));
        assert!(snap.visible_spend(false).is_none());
        assert!(snap.visible_spend(true).is_none());
    }

    #[test]
    fn visible_spend_hidden_when_plan_has_headroom() {
        let snap = snapshot(
            Some(window(30.0)),
            Some(window(99.9)),
            Some(spend(20.0, true)),
        );
        assert!(snap.visible_spend(true).is_none());
    }

    #[test]
    fn visible_spend_shown_when_daily_maxed() {
        let snap = snapshot(
            Some(window(100.0)),
            Some(window(40.0)),
            Some(spend(20.0, true)),
        );
        assert!(snap.visible_spend(true).is_some());
    }

    #[test]
    fn visible_spend_shown_when_weekly_maxed() {
        let snap = snapshot(
            Some(window(30.0)),
            Some(window(100.0)),
            Some(spend(20.0, true)),
        );
        assert!(snap.visible_spend(true).is_some());
    }

    #[test]
    fn visible_spend_none_without_spend() {
        let snap = snapshot(Some(window(100.0)), None, None);
        assert!(snap.visible_spend(true).is_none());
        assert!(snap.visible_spend(false).is_none());
    }

    #[test]
    fn plan_maxed_requires_a_full_window() {
        assert!(!snapshot(Some(window(99.0)), Some(window(80.0)), None).plan_maxed());
        assert!(snapshot(Some(window(100.0)), None, None).plan_maxed());
        assert!(snapshot(None, Some(window(100.0)), None).plan_maxed());
        assert!(!snapshot(None, None, None).plan_maxed());
    }
}
