//! Exception list for the experimental cap mode.
//!
//! Windows matching an exception are invisible to the cap planner: they are
//! never queued for placement, never counted toward a workspace's cap and
//! never evicted or compacted. This is the escape hatch for dialogs — the
//! cosmic/ext toplevel protocols expose no parent/"is modal" flag, so the
//! user (or the built-in list) has to name them.
//!
//! Patterns are regexes matched with `is_match` (search) semantics against
//! the app id and title, mirroring cosmic-comp's floating exceptions. The
//! defaults are seeded from its `data/tiling-exceptions.ron`.

use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapException {
    pub enabled: bool,
    /// Regex searched against the app id. Empty matches any app id.
    pub appid: String,
    /// Regex searched against the title. Empty matches any title.
    pub title: String,
}

impl CapException {
    pub fn new(appid: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            enabled: true,
            appid: appid.into(),
            title: title.into(),
        }
    }
}

/// Compiled form of the exception list, built once per config change so the
/// per-snapshot matching doesn't recompile regexes. Disabled entries and
/// entries with an invalid regex (warned once at build time) never match.
#[derive(Debug, Default)]
pub struct ExceptionMatcher {
    /// `None` = empty pattern = match any.
    compiled: Vec<(Option<Regex>, Option<Regex>)>,
}

impl ExceptionMatcher {
    pub fn new(exceptions: &[CapException]) -> Self {
        let compile = |pattern: &str, field: &str| -> Result<Option<Regex>, ()> {
            if pattern.is_empty() {
                return Ok(None);
            }
            Regex::new(pattern).map(Some).map_err(|e| {
                tracing::warn!(pattern, field, error = %e, "cap: invalid exception regex; entry ignored");
            })
        };
        let compiled = exceptions
            .iter()
            .filter(|e| e.enabled)
            .filter_map(|e| {
                Some((
                    compile(&e.appid, "appid").ok()?,
                    compile(&e.title, "title").ok()?,
                ))
            })
            .collect();
        Self { compiled }
    }

    pub fn matches(&self, app_id: &str, title: &str) -> bool {
        self.compiled.iter().any(|(a, t)| {
            a.as_ref().is_none_or(|re| re.is_match(app_id))
                && t.as_ref().is_none_or(|re| re.is_match(title))
        })
    }
}

/// True when the cap planner must leave this window alone entirely. An empty
/// title is always exempt — same reasoning as `Rule::skip_empty_title`: it's
/// the only protocol-level hint that a toplevel is a transient popup.
pub fn cap_exempt(matcher: &ExceptionMatcher, app_id: &str, title: &str) -> bool {
    title.is_empty() || matcher.matches(app_id, title)
}

