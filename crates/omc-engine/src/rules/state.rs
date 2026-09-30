//! The rule table and activity history: what `rules.toml` holds, plus the validation and
//! bookkeeping that needs no I/O and no clock (callers pass `now`).

use omc_proto::jobs::ScanArea;
use omc_proto::rules::{
    Rule, RuleId, RuleInfo, RuleRun, RuleScope, RunId, RunState, Timestamp, Trigger,
};
use omc_proto::{ErrorCode, RpcError};
use serde::{Deserialize, Serialize};

use super::schedule::Zone;

/// Finished runs kept in the history (unfinished ones are never dropped).
pub(crate) const MAX_RUNS: usize = 200;
/// Items kept on a run for display; `item_count` and `bytes` cover all of them.
pub(crate) const RUN_ITEMS: usize = 25;
/// Longest rule name.
const MAX_NAME_CHARS: usize = 120;
/// Allowed `Trigger::Every::days`.
const DAYS: std::ops::RangeInclusive<u32> = 1..=90;
/// Last valid `Trigger::Every::hour`.
const LAST_HOUR: u8 = 23;

/// One stored rule with its schedule state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RuleEntry {
    pub(crate) rule: Rule,
    /// Start of the last run.
    #[serde(default)]
    pub(crate) last_run: Option<Timestamp>,
    /// When the schedule (re)started: creation, or the moment a paused rule was resumed.
    /// A last run before the anchor does not count (see `schedule`).
    pub(crate) anchor: Timestamp,
}

/// Everything `rules.toml` persists.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct State {
    next_rule: RuleId,
    next_run: RunId,
    pub(crate) rules: Vec<RuleEntry>,
    /// Oldest first.
    pub(crate) runs: Vec<RuleRun>,
}

fn bad_request(message: impl Into<String>) -> RpcError {
    RpcError::new(ErrorCode::BadRequest, message)
}

fn not_found(message: impl Into<String>) -> RpcError {
    RpcError::new(ErrorCode::NotFound, message)
}

/// `true` for the scan areas whose output is a junk report.
pub(crate) const fn is_junk_area(area: &ScanArea) -> bool {
    matches!(
        area,
        ScanArea::SystemJunk
            | ScanArea::BrowserData
            | ScanArea::DeveloperJunk
            | ScanArea::Trash
            | ScanArea::Installers
            | ScanArea::Leftovers
    )
}

/// Checks a rule as the user edits it and returns it normalised (trimmed name).
pub(crate) fn validate(mut rule: Rule) -> Result<Rule, RpcError> {
    let name = rule.name.trim();
    if name.is_empty() {
        return Err(bad_request("the rule needs a name"));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(bad_request(format!(
            "the rule name is longer than {MAX_NAME_CHARS} characters"
        )));
    }
    if name.len() != rule.name.len() {
        rule.name = name.to_owned();
    }
    let Trigger::Every { days, hour } = &rule.trigger;
    if !DAYS.contains(days) {
        return Err(bad_request(format!(
            "the interval must be {}..={} days, got {days}",
            DAYS.start(),
            DAYS.end()
        )));
    }
    if *hour > LAST_HOUR {
        return Err(bad_request(format!("the hour must be 0..=23, got {hour}")));
    }
    let RuleScope::Junk { area, .. } = &rule.scope;
    if !is_junk_area(area) {
        return Err(bad_request(
            "a rule can only look at a junk area (system, browser, developer, trash, installers or leftovers)",
        ));
    }
    Ok(rule)
}

impl State {
    /// Makes a freshly parsed state consistent: ids that never repeat, invalid rules paused
    /// (they cannot fire safely), runs the previous daemon was cleaning marked failed (the
    /// outcome is unknown).
    pub(crate) fn recover(&mut self) {
        let max_rule = self.rules.iter().map(|e| e.rule.id).max().unwrap_or(0);
        self.next_rule = self.next_rule.max(max_rule.saturating_add(1)).max(1);
        let max_run = self.runs.iter().map(|r| r.id).max().unwrap_or(0);
        self.next_run = self.next_run.max(max_run.saturating_add(1)).max(1);
        for entry in &mut self.rules {
            if entry.rule.enabled && validate(entry.rule.clone()).is_err() {
                tracing::warn!(rule = entry.rule.id, "invalid stored rule; paused");
                entry.rule.enabled = false;
            }
        }
        for run in &mut self.runs {
            if run.state == RunState::Cleaning {
                run.state = RunState::Failed {
                    message: "interrupted by a daemon restart".to_owned(),
                };
            }
        }
    }

    pub(crate) fn entry(&self, id: RuleId) -> Option<&RuleEntry> {
        self.rules.iter().find(|e| e.rule.id == id)
    }

