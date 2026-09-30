//! Client-side store of the daemon's automation rules and their runs (ADR 0024): ONE
//! [`Rules`] entity in a GPUI global that the sidebar badges, the Rules and Activity pages,
//! the overview dashboard, the command palette and the prompt windows all read.
//!
//! - Refreshed on every (re)connect, so a new daemon epoch replaces everything; the daemon
//!   pushes `rules_changed` (re-list the rules) and `run` (upsert one run).
//! - Mutations ([`Rules::save`], [`Rules::delete`], [`Rules::run_now`], [`Rules::decide`])
//!   are requests; the store applies the answer and reports failures through
//!   [`Rules::error`], never behind the user's back.
//! - [`RulesModel`] is the pure part (ordering, counts, "next / last run"), testable without
//!   a daemon.

use std::cmp::Reverse;

use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, Global, SharedString, Subscription, Task,
};
use omc_ipc::client::ClientEvent;
use omc_proto::Event;
use omc_proto::rules::{Decision, Rule, RuleId, RuleInfo, RuleRun, RunId, RunState};

use crate::engine;
use crate::jobs::{self, Failure};
use crate::pages::widgets::Connection;

/// Runs the store keeps (the daemon keeps a bounded tail too; this bounds live upserts).
const MAX_RUNS: usize = 200;

/// Rules and runs as the daemon last reported them.
#[derive(Debug, Default, Clone)]
pub(crate) struct RulesModel {
    rules: Vec<RuleInfo>,
    runs: Vec<RuleRun>,
}

impl RulesModel {
    /// Rules by name (case-insensitive), then id.
    pub(crate) fn rules(&self) -> &[RuleInfo] {
        &self.rules
    }

    /// Runs, newest first.
    pub(crate) fn runs(&self) -> &[RuleRun] {
        &self.runs
    }

    /// The rule `id`.
    pub(crate) fn rule(&self, id: RuleId) -> Option<&RuleInfo> {
        self.rules.iter().find(|info| info.rule.id == id)
    }

    /// The run `id`.
    pub(crate) fn run(&self, id: RunId) -> Option<&RuleRun> {
        self.runs.iter().find(|run| run.id == id)
    }

    /// Replaces every rule.
    pub(crate) fn replace_rules(&mut self, rules: Vec<RuleInfo>) {
        self.rules = rules;
        self.sort_rules();
    }

    /// Adds `info` or replaces the rule with its id.
    pub(crate) fn upsert_rule(&mut self, info: RuleInfo) {
        match self.rules.iter_mut().find(|r| r.rule.id == info.rule.id) {
            Some(slot) => *slot = info,
            None => self.rules.push(info),
        }
        self.sort_rules();
    }

    /// Forgets the rule `id` (its runs stay: the history outlives the rule).
    pub(crate) fn remove_rule(&mut self, id: RuleId) {
        self.rules.retain(|info| info.rule.id != id);
    }

    /// Replaces every run.
    pub(crate) fn replace_runs(&mut self, runs: Vec<RuleRun>) {
        self.runs = runs;
        self.runs.dedup_by_key(|run| run.id);
        self.sort_runs();
    }

    /// Adds `run` or replaces the run with its id.
    pub(crate) fn upsert_run(&mut self, run: RuleRun) {
        match self.runs.iter_mut().find(|r| r.id == run.id) {
            Some(slot) => *slot = run,
            None => self.runs.push(run),
        }
        self.sort_runs();
        self.runs.truncate(MAX_RUNS);
    }

    /// Runs waiting for the user: those asking now first, then the snoozed ones by the time
    /// they ask again; equal times newest first.
    pub(crate) fn pending(&self) -> Vec<&RuleRun> {
        let mut pending: Vec<&RuleRun> = self
            .runs
            .iter()
            .filter(|run| run.state.awaits_user())
            .collect();
        pending.sort_by_key(|run| {
            let until = match run.state {
                RunState::Pending { until } => until,
                _ => None,
            };
            (
                until.is_some(),
                until,
                Reverse(run.started),
                Reverse(run.id),
            )
        });
        pending
    }

    /// How many runs wait for the user.
    pub(crate) fn pending_count(&self) -> usize {
        self.runs
            .iter()
            .filter(|run| run.state.awaits_user())
            .count()
    }

    /// How many rules are switched on.
    pub(crate) fn enabled_count(&self) -> usize {
        self.rules.iter().filter(|info| info.rule.enabled).count()
    }

    /// The enabled rule that fires first.
    pub(crate) fn next_scheduled(&self) -> Option<&RuleInfo> {
        self.rules
            .iter()
            .filter(|info| info.rule.enabled)
            .filter_map(|info| Some((info.next_run?, info)))
            .min_by_key(|(at, info)| (*at, info.rule.id))
            .map(|(_, info)| info)
    }

