//! Automation rules (ADR 0024): long-lived objects the daemon runs on its own. A rule is
//! *trigger + scope + filter + action + confirmation*; every firing is a [`RuleRun`] kept in
//! the activity history. Rules and history persist in the daemon (`rules.toml`), so they
//! run with no UI attached.
//!
//! A run first scans its scope, keeps the items that pass the filter, then either cleans
//! them ([`Confirm::Auto`]) or waits in [`RunState::Pending`] for the user
//! ([`Confirm::Ask`]): clean now, snooze a few minutes, or skip this time. Removal goes
//! through the same clean job, `Guard` and delete method as a manual clean (ADR 0021).

use serde::{Deserialize, Serialize};

use crate::jobs::ScanArea;
use crate::junk::JunkKind;

/// Daemon-assigned rule id, stable across restarts.
pub type RuleId = u64;

/// Daemon-assigned run id, stable across restarts.
pub type RunId = u64;

/// Unix seconds.
pub type Timestamp = i64;

/// A rule as the user edits it. [`Request::PutRule`](crate::Request::PutRule) with
/// `id == 0` creates one; any other id replaces that rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// 0 for a new rule.
    #[serde(default)]
    pub id: RuleId,
    /// User-visible name.
    pub name: String,
    /// Paused rules never fire (a pending run of theirs stays until resolved).
    pub enabled: bool,
    /// When it fires.
    pub trigger: Trigger,
    /// What it looks at.
    pub scope: RuleScope,
    /// Which found items it acts on.
    #[serde(default)]
    pub filter: RuleFilter,
    /// What it does with them.
    #[serde(default)]
    pub action: RuleAction,
    /// Whether it asks first.
    #[serde(default)]
    pub confirm: Confirm,
}

/// When a rule fires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum Trigger {
    /// Every `days` days (1..=90), at the first check at or after `hour` (0..=23, local
    /// time). A run missed while the machine slept or was off happens once at wake, never
    /// several times.
    Every {
        /// Interval in days.
        days: u32,
        /// Local hour of day.
        hour: u8,
    },
}

/// What a rule looks at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum RuleScope {
    /// A junk-style scan area (`system_junk`, `browser_data`, `developer_junk`, `trash`,
    /// `installers`, `leftovers`; anything else is `bad_request`), narrowed to `kinds`
    /// (empty = every kind of the area).
    Junk {
        /// The area.
        area: ScanArea,
        /// Group kinds to keep; empty = all.
        #[serde(default)]
        kinds: Vec<JunkKind>,
    },
}

/// Item filter; an item must pass every set bound.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuleFilter {
    /// Only items whose newest modification is at least this many days old (for
    /// `project_artifacts`: the build output was not rebuilt for that long). 0 = any age.
    pub idle_days: u32,
    /// Only items of at least this many bytes. 0 = any size.
    pub min_bytes: u64,
    /// Also take items marked `review` (default: only `safe` ones).
    pub include_review: bool,
}

/// What a rule does with the filtered items.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleAction {
    /// Remove them with the stored delete method (Trash or permanent).
    #[default]
    Clean,
}

/// Whether a run asks before acting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confirm {
    /// Wait in [`RunState::Pending`] for the user's [`Decision`].
    #[default]
    Ask,
    /// Act at once.
    Auto,
}

/// A rule plus its schedule state, as listed by `list_rules`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleInfo {
    /// The rule.
    pub rule: Rule,
    /// Start of its last run.
    #[serde(default)]
    pub last_run: Option<Timestamp>,
    /// When it fires next; `None` while paused.
    #[serde(default)]
    pub next_run: Option<Timestamp>,
}

/// One firing of a rule (activity history, newest first; the daemon keeps a bounded tail).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleRun {
    /// The run.
    pub id: RunId,
    /// Its rule (may since have been deleted).
    pub rule: RuleId,
    /// The rule's name when it fired.
    pub rule_name: String,
    /// When it fired.
    pub started: Timestamp,
    /// Where it is.
    pub state: RunState,
    /// Items the filter kept (largest first, capped for display).
    #[serde(default)]
    pub items: Vec<RunItem>,
    /// Number of items the filter kept (may exceed `items.len()`).
    #[serde(default)]
    pub item_count: u64,
    /// Their total size.
    #[serde(default)]
    pub bytes: u64,
}

/// One item a run acts on (display only; the daemon addresses it by scan item id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunItem {
    /// Display name (project, app, file).
    pub name: String,
    /// Path or registry key.
    pub location: String,
    /// Size.
    pub bytes: u64,
    /// Newest modification inside.
    #[serde(default)]
    pub modified: Option<Timestamp>,
}

/// Lifecycle of a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "s", rename_all = "snake_case")]
pub enum RunState {
    /// Scanning the scope.
    Scanning,
    /// Waiting for the user; after a snooze, asks again at `until`.
    Pending {
        /// Snoozed until then; `None` = asking now.
        #[serde(default)]
        until: Option<Timestamp>,
    },
    /// A build tool holds the items (e.g. cargo's `target` lock); retried at `until`.
    Deferred {
        /// Next attempt.
        until: Timestamp,
    },
    /// Removing the items.
    Cleaning,
    /// Finished.
    Done {
        /// Bytes freed.
        freed: u64,
        /// Items that could not be removed.
        #[serde(default)]
        failed: u64,
    },
    /// Nothing passed the filter.
    Nothing,
    /// The user skipped this run.
    Skipped,
    /// Stopped by an error.
    Failed {
        /// What went wrong.
        message: String,
    },
}

impl RunState {
    /// `true` once the run will not change any more.
    pub const fn is_finished(&self) -> bool {
        matches!(
            self,
            Self::Done { .. } | Self::Nothing | Self::Skipped | Self::Failed { .. }
        )
    }

    /// `true` while the run waits for a [`Decision`] (asking now or snoozed).
    pub const fn awaits_user(&self) -> bool {
        matches!(self, Self::Pending { .. })
    }
}

/// The user's answer to a pending run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum Decision {
    /// Clean now.
    CleanNow,
    /// Ask again in `minutes` (1..=1440).
    Snooze {
        /// Delay.
        minutes: u32,
    },
    /// Skip this run; the rule fires again on its schedule.
    Skip,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_rules_without_optional_fields_load() {
        let json = serde_json::json!({
            "name": "Rust targets",
            "enabled": true,
            "trigger": {"k": "every", "days": 14, "hour": 10},
            "scope": {"k": "junk", "area": {"a": "developer_junk"}}
        });
        let rule = serde_json::from_value::<Rule>(json);
        assert!(
            rule.is_ok_and(|r| r.id == 0
                && r.confirm == Confirm::Ask
                && r.filter == RuleFilter::default()),
            "missing id, filter, action and confirm take their defaults"
        );
    }
}