    fn entry_mut(&mut self, id: RuleId) -> Option<&mut RuleEntry> {
        self.rules.iter_mut().find(|e| e.rule.id == id)
    }

    pub(crate) fn run(&self, id: RunId) -> Option<&RuleRun> {
        self.runs.iter().find(|r| r.id == id)
    }

    pub(crate) fn run_mut(&mut self, id: RunId) -> Option<&mut RuleRun> {
        self.runs.iter_mut().find(|r| r.id == id)
    }

    /// `true` while `rule` has a run that is not finished.
    pub(crate) fn has_unfinished_run(&self, rule: RuleId) -> bool {
        self.runs
            .iter()
            .any(|r| r.rule == rule && !r.state.is_finished())
    }

    /// A rule with its schedule.
    pub(crate) fn info(entry: &RuleEntry, zone: Zone) -> RuleInfo {
        RuleInfo {
            rule: entry.rule.clone(),
            last_run: entry.last_run,
            next_run: entry
                .rule
                .enabled
                .then(|| zone.next_run(&entry.rule.trigger, entry.last_run, entry.anchor))
                .flatten(),
        }
    }

    pub(crate) fn infos(&self, zone: Zone) -> Vec<RuleInfo> {
        self.rules.iter().map(|e| Self::info(e, zone)).collect()
    }

    /// Creates (`rule.id == 0`) or replaces a rule; the stored rule's id. A paused rule that
    /// gets enabled restarts its schedule at `now`.
    pub(crate) fn put_rule(&mut self, rule: Rule, now: Timestamp) -> Result<RuleId, RpcError> {
        let mut rule = validate(rule)?;
        if rule.id == 0 {
            rule.id = self.next_rule.max(1);
            self.next_rule = rule.id.saturating_add(1);
            let id = rule.id;
            self.rules.push(RuleEntry {
                rule,
                last_run: None,
                anchor: now,
            });
            return Ok(id);
        }
        let id = rule.id;
        let entry = self
            .entry_mut(id)
            .ok_or_else(|| not_found(format!("no rule {id}")))?;
        if rule.enabled && !entry.rule.enabled {
            entry.anchor = now;
        }
        entry.rule = rule;
        Ok(id)
    }

    /// Removes a rule. Its unfinished runs that have not started cleaning are skipped
    /// (nothing is left to answer them); those are returned, changed.
    pub(crate) fn delete_rule(&mut self, id: RuleId) -> Result<Vec<RuleRun>, RpcError> {
        let before = self.rules.len();
        self.rules.retain(|e| e.rule.id != id);
        if self.rules.len() == before {
            return Err(not_found(format!("no rule {id}")));
        }
        let mut skipped = Vec::new();
        for run in self.runs.iter_mut().filter(|r| r.rule == id) {
            if matches!(
                run.state,
                RunState::Scanning | RunState::Pending { .. } | RunState::Deferred { .. }
            ) {
                run.state = RunState::Skipped;
                skipped.push(run.clone());
            }
        }
        Ok(skipped)
    }

    /// Records a new run of `rule` (state `Scanning`) and moves the rule's last run to
    /// `now`. `bad_request` while the rule has an unfinished run.
    pub(crate) fn begin_run(&mut self, rule: RuleId, now: Timestamp) -> Result<RuleRun, RpcError> {
        if self.has_unfinished_run(rule) {
            return Err(bad_request(format!(
                "rule {rule} already has a run in progress"
            )));
        }
        let run_id = self.next_run.max(1);
        let entry = self
            .entry_mut(rule)
            .ok_or_else(|| not_found(format!("no rule {rule}")))?;
        entry.last_run = Some(now);
        let run = RuleRun {
            id: run_id,
            rule,
            rule_name: entry.rule.name.clone(),
            started: now,
            state: RunState::Scanning,
            items: Vec::new(),
            item_count: 0,
            bytes: 0,
        };
        self.next_run = run_id.saturating_add(1);
        self.runs.push(run.clone());
        self.prune();
        Ok(run)
    }

    /// Sets the last run of `rule` (a skipped run makes the rule wait a full interval).
    pub(crate) fn set_last_run(&mut self, rule: RuleId, at: Timestamp) {
        if let Some(entry) = self.entry_mut(rule) {
            entry.last_run = Some(at);
        }
    }

    /// Drops the oldest finished runs beyond [`MAX_RUNS`].
    pub(crate) fn prune(&mut self) {
        while self.runs.len() > MAX_RUNS {
            let Some(oldest) = self.runs.iter().position(|r| r.state.is_finished()) else {
                break;
            };
            self.runs.remove(oldest);
        }
    }