    /// The newest run that has ended.
    pub(crate) fn last_finished(&self) -> Option<&RuleRun> {
        self.runs.iter().find(|run| run.state.is_finished())
    }

    fn sort_rules(&mut self) {
        self.rules
            .sort_by_cached_key(|info| (info.rule.name.to_lowercase(), info.rule.id));
    }

    fn sort_runs(&mut self) {
        self.runs
            .sort_by_key(|run| (Reverse(run.started), Reverse(run.id)));
    }
}

/// A change other views react to beyond a plain re-render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RulesEvent {
    /// Rules and runs were fetched (after a connect).
    Loaded,
    /// A rule was stored (created or replaced).
    Saved(RuleId),
    /// A rule was deleted.
    Deleted(RuleId),
    /// The daemon accepted the user's answer to a pending run.
    Decided(RunId),
    /// [`Rules::load_run`] found no such run (the daemon forgot it, or was unreachable).
    RunMissing(RunId),
}

/// The shared store.
pub(crate) struct Rules {
    model: RulesModel,
    conn: Connection,
    loaded: bool,
    error: Option<SharedString>,
    fetching: Option<Task<()>>,
    relisting: Option<Task<()>>,
    _subscription: Subscription,
}

impl std::fmt::Debug for Rules {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rules")
            .field("loaded", &self.loaded)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<RulesEvent> for Rules {}

impl Rules {
    fn new(cx: &mut Context<'_, Self>) -> Self {
        let engine = engine::entity(cx);
        let subscription = cx.subscribe(&engine, |this, _, event: &ClientEvent, cx| {
            this.on_engine(event, cx);
        });
        let mut this = Self {
            model: RulesModel::default(),
            conn: Connection::new(cx),
            loaded: false,
            error: None,
            fetching: None,
            relisting: None,
            _subscription: subscription,
        };
        if this.conn.is_connected() {
            this.refresh(cx);
        }
        this
    }

    /// The rules and runs.
    pub(crate) fn model(&self) -> &RulesModel {
        &self.model
    }

    /// `true` while requests can be sent.
    pub(crate) fn is_connected(&self) -> bool {
        self.conn.is_connected()
    }

    /// `true` once the daemon's rules arrived at least once.
    pub(crate) fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// The last failed request, until the next success or [`Self::dismiss_error`].
    pub(crate) fn error(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }

    /// Hides the shown error.
    pub(crate) fn dismiss_error(&mut self, cx: &mut Context<'_, Self>) {
        if self.error.take().is_some() {
            cx.notify();
        }
    }

    fn on_engine(&mut self, event: &ClientEvent, cx: &mut Context<'_, Self>) {
        match event {
            ClientEvent::State(_) => {
                self.conn.observe(event);
                if self.conn.is_connected() {
                    self.refresh(cx);
                }
                cx.notify();
            }
            ClientEvent::Daemon(Event::RulesChanged) => self.refresh_rules(cx),
            ClientEvent::Daemon(Event::Run(run)) => {
                self.model.upsert_run(run.clone());
                cx.notify();
            }
            ClientEvent::Daemon(_) => {}
        }
    }

    /// Fetches rules and runs.
    pub(crate) fn refresh(&mut self, cx: &mut Context<'_, Self>) {
        let rules = jobs::list_rules(cx);
        let runs = jobs::list_runs(cx);
        self.fetching = Some(cx.spawn(async move |this, cx| {
            let (rules, runs) = (rules.await, runs.await);
            let applied = this.update(cx, |this, cx| this.fetched(rules, runs, cx));
            if let Err(err) = applied {
                tracing::debug!("rules store is gone: {err:#}");
            }
        }));
    }

    fn refresh_rules(&mut self, cx: &mut Context<'_, Self>) {
        let rules = jobs::list_rules(cx);
        self.relisting = Some(cx.spawn(async move |this, cx| {
            let rules = rules.await;
            let applied = this.update(cx, |this, cx| match rules {
                Ok(rules) => {
                    this.model.replace_rules(rules);
                    cx.notify();
                }
                Err(err) => this.fail(&err, cx),
            });
            if let Err(err) = applied {
                tracing::debug!("rules store is gone: {err:#}");
            }
        }));
    }

    fn fetched(
        &mut self,
        rules: Result<Vec<RuleInfo>, Failure>,
        runs: Result<Vec<RuleRun>, Failure>,
        cx: &mut Context<'_, Self>,
    ) {
        self.fetching = None;
        let mut ok = true;
        match rules {
            Ok(rules) => self.model.replace_rules(rules),
            Err(err) => {
                ok = false;
                self.fail(&err, cx);
            }
        }
        match runs {
            Ok(runs) => self.model.replace_runs(runs),
            Err(err) => {
                ok = false;
                self.fail(&err, cx);
            }
        }
        if ok {
            self.loaded = true;
            self.error = None;
            cx.emit(RulesEvent::Loaded);
        }
        cx.notify();
    }

