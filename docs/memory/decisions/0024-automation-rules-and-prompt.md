---
status: accepted
date: 2026-09-30
tags: [gui, design, runtime, protocol]
---
# 0024 Automation: daemon-owned rules (trigger + scope + filter + action + confirm), prompt window, Automation sidebar group

## Context and Problem Statement
The sidebar of ADR 0019 lists one-shot tools (scan → review → clean). Planned features are of another kind: recurring work that runs without the UI (e.g. clean a developer's stale `target`/`node_modules` every two weeks, asking first with clean now / snooze / skip) and, later, rule-driven file moving. Adding a sidebar page per feature would grow the list linearly and mix tools with long-lived background objects. Amends ADR 0019's taxonomy.

## Considered Options
* One page per automated feature vs one **rule** model (trigger + scope + filter + action + confirm) that every automation instantiates (**rule model**)
* Confirmation via OS notifications with action buttons vs a compact app-owned prompt window launched by the daemon + tray entry (**prompt window**: cross-platform identical, no ObjC/WinRT bindings, no `unsafe`; OS notifications at most for attention)
* Automation behind a "manual / automatic" mode switch vs its own sidebar group (**sidebar group**: nothing hidden)
* Scheduler in the UI vs in the daemon (**daemon**: the UI quits with its last window, ADR 0020)

## Decision Outcome
- **Model** (`omc_proto::rules`): `Rule { trigger, scope, filter, action, confirm }`; each firing is a `RuleRun` (Scanning → Pending/Deferred → Cleaning → Done/Nothing/Skipped/Failed) kept in a bounded activity history. New automation = a new `Trigger`/`RuleScope`/`RuleAction` variant, not a new page.
- **Daemon**: rules and history persist in `rules.toml`; a wall-clock scheduler (survives sleep; a missed run fires once) runs rules through the existing scan/clean jobs, so `Guard`, removal-by-item-id and the delete method of ADR 0021 apply unchanged. A Cargo `target` whose build lock is held defers the run instead of deleting under a running build.
- **Prompt**: a run needing a decision pushes `Event::Prompt`; with no UI attached the daemon launches `oh-my-clear --prompt <run>`, which opens only the prompt window. The tray lists pending runs. Snooze/skip state lives in the daemon.
- **IA**: sidebar adds the group Automation (Rules, Activity); groups collapse; Overview becomes the dashboard where pending/next/last runs surface; tool pages offer contextual rule creation (Developer Junk → "Clean automatically…"); ⌘K/Ctrl+K palette jumps to any page or rule.

### Consequences
* Good, because future automations reuse one store, scheduler, history, confirmation flow and UI.
* Good, because unattended removal cannot reach anything a manual clean could not.
* Bad, because the daemon now launches the GUI on its own for prompts; a prompt the user ignores stays pending until answered.

## Evidence
- ADR 0019 (taxonomy), 0020 (UI exits with last window, tray in daemon), 0021 (jobs, Guard); `crates/omc-proto/src/rules.rs` (2026-09-30)
- std `File::try_lock` stable since Rust 1.89 (cargo holds `<target>/<profile>/.cargo-lock` during builds) — 2026-09-30
