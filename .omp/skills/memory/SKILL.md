---
name: memory
description: Use when writing to docs/memory (lessons, ADRs, deps ledger, glossary, open questions) or consolidating, pruning, and promoting project memory
---

# Project memory procedure

Memory = plain Markdown in `docs/memory/`, versioned with the code. It records **development rules and their reasons** — decisions, lessons, dependency picks, vocabulary, undecided questions. It never records progress, milestones, schedules, roadmaps, next-step lists or session logs (user rule; `git log` is the history). Tiers:

| Tier | Files | Reaches the agent via | Budget |
|---|---|---|---|
| Core (always loaded) | `INDEX.md` | `@docs/memory/INDEX.md` import in `AGENTS.md` | ≤80 lines / ≤6 KB |
| On demand | `open-questions.md`, `lessons.md`, `deps.md`, `glossary.md`, `decisions/*` | `read`/`grep` guided by INDEX | see below |
| Archive | deleted entries | `git log -p docs/memory/` | — |

Per-file budgets (exceeding any ⇒ consolidate): `open-questions.md` ≤40 lines · `lessons.md` ≤40 active entries · `glossary.md` ≤100 terms · ADR ≤~80 lines · `deps.md` one row per category.

## Write triggers (who: main agent only)
| Trigger | Write |
|---|---|
| Non-obvious bug fixed (root cause not visible in diff; >1 hypothesis; platform/toolchain quirk) | lesson, or `hits += 1` + `verified` on the existing one |
| Architectural / cross-cutting decision (crate, runtime, module boundary, protocol, UI pattern, error strategy) | new ADR + INDEX "Decisions" line |
| Dependency added / removed / replaced | `deps.md` row (+ ADR if new category or replacement) |
| User correction or durable preference | lesson; hard constraint ⇒ promote (below) |
| New domain term | `glossary.md` line |
| A decision is needed but not yet taken | `open-questions.md` line (question, options, owner); delete it when an ADR/lesson answers it |
| Memory found wrong/stale | fix or delete it in the same change |

Never write: progress, status of work, milestones, schedules, roadmaps, next steps, session logs; facts derivable from code, `Cargo.toml`, or `git log`; task chatter; secrets; duplicates of AGENTS.md/rules.
Subagents: never edit `docs/memory/**`; end results with `## Memory candidates` (type, text, evidence). The main agent dedups and writes.

## Templates

Lesson (`lessons.md`, newest first):
```markdown
## L-NNNN <imperative one-line lesson>
- date: YYYY-MM-DD · verified: YYYY-MM-DD · hits: 1 · status: active
- tags: [build, nextest, gpui, async, deps, windows, omp, …]
- trigger: <situation in which this applies>
- lesson: <what to do / not do, and why — 1–3 lines>
- evidence: <file:line, command output, error text, URL>
```
`status` values: `active` | `promoted → <lint|rule://x|skill://x|AGENTS.md>` | `obsolete`. Number = highest existing + 1.

ADR (`decisions/NNNN-kebab-title.md`, copy `decisions/0000-template.md`): accepted bodies are immutable; only `status` / `superseded-by` may change. Changing a decision = new ADR with `supersedes: NNNN` + edit the old one's front matter. Number = highest existing + 1.

Deps row (`deps.md`): `| Category | Crate | Version req | Status | ADR | Verified | Evidence | Rejected |`.

Open question line (`open-questions.md`): `- <question> — options: <a / b> — owner: user|agent — since YYYY-MM-DD`.

Glossary line: `- **Term** — definition. Code: \`Type\`/module. Not: <confusable term>.`

## Staleness & conflicts
1. Precedence: user > current repo state > accepted ADR > AGENTS.md/rules > lessons > open-questions. Lower-tier conflicts are stale → fix in the same change.
2. Every entry has `date` + `verified` (ISO). Bump `verified` when you rely on an entry and confirm it.
3. Lessons with `hits: 1` and `verified` >90 days → delete candidates. Deps rows `verified` >90 days → re-run `skill://dep-review` §2 criteria 1–3.
4. Duplicates: keep the newer, merge evidence, sum `hits`.
5. Merge conflicts: ADR number collision → later-merged ADR renumbers to max+1; lessons/open-questions → union then dedup.

## Consolidation (self-iteration)
Run when any budget is exceeded or the user asks.
1. Read all `docs/memory/*` and `git log --oneline --since=<Last consolidated>`.
2. `lessons.md`: merge duplicates, delete obsolete, re-verify each against the repo (file/symbol/error still exists?).
3. Promote lessons with `hits ≥ 2` or any user hard constraint, strongest enforcement first:
   1. mechanically checkable → clippy lint in `[workspace.lints]` / `clippy.toml`, or `deny.toml` ban;
   2. pattern in written code/commands → TTSR rule in `.omp/rules/` (`condition` regex or `astCondition`), verified with
      `omp ttsr test --rule .omp/rules/<name>.md --source tool --tool edit --path src/x.rs '<snippet>'` (positive and negative case);
   3. multi-step procedure → project skill `.omp/skills/<name>/SKILL.md`;
   4. short always-true fact → one line in `AGENTS.md`.
   Mark the lesson `status: promoted → <target>`; delete it at the next consolidation.
4. Demote: a rule that never fires and whose risk is gone may be deleted (note it in a lesson or ADR).
5. Top ≤10 lessons by `hits` → INDEX "Hot lessons".
6. `open-questions.md`: drop questions an ADR or lesson has answered.
7. `deps.md`: re-verify rows >90 days; confirm one-crate-per-category still matches `Cargo.toml`.
8. Rewrite INDEX within budget; set `Last consolidated`.
9. Commit separately: `docs(memory): consolidate YYYY-MM-DD`.
