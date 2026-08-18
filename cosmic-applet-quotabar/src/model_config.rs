//! Best-effort readers for the *currently-configured model* of each provider.
//!
//! Neither provider's usage endpoint reports the active model, so we read it
//! from the local CLI config the tool already maintains — the same files the
//! credential loaders read next to. Everything here is best-effort: any
//! missing file, unreadable path, or parse failure yields `None` rather than
//! an error, because a missing model name should never break a quota refresh.
//!
//! The raw config string is returned verbatim (e.g. `"opus[1m]"`,
//! `"gpt-5-codex"`) — no alias resolution or suffix stripping.

use std::path::PathBuf;

/// Read the Anthropic model from `~/.claude/settings.json` (top-level `model`).
///
/// Note: this reads only the user-level settings file. It intentionally does
/// not consult the `ANTHROPIC_MODEL` env var or project-level
/// `.claude/settings.json` — a panel applet has no notion of a "current
/// project", and the user-level file is the stable single source.
pub fn anthropic_model() -> Option<String> {
    let home = dirs::home_dir()?;
    read_model(
        home.join(".claude").join("settings.json"),
        parse_anthropic_model,
    )
}

/// Read the Codex model from `~/.codex/config.toml` (top-level `model` key).
pub fn openai_model() -> Option<String> {
    let home = dirs::home_dir()?;
    read_model(home.join(".codex").join("config.toml"), parse_codex_model)
}

fn read_model(path: PathBuf, parse: fn(&str) -> Option<String>) -> Option<String> {
    let contents = std::fs::read_to_string(&path).ok()?;
    parse(&contents)
}

/// Pull the top-level `model` string out of Claude Code's `settings.json`.
fn parse_anthropic_model(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let model = value.get("model")?.as_str()?.trim();
    (!model.is_empty()).then(|| model.to_owned())
}

/// Pull the top-level `model` key out of Codex's `config.toml`.
///
/// Lightweight line scan rather than a full TOML parse (the crate has no
/// `toml` dependency): return the first `model = "..."` / `model = '...'`
/// key seen *before* any `[section]` header — top-level keys always precede
/// tables in TOML. Only the default top-level key is resolved;
/// profile-scoped (`[profiles.x]`) overrides are ignored.
fn parse_codex_model(toml: &str) -> Option<String> {
    for raw in toml.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // First table header ends the top-level section — stop looking.
        if line.starts_with('[') {
            return None;
        }
        let Some((key, rest)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "model" {
            continue;
        }
        // Strip an inline `# comment`, then surrounding quotes.
        let mut val = rest.trim();
        if let Some(idx) = val.find('#') {
            val = val[..idx].trim();
        }
        let val = val
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| val.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(val)
            .trim();
        return (!val.is_empty()).then(|| val.to_owned());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_reads_top_level_model() {
        assert_eq!(
            parse_anthropic_model(r#"{"model": "opus[1m]", "theme": "dark"}"#),
            Some("opus[1m]".to_owned())
        );
    }

    #[test]
    fn anthropic_none_when_field_absent() {
        assert_eq!(parse_anthropic_model(r#"{"theme": "dark"}"#), None);
    }

    #[test]
    fn anthropic_none_on_empty_or_blank_value() {
        assert_eq!(parse_anthropic_model(r#"{"model": ""}"#), None);
        assert_eq!(parse_anthropic_model(r#"{"model": "   "}"#), None);
    }

    #[test]
    fn anthropic_none_on_garbage() {
        assert_eq!(parse_anthropic_model("not json"), None);
        assert_eq!(parse_anthropic_model(""), None);
    }

    #[test]
    fn codex_reads_double_quoted() {
        assert_eq!(
            parse_codex_model("model = \"gpt-5-codex\"\n"),
            Some("gpt-5-codex".to_owned())
        );
    }

    #[test]
    fn codex_reads_single_quoted() {
        assert_eq!(
            parse_codex_model("model = 'gpt-5-codex'\n"),
            Some("gpt-5-codex".to_owned())
        );
    }

    #[test]
    fn codex_skips_comments_and_blanks() {
        let toml = "# codex config\n\n  model = \"gpt-5\"  # active\n";
        assert_eq!(parse_codex_model(toml), Some("gpt-5".to_owned()));
    }

    #[test]
    fn codex_ignores_model_inside_a_section() {
        let toml = "approval = \"on\"\n[profiles.work]\nmodel = \"gpt-5-codex\"\n";
        assert_eq!(parse_codex_model(toml), None);
    }

    #[test]
    fn codex_none_when_absent() {
        assert_eq!(parse_codex_model("approval = \"on\"\n"), None);
        assert_eq!(parse_codex_model(""), None);
    }
}
