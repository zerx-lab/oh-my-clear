<!-- Research snapshot 2026-09-28 (bootstrap session). Point-in-time evidence: versions/dates/activity go stale; /tmp paths mentioned below no longer exist. Decisions derived from this live in docs/memory/decisions/. -->

# File-based, self-iterating memory for `oh-my-clear` — research + recommended design

Scope: zero-install (plain Markdown in git), agent-maintained memory for a Rust desktop app developed by omp agents. Everything below was checked against the cited docs on 2026-09-28 unless marked [UNVERIFIED].

---

## 0. Key facts from omp itself (these constrain the design)

Source: `omp://context-files.md`, `omp://rulebook-matching-pipeline.md`, `omp://skills.md`, `omp://config-usage.md`, `omp://memory.md`, `omp://tools/learn.md`, `omp://tools/context-notes.md`.

1. **Root `AGENTS.md` is auto-loaded** (`agents-md` provider, priority 10) into `<repo-rules>`. No need to tell agents to read it.
2. **Gotcha: `.omp/AGENTS.md` shadows root `AGENTS.md`.** "Config subdirectories of an ancestor (`.claude/`, `.github/`, …) count as the same depth as that ancestor" and "At the same depth, the higher-priority provider shadows the rest" (`native` = 100 vs `agents-md` = 10). A `CLAUDE.md` at root (`claude-md`, priority 10) competes too. → Use **only** root `AGENTS.md`; do NOT create `.omp/AGENTS.md` or a root `CLAUDE.md` (if Claude Code compatibility is wanted later, `CLAUDE.md` containing `@AGENTS.md` is the documented pattern, but under omp it would compete at depth 0 — skip it).
3. **`@path` imports** in context files expand inline at session start (relative to the importing file, ≤5 hops, skipped inside code spans/fences, missing target leaves the token). Imported content is always-loaded context → only import the small core file.
4. **Project rules** load from `<cwd>/.omp/rules/*.{md,mdc}` "when the cwd's `.omp/` directory is non-empty" — i.e. launch omp from repo root. Frontmatter fields: `description`, `globs`, `alwaysApply`, `condition` (regex, TTSR), `astCondition`, `question` (judged), `scope`, `agents`, `interruptMode`. Buckets: TTSR (has condition/astCondition/question) > always-apply (full body in system prompt) > rulebook (needs `description`; listed by name, body on demand via `rule://<name>`).
5. **`.omp/RULES.md`** = sticky always-apply rule named `RULES`, re-sent every request; user `~/.omp/agent/RULES.md` shadows it (name dedup). Keep short.
6. **Project skills**: `<ancestor>/.omp/skills/<name>/SKILL.md` (`omp://config-usage.md` L278/284); native provider requires `description` frontmatter; listed by name+description; body read via `skill://<name>`.
7. **omp's built-in memory** (`memory.backend: local`, `autolearn.enabled` → `learn` tool) is **machine-local** (`~/.omp/agent/memories/<encoded-cwd>/learned.md`, `MEMORY.md`, `memory_summary.md`), off by default, skipped for subagents, `learn` not auto-given to subagents, 100-lesson cap, 5000-token injection cap. Its read-path guidance is worth copying: *"Treat memory as heuristic context… Prefer repo state and user instruction when they conflict with memory; treat conflicting memory as stale."* It is NOT shared via git → cannot be the project's source of truth. Optional personal layer only.
8. **`context_notes`** is experimental, per-session-branch, 16 KiB — a scratchpad for one session, not cross-session project memory.

---

## 1. Comparison table