    fn fail(&mut self, err: &Failure, cx: &mut Context<'_, Self>) {
        tracing::warn!("automation request failed: {err}");
        self.error = Some(err.clone().into());
        cx.notify();
    }

    /// Sends `answer` and applies its value; a failure becomes [`Self::error`].
    fn request<T: 'static>(
        answer: impl Future<Output = Result<T, Failure>> + 'static,
        apply: impl FnOnce(&mut Self, T, &mut Context<'_, Self>) + 'static,
        cx: &mut Context<'_, Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let result = answer.await;
            let applied = this.update(cx, |this, cx| match result {
                Ok(value) => {
                    this.error = None;
                    apply(this, value, cx);
                    cx.notify();
                }
                Err(err) => this.fail(&err, cx),
            });
            if let Err(err) = applied {
                tracing::debug!("rules store is gone: {err:#}");
            }
        })
        .detach();
    }

    /// Creates (`id == 0`) or replaces a rule. Run through the store entity:
    /// `rules.update(cx, |_, cx| Rules::save(rule, cx))`.
    pub(crate) fn save(rule: Rule, cx: &mut Context<'_, Self>) {
        let answer = jobs::put_rule(rule, cx);
        Self::request(
            answer,
            |this, info, cx| {
                let id = info.rule.id;
                this.model.upsert_rule(info);
                cx.emit(RulesEvent::Saved(id));
            },
            cx,
        );
    }

    /// Deletes a rule.
    pub(crate) fn delete(id: RuleId, cx: &mut Context<'_, Self>) {
        let answer = jobs::delete_rule(id, cx);
        Self::request(
            answer,
            move |this, (), cx| {
                this.model.remove_rule(id);
                cx.emit(RulesEvent::Deleted(id));
            },
            cx,
        );
    }

    /// Fires a rule now.
    pub(crate) fn run_now(id: RuleId, cx: &mut Context<'_, Self>) {
        let answer = jobs::run_rule(id, cx);
        Self::request(answer, |this, run, _| this.model.upsert_run(run), cx);
    }

    /// Answers a pending run; the daemon's `run` event carries the new state, and the run is
    /// fetched once more so the answer shows even if that event was missed.
    pub(crate) fn decide(run: RunId, decision: Decision, cx: &mut Context<'_, Self>) {
        let answer = jobs::decide_run(run, decision, cx);
        Self::request(
            answer,
            move |_, (), cx| {
                cx.emit(RulesEvent::Decided(run));
                let fetch = jobs::get_run(run, cx);
                Self::request(fetch, |this, run, _| this.model.upsert_run(run), cx);
            },
            cx,
        );
    }

    /// Fetches one run (a prompt window opens for runs the store may not hold yet). A
    /// failure emits [`RulesEvent::RunMissing`] instead of setting [`Self::error`].
    pub(crate) fn load_run(run: RunId, cx: &mut Context<'_, Self>) {
        let answer = jobs::get_run(run, cx);
        cx.spawn(async move |this, cx| {
            let result = answer.await;
            let applied = this.update(cx, |this, cx| {
                match result {
                    Ok(run) => this.model.upsert_run(run),
                    Err(err) => {
                        tracing::warn!(run, "cannot load the run: {err}");
                        cx.emit(RulesEvent::RunMissing(run));
                    }
                }
                cx.notify();
            });
            if let Err(err) = applied {
                tracing::debug!("rules store is gone: {err:#}");
            }
        })
        .detach();
    }
}

struct GlobalRules(Entity<Rules>);

impl Global for GlobalRules {}

/// The store, created on first use (after [`engine::start`]).
pub(crate) fn entity(cx: &mut App) -> Entity<Rules> {
    if let Some(global) = cx.try_global::<GlobalRules>() {
        return global.0.clone();
    }
    let rules = cx.new(Rules::new);
    cx.set_global(GlobalRules(rules.clone()));
    rules
}

#[cfg(test)]
mod tests {
    use omc_proto::jobs::ScanArea;
    use omc_proto::rules::{Confirm, RuleAction, RuleFilter, RuleScope, Trigger};

    use super::*;

    fn info(id: RuleId, name: &str, enabled: bool, next_run: Option<i64>) -> RuleInfo {
        RuleInfo {
            rule: Rule {
                id,
                name: name.to_owned(),
                enabled,
                trigger: Trigger::Every { days: 14, hour: 10 },
                scope: RuleScope::Junk {
                    area: ScanArea::DeveloperJunk,
                    kinds: Vec::new(),
                },
                filter: RuleFilter::default(),
                action: RuleAction::Clean,
                confirm: Confirm::Ask,
            },
            last_run: None,
            next_run,
        }
    }

