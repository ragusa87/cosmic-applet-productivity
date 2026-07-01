use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Local, NaiveDate, TimeZone};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::atomic;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub start: DateTime<Local>,
    pub end: Option<DateTime<Local>>,
    #[serde(default)]
    pub description: String,
}

impl Session {
    pub fn is_running(&self) -> bool {
        self.end.is_none()
    }

    pub fn duration(&self, now: DateTime<Local>) -> Duration {
        let end = self.end.unwrap_or(now);
        end - self.start
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Timer {
    pub id: Uuid,
    pub alias: String,
    #[serde(default)]
    pub default_description: String,
    #[serde(default)]
    pub selected: bool,
    #[serde(default)]
    pub auto_resume: bool,
    /// Whether this timer pauses on screen lock / suspend. Defaults to `true`
    /// (opt-out per timer). Old state files lacking the field default to `true`
    /// via [`default_true`], preserving the original behaviour.
    #[serde(default = "default_true")]
    pub auto_pause: bool,
    #[serde(default)]
    pub sessions: Vec<Session>,
}

fn default_true() -> bool {
    true
}

impl Timer {
    pub fn new(alias: impl Into<String>, default_description: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            alias: alias.into(),
            default_description: default_description.into(),
            selected: false,
            auto_resume: false,
            auto_pause: true,
            sessions: Vec::new(),
        }
    }

    pub fn running_session(&self) -> Option<&Session> {
        self.sessions.last().filter(|s| s.is_running())
    }

    pub fn running_session_mut(&mut self) -> Option<&mut Session> {
        self.sessions.last_mut().filter(|s| s.is_running())
    }

    pub fn is_running(&self) -> bool {
        self.running_session().is_some()
    }
}

/// Reserved alias for the auto-logged away-from-keyboard timer. Its sessions
/// span lock→unlock and are exported *commented out* so `taxi` ignores them.
pub const AFK_ALIAS: &str = "AFK";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppState {
    #[serde(default)]
    pub timers: Vec<Timer>,
    #[serde(default)]
    pub suppressed_aliases: Vec<String>,
    #[serde(default)]
    pub total_selected: bool,
    #[serde(default)]
    pub schema_version: u32,
    /// Timestamp of the most recent screen-lock / suspend, so the away
    /// duration can be recorded as an AFK session on unlock. `None` when not
    /// locked. Persisted so it survives an applet restart mid-lock.
    #[serde(default)]
    pub locked_at: Option<DateTime<Local>>,
}

const SCHEMA_VERSION: u32 = 1;

impl AppState {
    pub fn state_path() -> Result<PathBuf> {
        let dir = dirs::state_dir()
            .or_else(dirs::data_local_dir)
            .context("could not resolve XDG state dir")?;
        Ok(dir.join("cosmic-applet-taxi").join("state.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::state_path()?;
        if !path.exists() {
            return Ok(Self {
                schema_version: SCHEMA_VERSION,
                total_selected: true,
                ..Self::default()
            });
        }
        let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        let mut state: AppState =
            serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
        if state.schema_version == 0 {
            state.schema_version = SCHEMA_VERSION;
        }
        Ok(state)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::state_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dir {}", parent.display()))?;
        }
        let json = serde_json::to_vec_pretty(self).context("serialize state")?;
        atomic::write_preserving_mode(&path, &json, 0o644)
            .with_context(|| format!("atomic write {}", path.display()))?;
        Ok(())
    }

    pub fn find_timer(&self, id: Uuid) -> Option<&Timer> {
        self.timers.iter().find(|t| t.id == id)
    }

    pub fn find_timer_mut(&mut self, id: Uuid) -> Option<&mut Timer> {
        self.timers.iter_mut().find(|t| t.id == id)
    }

    pub fn find_by_alias(&self, alias: &str) -> Option<&Timer> {
        self.timers.iter().find(|t| t.alias == alias)
    }

    pub fn running_timer(&self) -> Option<&Timer> {
        self.timers.iter().find(|t| t.is_running())
    }

    /// True when a timer is currently running *and* it opted out of auto-pause
    /// — lock should then leave it counting (no pause, no AFK, no notify).
    pub fn running_opts_out_of_autopause(&self) -> bool {
        self.running_timer().is_some_and(|t| !t.auto_pause)
    }

    pub fn add_timer(&mut self, alias: String, default_description: String) -> Option<Uuid> {
        if self.timers.iter().any(|t| t.alias == alias) {
            return None;
        }
        self.suppressed_aliases.retain(|a| a != &alias);
        let timer = Timer::new(alias, default_description);
        let id = timer.id;
        self.timers.push(timer);
        Some(id)
    }

    pub fn remove_timer(&mut self, id: Uuid) {
        let Some(idx) = self.timers.iter().position(|t| t.id == id) else {
            return;
        };
        let timer = self.timers.remove(idx);
        if !timer.alias.is_empty() && !self.suppressed_aliases.contains(&timer.alias) {
            self.suppressed_aliases.push(timer.alias);
        }
    }

    pub fn pause_all_running(&mut self, now: DateTime<Local>) {
        for t in &mut self.timers {
            if let Some(s) = t.running_session_mut() {
                s.end = Some(now);
            }
            // Clear `auto_resume` on every iteration, not just on the
            // timer we just closed. Pause is an explicit user action (or
            // a `start_timer` switching tracks) and must cancel any
            // pending "resume me on unlock" intent — otherwise unlocking
            // after a screen-lock-then-manual-pause would silently
            // restart the timer the user thought they had paused.
            t.auto_resume = false;
        }
    }

    pub fn start_timer(&mut self, id: Uuid, now: DateTime<Local>) {
        self.pause_all_running(now);
        let Some(t) = self.find_timer_mut(id) else {
            return;
        };
        let description = t.default_description.clone();
        t.sessions.push(Session {
            start: now,
            end: None,
            description,
        });
        t.auto_resume = false;
    }

    pub fn pause_timer(&mut self, id: Uuid, now: DateTime<Local>) {
        let Some(t) = self.find_timer_mut(id) else {
            return;
        };
        if let Some(s) = t.running_session_mut() {
            s.end = Some(now);
        }
        t.auto_resume = false;
    }

    /// Mark all currently-running timers as auto-resume and pause them.
    /// Used by screen-lock / suspend.
    pub fn auto_pause_all(&mut self, now: DateTime<Local>) {
        for t in &mut self.timers {
            if t.is_running() {
                t.auto_resume = true;
                if let Some(s) = t.running_session_mut() {
                    s.end = Some(now);
                }
            }
        }
    }

    /// Return display labels (`alias: description`, or just `alias` when the
    /// session has no description) for every timer marked paused-by-lock, and
    /// clear the marker. Consumed on unlock to notify the user — we deliberately
    /// do NOT auto-resume; the user resumes manually.
    pub fn take_lock_paused_labels(&mut self) -> Vec<String> {
        let mut labels = Vec::new();
        for t in &mut self.timers {
            if t.auto_resume {
                t.auto_resume = false;
                let desc = t.sessions.last().map_or("", |s| s.description.as_str());
                if desc.is_empty() {
                    labels.push(t.alias.clone());
                } else {
                    labels.push(format!("{}: {desc}", t.alias));
                }
            }
        }
        labels
    }

    /// Take and clear the stored lock timestamp (set on lock, consumed on
    /// unlock to compute the away duration).
    pub fn take_locked_at(&mut self) -> Option<DateTime<Local>> {
        self.locked_at.take()
    }

    /// Record an away period (`from`..`to`) as a closed session on the reserved
    /// [`AFK_ALIAS`] timer, creating that timer if it doesn't exist yet. No-op
    /// for a non-positive span.
    pub fn record_afk(&mut self, from: DateTime<Local>, to: DateTime<Local>) {
        if to <= from {
            return;
        }
        let session = Session {
            start: from,
            end: Some(to),
            description: String::new(),
        };
        if let Some(t) = self.timers.iter_mut().find(|t| t.alias == AFK_ALIAS) {
            t.sessions.push(session);
        } else {
            let mut t = Timer::new(AFK_ALIAS, "");
            t.sessions.push(session);
            self.timers.push(t);
        }
    }

    pub fn reset_timer(&mut self, id: Uuid) {
        if let Some(t) = self.find_timer_mut(id) {
            t.sessions.clear();
            t.auto_resume = false;
        }
    }
}

/// Return all sessions whose effective work-date (cut-over-shifted) equals `day`.
pub fn sessions_for_date(
    timer: &Timer,
    day: NaiveDate,
    cutover_hour: u8,
) -> impl Iterator<Item = &Session> {
    timer
        .sessions
        .iter()
        .filter(move |s| cutover_date(s.start, cutover_hour) == day)
}

pub fn sum_for_date(
    timer: &Timer,
    day: NaiveDate,
    cutover_hour: u8,
    now: DateTime<Local>,
) -> Duration {
    sessions_for_date(timer, day, cutover_hour)
        .fold(Duration::zero(), |acc, s| acc + s.duration(now))
}

/// Effective "work date" of an instant, shifted by the cut-over hour.
/// Anything strictly before `cutover_hour:00:00` belongs to the previous date.
pub fn cutover_date(t: DateTime<Local>, cutover_hour: u8) -> NaiveDate {
    let shifted = t - Duration::hours(i64::from(cutover_hour));
    shifted.date_naive()
}

/// Build a Local datetime for `day` at `hour:minute`. Used to anchor parsed
/// time ranges from the .tks file to a specific date.
pub fn datetime_on(day: NaiveDate, hour: u32, minute: u32) -> Option<DateTime<Local>> {
    let naive = day.and_hms_opt(hour, minute, 0)?;
    Local.from_local_datetime(&naive).single()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(h: u32, m: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 5, 13, h, m, 0).unwrap()
    }