/// Built-in exceptions, seeded from cosmic-comp's `data/tiling-exceptions.ron`
/// (entries with several titles are flattened to one row per title), plus a
/// Firefox entry for its download dialog — the case that motivated this list.
pub fn default_cap_exceptions() -> Vec<CapException> {
    [
        // Title-only matches (any app id).
        ("", "Discord Updater"),
        ("", "Steam"),
        ("", "wl-clipboard"),
        // App-id matches.
        ("Authy Desktop", ""),
        ("Com.github.amezin.ddterm", ""),
        ("Com.github.donadigo.eddy", ""),
        ("com.system76.CosmicFilesDialog", ""),
        ("com.system76.CosmicStoreDialog", ""),
        ("Enpass", "Enpass Assistant"),
        ("Gjs", "Settings"),
        ("Gnome-initial-setup", ""),
        ("Gnome-terminal", "Preferences - General"),
        ("Guake", ""),
        ("Io.elementary.sideload", ""),
        ("KotatogramDesktop", "Media viewer"),
        ("Mozilla VPN", ""),
        ("update-manager", "Software Updater"),
        ("Solaar", ""),
        ("Steam", "^.*?(Guard|Login).*"),
        ("TelegramDesktop", "Media viewer"),
        ("Zotero", "Quick Format Citation"),
        ("gjs", ""),
        ("gnome-screenshot", ""),
        ("ibus-.*", ""),
        ("jetbrains-toolbox", ""),
        ("jetbrains-webstorm", "Customize WebStorm"),
        ("jetbrains-webstorm", "License Activation"),
        ("jetbrains-webstorm", "Welcome to WebStorm"),
        ("krunner", ""),
        ("pritunl", ""),
        ("re.sonny.Junction", ""),
        ("system76-driver", ""),
        ("tilda", ""),
        ("zoom", ""),
        ("Tor Browser", ""),
        ("^.*?action=join.*$", ""),
        ("^(slack|com.slack.Slack)", "^.*?(Huddle Preview).*"),
        (
            "^(thunderbird|org.mozilla.thunderbird)(-esr|_esr)*",
            "^(Write:).*",
        ),
        // Not from cosmic-comp: Firefox download dialog ("Opening <file>").
        // The title is locale-dependent; edit it if Firefox runs localized.
        ("^(firefox|org.mozilla.firefox)", "^Opening "),
    ]
    .into_iter()
    .map(|(appid, title)| CapException::new(appid, title))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matcher(entries: &[(&str, &str)]) -> ExceptionMatcher {
        let list: Vec<CapException> = entries
            .iter()
            .map(|(a, t)| CapException::new(*a, *t))
            .collect();
        ExceptionMatcher::new(&list)
    }

    #[test]
    fn empty_patterns_match_anything_on_that_field() {
        let m = matcher(&[("", "Discord Updater")]);
        assert!(m.matches("whatever", "Discord Updater v2"));
        assert!(!m.matches("whatever", "Discord"));

        let m = matcher(&[("zoom", "")]);
        assert!(m.matches("zoom", "any title at all"));
        assert!(!m.matches("firefox", "zoom"));
    }

    #[test]
    fn regex_search_semantics_not_full_match() {
        let m = matcher(&[("^(firefox|org.mozilla.firefox)", "^Opening ")]);
        assert!(m.matches("firefox", "Opening report.pdf"));
        assert!(m.matches("org.mozilla.firefox", "Opening data.zip"));
        assert!(!m.matches("firefox", "Mozilla Firefox"));
        assert!(!m.matches("librewolf", "Opening x"));
    }

    #[test]
    fn disabled_entries_never_match() {
        let mut e = CapException::new("zoom", "");
        e.enabled = false;
        let m = ExceptionMatcher::new(&[e]);
        assert!(!m.matches("zoom", "Meeting"));
    }

    #[test]
    fn invalid_regex_is_ignored_not_fatal() {
        let m = matcher(&[("([", ""), ("zoom", "")]);
        assert!(!m.matches("([", "x"), "broken entry dropped");
        assert!(m.matches("zoom", "x"), "valid entries still work");
    }

    #[test]
    fn empty_title_is_always_exempt() {
        let m = ExceptionMatcher::new(&[]);
        assert!(cap_exempt(&m, "jetbrains-idea", ""));
        assert!(!cap_exempt(&m, "jetbrains-idea", "Main.rs"));
    }

    #[test]
    fn defaults_cover_steam_and_firefox_dialog() {
        let m = ExceptionMatcher::new(&default_cap_exceptions());
        assert!(m.matches("Steam", "Steam Guard - Computer Authorization"));
        assert!(m.matches("firefox", "Opening report.pdf"));
        assert!(m.matches("anything", "Steam"));
        assert!(!m.matches("firefox", "My Tab - Mozilla Firefox"));
        assert!(!m.matches("com.system76.CosmicTerm", "~"));
    }

    #[test]
    fn defaults_all_compile() {
        let defaults = default_cap_exceptions();
        let m = ExceptionMatcher::new(&defaults);
        assert_eq!(
            m.compiled.len(),
            defaults.len(),
            "no default regex is invalid"
        );
    }
}