| System | Structure | Load strategy | Update triggers | Decay / pruning | Conflict handling | Fit for `oh-my-clear` |
|---|---|---|---|---|---|---|
| **Cline Memory Bank** ([docs](https://docs.cline.bot/best-practices/memory-bank)) | `memory-bank/` with 6 fixed files: `projectbrief.md`, `productContext.md`, `activeContext.md`, `systemPatterns.md`, `techContext.md`, `progress.md` | **Read ALL files at start of EVERY task** (rule text: "I MUST read ALL memory bank files") | New patterns discovered; after significant changes; user says "update memory bank" (must review ALL files); context needs clarification | None defined; relies on rewrite during "update memory bank" | None; hierarchy `projectbrief` is "source of truth for project scope" | Good taxonomy (active context / progress split); **bad load strategy** — reading everything every task wastes context and scales poorly |
| **Roo Code Memory Bank** (community, [README](https://github.com/GreatScottyMac/roo-code-memory-bank)) | `memory-bank/{activeContext,productContext,progress,decisionLog,systemPatterns}.md` + optional `projectBrief.md`; per-mode YAML strategies | Read at session start per mode | Per-mode "real-time update triggers" (architect: decisions; debug: bug discoveries, fix verifications) | None | None | Adds `decisionLog.md` (append log) — ADRs are a stronger version. Author moved on to an MCP server (Context Portal) → methodology not maintained |
| **Claude Code CLAUDE.md** ([docs](https://code.claude.com/docs/en/memory)) | Hierarchical `CLAUDE.md` (+ `.claude/rules/*.md` with `paths:` globs) | Always at launch (ancestors); subdirs + path rules on demand; imports load at launch | Human-driven: "Claude makes the same mistake a second time", review catches missing knowledge, repeated correction | Guidance: **<200 lines per file**; `/doctor prompt-audit` finds stale/conflicting/nonexistent refs | "If two rules contradict each other, Claude may pick one arbitrarily" → review periodically | Model for our AGENTS.md sizing + "promote on 2nd occurrence" trigger |
| **Claude Code auto memory** (same doc) | `~/.claude/projects/<p>/memory/MEMORY.md` **index, one line per memory** + one topic file per memory; frontmatter `type: user|feedback|project|reference`, auto `modified:` ISO timestamp | Index: first **200 lines / 25 KB** always; topic files **on demand** | Agent decides; skips anything derivable from code/git or already in CLAUDE.md | Harness warns near limit: "keep one line per entry, move detail into topic files, and merge or drop stale entries"; errors when over | `modified` timestamp shows freshness | **Best-in-class pattern: small always-loaded index + on-demand topic files + hard budget + timestamps.** Machine-local, so we replicate it in-repo |
| **Anthropic memory tool** ([docs](https://platform.claude.com/docs/en/agents-and-tools/tool-use/memory-tool)) | Client-side `/memories` directory; `view/create/str_replace/insert/delete/rename` | Agent `view`s directory first ("ALWAYS VIEW YOUR MEMORY DIRECTORY BEFORE DOING ANYTHING ELSE"), then reads files just-in-time | Continuous: "record status / progress / thoughts"; "ASSUME INTERRUPTION" | Prompt: "keep its content up-to-date, coherent and organized… rename or delete files that are no longer relevant. Do not create new files unless necessary." | None built-in | Just-in-time retrieval + "assume interruption" + multi-session pattern (initializer → progress log + feature checklist → end-of-session update). API feature, not applicable directly; pattern applies |
| **Anthropic long-running harness** ([blog](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents)) | `claude-progress.txt` + `feature_list.json` (`passes: false`) + `init.sh` + git history | Read progress + `git log` at start | End every session with git commit + progress update; mark feature passing only after E2E verification | Implicit via git | JSON chosen because "the model is less likely to inappropriately change or overwrite JSON files compared to Markdown" | Validates `progress.md` + git-as-archive; JSON for status lists that must not be rewritten |
| **Cursor rules** ([docs](https://cursor.com/docs/rules)) | `.cursor/rules/*.mdc` frontmatter `description`, `globs`, `alwaysApply` | Always / glob auto-attach / agent-requested by description / manual @ | Human or agent (`/create-rule`); "Add rules only when you notice Agent making the same mistake repeatedly" | "Keep rules under 500 lines"; reference files instead of copying (prevents staleness) | Team rules take precedence | Same 3-mode model as omp rules (omp reads `.cursor/rules` too). Cursor "Memories" (auto-generated, user-approved, stored in app not repo) [UNVERIFIED: status/removal] — not file-based, irrelevant |
| **Kiro steering** ([docs](https://kiro.dev/docs/steering/)) | `.kiro/steering/*.md`; foundation `product.md`, `tech.md`, `structure.md` | Frontmatter `inclusion: always` (default) / `fileMatch` + `fileMatchPattern` / `manual` / `auto` + `name` + `description` | Human/agent-generated ("Generate Steering Docs") | None | Workspace overrides global | Confirms the always/glob/manual/description taxonomy; `#[[file:…]]` live references ≈ omp `@` imports |
| **GitHub Copilot** ([docs](https://docs.github.com/en/copilot/how-tos/copilot-on-github/customize-copilot/add-custom-instructions/add-repository-instructions)) | `.github/copilot-instructions.md`; `.github/instructions/**/NAME.instructions.md` with `applyTo:` glob, `excludeAgent:`; `AGENTS.md` nearest wins | Always + path-specific | Human; generator prompt: "no longer than 2 pages", "not task specific", "instruct the agent to trust the instructions and only perform a search if … incomplete or found to be in error" | None | Personal > repo > org; all provided | 2-page cap + "trust but verify" phrasing reusable. omp reads `.github/instructions` too (don't create — avoid duplicate sources) |
| **agents.md spec** ([agents.md](https://agents.md/)) | Plain Markdown, no required fields; nested per package | Always (nearest file) | "Treat AGENTS.md as living documentation" | None | "The closest AGENTS.md to the edited file wins; explicit user chat prompts override everything." | Our entrypoint (cross-tool, auto-loaded by omp) |
| **Aider conventions** ([docs](https://aider.chat/docs/usage/conventions.html)) | `CONVENTIONS.md` loaded read-only (`read:` in `.aider.conf.yml`) | Always, marked read-only + prompt-cached | Human | None | None | Read-only + cache-friendly idea: keep always-loaded files stable; volatile state goes on-demand |
| **Devin Knowledge** ([docs](https://docs.devin.ai/product-guides/knowledge)) | Items = name + **trigger description** + short content; folders; pin to repo | Retrieved when work matches trigger (pinned = always) | **Devin suggests knowledge from user feedback**; can suggest updates to existing items | Enable/disable per item/folder | — | **Now deprecated → migrated to Skills** (trigger → skill `description`). Confirms: procedural knowledge belongs in skills with good descriptions |
| **Letta / MemGPT** ([memory blocks](https://docs.letta.com/v1-sdk/memory/memory-blocks)) | Core memory **blocks** (`label`, `description`, `value`, `limit` chars) always in context; archival memory searched on demand | Core: always ("never need retrieval"); archival: tool search | Agent self-edits blocks with memory tools | Hard `chars_limit` per block forces rewriting | `read_only` shared blocks; shared blocks across agents | Tiering concept maps to files: **INDEX.md = core block with char budget; everything else = archival (grep/read)**; read-only policy blocks ≈ rules |
| **ADRs** ([MADR](https://adr.github.io/madr/), [template](https://github.com/adr/madr/blob/develop/template/adr-template-minimal.md)) | One immutable file per decision, numbered; status + supersedes | On demand | When an architecturally significant decision is made | Never deleted; `Superseded by NNNN` | Explicit supersession chain | **Essential** for decisions (deps choice, runtime, GUI arch); immutable → no merge conflicts |
| **Errata / lessons log** (e.g. omp's own `ERRATA-GPT5-HARMONY.md` in `omp://`) | Dated entries: symptom → cause → fix | On demand (grep) | After non-obvious bug / surprise | Manual | Newest-first | Needed for "non-obvious bug fixed" trigger; must have dedup + promotion path |
| **omp local memory + `learn`** (`omp://memory.md`) | Machine-local `learned.md` (newest-first, dedup, ≤100 entries, ≤2000 chars each) + generated `MEMORY.md`/skills from session transcripts | Summary+lessons injected at start (≤5000 tokens) | `learn` tool calls; background consolidation of past sessions | Cap 100; consolidation prunes skills | "Prefer repo state… treat conflicting memory as stale" | Not in git, off by default, not for subagents → optional personal layer only |

**Synthesis:** everything converges on (a) a tiny always-loaded core + on-demand detail, (b) explicit triggers ("2nd time the same mistake", "after decision", "end of session"), (c) hard size budgets that force consolidation, (d) timestamps + supersession for staleness, (e) promotion of repeated knowledge into enforced rules/skills. None of the file-based systems handle **concurrent writers**; that must be added for a multi-agent (omp `task` subagents) workflow.

---

## 2. Recommended design for `oh-my-clear`

### 2.1 Location & layout

Use `docs/memory/` (visible to humans and every agent tool, reviewed in PRs, not tied to omp's `.omp/` discovery semantics). omp wiring lives in `.omp/`.

```text
AGENTS.md                        # always loaded; contains "Memory" section + @docs/memory/INDEX.md
.omp/
  RULES.md                       # sticky hard constraints (owned by rules workstream); 1 memory line
  rules/
    memory-protocol.md           # alwaysApply: MUST-write triggers (≤20 lines)
    memory-deps-ledger.md        # TTSR: cargo add/remove or Cargo.toml edit → update deps.md + ADR
    memory-no-silent-decision.md # TTSR (optional): prose "we'll use X instead of Y" → ADR reminder
  skills/
    memory/SKILL.md              # full procedure: templates, consolidation, promotion
docs/memory/
  INDEX.md                       # CORE tier (auto-loaded via @import): ≤80 lines / ≤6 KB
  active-context.md              # current focus, open questions, next steps (volatile)
  progress.md                    # milestone checklist + last-10 session log
  lessons.md                     # errata / lessons learned (L-NNNN entries)
  deps.md                        # dependency ledger: one crate per category + review evidence
  glossary.md                    # domain terms
  decisions/
    0000-template.md
    0001-record-architecture-decisions.md
    NNNN-kebab-title.md          # ADRs, immutable once accepted
```

No `archive/` directory: **git history is the archival tier**. Pruned entries are deleted; `git log -p docs/memory/lessons.md` recovers them.

### 2.2 Tiers: auto-loaded vs on demand

| Tier | Files | How it reaches the agent | Budget |
|---|---|---|---|
| Always (core) | `AGENTS.md` (incl. imported `docs/memory/INDEX.md`), `.omp/RULES.md`, `.omp/rules/memory-protocol.md` | Context file + `@` import; always-apply rules | AGENTS.md body ≤150 lines; INDEX.md ≤80 lines/6 KB; memory-protocol rule ≤20 lines; **total memory-related always-loaded ≤ ~2.5k tokens** |
| Listed (name+description only) | `skill://memory`, rulebook rules | System prompt listing | description ≤1 line |
| Triggered | TTSR rules | Injected when regex/AST/judge matches | body ≤10 lines |
| On demand (archival) | `active-context.md`, `progress.md`, `lessons.md`, `deps.md`, `glossary.md`, `decisions/*` | Agent `read`/`grep` guided by INDEX | see per-file budgets below |

Rationale: Claude Code auto memory (200 lines/25 KB index, topic files on demand), Letta core-vs-archival, Anthropic just-in-time retrieval; Cline's "read ALL files every task" explicitly rejected.

Per-file budgets (exceeding any = consolidation required, §2.6):

| File | Budget |
|---|---|
| `INDEX.md` | ≤80 lines, ≤6 KB |
| `active-context.md` | ≤60 lines |
| `progress.md` | ≤120 lines; session log keeps last 10 entries |
| `lessons.md` | ≤40 active entries, ≤400 lines |
| `deps.md` | one row per category; no cap (grows with deps, each row ≤ 3 lines) |
| `glossary.md` | ≤100 terms, 1–2 lines each |
| ADR | ≤1 page (~80 lines) |

Why INDEX is imported but `active-context.md` is not: always-loaded content should be stable (Aider read-only/cached; prompt-cache friendliness). INDEX carries only a 3-line "Current focus" digest; the volatile detail stays on demand.

### 2.3 Templates (verbatim)

#### `docs/memory/INDEX.md`

```markdown
# Memory index
<!-- CORE memory: auto-loaded via AGENTS.md. Budget: ≤80 lines / ≤6 KB. One line per entry. Detail lives in linked files. -->
Last consolidated: YYYY-MM-DD · Sessions since consolidation: N

Memory is heuristic context, not ground truth: verify against the repo before acting; if repo state or the user contradicts memory, the repo/user wins and the entry is stale — fix or delete it.

## Current focus
- <1–3 lines; detail in active-context.md>

## Files (read on demand)
- active-context.md — current work, open questions, next steps
- progress.md — milestones checklist, last 10 session log entries
- lessons.md — non-obvious bugs, gotchas, errata (grep by tag)
- deps.md — dependency ledger, one crate per category, review evidence
- glossary.md — domain vocabulary
- decisions/ — ADRs (list below)

## Decisions (accepted)
- 0001 Record architecture decisions
- NNNN <title> — <one-line gist>

## Hot lessons (top ≤10, promoted by hit count)
- L-NNNN <one-line rule of thumb> (tags)
```

#### ADR — `docs/memory/decisions/NNNN-kebab-title.md` (MADR-minimal + status/supersession/date)

```markdown
---
status: proposed | accepted | superseded | deprecated
date: YYYY-MM-DD
supersedes: NNNN            # optional
superseded-by: NNNN         # set only when superseded
tags: [deps, runtime, gui, ipc, ...]
---
# NNNN <short title of solved problem and chosen solution>

## Context and Problem Statement
<2–4 sentences; the question being decided; scope.>

## Considered Options
* <option 1>
* <option 2>

## Decision Outcome
Chosen option: "<option>", because <justification tied to constraints: no-panic, one-crate-per-category, maintained/secure/performant>.

### Consequences
* Good, because <…>
* Bad, because <…>

## Evidence
- <links: crate page, advisory db check, benchmark, doc URL; verified YYYY-MM-DD>
```

Rules: accepted ADR bodies are immutable; only `status`/`superseded-by` may change. Changing a decision = new ADR with `supersedes:` + edit old ADR's front matter. Number = highest existing + 1.

#### Lesson — entry in `docs/memory/lessons.md`

```markdown
# Lessons learned
<!-- Newest first. ≤40 active entries. Grep by tag. Promote when hits ≥ 2. Delete (git keeps history) when obsolete. -->

## L-NNNN <imperative one-line lesson>
- date: YYYY-MM-DD · verified: YYYY-MM-DD · hits: 1 · status: active | promoted → <rule://name | skill://name | AGENTS.md | clippy lint> | obsolete
- tags: [build, nextest, gpui, async, deps, windows, ...]
- trigger: <when does this apply — the situation an agent will be in>
- lesson: <what to do / not do, and why (1–3 lines)>
- evidence: <file:line, commit, error message, issue URL>
```

#### Dependency ledger — `docs/memory/deps.md`

```markdown
# Dependency ledger
<!-- Invariant: exactly one crate per category. Adding/replacing a crate REQUIRES an ADR and a row update here. Re-verify rows older than 90 days during consolidation. -->

| Category | Crate | Version req | ADR | Added | Verified | Maintenance | Security | Perf / notes | Rejected alternatives |
|---|---|---|---|---|---|---|---|---|---|
| error handling | <crate> | "x.y" | 000N | YYYY-MM-DD | YYYY-MM-DD | last release YYYY-MM-DD; <N> maintainers | `cargo deny check advisories` clean | <note> | <crate (reason)> |
```

(Which crates fill these rows, and the cargo-deny config, are the Rust-gate workstream's deliverable; this ledger is where their evidence is recorded.)

#### `docs/memory/active-context.md`

```markdown
# Active context
Updated: YYYY-MM-DD by <agent/session>

## Focus
- <what is being built right now>

## Open questions
- <question> — owner: <user|agent>, since YYYY-MM-DD

## Next steps
1. <step>

## Recent changes (last 5; older → progress.md log or delete)
- YYYY-MM-DD <change> (<commit/PR>)
```

#### `docs/memory/progress.md`

```markdown
# Progress

## Milestones
- [ ] M1 <name> — acceptance: <observable E2E behavior>
- [x] M0 Repo scaffolding — done YYYY-MM-DD

## Known issues
- <issue> (L-NNNN / issue link)

## Session log (last 10, newest first)
- YYYY-MM-DD <agent>: <did> · verified by <command/run> · next: <next>
```

Mark a milestone `[x]` only after end-to-end verification (Anthropic harness: "Mark a feature complete only after end-to-end verification confirms it works").

#### `docs/memory/glossary.md`

```markdown
# Glossary
<!-- Alphabetical. 1–2 lines per term. Use these exact terms in code, docs, and UI. -->
- **<Term>** — <definition>. Code: `<type/module>`. Not to be confused with <other term>.
```

### 2.4 Update protocol (when an agent MUST write)

| Trigger | MUST write | Where |
|---|---|---|
| Fixed a non-obvious bug (root cause not evident from the diff; took >1 hypothesis; platform/toolchain quirk) | Lesson entry (or `hits += 1` + `verified` on existing one) | `lessons.md` |
| Made/changed an architectural or cross-cutting decision (crate choice, runtime, module boundary, IPC/protocol, GUI pattern, error strategy) | New ADR; update INDEX "Decisions" line | `decisions/`, `INDEX.md` |
| Added/removed/replaced a dependency | ADR (if new category or replacement) + ledger row | `deps.md`, `decisions/` |
| User corrected the agent / stated a durable preference or constraint | Lesson; if it is a hard rule → propose promotion (AGENTS.md / rule) | `lessons.md` |
| Introduced a new domain term | Glossary line | `glossary.md` |
| End of session / before final yield of a top-level task with code changes | Update `active-context.md` (focus/next steps) and append one session-log line to `progress.md`; bump "Sessions since consolidation" in INDEX | `active-context.md`, `progress.md`, `INDEX.md` |
| Discovered memory is wrong/stale | Fix or delete the entry in the same change | the file |

Do NOT write: anything derivable from code, `Cargo.toml`, or `git log` (Claude auto memory rule: "skips anything it can derive from the codebase"); task-specific chatter; secrets; duplicates of AGENTS.md/rules.

**Multi-agent single-writer rule (added; none of the surveyed systems covers it):** subagents spawned via omp `task` MUST NOT edit `docs/memory/**`. They include a `memory candidates` section in their result (lesson / decision / dep / term, with evidence). The top-level (main) agent, as integration owner, dedups and writes. Exception: a subagent explicitly assigned a memory-maintenance task. This avoids concurrent-edit clobbering (omp's Coop guidance: shared edits need one integration owner) and keeps quality gated. Stated in the always-apply rule (§3.2); optionally split per agent with the rule `agents:` field.

### 2.5 Staleness & conflict rules

1. **Precedence:** user instruction > current repo state (code, `Cargo.toml`, tests) > accepted ADR > `AGENTS.md`/rules > lessons > active-context. Conflicting lower-tier memory is stale → fix it in the same change (mirrors `omp://memory.md` read-path guidance and agents.md "explicit user chat prompts override everything").
2. **Dates everywhere:** every entry carries `date` and `verified` (ISO `YYYY-MM-DD`). Re-verify on use: when an agent relies on an entry and confirms it, bump `verified`.
3. **Staleness thresholds:** lessons with `verified` > 90 days and `hits: 1` → deletion candidates at consolidation; deps rows `verified` > 90 days → re-run maintenance/advisory check.
4. **Supersession, not edits:** ADRs change only via a new ADR (`supersedes`/`superseded-by`). Lessons that become wrong → delete (or `status: obsolete` until next consolidation), never silently rewrite meaning.
5. **Same-topic duplicates:** keep the newer, merge evidence, sum `hits`, delete the other.
6. **Git merge conflicts:** ADR number collision across branches → the later-merged ADR is renumbered (max+1) at merge time and INDEX updated. Lessons/active-context conflicts → resolve by union then dedup.

### 2.6 Self-iteration: consolidation & promotion protocol

**When to consolidate** (any one triggers; agent does it at end of the current task, or when the user says "consolidate memory"):
- any file exceeds its budget (§2.2), or
- INDEX "Sessions since consolidation" ≥ 10, or
- a milestone is checked off in `progress.md`.

**Consolidation steps** (the `memory` skill spells these out):
1. Read all `docs/memory/*` files and `git log --since=<Last consolidated> --oneline`.
2. `lessons.md`: dedup/merge; delete obsolete; delete `hits: 1` + unverified >90 days unless still clearly relevant; re-verify the rest against the repo (does the file/symbol/error still exist?).
3. **Promote** (the self-improving loop):
   - lesson with `hits ≥ 2`, or any user-stated hard constraint → promote:
     - mechanically checkable in source (e.g. forbidden API, pattern) → **clippy lint / `Cargo.toml [lints]` / cargo-deny** first (true enforcement), else **TTSR rule** in `.omp/rules/` with `condition`/`astCondition`;
     - repeatable multi-step procedure → **project skill** `.omp/skills/<name>/SKILL.md`;
     - short always-true project fact → **AGENTS.md** line;
     - then mark lesson `status: promoted → <target>` and remove it at the next consolidation.
   - Top-10 by `hits` → "Hot lessons" in INDEX.
4. `progress.md`: trim session log to last 10; roll milestones.
5. `active-context.md`: drop resolved questions and completed steps.
6. `deps.md`: re-verify rows >90 days; ensure one-crate-per-category still holds vs `Cargo.toml`.
7. Rewrite INDEX within budget; set `Last consolidated`, reset session counter.
8. Commit as its own change: `docs(memory): consolidate YYYY-MM-DD`.

**Demotion:** a rule/TTSR that never fires and whose underlying risk is gone may be removed with a short ADR or lesson note — rules are memory too and must not accumulate.

---

## 3. Wiring into omp

### 3.1 `AGENTS.md` — Memory section (always loaded)

```markdown
## Memory
Project memory lives in `docs/memory/` (index auto-loaded below; everything else read on demand with read/grep).
- Before non-trivial work: check the index; read linked ADRs/lessons relevant to the area you touch.
- Memory is heuristic: repo state and the user win; fix or delete stale entries in the same change.
- Update procedure, templates, consolidation: `skill://memory`. Write triggers: `rule://memory-protocol`.

@docs/memory/INDEX.md
```

(`@docs/memory/INDEX.md` must be outside backticks/fences to expand — `omp://context-files.md` "@ imports".)

### 3.2 Always-apply rule — `.omp/rules/memory-protocol.md`

```markdown
---
description: When agents must write to docs/memory (lessons, ADRs, deps ledger, session log)
alwaysApply: true
---
Memory write triggers (procedure + templates: read `skill://memory` before writing):
- Fixed a non-obvious bug → lesson in docs/memory/lessons.md (or bump hits/verified).
- Architectural/cross-cutting decision → new ADR in docs/memory/decisions/, add line to INDEX.md.
- Dependency added/removed/replaced → docs/memory/deps.md row (+ ADR for new category/replacement).
- User correction or durable preference → lesson; hard constraint → propose promotion to a rule.
- New domain term → docs/memory/glossary.md.
- End of a top-level task with code changes → update active-context.md, one line in progress.md session log, bump INDEX session counter.
- Budgets exceeded or ≥10 sessions since consolidation → consolidate per skill://memory.
Subagents: do NOT edit docs/memory/**; put "memory candidates" (with evidence) in your final result for the main agent to record.
Never record what code, Cargo.toml, or git log already say; never record secrets.
```

Notes: `alwaysApply: true` + `description` → always-apply bucket only (doc: "A rule with both `alwaysApply` and `description` goes to always-apply only"). Keep it here rather than in `.omp/RULES.md` so RULES.md stays reserved for the hard Rust constraints (a user-level `~/.omp/agent/RULES.md` would shadow the project one anyway). Optionally split into two rules using `agents:` (e.g. `agents: main` for the write triggers, and a one-liner for all non-main agents) — `agents` accepts `main`/`sub`/agent names per `omp://rulebook-matching-pipeline.md` §6.

### 3.3 TTSR rules (triggered reminders)

`.omp/rules/memory-deps-ledger.md`:

```markdown
---
description: Dependency changes must update the deps ledger and ADRs
condition: "cargo (add|remove|rm)\\b"
scope: tool
interruptMode: never
---
You are changing dependencies. Before finishing: (1) confirm this is the only crate in its category, (2) record maintenance/security/perf evidence in docs/memory/deps.md, (3) write/supersede an ADR in docs/memory/decisions/ if this adds a category or replaces a crate. See skill://memory.
```

`.omp/rules/memory-cargo-toml.md` (edits to manifests; glob-looking `condition` becomes `tool:edit(<glob>)`/`tool:write(<glob>)` scope with catch-all `.*` per the pipeline doc):

```markdown
---
description: Cargo.toml edits must keep the deps ledger in sync
condition: "**/Cargo.toml"
interruptMode: never
---
Cargo.toml changed. If [dependencies]/[dev-dependencies]/[build-dependencies] changed, update docs/memory/deps.md (and ADR if needed) in the same change.
```

Optional judged rule (costs a `judge` model call per in-scope output; `question` rules never interrupt, deliver as warning — `omp://ttsr-injection-lifecycle.md` §10):

```markdown
---
description: Decisions stated in prose must be recorded as ADRs
question: "Does this reply announce a new architectural or dependency decision (choosing one approach/crate over another) without mentioning an ADR in docs/memory/decisions/?"
scope: text
---
You just made an architectural decision. Record it as an ADR (skill://memory) or state why it is not architecturally significant.
```

[UNVERIFIED] Exact regex escaping behavior of `condition` in YAML double-quoted strings (`\\b`) — test with omp's TTSR CLI (`packages/coding-agent/src/cli/ttsr-cli.ts` is referenced in the pipeline doc) before relying on it; a plain `cargo (add|remove|rm) ` with trailing space avoids escaping entirely.

### 3.4 Project skill — `.omp/skills/memory/SKILL.md`

Frontmatter (native provider requires `description`):

```markdown
---
name: memory
description: Use when writing to docs/memory (lessons, ADRs, deps ledger, glossary, session log) or consolidating/pruning/promoting project memory.
---
```

Body = §2.3 templates + §2.4 triggers table + §2.5 staleness rules + §2.6 consolidation/promotion steps (verbatim from this report). Keep it ≤ ~250 lines; it is read only when invoked (`skill://memory`), so it can be detailed. Users can also invoke `/skill:memory consolidate`.

### 3.5 What NOT to wire
- No `.omp/AGENTS.md` (shadows root `AGENTS.md`, §0.2).
- No `.github/copilot-instructions.md`, `.cursor/rules`, `.clinerules`, `CLAUDE.md` duplicates — omp discovers all of them, so duplicates create conflicting sources.
- Don't depend on `memory.backend`/`learn`: machine-local, off by default, unavailable to subagents. If a developer enables it, it's a personal supplement; repo files remain canonical.
- Don't use `context_notes` for project memory (experimental, per-session-branch).

---

## 4. Sources
- omp: `omp://memory.md`, `omp://tools/learn.md`, `omp://tools/context-notes.md`, `omp://context-files.md`, `omp://rulebook-matching-pipeline.md`, `omp://skills.md`, `omp://config-usage.md` (L277–287), `omp://ttsr-injection-lifecycle.md`
- Cline Memory Bank: https://docs.cline.bot/best-practices/memory-bank
- Roo Code Memory Bank: https://github.com/GreatScottyMac/roo-code-memory-bank ; Roo rules: https://docs.roocode.com/features/custom-instructions
- Claude Code memory (CLAUDE.md + auto memory): https://code.claude.com/docs/en/memory
- Anthropic memory tool: https://platform.claude.com/docs/en/agents-and-tools/tool-use/memory-tool
- Anthropic long-running harness: https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents
- Cursor rules: https://cursor.com/docs/rules
- Kiro steering: https://kiro.dev/docs/steering/
- GitHub Copilot repo instructions: https://docs.github.com/en/copilot/how-tos/copilot-on-github/customize-copilot/add-custom-instructions/add-repository-instructions
- AGENTS.md: https://agents.md/
- Aider conventions: https://aider.chat/docs/usage/conventions.html
- Devin Knowledge (deprecated → Skills): https://docs.devin.ai/product-guides/knowledge
- Letta memory blocks: https://docs.letta.com/v1-sdk/memory/memory-blocks
- MADR: https://adr.github.io/madr/ ; minimal template: https://github.com/adr/madr/blob/develop/template/adr-template-minimal.md