    #[test]
    fn cutover_shifts_pre_cutover_into_previous_day() {
        let t1 = Local.with_ymd_and_hms(2026, 5, 13, 3, 0, 0).unwrap();
        let t2 = Local.with_ymd_and_hms(2026, 5, 13, 4, 0, 0).unwrap();
        let t3 = Local.with_ymd_and_hms(2026, 5, 13, 23, 30, 0).unwrap();
        assert_eq!(
            cutover_date(t1, 4),
            NaiveDate::from_ymd_opt(2026, 5, 12).unwrap()
        );
        assert_eq!(
            cutover_date(t2, 4),
            NaiveDate::from_ymd_opt(2026, 5, 13).unwrap()
        );
        assert_eq!(
            cutover_date(t3, 4),
            NaiveDate::from_ymd_opt(2026, 5, 13).unwrap()
        );
    }

    #[test]
    fn start_timer_pauses_others() {
        let mut s = AppState::default();
        let a = s.add_timer("_a".into(), String::new()).unwrap();
        let b = s.add_timer("_b".into(), String::new()).unwrap();
        s.start_timer(a, at(9, 0));
        s.start_timer(b, at(9, 30));
        assert_eq!(s.timers.iter().filter(|t| t.is_running()).count(), 1);
        assert_eq!(
            s.timers.iter().find(|t| t.id == b).unwrap().sessions.len(),
            1
        );
        assert_eq!(
            s.timers.iter().find(|t| t.id == a).unwrap().sessions[0]
                .end
                .unwrap(),
            at(9, 30)
        );
    }

