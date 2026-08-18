//! Pure rule-selection logic, shared by the running applet (`app.rs`) and the
//! `--debug` dry-run explainer (`debug.rs`).
//!
//! Kept free of any Wayland/iced state so it can be unit-tested in isolation:
//! it takes plain slices of rules and workspace snapshots and returns which
//! rule (if any) governs a given window. `debug.rs` reuses the exact same
//! decision the applet makes, so the logged "would move / would switch"
//! explanation matches what really happens in the panel.

use crate::models::Rule;
use crate::wayland::WorkspaceSnapshot;

/// Whether `rule`'s target monitor is currently connected. A rule with no
/// `target_output` is always "available"; one that names an output requires
/// that output to currently expose at least one workspace.
pub fn output_available(rule: &Rule, workspaces: &[WorkspaceSnapshot]) -> bool {
    match &rule.target_output {
        None => true,
        Some(out) => workspaces
            .iter()
            .any(|w| w.output_name.as_deref() == Some(out.as_str())),
    }
}

/// Every enabled rule whose predicate accepts this window, in config order.
/// Exposed so the debugger can show *all* competitors, not just the winner —
/// that's how you spot an unexpected rule (e.g. a catch-all) grabbing a
/// transient window like flameshot's overlay.
pub fn matching_rules<'a>(rules: &'a [Rule], app_id: &str, title: &str) -> Vec<&'a Rule> {
    rules.iter().filter(|r| r.matches(app_id, title)).collect()
}

/// Pick the rule to apply to a window. Rules matching the same window form an
/// ordered fallback list: prefer the first (top-most) whose target monitor is
/// currently connected, so "workspace 1 on the external screen" wins when it's
/// plugged in and "workspace 1 on the laptop panel" takes over when it isn't.
/// If none of the targets' monitors are present, fall back to the first match
/// (best effort — the move may silently no-op).
pub fn select_rule<'a>(
    rules: &'a [Rule],
    workspaces: &[WorkspaceSnapshot],
    app_id: &str,
    title: &str,
) -> Option<&'a Rule> {
    let matches = matching_rules(rules, app_id, title);
    matches
        .iter()
        .copied()
        .find(|r| output_available(r, workspaces))
        .or_else(|| matches.first().copied())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::WorkspaceTarget;
    use uuid::Uuid;

    fn rule(app: &str, output: Option<&str>) -> Rule {
        Rule {
            id: Uuid::new_v4(),
            label: app.into(),
            enabled: true,
            app_id: app.into(),
            title_contains: None,
            target: WorkspaceTarget::ByName("1".into()),
            target_output: output.map(Into::into),
            switch_to_workspace: false,
            skip_empty_title: true,
        }
    }

    fn ws(output: &str) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            name: "1".into(),
            index: 0,
            output_name: Some(output.into()),
            is_pinned: false,
            is_active: false,
        }
    }

    #[test]
    fn no_rules_no_match() {
        assert!(select_rule(&[], &[], "flameshot", "Flameshot").is_none());
    }

    #[test]
    fn matches_by_app_id() {
        let rules = vec![rule("firefox", None)];
        assert!(select_rule(&rules, &[], "firefox", "Page").is_some());
        assert!(select_rule(&rules, &[], "flameshot", "Page").is_none());
    }

    #[test]
    fn skip_empty_title_hides_transient_from_selection() {
        // A transient overlay (empty title) sharing an app_id must NOT be
        // captured while skip_empty_title is on — this is the flameshot case.
        let rules = vec![rule("flameshot", None)];
        assert!(select_rule(&rules, &[], "flameshot", "").is_none());
        assert!(select_rule(&rules, &[], "flameshot", "Flameshot").is_some());
    }

    #[test]
    fn prefers_rule_whose_output_is_connected() {
        // Two fallback rules for the same window on different monitors.
        let rules = vec![
            rule("firefox", Some("DP-4")),
            rule("firefox", Some("eDP-1")),
        ];
        // Only the laptop panel is connected → the eDP-1 rule wins.
        let picked = select_rule(&rules, &[ws("eDP-1")], "firefox", "Page").unwrap();
        assert_eq!(picked.target_output.as_deref(), Some("eDP-1"));
    }

    #[test]
    fn falls_back_to_first_match_when_no_output_present() {
        let rules = vec![
            rule("firefox", Some("DP-4")),
            rule("firefox", Some("eDP-1")),
        ];
        // Neither monitor connected → first match wins (best effort).
        let picked = select_rule(&rules, &[], "firefox", "Page").unwrap();
        assert_eq!(picked.target_output.as_deref(), Some("DP-4"));
    }

    #[test]
    fn matching_rules_lists_all_competitors() {
        let rules = vec![
            rule("firefox", Some("DP-4")),
            rule("firefox", Some("eDP-1")),
        ];
        assert_eq!(matching_rules(&rules, "firefox", "Page").len(), 2);
    }
}
