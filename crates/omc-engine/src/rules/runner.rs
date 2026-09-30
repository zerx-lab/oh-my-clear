//! `Engine` side of automation: the request handlers, the wall-clock scheduler and the run
//! pipeline.
//!
//! A run moves through the states of [`RunState`] by *conditional transitions*
//! ([`Engine::transition`]): each names the state it leaves, so a deleted rule, a user
//! decision or a restart racing the pipeline can never be overwritten by it; a pipeline
//! step whose transition does not apply simply ends (and lets go of its jobs).
//!
//! ```text
//! fire ──► Scanning ──► (nothing kept) ──► Nothing
//!              │
//!              ├─ Ask ──► Pending ──CleanNow──► Cleaning ──► Done | Failed
//!              │            ├─ Snooze ──► Pending{until} ──(scheduler)──► Pending
//!              │            └─ Skip ──► Skipped
//!              └─ Auto ─────────────────────────► Cleaning
//!                            Cleaning ──(cargo build running)──► Deferred ──(retry)──► Cleaning
//! ```
//!
//! Scanning and removal are the ordinary scan and clean jobs (`jobs.rs`). The scan job of a
//! pending or deferred run is pinned in the job table; when it is gone (daemon restart,
//! eviction) a clean rescans first and cleans what that scan keeps.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

use omc_proto::jobs::{CleanSpec, JobId, JobOutput, JobSpec, JobState};
use omc_proto::junk::{JunkKind, JunkReport};
use omc_proto::rules::{
    Decision, Rule, RuleId, RuleInfo, RuleRun, RuleScope, RunId, RunItem, RunState, Timestamp,
};
use omc_proto::{ErrorCode, Event, RpcError};
use tokio::sync::{broadcast, watch};

use super::state::{RUN_ITEMS, State};
use super::{Automation, AutomationStatus, build_lock, filter};
use crate::Engine;

/// How often the scheduler looks at the clock. Wall-clock time is compared at every look,
/// so a machine that slept fires its due rules at the first look after waking.
const TICK: Duration = Duration::from_secs(60);
/// How long a run waits when a build holds its `target` folder.
const DEFER_SECS: i64 = 600;
/// Snooze bounds in minutes.
const SNOOZE_MINUTES: std::ops::RangeInclusive<u32> = 1..=1440;

fn bad_request(message: impl Into<String>) -> RpcError {
    RpcError::new(ErrorCode::BadRequest, message)
}

fn not_found(message: impl Into<String>) -> RpcError {
    RpcError::new(ErrorCode::NotFound, message)
}

fn internal(message: impl Into<String>) -> RpcError {
    RpcError::new(ErrorCode::Internal, message)
}

/// Unix seconds now.
fn now() -> Timestamp {
    omc_scan::paths::now_secs()
}

/// What a run does after a scan finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum After {
    /// Ask (or clean at once for `Confirm::Auto`).
    Confirm,
    /// Clean whatever the scan keeps: the user (or the retry) already decided.
    Clean,
}

/// The next step of a run's pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Scan(After),
    Clean,
    Stop,
}