    #[test]
    fn delete_adds_to_suppressed_aliases() {
        let mut s = AppState::default();
        let a = s.add_timer("_a".into(), String::new()).unwrap();
        s.remove_timer(a);
        assert!(s.suppressed_aliases.contains(&"_a".to_string()));
    }

    #[test]
    fn add_timer_unsuppresses() {
        let mut s = AppState::default();
        s.suppressed_aliases.push("_a".to_owned());
        s.add_timer("_a".into(), String::new());
        assert!(!s.suppressed_aliases.contains(&"_a".to_string()));
    }

    #[test]
    fn add_timer_with_existing_alias_returns_none() {
        let mut s = AppState::default();
        s.add_timer("_a".into(), String::new()).unwrap();
        assert!(s.add_timer("_a".into(), String::new()).is_none());
    }

    #[test]
    fn auto_pause_all_closes_session_and_marks() {
        let mut s = AppState::default();
        let a = s.add_timer("_a".into(), String::new()).unwrap();
        s.start_timer(a, at(9, 0));
        s.auto_pause_all(at(9, 30));
        assert!(!s.timers[0].is_running());
        assert_eq!(s.timers[0].sessions[0].end.unwrap(), at(9, 30));
        assert!(s.timers[0].auto_resume);
    }

    #[test]
    fn take_lock_paused_labels_returns_then_clears_without_resuming() {
        let mut s = AppState::default();
        let a = s.add_timer("_a".into(), "designing".into()).unwrap();
        s.start_timer(a, at(9, 0));
        s.auto_pause_all(at(9, 30));

        let labels = s.take_lock_paused_labels();
        assert_eq!(labels, vec!["_a: designing".to_string()]);
        // Marker cleared, and the timer is NOT resumed (no new session).
        assert!(!s.timers[0].auto_resume);
        assert!(!s.timers[0].is_running());
        assert_eq!(s.timers[0].sessions.len(), 1);
        // A second call finds nothing.
        assert!(s.take_lock_paused_labels().is_empty());
    }