    fn run(id: RunId, started: i64, state: RunState) -> RuleRun {
        RuleRun {
            id,
            rule: 1,
            rule_name: "r".to_owned(),
            started,
            state,
            items: Vec::new(),
            item_count: 0,
            bytes: 0,
        }
    }

    #[test]
    fn runs_are_newest_first_and_an_upsert_replaces_in_place() {
        let mut model = RulesModel::default();
        model.replace_runs(vec![
            run(1, 100, RunState::Scanning),
            run(3, 300, RunState::Nothing),
            run(2, 200, RunState::Nothing),
        ]);
        let ids: Vec<RunId> = model.runs().iter().map(|r| r.id).collect();
        assert_eq!(ids, [3, 2, 1], "newest first");

        model.upsert_run(run(1, 100, RunState::Pending { until: None }));
        assert_eq!(model.runs().len(), 3, "the same id is replaced, not added");
        assert_eq!(model.pending_count(), 1, "the update shows in the counts");

        model.upsert_run(run(4, 300, RunState::Scanning));
        let ids: Vec<RunId> = model.runs().iter().map(|r| r.id).collect();
        assert_eq!(ids, [4, 3, 2, 1], "equal start times: higher id first");
    }

    #[test]
    fn the_history_is_bounded() {
        let mut model = RulesModel::default();
        let total = u64::try_from(MAX_RUNS).unwrap_or(0).saturating_add(5);
        for id in 0..total {
            model.upsert_run(run(id, i64::try_from(id).unwrap_or(0), RunState::Nothing));
        }
        assert_eq!(model.runs().len(), MAX_RUNS, "the oldest runs are dropped");
        assert!(
            model.run(0).is_none() && model.run(total.saturating_sub(1)).is_some(),
            "the oldest went, the newest stayed"
        );
    }

    #[test]
    fn pending_asks_now_before_snoozed_and_finished_runs_never_count() {
        let mut model = RulesModel::default();
        model.replace_runs(vec![
            run(1, 100, RunState::Pending { until: Some(900) }),
            run(2, 200, RunState::Pending { until: Some(500) }),
            run(3, 300, RunState::Pending { until: None }),
            run(4, 400, RunState::Skipped),
            run(5, 500, RunState::Cleaning),
        ]);
        let ids: Vec<RunId> = model.pending().iter().map(|r| r.id).collect();
        assert_eq!(ids, [3, 2, 1], "asking now, then the earliest snooze end");
        assert_eq!(
            model.pending_count(),
            3,
            "skipped and running runs do not count"
        );
    }

    #[test]
    fn next_scheduled_ignores_paused_rules_and_last_finished_ignores_open_runs() {
        let mut model = RulesModel::default();
        model.replace_rules(vec![
            info(1, "b", true, Some(2_000)),
            info(2, "a", true, Some(1_000)),
            info(3, "c", false, Some(10)),
            info(4, "d", true, None),
        ]);
        assert_eq!(model.enabled_count(), 3, "paused rules are not counted");
        assert_eq!(
            model.next_scheduled().map(|i| i.rule.id),
            Some(2),
            "the earliest enabled rule with a schedule"
        );
        let names: Vec<&str> = model.rules().iter().map(|i| i.rule.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c", "d"], "rules are listed by name");

        model.replace_runs(vec![
            run(9, 900, RunState::Scanning),
            run(
                8,
                800,
                RunState::Done {
                    freed: 5,
                    failed: 0,
                },
            ),
            run(7, 700, RunState::Nothing),
        ]);
        assert_eq!(
            model.last_finished().map(|r| r.id),
            Some(8),
            "the newest run that ended"
        );

        model.remove_rule(2);
        assert_eq!(
            model.next_scheduled().map(|i| i.rule.id),
            Some(1),
            "a deleted rule no longer fires"
        );
    }

    #[gpui_kit::test]
    fn daemon_run_events_reach_the_store(cx: &mut gpui_kit::TestAppContext) {
        let init = cx.update(crate::init);
        assert!(init.is_ok(), "UI initialises headless: {init:?}");
        let (engine, rules) = cx.update(|cx| (engine::entity(cx), entity(cx)));
        cx.update(|cx| {
            engine.update(cx, |_, cx| {
                cx.emit(ClientEvent::Daemon(Event::Run(run(
                    7,
                    10,
                    RunState::Pending { until: None },
                ))));
            });
        });
        cx.run_until_parked();
        let pending = cx.update(|cx| rules.read(cx).model().pending_count());
        assert_eq!(pending, 1, "a run event becomes a pending run");
    }
}
