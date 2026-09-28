---
status: accepted
date: 2026-09-28
tags: [memory, process, docs]
---
# 0012 Docs and memory record development rules only — no progress, milestones or schedules

## Context and Problem Statement
The user ruled that the project must not design progress or schedules; it only plans and records the rules that govern development. ADR 0001's memory kept `progress.md` (milestones, session log) and `active-context.md` (focus, next steps), and later ADRs and research write-ups described milestones and phased roadmaps.

## Considered Options
* Keep progress tracking in memory, and stop adding new plans
* Remove every progress and schedule artifact, and enforce that none come back (**chosen**)

## Decision Outcome
- Memory holds: ADRs, lessons, the dependency ledger, the glossary, and `open-questions.md`. Open questions are undecided choices written as options; the line is deleted once an ADR or lesson answers it.
- Removed: `progress.md`, the focus/next-step/recent-change parts of `active-context.md` (the file is now `open-questions.md`), the INDEX session counter, and the end-of-task memory write trigger. Consolidation runs when a budget is exceeded or the user asks.
- No document in the repo (ADRs, AGENTS.md, `.omp/**`, crate docs, research write-ups) states milestones, schedules, roadmaps, phase plans, next-step lists or session logs. Designs are stated as rules and constraints, not as build order. `git log` is the history.
- Enforcement:
  - The always-apply `rule://memory-protocol` and `skill://memory` state the rule.
  - `.omp/RULES.md` and `AGENTS.md` carry it as a non-negotiable.
  - The TTSR rule `docs-no-schedule` fires on schedule vocabulary in `*.md` edits.
- Amends ADR 0001 (its memory tiers). The earlier research snapshots stay as historical evidence.

### Consequences
* Good: memory stays small and always true. Nothing in the repo goes stale because a plan changed.
* Bad: "what is done" must be read from code and `git log`, not from a checklist.

## Evidence
- User instruction, 2026-09-28: "不要设计任何进度和排期，只需要规划好记录好开发相关的规则即可。" ("Don't design any progress or schedule; just plan and record the development-related rules.")
