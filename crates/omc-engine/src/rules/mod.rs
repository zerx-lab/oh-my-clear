//! Automation rules (ADR 0024): the rule store with its activity history (`rules.toml`, next
//! to `settings.toml`), the wall-clock scheduler and the run pipeline that drives the
//! existing scan and clean jobs.
//!
//! - `state`: the tables, validation and bookkeeping (pure, unit-tested);
//! - `schedule`: when a rule is next due (pure);
//! - `filter`: which scanned items a rule acts on (pure);
//! - `build_lock`: is a Cargo build running in a `target` folder;
//! - `runner`: `Engine` methods: request handlers, scheduler tick, run pipeline.
//!
//! Unattended removal is never broader than a manual clean: the clean is the ordinary clean
//! job over items of a scan job of the same daemon, so `Guard` and the stored delete method
//! apply unchanged. On top of that a run never takes items that need administrator rights
//! (no elevation prompt out of nowhere) or belong to a running app.

mod build_lock;
mod filter;
mod runner;
mod schedule;
mod state;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use omc_proto::jobs::{ItemId, JobId};
use omc_proto::rules::RunId;
use omc_proto::{ErrorCode, RpcError};
use parking_lot::Mutex;
use tokio::sync::{broadcast, watch};

pub(crate) use schedule::Zone;
use state::State;

use crate::settings::{keep_backup, write_atomic};
use crate::{Error, Result};

/// File name inside the config dir.
const FILE_NAME: &str = "rules.toml";
/// Prompt requests buffered for the daemon.
const PROMPTS: usize = 64;

/// Where the daemon keeps `rules.toml`: next to `settings.toml` (see
/// [`crate::default_settings_path`]).
pub fn default_rules_path() -> Option<PathBuf> {
    crate::settings::config_dir().map(|dir| dir.join(FILE_NAME))
}

/// A run waiting for the user, as the tray shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRun {
    /// The run.
    pub id: RunId,
    /// Its rule's name.
    pub rule_name: String,
    /// Number of items it would clean.
    pub item_count: u64,
    /// Their total size.
    pub bytes: u64,
}

/// What the daemon shell needs to know about automation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AutomationStatus {
    /// Runs waiting for a decision (asking now or snoozed), oldest first.
    pub pending: Vec<PendingRun>,
    /// `true` while an enabled rule or an unfinished run exists: the daemon has to stay
    /// running even with no UI attached.
    pub active: bool,
}

impl AutomationStatus {
    fn of(state: &State) -> Self {
        Self {
            pending: state
                .runs
                .iter()
                .filter(|run| run.state.awaits_user())
                .map(|run| PendingRun {
                    id: run.id,
                    rule_name: run.rule_name.clone(),
                    item_count: run.item_count,
                    bytes: run.bytes,
                })
                .collect(),
            active: state.rules.iter().any(|e| e.rule.enabled)
                || state.runs.iter().any(|run| !run.state.is_finished()),
        }
    }
}

/// What the daemon keeps in memory for an unfinished run (lost with the daemon: a pending
/// run whose scan is gone is rescanned when the user says "clean now").
#[derive(Debug, Default)]
pub(crate) struct RunCtx {
    /// The retained scan job whose items the run cleans, or the scan still running.
    pub(crate) scan_job: Option<JobId>,
    /// The items to clean (ids inside `scan_job`).
    pub(crate) kept: Vec<ItemId>,
    /// Folders named `target` among them, probed for a running Cargo build.
    pub(crate) cargo_targets: Vec<PathBuf>,
}

/// The rule store plus everything the scheduler and the pipeline share.
#[derive(Debug)]
pub(crate) struct Automation {
    /// `None`: in memory only (tests).
    path: Option<PathBuf>,
    pub(crate) zone: Zone,
    state: Mutex<State>,
    /// Serializes writers so the file and `state` always agree.
    writing: Mutex<()>,
    ctx: Mutex<HashMap<RunId, RunCtx>>,
    prompts: broadcast::Sender<RunId>,
    status: watch::Sender<AutomationStatus>,
    /// The scheduler task was started.
    pub(crate) started: AtomicBool,
}

impl Automation {
    /// Loads `path` (blocking): a missing file is an empty store; an unparsable one is
    /// logged, renamed to `<path>.bak` and replaced by an empty store.
    pub(crate) fn load(path: Option<PathBuf>, zone: Zone) -> Self {
        let state = path.as_deref().map(read_file).unwrap_or_default();
        Self {
            path,
            zone,
            status: watch::Sender::new(AutomationStatus::of(&state)),
            state: Mutex::new(state),
            writing: Mutex::new(()),
            ctx: Mutex::new(HashMap::new()),
            prompts: broadcast::Sender::new(PROMPTS),
            started: AtomicBool::new(false),
        }
    }

    /// Reads the state (memory only; safe on any task).
    pub(crate) fn read<R>(&self, f: impl FnOnce(&State) -> R) -> R {
        f(&self.state.lock())
    }

    /// Applies `f` and persists the result. On a failed write the change is undone and the
    /// error returned, as does an `Err` from `f` (nothing is written then). Blocking.
    pub(crate) fn apply_user<R>(
        &self,
        f: impl FnOnce(&mut State) -> Result<R, RpcError>,
    ) -> Result<R, RpcError> {
        let _writer = self.writing.lock();
        let mut state = self.state.lock();
        let backup = state.clone();
        let out = f(&mut state)?;
        if let Err(err) = self.write(&state) {
            tracing::warn!(%err, "cannot save rules");
            *state = backup;
            return Err(RpcError::new(ErrorCode::Io, err.to_string()));
        }
        self.publish(&state);
        Ok(out)
    }