    /// Enabled rules that are due at `now` and have no unfinished run.
    pub(crate) fn due_rules(&self, now: Timestamp, zone: Zone) -> Vec<RuleId> {
        self.rules
            .iter()
            .filter(|e| e.rule.enabled && !self.has_unfinished_run(e.rule.id))
            .filter(|e| {
                zone.next_run(&e.rule.trigger, e.last_run, e.anchor)
                    .is_some_and(|next| now >= next)
            })
            .map(|e| e.rule.id)
            .collect()
    }

    /// Runs whose snooze ended by `now`.
    pub(crate) fn snoozes_over(&self, now: Timestamp) -> Vec<RunId> {
        self.runs
            .iter()
            .filter(|r| matches!(r.state, RunState::Pending { until: Some(t) } if now >= t))
            .map(|r| r.id)
            .collect()
    }

    /// Runs whose deferral ended by `now`.
    pub(crate) fn deferrals_over(&self, now: Timestamp) -> Vec<RunId> {
        self.runs
            .iter()
            .filter(|r| matches!(r.state, RunState::Deferred { until } if now >= until))
            .map(|r| r.id)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use omc_proto::rules::{Confirm, RuleAction, RuleFilter};

    use super::*;

    const ZONE: Zone = Zone::Fixed(0);
    const HOUR: i64 = 3600;
    const DAY: i64 = 86_400;
    /// 2026-01-01 00:00 UTC.
    const T0: i64 = 1_767_225_600;

    fn rule(name: &str) -> Rule {
        Rule {
            id: 0,
            name: name.to_owned(),
            enabled: true,
            trigger: Trigger::Every { days: 14, hour: 10 },
            scope: RuleScope::Junk {
                area: ScanArea::DeveloperJunk,
                kinds: Vec::new(),
            },
            filter: RuleFilter::default(),
            action: RuleAction::Clean,
            confirm: Confirm::Ask,
        }
    }

    fn code<T>(result: &Result<T, RpcError>) -> Option<ErrorCode> {
        result.as_ref().err().map(|err| err.code)
    }

    #[test]
    fn invalid_rules_are_bad_requests_and_unknown_ids_are_not_found() {
        let mut state = State::default();
        let mut bad = |edit: fn(&mut Rule)| {
            let mut r = rule("x");
            edit(&mut r);
            code(&state.put_rule(r, T0))
        };
        assert_eq!(
            bad(|r| r.name = "   ".to_owned()),
            Some(ErrorCode::BadRequest),
            "blank name"
        );
        assert_eq!(
            bad(|r| r.trigger = Trigger::Every { days: 0, hour: 10 }),
            Some(ErrorCode::BadRequest),
            "zero days"
        );
        assert_eq!(
            bad(|r| r.trigger = Trigger::Every { days: 91, hour: 10 }),
            Some(ErrorCode::BadRequest),
            "over 90 days"
        );
        assert_eq!(
            bad(|r| r.trigger = Trigger::Every { days: 1, hour: 24 }),
            Some(ErrorCode::BadRequest),
            "hour 24"
        );
        for area in [
            ScanArea::LargeOldFiles,
            ScanArea::Duplicates,
            ScanArea::SpaceLens {
                root: "/".to_owned(),
            },
        ] {
            let mut r = rule("files");
            r.scope = RuleScope::Junk {
                area,
                kinds: Vec::new(),
            };
            assert_eq!(
                code(&state.put_rule(r, T0)),
                Some(ErrorCode::BadRequest),
                "non-junk area"
            );
        }
        let mut ghost = rule("ghost");
        ghost.id = 42;
        assert_eq!(
            code(&state.put_rule(ghost, T0)),
            Some(ErrorCode::NotFound),
            "unknown id"
        );
        assert_eq!(
            code(&state.delete_rule(42)),
            Some(ErrorCode::NotFound),
            "delete unknown"
        );
        assert!(
            state.rules.is_empty(),
            "nothing was stored by the rejected puts"
        );

        for days in [1, 90] {
            let mut edge = rule(" edge ");
            edge.trigger = Trigger::Every { days, hour: 0 };
            assert!(state.put_rule(edge, T0).is_ok(), "days {days} is valid");
        }
        assert!(
            state.rules.iter().all(|e| e.rule.name == "edge"),
            "names are trimmed"
        );
    }

    #[test]
    fn put_assigns_ids_replaces_and_restarts_the_schedule_on_resume() {
        let mut state = State::default();
        let a = state.put_rule(rule("a"), T0).unwrap_or(0);
        let b = state.put_rule(rule("b"), T0).unwrap_or(0);
        assert_eq!((a, b), (1, 2), "ids count up from 1");
        state.set_last_run(a, T0 + DAY);
        let mut paused = rule("a2");
        paused.id = a;
        paused.enabled = false;
        assert!(
            state.put_rule(paused.clone(), T0 + 2 * DAY).is_ok(),
            "pause"
        );
        let info = state.entry(a).map(|e| State::info(e, ZONE));
        assert!(
            info.is_some_and(|i| i.next_run.is_none()
                && i.last_run == Some(T0 + DAY)
                && i.rule.name == "a2"),
            "a paused rule has no next run but keeps its last run"
        );
        paused.enabled = true;
        assert!(state.put_rule(paused, T0 + 30 * DAY).is_ok(), "resume");
        assert!(
            !state.due_rules(T0 + 30 * DAY, ZONE).contains(&a),
            "resuming does not fire the rule for the interval that passed while paused"
        );
        assert!(
            state.entry(a).is_some_and(|e| e.anchor == T0 + 30 * DAY),
            "the schedule restarted at the resume"
        );
        assert_eq!(
            state.due_rules(T0 + 31 * DAY, ZONE),
            vec![a, b],
            "both due once their hour passed"
        );
    }

    #[test]
    fn one_unfinished_run_per_rule_and_history_prunes_only_finished_runs() {
        let mut state = State::default();
        let id = state.put_rule(rule("a"), T0).unwrap_or(0);
        let first = state.begin_run(id, T0 + 10 * HOUR);
        assert!(first.is_ok(), "first run starts");
        assert_eq!(
            code(&state.begin_run(id, T0 + 11 * HOUR)),
            Some(ErrorCode::BadRequest),
            "a second run while one is unfinished"
        );
        assert!(
            state.due_rules(T0 + 900 * DAY, ZONE).is_empty(),
            "a rule with an unfinished run never fires"
        );
        let first = first.map_or(0, |r| r.id);
        // Fill the history with finished runs of another rule: the unfinished first run
        // is older than all of them and must survive.
        let other = state.put_rule(rule("b"), T0).unwrap_or(0);
        for i in 0..(MAX_RUNS + 20) {
            let at = T0 + 20 * HOUR + i64::try_from(i).unwrap_or(0);
            let run = state.begin_run(other, at).map_or(0, |r| r.id);
            if let Some(run) = state.run_mut(run) {
                run.state = RunState::Nothing;
            }
        }
        assert_eq!(state.runs.len(), MAX_RUNS, "history is bounded");
        assert!(
            state.run(first).is_some_and(|r| !r.state.is_finished()),
            "the old unfinished run is kept"
        );
        assert!(
            state
                .runs
                .windows(2)
                .all(|w| w.first().map(|r| r.id) < w.get(1).map(|r| r.id)),
            "the oldest finished runs went first"
        );
    }

    #[test]
    fn deleting_a_rule_skips_runs_that_wait_but_not_the_one_cleaning() {
        let mut state = State::default();
        let id = state.put_rule(rule("a"), T0).unwrap_or(0);
        let run = state.begin_run(id, T0).map_or(0, |r| r.id);
        if let Some(r) = state.run_mut(run) {
            r.state = RunState::Pending { until: None };
        }
        let skipped = state.delete_rule(id);
        assert!(
            skipped.is_ok_and(|runs| runs.len() == 1),
            "the pending run is skipped"
        );
        assert!(
            state.run(run).is_some_and(|r| r.state == RunState::Skipped),
            "and stays in the history"
        );

        let id = state.put_rule(rule("b"), T0).unwrap_or(0);
        let run = state.begin_run(id, T0).map_or(0, |r| r.id);
        if let Some(r) = state.run_mut(run) {
            r.state = RunState::Cleaning;
        }
        assert!(
            state.delete_rule(id).is_ok_and(|runs| runs.is_empty()),
            "cleaning runs are left alone"
        );
    }

    #[test]
    fn recovery_repairs_ids_and_interrupted_runs() {
        let mut state = State::default();
        let id = state.put_rule(rule("a"), T0).unwrap_or(0);
        let run = state.begin_run(id, T0).map_or(0, |r| r.id);
        if let Some(r) = state.run_mut(run) {
            r.state = RunState::Cleaning;
        }
        let mut reloaded = State {
            next_rule: 0,
            next_run: 0,
            ..state.clone()
        };
        if let Some(entry) = reloaded.rules.first_mut() {
            entry.rule.trigger = Trigger::Every { days: 0, hour: 3 };
        }
        reloaded.recover();
        assert_eq!(
            reloaded.put_rule(rule("b"), T0).unwrap_or(0),
            id + 1,
            "ids never repeat"
        );
        assert!(
            reloaded.entry(id).is_some_and(|e| !e.rule.enabled),
            "an invalid stored rule is paused"
        );
        assert!(
            reloaded
                .run(run)
                .is_some_and(|r| matches!(r.state, RunState::Failed { .. })),
            "a clean the old daemon never finished is a failure"
        );
    }
}
