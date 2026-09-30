//! Automation rules over the wire: validation and persistence, the run state machine on a
//! temp developer-project fixture (`file_roots` points the scan at it; removal is permanent
//! and only ever touches the fixture), decisions, snooze prompts, Cargo build deferral,
//! restart recovery and the scheduler tick.

#![expect(
    clippy::arithmetic_side_effects,
    reason = "fixture timestamps and deadlines are small constants"
)]

use std::fs::{File, OpenOptions};

use omc_proto::jobs::ScanArea;
use omc_proto::junk::JunkKind;
use omc_proto::rules::{
    Confirm, Decision, Rule, RuleAction, RuleFilter, RuleInfo, RuleRun, RuleScope, RunState,
    Trigger,
};

use super::*;
use crate::rules::Zone;

const RUN_WAIT: Duration = Duration::from_secs(120);

/// Turns a valid rule into one that `put_rule` must reject.
type Break = fn(&mut Rule);

/// A developer project with a Cargo `target` in a temp folder.
struct Fixture {
    root: PathBuf,
    target: PathBuf,
}

fn fixture(name: &str) -> TestResult<Fixture> {
    let root = temp_dir(name)?;
    write_file(&root.join("tool").join("Cargo.toml"), 10)?;
    let target = root.join("tool").join("target");
    write_file(&target.join("debug").join("tool"), 70_000)?;
    Ok(Fixture { root, target })
}

/// Points the developer scan at the fixture only and makes removal permanent.
async fn scan_only(client: &mut Client, root: &Path) -> TestResult {
    let mut settings = Settings::default();
    settings.clean.files_delete = DeleteMethod::Permanent;
    settings.clean.junk_delete = DeleteMethod::Permanent;
    settings.clean.file_roots = vec![root.display().to_string()];
    settings.clean.dev_project_min_age_days = 0;
    // Keep the scan off the real home folder (slow, and none of it is ours) when the
    // fixture is not inside it.
    if let Some(home) = omc_scan::paths::home().filter(|home| !root.starts_with(home)) {
        settings.clean.exclude = vec![home.display().to_string()];
    }
    ensure_eq!(
        client.call(Request::PutSettings(settings)).await?,
        Ok(Response::Unit),
        "settings accepted"
    );
    Ok(())
}

fn dev_rule(name: &str, confirm: Confirm) -> Rule {
    Rule {
        id: 0,
        name: name.to_owned(),
        enabled: true,
        // 90 days: nothing here becomes due by a few minutes of test time.
        trigger: Trigger::Every { days: 90, hour: 3 },
        scope: RuleScope::Junk {
            area: ScanArea::DeveloperJunk,
            kinds: vec![JunkKind::ProjectArtifacts],
        },
        filter: RuleFilter {
            include_review: true,
            ..RuleFilter::default()
        },
        action: RuleAction::Clean,
        confirm,
    }
}

async fn put_rule(client: &mut Client, rule: Rule) -> TestResult<RuleInfo> {
    match client.call(Request::PutRule(rule)).await? {
        Ok(Response::Rule(info)) => Ok(info),
        other => Err(format!("put_rule answered {other:?}")),
    }
}

async fn run_rule(client: &mut Client, id: u64) -> TestResult<RuleRun> {
    match client.call(Request::RunRule { id }).await? {
        Ok(Response::Run(run)) => Ok(run),
        other => Err(format!("run_rule answered {other:?}")),
    }
}

async fn get_run(client: &mut Client, id: u64) -> TestResult<RuleRun> {
    match client.call(Request::GetRun { id }).await? {
        Ok(Response::Run(run)) => Ok(run),
        other => Err(format!("get_run answered {other:?}")),
    }
}

async fn list_runs(client: &mut Client) -> TestResult<Vec<RuleRun>> {
    match client.call(Request::ListRuns).await? {
        Ok(Response::Runs(runs)) => Ok(runs),
        other => Err(format!("list_runs answered {other:?}")),
    }
}

