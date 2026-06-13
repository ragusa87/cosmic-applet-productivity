use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rule {
    pub id: Uuid,
    pub label: String,
    pub enabled: bool,
    pub app_id: String,
    pub title_contains: Option<String>,
    pub target: WorkspaceTarget,
    /// Output (monitor) name the target workspace is on, e.g. `"DP-4"` or
    /// `"eDP-1"`. Required to disambiguate workspaces with identical names
    /// across monitors (COSMIC names workspaces `"1"`, `"2"`, … per output,
    /// so name alone is ambiguous). `None` on rules persisted before this
    /// field existed; behaviour in that case is "first match wins".
    #[serde(default)]
    pub target_output: Option<String>,
    /// After moving the matching window, also switch the current workspace
    /// to the target. Default `false` so the window is moved silently.
    #[serde(default)]
    pub switch_to_workspace: bool,
    /// Skip toplevels with an empty title. The cosmic/ext protocols don't
    /// expose a parent/child or "is modal" flag, so empty-title is the only
    /// signal we have for transient popups (e.g. `JetBrains`' search overlay
    /// shares the IDE's `app_id` but has no title and shouldn't be moved).
    #[serde(default = "default_skip_empty_title")]
    pub skip_empty_title: bool,
}

fn default_skip_empty_title() -> bool {
    true
}

impl Rule {
    pub fn matches(&self, app_id: &str, title: &str) -> bool {
        if !self.enabled {
            return false;
        }
        if self.skip_empty_title && title.is_empty() {
            return false;
        }
        if self.app_id != app_id {
            return false;
        }
        if let Some(needle) = &self.title_contains
            && !title.to_lowercase().contains(&needle.to_lowercase())
        {
            return false;
        }
        true
    }

    pub fn new(app_id: impl Into<String>, target: WorkspaceTarget) -> Self {
        let app_id = app_id.into();
        Self {
            id: Uuid::new_v4(),
            label: app_id.clone(),
            enabled: true,
            app_id,
            title_contains: None,
            target,
            target_output: None,
            switch_to_workspace: false,
            skip_empty_title: true,
        }
    }

    /// Returns `true` when `other` covers exactly the same windows as `self`
    /// — same `app_id`, same `title_contains` (case-insensitive). Two such
    /// rules would compete for the same toplevel; `find_matching_rule` picks
    /// whichever was added first, which is confusing. Used by the settings
    /// dialog to reject duplicate creations.
    pub fn matches_same_windows(&self, other: &Rule) -> bool {
        if self.app_id != other.app_id {
            return false;
        }
        match (&self.title_contains, &other.title_contains) {
            (None, None) => true,
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum WorkspaceTarget {
    ByName(String),
    ByIndex(u32),
}

impl WorkspaceTarget {
    pub fn display(&self) -> String {
        match self {
            Self::ByName(n) => format!("\"{n}\""),
            Self::ByIndex(i) => format!("#{i}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_rule(app: &str, title_contains: Option<&str>, enabled: bool) -> Rule {
        Rule {
            id: Uuid::nil(),
            label: app.into(),
            enabled,
            app_id: app.into(),
            title_contains: title_contains.map(Into::into),
            target: WorkspaceTarget::ByIndex(0),
            target_output: None,
            switch_to_workspace: false,
            skip_empty_title: true,
        }
    }

    #[test]
    fn matches_exact_app_id() {
        let r = make_rule("firefox", None, true);
        assert!(r.matches("firefox", "Some Page"));
        assert!(!r.matches("Firefox", "Some Page"));
        assert!(!r.matches("firefox-extra", "Some Page"));
    }

    #[test]
    fn ignores_when_disabled() {
        let r = make_rule("firefox", None, false);
        assert!(!r.matches("firefox", "Some Page"));
    }

    #[test]
    fn skips_empty_title_by_default() {
        let r = make_rule("jetbrains-idea", None, true);
        assert!(!r.matches("jetbrains-idea", ""));
        assert!(r.matches("jetbrains-idea", "Project — Main.rs"));
    }

    #[test]
    fn empty_title_matches_when_skip_disabled() {
        let mut r = make_rule("jetbrains-idea", None, true);
        r.skip_empty_title = false;
        assert!(r.matches("jetbrains-idea", ""));
    }

    #[test]
    fn title_substring_is_case_insensitive() {
        let r = make_rule("firefox", Some("private"), true);
        assert!(r.matches("firefox", "Private Browsing"));
        assert!(r.matches("firefox", "PRIVATE"));
        assert!(!r.matches("firefox", "Normal Window"));
    }

    #[test]
    fn workspace_target_display() {
        assert_eq!(
            WorkspaceTarget::ByName("Coding".into()).display(),
            "\"Coding\""
        );
        assert_eq!(WorkspaceTarget::ByIndex(2).display(), "#2");
    }

    #[test]
    fn matches_same_windows_catches_duplicate_app_id() {
        let a = make_rule("Spotify", None, true);
        let b = make_rule("Spotify", None, true);
        assert!(a.matches_same_windows(&b));
    }

    #[test]
    fn matches_same_windows_ignores_different_app_id() {
        let a = make_rule("Spotify", None, true);
        let b = make_rule("Firefox", None, true);
        assert!(!a.matches_same_windows(&b));
    }

    #[test]
    fn matches_same_windows_allows_distinct_title_filters() {
        let a = make_rule("Firefox", None, true);
        let b = make_rule("Firefox", Some("Private"), true);
        // One filter present, other absent → not the same coverage.
        assert!(!a.matches_same_windows(&b));
        let c = make_rule("Firefox", Some("Work"), true);
        // Two different filters → not the same coverage.
        assert!(!b.matches_same_windows(&c));
    }

    #[test]
    fn matches_same_windows_collapses_case_in_title_filter() {
        let a = make_rule("Firefox", Some("PRIVATE"), true);
        let b = make_rule("Firefox", Some("private"), true);
        assert!(a.matches_same_windows(&b));
    }
}