    /// Applies `f`; when it returns `Some` (it changed something) persists the result. A
    /// failed write is logged and the change stays in memory (a run must not lose a state
    /// change over a full disk). Blocking.
    pub(crate) fn apply<R>(&self, f: impl FnOnce(&mut State) -> Option<R>) -> Option<R> {
        let _writer = self.writing.lock();
        let mut state = self.state.lock();
        let out = f(&mut state)?;
        if let Err(err) = self.write(&state) {
            tracing::warn!(%err, "cannot save rules; the change stays in memory");
        }
        self.publish(&state);
        Some(out)
    }

    fn write(&self, state: &State) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let text = toml::to_string_pretty(state).map_err(Error::RulesEncode)?;
        write_atomic(path, &text)
    }

    fn publish(&self, state: &State) {
        let status = AutomationStatus::of(state);
        self.status.send_if_modified(|current| {
            let changed = *current != status;
            if changed {
                *current = status;
            }
            changed
        });
    }

    pub(crate) fn status(&self) -> watch::Receiver<AutomationStatus> {
        self.status.subscribe()
    }

    pub(crate) fn prompts(&self) -> broadcast::Receiver<RunId> {
        self.prompts.subscribe()
    }

    /// Asks the daemon to prompt for `run`.
    pub(crate) fn request_prompt(&self, run: RunId) {
        if self.prompts.send(run).is_err() {
            tracing::debug!(run, "nobody listens for prompts");
        }
    }

    pub(crate) fn set_ctx(&self, run: RunId, edit: impl FnOnce(&mut RunCtx)) {
        edit(self.ctx.lock().entry(run).or_default());
    }

    pub(crate) fn take_ctx(&self, run: RunId) -> Option<RunCtx> {
        self.ctx.lock().remove(&run)
    }

    /// The retained items of `run`: scan job, item ids and Cargo `target` folders.
    pub(crate) fn ctx_items(&self, run: RunId) -> Option<(JobId, Vec<ItemId>, Vec<PathBuf>)> {
        let ctx = self.ctx.lock();
        let ctx = ctx.get(&run)?;
        Some((ctx.scan_job?, ctx.kept.clone(), ctx.cargo_targets.clone()))
    }
}

/// Reads and repairs the rules file; any problem yields an empty store.
fn read_file(path: &Path) -> State {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!(path = %path.display(), "no rules file; starting empty");
            return State::default();
        }
        Err(err) if err.kind() == std::io::ErrorKind::InvalidData => {
            tracing::warn!(path = %path.display(), %err, "rules file is not UTF-8");
            keep_backup(path);
            return State::default();
        }
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "cannot read rules; starting empty");
            return State::default();
        }
    };
    match toml::from_str::<State>(&text) {
        Ok(mut state) => {
            state.recover();
            state
        }
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "corrupt rules file; starting empty");
            keep_backup(path);
            State::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use omc_proto::jobs::ScanArea;
    use omc_proto::junk::{Browser, JunkKind};
    use omc_proto::rules::{
        Confirm, Rule, RuleAction, RuleFilter, RuleRun, RuleScope, RunItem, RunState, Trigger,
    };

    use super::*;

    fn run(id: RunId, state: RunState) -> RuleRun {
        RuleRun {
            id,
            rule: 1,
            rule_name: "Rust targets".to_owned(),
            started: 1_767_225_600,
            state,
            items: vec![RunItem {
                name: "proj".to_owned(),
                location: "/work/proj/target".to_owned(),
                bytes: 5_000_000_000,
                modified: None,
            }],
            item_count: 1,
            bytes: 5_000_000_000,
        }
    }

    #[test]
    fn every_state_survives_a_toml_roundtrip() {
        let mut state = State::default();
        let rule = Rule {
            id: 0,
            name: "Dev junk".to_owned(),
            enabled: true,
            trigger: Trigger::Every { days: 14, hour: 10 },
            scope: RuleScope::Junk {
                area: ScanArea::DeveloperJunk,
                kinds: vec![JunkKind::ProjectArtifacts, JunkKind::Browser(Browser::Arc)],
            },
            filter: RuleFilter {
                idle_days: 14,
                min_bytes: 1 << 20,
                include_review: true,
            },
            action: RuleAction::Clean,
            confirm: Confirm::Ask,
        };
        assert!(state.put_rule(rule, 1_767_225_000).is_ok(), "rule accepted");
        let states = [
            RunState::Scanning,
            RunState::Pending { until: None },
            RunState::Pending {
                until: Some(1_767_229_200),
            },
            RunState::Deferred {
                until: 1_767_229_800,
            },
            RunState::Cleaning,
            RunState::Done {
                freed: 4_000_000_000,
                failed: 2,
            },
            RunState::Nothing,
            RunState::Skipped,
            RunState::Failed {
                message: "disk \"x\" gone".to_owned(),
            },
        ];
        for (id, run_state) in (1..).zip(states) {
            state.runs.push(run(id, run_state));
        }
        state.recover();
        let text = toml::to_string_pretty(&state);
        assert!(text.is_ok(), "encodes: {:?}", text.as_ref().err());
        let parsed = text
            .as_deref()
            .map(toml::from_str::<State>)
            .map_err(ToString::to_string);
        assert_eq!(
            parsed
                .map_err(|e| e.clone())
                .and_then(|r| r.map_err(|e| e.to_string())),
            Ok(state),
            "the file parses back to the same rules and history"
        );
    }
}