/// Polls until the run's state satisfies `want`.
async fn wait_run(
    client: &mut Client,
    id: u64,
    what: &str,
    want: fn(&RunState) -> bool,
) -> TestResult<RuleRun> {
    let deadline = tokio::time::Instant::now() + RUN_WAIT;
    loop {
        let run = get_run(client, id).await?;
        if want(&run.state) {
            return Ok(run);
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "run {id} never reached {what}; last state {:?}",
            run.state
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn is_pending_now(state: &RunState) -> bool {
    matches!(state, RunState::Pending { until: None })
}

fn is_done(state: &RunState) -> bool {
    matches!(state, RunState::Done { .. })
}

fn is_deferred(state: &RunState) -> bool {
    matches!(state, RunState::Deferred { .. })
}

fn now_secs() -> i64 {
    omc_scan::paths::now_secs()
}

async fn next_prompt(prompts: &mut broadcast::Receiver<u64>) -> TestResult<u64> {
    match tokio::time::timeout(WAIT, prompts.recv()).await {
        Ok(Ok(run)) => Ok(run),
        other => Err(format!("expected a prompt request, got {other:?}")),
    }
}

fn test_engine(rules: Option<PathBuf>) -> Engine {
    Engine::with_zone(
        EngineConfig {
            rules_path: rules,
            ..EngineConfig::default()
        },
        Zone::Fixed(0),
    )
}

#[tokio::test]
async fn rule_requests_validate_and_persist() -> TestResult {
    let dir = temp_dir("rules-store")?;
    let path = dir.join("nested").join("rules.toml");
    let engine = test_engine(Some(path.clone()));
    let mut ui = Client::connect(&engine, ClientKind::Ui).await?;

    let created = put_rule(&mut ui, dev_rule("Rust targets", Confirm::Ask)).await?;
    ensure_eq!(created.rule.id, 1, "the first rule gets id 1");
    ensure!(
        created.next_run.is_some_and(|next| next > now_secs() - 1) && created.last_run.is_none(),
        "a new rule is scheduled in the future and never ran: {created:?}"
    );
    ensure_eq!(
        ui.next_event().await?,
        Event::RulesChanged,
        "UIs hear about the new rule"
    );
    ensure!(path.is_file(), "rules.toml is written");

    let invalid: [(&str, Break); 6] = [
        ("blank name", |r| r.name = "  ".to_owned()),
        ("zero days", |r| {
            r.trigger = Trigger::Every { days: 0, hour: 3 }
        }),
        ("91 days", |r| {
            r.trigger = Trigger::Every { days: 91, hour: 3 }
        }),
        ("hour 24", |r| {
            r.trigger = Trigger::Every { days: 1, hour: 24 }
        }),
        ("files area", |r| {
            r.scope = RuleScope::Junk {
                area: ScanArea::LargeOldFiles,
                kinds: Vec::new(),
            };
        }),
        ("space lens", |r| {
            r.scope = RuleScope::Junk {
                area: ScanArea::SpaceLens {
                    root: "/".to_owned(),
                },
                kinds: Vec::new(),
            };
        }),
    ];
    for (what, edit) in invalid {
        let mut rule = dev_rule("bad", Confirm::Ask);
        edit(&mut rule);
        ensure_eq!(
            code(&ui.call(Request::PutRule(rule)).await?),
            Some(ErrorCode::BadRequest),
            "{what}"
        );
    }
    let mut ghost = dev_rule("ghost", Confirm::Ask);
    ghost.id = 77;
    ensure_eq!(
        code(&ui.call(Request::PutRule(ghost)).await?),
        Some(ErrorCode::NotFound),
        "replacing an unknown id"
    );
    ensure_eq!(
        code(&ui.call(Request::DeleteRule { id: 77 }).await?),
        Some(ErrorCode::NotFound),
        "deleting an unknown id"
    );
    ensure_eq!(
        code(&ui.call(Request::RunRule { id: 77 }).await?),
        Some(ErrorCode::NotFound),
        "running an unknown rule"
    );
    ensure_eq!(
        code(&ui.call(Request::GetRun { id: 5 }).await?),
        Some(ErrorCode::NotFound),
        "unknown run"
    );

    let mut paused = created.rule.clone();
    paused.enabled = false;
    paused.name = " Renamed ".to_owned();
    let replaced = put_rule(&mut ui, paused).await?;
    ensure!(
        replaced.rule.id == 1 && replaced.rule.name == "Renamed" && replaced.next_run.is_none(),
        "replace keeps the id, trims the name and a paused rule has no next run: {replaced:?}"
    );
    let second = put_rule(&mut ui, dev_rule("Second", Confirm::Auto)).await?;
    ensure_eq!(second.rule.id, 2, "ids count up");

    // A new engine on the same file sees the same rules and never reuses an id.
    let reloaded = test_engine(Some(path));
    let mut cli = Client::connect(&reloaded, ClientKind::Cli).await?;
    ensure_eq!(
        cli.call(Request::ListRules).await?,
        Ok(Response::Rules(vec![replaced, second])),
        "rules survive a restart"
    );
    ensure_eq!(
        cli.call(Request::DeleteRule { id: 2 }).await?,
        Ok(Response::Unit),
        "delete"
    );
    let third = put_rule(&mut cli, dev_rule("Third", Confirm::Ask)).await?;
    ensure_eq!(third.rule.id, 3, "a deleted rule's id is not reused");
    let _ignored = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[tokio::test]
async fn ask_run_waits_and_a_snooze_asks_again() -> TestResult {
    let fx = fixture("rules-ask")?;
    let engine = test_engine(None);
    let mut prompts = engine.subscribe_prompts();
    let mut status = engine.automation_status();
    let mut ui = Client::connect(&engine, ClientKind::Ui).await?;
    scan_only(&mut ui, &fx.root).await?;
    let rule = put_rule(&mut ui, dev_rule("Rust targets", Confirm::Ask)).await?;

    let started = run_rule(&mut ui, rule.rule.id).await?;
    ensure_eq!(started.state, RunState::Scanning, "a run starts scanning");
    ensure_eq!(
        code(&ui.call(Request::RunRule { id: rule.rule.id }).await?),
        Some(ErrorCode::BadRequest),
        "a rule with an unfinished run cannot run again"
    );
    let pending = wait_run(&mut ui, started.id, "pending", is_pending_now).await?;
    ensure_eq!(
        pending.item_count,
        1,
        "the fixture's target passes the filter"
    );
    ensure!(
        pending.bytes >= 70_000
            && pending
                .items
                .first()
                .is_some_and(|i| i.name == "tool" && i.location.ends_with("target")),
        "items carry the project and path: {pending:?}"
    );
    ensure_eq!(
        next_prompt(&mut prompts).await?,
        started.id,
        "the daemon is asked to prompt"
    );
    let seen = status.wait_for(|s| !s.pending.is_empty()).await;
    ensure!(
        seen.is_ok_and(|s| s.active && s.pending.first().is_some_and(|p| p.id == started.id)),
        "the status feed lists the pending run"
    );
    ensure!(fx.target.exists(), "nothing is removed while asking");

    // Snooze: still pending, silent until the snooze ends, then asks again.
    let before = now_secs();
    ensure_eq!(
        ui.call(Request::DecideRun {
            id: started.id,
            decision: Decision::Snooze { minutes: 5 },
        })
        .await?,
        Ok(Response::Unit),
        "snooze accepted"
    );
    let snoozed = get_run(&mut ui, started.id).await?;
    let RunState::Pending { until: Some(until) } = snoozed.state else {
        return Err(format!("snoozed run is {:?}", snoozed.state));
    };
    ensure!(
        until >= before + 300 && until <= now_secs() + 300,
        "snoozed for five minutes: until {until}, now {before}"
    );
    engine.tick(until - 1).await;
    ensure!(
        get_run(&mut ui, started.id).await?.state == RunState::Pending { until: Some(until) }
            && prompts.try_recv().is_err(),
        "no prompt before the snooze ends"
    );
    engine.tick(until).await;
    ensure_eq!(
        get_run(&mut ui, started.id).await?.state,
        RunState::Pending { until: None },
        "the snooze ended"
    );
    ensure_eq!(next_prompt(&mut prompts).await?, started.id, "asked again");

    ensure!(fx.target.exists(), "nothing is removed while snoozing");
    let _ignored = std::fs::remove_dir_all(&fx.root);
    Ok(())
}

#[tokio::test]
async fn clean_now_removes_the_items_and_finishes_the_run() -> TestResult {
    let fx = fixture("rules-ask")?;
    let engine = test_engine(None);
    let mut prompts = engine.subscribe_prompts();
    let mut status = engine.automation_status();
    let mut ui = Client::connect(&engine, ClientKind::Ui).await?;
    scan_only(&mut ui, &fx.root).await?;
    let rule = put_rule(&mut ui, dev_rule("Rust targets", Confirm::Ask)).await?;

    let started = run_rule(&mut ui, rule.rule.id).await?;
    let pending = wait_run(&mut ui, started.id, "pending", is_pending_now).await?;
    ensure_eq!(pending.item_count, 1, "one item is waiting");
    ensure_eq!(next_prompt(&mut prompts).await?, started.id, "prompted");
    ensure_eq!(
        ui.call(Request::DecideRun {
            id: started.id,
            decision: Decision::CleanNow,
        })
        .await?,
        Ok(Response::Unit),
        "clean now accepted"
    );
    let done = wait_run(&mut ui, started.id, "done", is_done).await?;
    ensure!(
        matches!(done.state, RunState::Done { freed, failed: 0 } if freed >= 70_000),
        "freed the target: {:?}",
        done.state
    );
    ensure!(!fx.target.exists(), "the target folder is gone");
    ensure!(
        fx.root.join("tool").join("Cargo.toml").is_file(),
        "only the artifacts were removed, not the project"
    );
    ensure_eq!(
        code(
            &ui.call(Request::DecideRun {
                id: started.id,
                decision: Decision::CleanNow,
            })
            .await?
        ),
        Some(ErrorCode::BadRequest),
        "a finished run cannot be decided"
    );
    ensure!(
        status
            .wait_for(|s| s.pending.is_empty())
            .await
            .is_ok_and(|s| s.pending.is_empty()),
        "no longer pending"
    );
    let runs = list_runs(&mut ui).await?;
    ensure!(
        runs.len() == 1 && runs.first().is_some_and(|r| r.id == started.id),
        "the run stays in the history: {runs:?}"
    );
    let info = match ui.call(Request::ListRules).await? {
        Ok(Response::Rules(rules)) => rules,
        other => return Err(format!("list_rules answered {other:?}")),
    };
    ensure!(
        info.first()
            .is_some_and(|i| i.last_run == Some(started.started)),
        "the rule remembers its last run: {info:?}"
    );
    let _ignored = std::fs::remove_dir_all(&fx.root);
    Ok(())
}

#[tokio::test]
async fn skip_keeps_the_files_and_the_schedule_waits_a_full_interval() -> TestResult {
    let fx = fixture("rules-skip")?;
    let engine = test_engine(None);
    let mut ui = Client::connect(&engine, ClientKind::Ui).await?;
    scan_only(&mut ui, &fx.root).await?;
    let rule = put_rule(&mut ui, dev_rule("Rust targets", Confirm::Ask)).await?;
    let run = run_rule(&mut ui, rule.rule.id).await?;
    wait_run(&mut ui, run.id, "pending", is_pending_now).await?;
    ensure_eq!(
        ui.call(Request::DecideRun {
            id: run.id,
            decision: Decision::Skip,
        })
        .await?,
        Ok(Response::Unit),
        "skip accepted"
    );
    ensure_eq!(
        get_run(&mut ui, run.id).await?.state,
        RunState::Skipped,
        "skipped"
    );
    ensure!(fx.target.exists(), "skipping removes nothing");
    let listed = match ui.call(Request::ListRules).await? {
        Ok(Response::Rules(rules)) => rules,
        other => return Err(format!("list_rules answered {other:?}")),
    };
    ensure!(
        listed
            .first()
            .is_some_and(|i| i.last_run == Some(run.started)
                && i.next_run.is_some_and(|n| n > run.started + 89 * 86_400)),
        "the next run is a full interval after the skipped one: {listed:?}"
    );
    // Not due before that; a skipped run is finished, so the rule can run again by hand.
    engine.tick(run.started + 80 * 86_400).await;
    ensure_eq!(list_runs(&mut ui).await?.len(), 1, "not due yet");
    ensure!(
        run_rule(&mut ui, rule.rule.id).await.is_ok(),
        "manual run after a skip"
    );
    let _ignored = std::fs::remove_dir_all(&fx.root);
    Ok(())
}

#[tokio::test]
async fn auto_rules_clean_without_asking_and_empty_filters_end_as_nothing() -> TestResult {
    let fx = fixture("rules-auto")?;
    let engine = test_engine(None);
    let mut prompts = engine.subscribe_prompts();
    let mut ui = Client::connect(&engine, ClientKind::Ui).await?;
    scan_only(&mut ui, &fx.root).await?;

    let mut picky = dev_rule("Too big", Confirm::Auto);
    picky.filter.min_bytes = 1 << 40;
    let picky = put_rule(&mut ui, picky).await?;
    let run = run_rule(&mut ui, picky.rule.id).await?;
    wait_run(&mut ui, run.id, "nothing", |s| *s == RunState::Nothing).await?;
    ensure!(fx.target.exists(), "an empty selection removes nothing");

    let auto = put_rule(&mut ui, dev_rule("Auto", Confirm::Auto)).await?;
    let run = run_rule(&mut ui, auto.rule.id).await?;
    let done = wait_run(&mut ui, run.id, "done", is_done).await?;
    ensure!(
        matches!(done.state, RunState::Done { freed, .. } if freed >= 70_000),
        "auto run freed the target: {:?}",
        done.state
    );
    ensure!(!fx.target.exists(), "auto removed the target");
    ensure!(prompts.try_recv().is_err(), "an auto run never asks");
    let _ignored = std::fs::remove_dir_all(&fx.root);
    Ok(())
}

#[tokio::test]
async fn a_running_cargo_build_defers_the_clean_until_it_ends() -> TestResult {
    let fx = fixture("rules-defer")?;
    let lock_path = fx.target.join("debug").join(".cargo-lock");
    let lock: File = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|err| format!("open lock: {err}"))?;
    lock.try_lock()
        .map_err(|err| format!("hold the build lock: {err}"))?;

    let engine = test_engine(None);
    let mut ui = Client::connect(&engine, ClientKind::Ui).await?;
    scan_only(&mut ui, &fx.root).await?;
    let rule = put_rule(&mut ui, dev_rule("Auto", Confirm::Auto)).await?;
    let run = run_rule(&mut ui, rule.rule.id).await?;
    let deferred = wait_run(&mut ui, run.id, "deferred", is_deferred).await?;
    let RunState::Deferred { until } = deferred.state else {
        return Err(format!("deferred run is {:?}", deferred.state));
    };
    ensure!(
        until > now_secs() + 500 && until <= now_secs() + 601,
        "retried in ten minutes: {until}"
    );
    ensure!(fx.target.exists(), "nothing removed under a running build");

    engine.tick(until - 1).await;
    ensure!(
        is_deferred(&get_run(&mut ui, run.id).await?.state),
        "not retried before the deferral ends"
    );
    // Retry while the build still runs: deferred again, later.
    engine.tick(until).await;
    let again = wait_run(&mut ui, run.id, "deferred again", |s| {
        matches!(s, RunState::Deferred { .. })
    })
    .await?;
    ensure!(
        matches!(again.state, RunState::Deferred { until: next } if next > until - 1),
        "still deferred while the lock is held: {:?}",
        again.state
    );
    ensure!(fx.target.exists(), "still nothing removed");

    lock.unlock().map_err(|err| format!("release: {err}"))?;
    let RunState::Deferred { until } = get_run(&mut ui, run.id).await?.state else {
        return Err("run left the deferral by itself".to_owned());
    };
    engine.tick(until).await;
    let done = wait_run(&mut ui, run.id, "done", is_done).await?;
    ensure!(
        is_done(&done.state) && !fx.target.exists(),
        "cleaned once the build ended"
    );
    let _ignored = std::fs::remove_dir_all(&fx.root);
    Ok(())
}

#[tokio::test]
async fn a_pending_run_survives_a_restart_and_rescans_on_clean_now() -> TestResult {
    let fx = fixture("rules-restart")?;
    let dir = temp_dir("rules-restart-store")?;
    let path = dir.join("rules.toml");
    let first = test_engine(Some(path.clone()));
    let mut ui = Client::connect(&first, ClientKind::Ui).await?;
    scan_only(&mut ui, &fx.root).await?;
    let rule = put_rule(&mut ui, dev_rule("Rust targets", Confirm::Ask)).await?;
    let run = run_rule(&mut ui, rule.rule.id).await?;
    let pending = wait_run(&mut ui, run.id, "pending", is_pending_now).await?;
    drop(ui);
    drop(first);

    // The new daemon has no scan job: the run is restored from the file and asks again.
    let second = test_engine(Some(path));
    let mut prompts = second.subscribe_prompts();
    second.start_automation();
    let mut cli = Client::connect(&second, ClientKind::Ui).await?;
    scan_only(&mut cli, &fx.root).await?;
    ensure_eq!(
        get_run(&mut cli, run.id).await?,
        pending,
        "the pending run and its items are restored"
    );
    ensure_eq!(
        next_prompt(&mut prompts).await?,
        run.id,
        "asks again after a restart"
    );
    ensure_eq!(
        cli.call(Request::DecideRun {
            id: run.id,
            decision: Decision::CleanNow,
        })
        .await?,
        Ok(Response::Unit),
        "clean now"
    );
    let done = wait_run(&mut cli, run.id, "done", is_done).await?;
    ensure!(
        is_done(&done.state) && !fx.target.exists(),
        "the rescan found the target and cleaned it: {done:?}"
    );
    let _ignored = std::fs::remove_dir_all(&fx.root);
    let _ignored = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[tokio::test]
async fn deleting_a_rule_skips_its_pending_run() -> TestResult {
    let fx = fixture("rules-delete")?;
    let engine = test_engine(None);
    let mut ui = Client::connect(&engine, ClientKind::Ui).await?;
    scan_only(&mut ui, &fx.root).await?;
    let rule = put_rule(&mut ui, dev_rule("Rust targets", Confirm::Ask)).await?;
    let run = run_rule(&mut ui, rule.rule.id).await?;
    wait_run(&mut ui, run.id, "pending", is_pending_now).await?;
    ensure_eq!(
        ui.call(Request::DeleteRule { id: rule.rule.id }).await?,
        Ok(Response::Unit),
        "delete"
    );
    ensure_eq!(
        get_run(&mut ui, run.id).await?.state,
        RunState::Skipped,
        "nobody is left to answer the run"
    );
    ensure!(fx.target.exists(), "deleting a rule removes nothing");
    let _ignored = std::fs::remove_dir_all(&fx.root);
    Ok(())
}

#[tokio::test]
async fn the_scheduler_fires_due_rules_once_and_never_while_a_run_is_unfinished() -> TestResult {
    let fx = fixture("rules-tick")?;
    let engine = test_engine(None);
    let mut prompts = engine.subscribe_prompts();
    let mut ui = Client::connect(&engine, ClientKind::Ui).await?;
    scan_only(&mut ui, &fx.root).await?;
    let asking = put_rule(&mut ui, dev_rule("Asking", Confirm::Ask)).await?;
    let mut paused = dev_rule("Paused", Confirm::Ask);
    paused.enabled = false;
    put_rule(&mut ui, paused).await?;

    let created = now_secs();
    engine.tick(created + 3_600).await;
    ensure!(
        list_runs(&mut ui).await?.is_empty(),
        "nothing is due within 90 days of creation"
    );

    // The machine slept for 200 days: one run fires, not two dozen.
    let wake = created + 200 * 86_400;
    engine.tick(wake).await;
    let runs = list_runs(&mut ui).await?;
    ensure_eq!(runs.len(), 1, "one catch-up run for the enabled rule only");
    let Some(run) = runs.first() else {
        return Err("no run".to_owned());
    };
    ensure!(
        run.rule == asking.rule.id && run.started == wake && run.rule_name == "Asking",
        "the run belongs to the due rule and starts at wake time: {run:?}"
    );
    wait_run(&mut ui, run.id, "pending", is_pending_now).await?;
    ensure_eq!(
        next_prompt(&mut prompts).await?,
        run.id,
        "a scheduled ask prompts"
    );

    engine.tick(wake + 300 * 86_400).await;
    ensure_eq!(
        list_runs(&mut ui).await?.len(),
        1,
        "a rule with an unfinished run never fires again"
    );
    let listed = match ui.call(Request::ListRules).await? {
        Ok(Response::Rules(rules)) => rules,
        other => return Err(format!("list_rules answered {other:?}")),
    };
    ensure!(
        listed
            .iter()
            .any(|i| i.rule.id == asking.rule.id && i.last_run == Some(wake)),
        "the rule's last run moved to the wake time: {listed:?}"
    );
    let _ignored = std::fs::remove_dir_all(&fx.root);
    Ok(())
}

#[tokio::test]
async fn decisions_are_validated_before_anything_else() -> TestResult {
    let engine = test_engine(None);
    let mut ui = Client::connect(&engine, ClientKind::Ui).await?;
    for (minutes, what) in [(0, "snooze below 1 minute"), (1441, "snooze above a day")] {
        ensure_eq!(
            code(
                &ui.call(Request::DecideRun {
                    id: 1,
                    decision: Decision::Snooze { minutes },
                })
                .await?
            ),
            Some(ErrorCode::BadRequest),
            "{what}"
        );
    }
    ensure_eq!(
        code(
            &ui.call(Request::DecideRun {
                id: 9_999,
                decision: Decision::Skip,
            })
            .await?
        ),
        Some(ErrorCode::NotFound),
        "an unknown run"
    );
    Ok(())
}