impl Engine {
    /// Starts the scheduler (idempotent). It resumes runs the previous daemon left
    /// unfinished, prompts again for those asking, then checks the clock every minute until
    /// shutdown. Call once the daemon shell listens to [`Self::subscribe_prompts`].
    pub fn start_automation(&self) {
        if self.automation().started.swap(true, Ordering::AcqRel) {
            return;
        }
        let engine = self.clone();
        tokio::spawn(async move {
            engine.resume();
            let mut tick = tokio::time::interval(TICK);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    () = engine.shutdown_requested() => return,
                    _ = tick.tick() => engine.tick(now()).await,
                }
            }
        });
    }

    /// Run ids that need the user's attention now (a run fired with `Confirm::Ask`, or its
    /// snooze ended, or the daemon restarted with it pending). The daemon answers each with
    /// `Event::Prompt` for attached UIs, or launches one. Subscribe before
    /// [`Self::start_automation`]; only later ids are delivered.
    pub fn subscribe_prompts(&self) -> broadcast::Receiver<RunId> {
        self.automation().prompts()
    }

    /// Pending runs and whether automation needs the daemon to stay resident; updated on
    /// every change.
    pub fn automation_status(&self) -> watch::Receiver<AutomationStatus> {
        self.automation().status()
    }

    // ---- requests ---------------------------------------------------------------------

    pub(crate) fn list_rules(&self) -> Vec<RuleInfo> {
        let a = self.automation();
        a.read(|s| s.infos(a.zone))
    }

    pub(crate) async fn put_rule(&self, rule: Rule) -> Result<RuleInfo, RpcError> {
        let at = now();
        let info = self
            .blocking(move |a| {
                let zone = a.zone;
                a.apply_user(|s| {
                    let id = s.put_rule(rule, at)?;
                    s.entry(id)
                        .map(|entry| State::info(entry, zone))
                        .ok_or_else(|| internal("the stored rule vanished"))
                })
            })
            .await??;
        tracing::info!(rule = info.rule.id, "rule saved");
        self.notify_ui(Event::RulesChanged);
        Ok(info)
    }

    pub(crate) async fn delete_rule(&self, id: RuleId) -> Result<(), RpcError> {
        let skipped = self
            .blocking(move |a| a.apply_user(|s| s.delete_rule(id)))
            .await??;
        tracing::info!(rule = id, skipped = skipped.len(), "rule deleted");
        for run in skipped {
            self.abandon(run.id);
            self.emit(&run);
        }
        self.notify_ui(Event::RulesChanged);
        Ok(())
    }

    /// Fires `id` now, off schedule.
    pub(crate) async fn run_rule(&self, id: RuleId) -> Result<RuleRun, RpcError> {
        self.fire(id, now()).await
    }

    pub(crate) fn list_runs(&self) -> Vec<RuleRun> {
        self.automation()
            .read(|s| s.runs.iter().rev().cloned().collect())
    }

    pub(crate) fn get_run(&self, id: RunId) -> Result<RuleRun, RpcError> {
        self.automation()
            .read(|s| s.run(id).cloned())
            .ok_or_else(|| not_found(format!("no run {id}")))
    }

    pub(crate) async fn decide_run(&self, id: RunId, decision: Decision) -> Result<(), RpcError> {
        if let Decision::Snooze { minutes } = decision
            && !SNOOZE_MINUTES.contains(&minutes)
        {
            return Err(bad_request(format!(
                "a snooze is {}..={} minutes, got {minutes}",
                SNOOZE_MINUTES.start(),
                SNOOZE_MINUTES.end()
            )));
        }
        let at = now();
        let run = self
            .blocking(move |a| {
                a.apply_user(|s| {
                    let run = s.run(id).ok_or_else(|| not_found(format!("no run {id}")))?;
                    if !run.state.awaits_user() {
                        return Err(bad_request(format!(
                            "run {id} is not waiting for a decision"
                        )));
                    }
                    let (rule, started) = (run.rule, run.started);
                    let next = match decision {
                        Decision::CleanNow => RunState::Cleaning,
                        Decision::Snooze { minutes } => RunState::Pending {
                            until: Some(at.saturating_add(i64::from(minutes).saturating_mul(60))),
                        },
                        Decision::Skip => {
                            s.set_last_run(rule, started);
                            RunState::Skipped
                        }
                    };
                    let run = s.run_mut(id).ok_or_else(|| internal("the run vanished"))?;
                    run.state = next;
                    Ok(run.clone())
                })
            })
            .await??;
        tracing::info!(run = id, ?decision, "run decided");
        self.emit(&run);
        match decision {
            Decision::CleanNow => self.spawn_drive(id, Step::Clean),
            Decision::Snooze { .. } => {}
            Decision::Skip => {
                self.abandon(id);
                self.notify_ui(Event::RulesChanged);
            }
        }
        Ok(())
    }

    // ---- scheduler --------------------------------------------------------------------

    /// One look at the clock: ends snoozes (asking again), retries deferred runs, fires due
    /// rules.
    pub(crate) async fn tick(&self, at: Timestamp) {
        let a = self.automation();
        let (rules, snoozed, deferred) = a.read(|s| {
            (
                s.due_rules(at, a.zone),
                s.snoozes_over(at),
                s.deferrals_over(at),
            )
        });
        for run in snoozed {
            let ended = self
                .transition(
                    run,
                    move |st| matches!(st, RunState::Pending { until: Some(t) } if at >= *t),
                    |r| r.state = RunState::Pending { until: None },
                )
                .await;
            if let Some(run) = ended {
                a.request_prompt(run.id);
            }
        }
        for run in deferred {
            let retry = self
                .transition(
                    run,
                    move |st| matches!(st, RunState::Deferred { until } if at >= *until),
                    |r| r.state = RunState::Cleaning,
                )
                .await;
            if let Some(run) = retry {
                self.spawn_drive(run.id, Step::Clean);
            }
        }
        for rule in rules {
            if let Err(err) = self.fire(rule, at).await {
                tracing::warn!(rule, %err, "scheduled run did not start");
            }
        }
    }

    /// Resumes what a previous daemon left: scans that never finished start over, runs that
    /// were asking ask again.
    fn resume(&self) {
        let a = self.automation();
        let (scanning, asking) = a.read(|s| {
            let ids = |pred: fn(&RunState) -> bool| {
                s.runs
                    .iter()
                    .filter(|r| pred(&r.state))
                    .map(|r| r.id)
                    .collect::<Vec<_>>()
            };
            (
                ids(|st| *st == RunState::Scanning),
                ids(|st| *st == RunState::Pending { until: None }),
            )
        });
        for run in scanning {
            tracing::info!(run, "resuming an interrupted scan");
            self.spawn_drive(run, Step::Scan(After::Confirm));
        }
        for run in asking {
            a.request_prompt(run);
        }
    }

    /// Creates a run of `rule` and starts its scan.
    async fn fire(&self, rule: RuleId, at: Timestamp) -> Result<RuleRun, RpcError> {
        let run = self
            .blocking(move |a| a.apply_user(|s| s.begin_run(rule, at)))
            .await??;
        tracing::info!(rule, run = run.id, "rule fired");
        self.notify_ui(Event::RulesChanged);
        self.emit(&run);
        self.spawn_drive(run.id, Step::Scan(After::Confirm));
        Ok(run)
    }

    // ---- pipeline ---------------------------------------------------------------------

    fn spawn_drive(&self, run: RunId, first: Step) {
        let engine = self.clone();
        tokio::spawn(async move {
            let mut step = first;
            loop {
                step = match step {
                    Step::Scan(after) => engine.scan_phase(run, after).await,
                    Step::Clean => engine.clean_phase(run).await,
                    Step::Stop => return,
                };
            }
        });
    }

    /// Scans the run's scope and filters the result; then asks, cleans or ends.
    async fn scan_phase(&self, run: RunId, after: After) -> Step {
        let a = self.automation();
        let Some(rule) = a.read(|s| {
            s.run(run)
                .and_then(|r| s.entry(r.rule))
                .map(|e| e.rule.clone())
        }) else {
            self.finish(
                run,
                RunState::Failed {
                    message: "its rule no longer exists".to_owned(),
                },
            )
            .await;
            return Step::Stop;
        };
        let RuleScope::Junk { area, kinds } = rule.scope.clone();
        let job = match self.start_job(JobSpec::Scan(area)).await {
            Ok(job) => job,
            Err(err) => {
                self.finish(
                    run,
                    RunState::Failed {
                        message: err.message,
                    },
                )
                .await;
                return Step::Stop;
            }
        };
        a.set_ctx(run, |c| c.scan_job = Some(job));
        // A delete or skip may have ended the run before the job existed to be cancelled.
        if a.read(|s| s.run(run).is_none_or(|r| r.state.is_finished())) {
            self.abandon(run);
            return Step::Stop;
        }
        let Some(report) = self.scan_report(run, job).await else {
            return Step::Stop;
        };
        let kept = filter::apply(&report, &kinds, &rule.filter, now());
        drop(report);
        if kept.is_empty() {
            self.finish(run, RunState::Nothing).await;
            return Step::Stop;
        }
        self.jobs().pin(job);
        let targets: Vec<PathBuf> = kept
            .iter()
            .filter(|k| k.kind == JunkKind::ProjectArtifacts)
            .filter_map(|k| k.path.as_deref().map(PathBuf::from))
            .filter(|path| build_lock::is_target_dir(path))
            .collect();
        let ids = kept.iter().map(|k| k.id).collect();
        a.set_ctx(run, |c| {
            c.scan_job = Some(job);
            c.kept = ids;
            c.cargo_targets = targets;
        });
        let count = u64::try_from(kept.len()).unwrap_or(u64::MAX);
        let bytes = kept
            .iter()
            .fold(0_u64, |sum, k| sum.saturating_add(k.item.bytes));
        let items: Vec<RunItem> = kept.into_iter().take(RUN_ITEMS).map(|k| k.item).collect();
        let asks = after == After::Confirm && rule.confirm == omc_proto::rules::Confirm::Ask;
        let next = if asks {
            RunState::Pending { until: None }
        } else {
            RunState::Cleaning
        };
        let moved = self
            .transition(
                run,
                |st| *st == RunState::Scanning,
                move |r| {
                    r.items = items;
                    r.item_count = count;
                    r.bytes = bytes;
                    r.state = next;
                },
            )
            .await;
        match moved {
            None => {
                self.abandon(run);
                Step::Stop
            }
            Some(_) if asks => {
                a.request_prompt(run);
                Step::Stop
            }
            Some(_) => Step::Clean,
        }
    }

    /// The report of the finished scan `job`; ends the run as failed and gives `None` when
    /// the scan produced none.
    async fn scan_report(&self, run: RunId, job: JobId) -> Option<JunkReport> {
        let state = self.jobs().wait_finished(job).await;
        let message = match (state, self.jobs().result(job)) {
            (Ok(JobState::Done), Ok(JobOutput::Junk(report))) => return Some(report),
            (Ok(JobState::Failed { message }), _) => message,
            (state, output) => {
                tracing::warn!(
                    run,
                    ?state,
                    output_ok = output.is_ok(),
                    "scan gave no report"
                );
                "the scan did not finish".to_owned()
            }
        };
        self.finish(run, RunState::Failed { message }).await;
        None
    }

    /// Removes the items of a run that is `Cleaning`: waits out a running Cargo build,
    /// rescans when the scan job is gone, otherwise runs the clean job.
    async fn clean_phase(&self, run: RunId) -> Step {
        let a = self.automation();
        let retained = a
            .ctx_items(run)
            .filter(|(scan_job, ..)| self.jobs().status(*scan_job).is_ok());
        let Some((scan_job, items, targets)) = retained else {
            return self.rescan(run).await;
        };
        if !targets.is_empty() {
            let probe = tokio::task::spawn_blocking(move || {
                build_lock::held(targets.iter().map(PathBuf::as_path))
                    .map(std::path::Path::to_path_buf)
            })
            .await;
            let busy = match probe {
                Ok(busy) => busy,
                Err(err) => {
                    tracing::warn!(run, %err, "build lock probe failed; deferring");
                    Some(PathBuf::new())
                }
            };
            if let Some(target) = busy {
                let until = now().saturating_add(DEFER_SECS);
                tracing::info!(run, target = %target.display(), until, "a build holds the target; deferring");
                self.transition(
                    run,
                    |st| *st == RunState::Cleaning,
                    move |r| r.state = RunState::Deferred { until },
                )
                .await;
                return Step::Stop;
            }
        }
        let job = match self
            .start_job(JobSpec::Clean(CleanSpec { scan_job, items }))
            .await
        {
            Ok(job) => job,
            Err(err) if err.code == ErrorCode::NotFound => return self.rescan(run).await,
            Err(err) => {
                self.finish(
                    run,
                    RunState::Failed {
                        message: err.message,
                    },
                )
                .await;
                return Step::Stop;
            }
        };
        let end = match (
            self.jobs().wait_finished(job).await,
            self.jobs().result(job),
        ) {
            (Ok(JobState::Done), Ok(JobOutput::Clean(report))) => RunState::Done {
                freed: report.freed,
                failed: u64::try_from(report.failures.len()).unwrap_or(u64::MAX),
            },
            (Ok(JobState::Failed { message }), _) => RunState::Failed { message },
            (state, _) => RunState::Failed {
                message: format!("the clean did not finish ({state:?})"),
            },
        };
        self.jobs().discard(job);
        self.finish(run, end).await;
        Step::Stop
    }

    /// The scan a clean needs is gone: back to `Scanning`, then clean what it keeps.
    async fn rescan(&self, run: RunId) -> Step {
        tracing::info!(run, "the scan is gone; scanning again before cleaning");
        self.abandon(run);
        let moved = self
            .transition(
                run,
                |st| *st == RunState::Cleaning,
                |r| r.state = RunState::Scanning,
            )
            .await;
        if moved.is_some() {
            Step::Scan(After::Clean)
        } else {
            Step::Stop
        }
    }

    // ---- helpers ----------------------------------------------------------------------

    /// Ends an unfinished run in `end` and lets go of its jobs.
    async fn finish(&self, run: RunId, end: RunState) {
        self.transition(run, |st| !st.is_finished(), move |r| r.state = end)
            .await;
        self.abandon(run);
    }

    /// Drops what the daemon holds for `run`: its scan job (cancelled when still running).
    fn abandon(&self, run: RunId) {
        if let Some(job) = self.automation().take_ctx(run).and_then(|c| c.scan_job) {
            self.jobs().discard(job);
        }
    }

    /// Moves `id` to the state `edit` sets, only when its current state passes `from`;
    /// persists and announces the run. `None` when it did not apply.
    async fn transition(
        &self,
        id: RunId,
        from: impl FnOnce(&RunState) -> bool + Send + 'static,
        edit: impl FnOnce(&mut RuleRun) + Send + 'static,
    ) -> Option<RuleRun> {
        let moved = self
            .blocking(move |a| {
                a.apply(|s| {
                    let run = s.run_mut(id)?;
                    if !from(&run.state) {
                        return None;
                    }
                    edit(run);
                    let run = run.clone();
                    s.prune();
                    Some(run)
                })
            })
            .await;
        match moved {
            Ok(Some(run)) => {
                tracing::debug!(run = run.id, state = ?run.state, "run state changed");
                self.emit(&run);
                Some(run)
            }
            Ok(None) => None,
            Err(err) => {
                tracing::warn!(run = id, %err, "cannot update the run");
                None
            }
        }
    }

    fn emit(&self, run: &RuleRun) {
        self.notify_ui(Event::Run(run.clone()));
    }

    /// Runs `f` on a blocking thread (the store writes its file).
    async fn blocking<R: Send + 'static>(
        &self,
        f: impl FnOnce(&Automation) -> R + Send + 'static,
    ) -> Result<R, RpcError> {
        let engine = self.clone();
        tokio::task::spawn_blocking(move || f(engine.automation()))
            .await
            .map_err(|err| internal(format!("rules: {err}")))
    }
}