    #[test]
    fn take_lock_paused_labels_omits_empty_description() {
        let mut s = AppState::default();
        let a = s.add_timer("_a".into(), String::new()).unwrap();
        s.start_timer(a, at(9, 0));
        s.auto_pause_all(at(9, 30));
        assert_eq!(s.take_lock_paused_labels(), vec!["_a".to_string()]);
    }

    #[test]
    fn record_afk_creates_timer_and_appends_sessions() {
        let mut s = AppState::default();
        s.record_afk(at(9, 0), at(9, 30));
        let afk = s.find_by_alias(AFK_ALIAS).expect("AFK timer created");
        assert_eq!(afk.sessions.len(), 1);
        assert_eq!(afk.sessions[0].start, at(9, 0));
        assert_eq!(afk.sessions[0].end.unwrap(), at(9, 30));
        assert!(
            !afk.selected,
            "AFK stays out of the picked total by default"
        );

        // A second away period appends to the same timer.
        s.record_afk(at(12, 0), at(12, 45));
        assert_eq!(s.find_by_alias(AFK_ALIAS).unwrap().sessions.len(), 2);
    }

    #[test]
    fn record_afk_ignores_non_positive_span() {
        let mut s = AppState::default();
        s.record_afk(at(9, 30), at(9, 30)); // zero
        s.record_afk(at(10, 0), at(9, 0)); // negative
        assert!(s.find_by_alias(AFK_ALIAS).is_none());
    }

    #[test]
    fn take_locked_at_returns_then_clears() {
        let mut s = AppState::default();
        assert!(s.take_locked_at().is_none());
        s.locked_at = Some(at(9, 0));
        assert_eq!(s.take_locked_at(), Some(at(9, 0)));
        assert!(s.take_locked_at().is_none());
    }

    #[test]
    fn running_opts_out_of_autopause_reflects_active_timer_flag() {
        let mut s = AppState::default();
        let a = s.add_timer("_a".into(), String::new()).unwrap();
        s.start_timer(a, at(9, 0));
        // New timers default to auto_pause = true → opted in.
        assert!(!s.running_opts_out_of_autopause());
        s.find_timer_mut(a).unwrap().auto_pause = false;
        assert!(s.running_opts_out_of_autopause());
        // Nothing running → not an opt-out (AFK still applies when locked).
        s.pause_timer(a, at(9, 30));
        assert!(!s.running_opts_out_of_autopause());
    }

    #[test]
    fn timer_without_auto_pause_field_defaults_true() {
        // Old state.json predating the field must deserialise to auto_pause=true.
        let json = r#"{"id":"00000000-0000-0000-0000-000000000001","alias":"_a"}"#;
        let t: Timer = serde_json::from_str(json).unwrap();
        assert!(t.auto_pause);
    }

    #[test]
    fn auto_pause_all_leaves_manually_paused_timer_untouched() {
        let mut s = AppState::default();
        let a = s.add_timer("_a".into(), String::new()).unwrap();
        s.start_timer(a, at(9, 0));
        s.pause_timer(a, at(9, 15)); // explicit manual pause
        s.auto_pause_all(at(9, 30));
        // Not marked for resume, so unlock will neither notify nor resume it.
        assert!(!s.timers[0].auto_resume);
        assert!(s.take_lock_paused_labels().is_empty());
    }
}
