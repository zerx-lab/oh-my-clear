<!-- Research snapshot 2026-09-28 (bootstrap session). Point-in-time evidence: versions/dates/activity go stale; /tmp paths mentioned below no longer exist. Decisions derived from this live in docs/memory/decisions/. -->

# omp project-level configuration — research for `oh-my-clear`

Sources read: `omp://context-files.md`, `omp://rulebook-matching-pipeline.md`, `omp://ttsr-injection-lifecycle.md`, `omp://memory.md`, `omp://skills.md`, `omp://settings.md`, `omp://config-usage.md`, `omp://magic-keywords.md`, `omp://hooks.md`, `omp://task-agent-discovery.md`, `omp://tools/{learn,retain,recall,reflect,memory_edit,context-notes}.md`, `omp://mnemosyne-memory-backend.md`.
Also verified against the installed omp package (`~/.bun/install/global/node_modules/@oh-my-pi/pi-coding-agent/src/…`), `omp config list --json`, and `omp ttsr list/test` in throwaway dirs under `/tmp` (not in the repo).

---

## 1. Where project-level files live (exact paths + precedence)

All paths relative to the repo root. **Launch omp from the repo root** — several native loaders are cwd-only.

| Path | What | Discovery rule | Source |
|---|---|---|---|
| `AGENTS.md` (repo root, any ancestor up to repo root) | Context file (provider `agents-md`, priority 10) | Walks cwd → repo root; one file per directory depth; injected into `<repo-rules>` at session start | context-files.md |
| `.omp/AGENTS.md` | Native context file (priority 100) | Read **only** from the *nearest non-empty* `.omp/` dir walking cwd→root. **Shadows `AGENTS.md` at the same depth** (cwd's `.omp/` counts as depth 0 together with `./AGENTS.md`) | context-files.md "Load order and shadowing" |
| `.omp/RULES.md` | Sticky always-apply rule named `RULES`, full body sent on **every** request, frontmatter can't make it non-sticky | Nearest non-empty `.omp/`; re-read on session start and `/clear` `/new`. **A user `~/.omp/agent/RULES.md` shadows it** (name-based dedup; checked: this machine has none) | context-files.md "Sticky rules" |
| `.omp/rules/*.md` / `*.mdc` | Rules (always / rulebook / TTSR) | **`<cwd>/.omp/rules` only** (no ancestor walk), `.omp/` must be non-empty. Name = filename sans ext | rulebook-matching-pipeline.md §2 |
| `.omp/skills/<name>/SKILL.md` | Skills | `<ancestor>/.omp/skills` for each ancestor cwd→repo root; exactly one level under `skills/` (nested not discovered); `description` REQUIRED for native | skills.md, config-usage.md §6 |
| `.omp/agents/*.md` | Custom task (sub)agents | Nearest project `.omp/agents` (walks up); project overrides user overrides bundled (first-wins by exact `name`) | task-agent-discovery.md |
| `.omp/config.yml` (+ legacy `.omp/settings.json`) | Project settings layer | **`<cwd>/.omp/` only, no ancestor walk**; YAML mapping; deep-merged objects, arrays **replace** | settings.md |
| `.omp/hooks/pre/*.ts`, `.omp/hooks/post/*.ts` | JS/TS hook factories (e.g. hard-block a bash command) | Only `pre/` or `post/` subdirs; file directly in `hooks/` silently ignored | hooks.md |
| `.omp/commands/*.md`, `.omp/prompts/*.md`, `.omp/tools/…`, `.omp/mcp.json` | Slash commands, prompts, custom tools, MCP | native provider | config-usage.md §6 |
| `.omp/SYSTEM.md` | System-prompt customization (not needed here) | nearest non-empty `.omp/` | config-usage.md §6 |

Settings precedence (low→high): defaults < `~/.omp/agent/config.yml` < `<cwd>/.omp/settings.json` < `<cwd>/.omp/config.yml` < `--config` overlays / `PI_CONFIG_FILES` < runtime flags < setting env var.

Rule provider precedence (dedup by rule **name**, first wins): `native` (100: project `.omp/rules` → user `~/.omp/agent/rules` → user RULES.md → project RULES.md) > `omp-plugins` 90 > `agents` 70 (`.agent(s)/rules`) > `cursor`/`windsurf` 50 > `cline` 40 > `github` 30 > `builtin-defaults` 1. ⇒ a project rule named e.g. `rs-parking-lot` overrides the builtin of the same name.

Context-file injection order: farther ancestors first, then closer, then the single user file (`~/.omp/agent/AGENTS.md`, which exists on this machine). Deeper `AGENTS.md` below cwd are only listed as pointers in `<dir-context>`.

### Recommendation for `oh-my-clear`
- Put the canonical instructions in **root `AGENTS.md`** (tool-agnostic: Codex, Cursor etc. read it too). **Do not also create `.omp/AGENTS.md`** — it would shadow root `AGENTS.md` at depth 0.
- Don't create a separate standalone `CLAUDE.md` with different content (provider `claude-md`, same priority 10 as `agents-md`, same depth → one of them is dropped; tie-break order [UNVERIFIED]). If wanted for Claude Code, make `CLAUDE.md` a symlink to `AGENTS.md` (byte-identical files are collapsed).
- Use `.omp/RULES.md` for the handful of non-negotiables (no panics, nextest only, dependency gate) — it stays in context through long sessions. Keep it short.
- `.omp/` becomes non-empty as soon as any of the above exists, which is the admission precondition for rules/settings.

---

## 2. Rule file format (`.omp/rules/<name>.md`)

YAML frontmatter between `---` lines (parsed by `parseFrontmatter`; must start at byte 0 with `---` and close with `\n---`). Hyphenated keys are normalized to camelCase. Body = rule content (frontmatter stripped). Canonical shape (rulebook-matching-pipeline.md §1):

| Field | Type | Meaning |
|---|---|---|
| `description` | string | Required for **rulebook** listing (`<domain-rules>`: `- name (globs): description`); body read on demand via `rule://<name>` |
| `globs` | string or list | Shown in rulebook listing (advisory only, NOT enforced for rulebook); for TTSR rules it is a hard **file-path gate** |
| `alwaysApply` | bool | Full body injected into system prompt (`<generic-rules>`) every session |
| `condition` | regex string or list (legacy `ttsr_trigger`) | TTSR regex trigger; leading `(?i)`/`(?m)`/`(?s)` translated to JS flags. A token that *looks like a file glob* is converted to `tool:edit(glob)`/`tool:write(glob)` scope + catch-all `.*` |
| `astCondition` | string or list | ast-grep structural patterns; only on edit/write tool streams, language inferred from file extension |
| `question` | string | Judged rule: `judge` model role answers yes/no after output completes; never interrupts; delivered as a warning |
| `scope` | `"a, b"` string or YAML list | TTSR stream allowlist: `text`, `thinking`, `tool`/`toolcall`, `tool:<name>`, `tool:<name>(<path-glob>)`. Default = `text` + all `tool`, **not** `thinking` |
| `interruptMode` | `never` \| `prose-only` \| `tool-only` \| `always` | Per-rule override of `ttsr.interruptMode` |
| `agents` | list / string / CSV of lowercase globs | Restrict to agent names; `main` = top-level session, `sub` = unnamed subagent |

Bucketing order (per session, `bucketRules`): drop `ttsr.disabledRules` → drop builtins if `ttsr.builtinRules: false` → drop `agents` mismatches → **TTSR** if it has `condition`/`astCondition`/`question` and registers OK → else **always-apply** if `alwaysApply: true` → else **rulebook** if `description` → else the rule is invisible (not even `rule://`).

### Minimal verbatim examples

Always-apply (`.omp/rules/rust-core.md`):
```markdown
---
alwaysApply: true
---

Never use panicking APIs in Rust …
```

Rulebook / description-based (model reads `rule://dependency-gate` when relevant; `globs` are advisory):
```markdown
---
description: Dependency review gate — read before adding or changing any crate in Cargo.toml
globs: ["Cargo.toml", "**/Cargo.toml"]
---

Before adding a dependency …
```

TTSR regex (see §3 for verified Rust examples):
```markdown
---
description: Never write .unwrap() in Rust
condition: "\\.unwrap\\(\\)"
scope: "tool:edit(*.rs), tool:write(*.rs)"
---

Replace with `?` …
```

Judged (`question`) — needs a judge model; `ttsr.judge: auto` only judges when the `judge` role resolves to a native TypeSafe jev model, `on` uses any model:
```markdown
---
description: Claims tests pass without evidence
question: "Does the reply claim tests pass without showing they were run?"
scope: text
---
```

Verification: in `/tmp/ttsr-proj/.omp/rules/` (git repo) `omp ttsr list` listed my two TTSR files as `[native]`, confirming `.omp/rules/*.md` discovery and frontmatter parse.

---

## 3. TTSR (Time-Traveling Stream Rules)

**What is watched** (ttsr-injection-lifecycle.md §2): streamed `text_delta` (assistant prose), `thinking_delta` (only if `scope` includes `thinking`), `toolcall_delta` (tool-call arguments). For edit/write tools the regex is matched against a **reconstructed source snapshot of the added/new content** (per file for multi-file edits, `matcherEntries`), not the raw wire JSON and not pre-existing file content. For other tools (e.g. `bash`) the raw argument deltas are buffered and matched. `globs` + `tool:x(glob)` path scopes use the file path(s) from the tool args.

**On match** (`interruptMode: always`, default): stream aborted immediately, partial output discarded (`contextMode: discard`), rule body injected as
`<system-interrupt reason="rule_violation" rule="…" path="…">…</system-interrupt>`, generation retried after 50 ms.
`interruptMode: never`: tool-source match → `<system-reminder reason="rule_violation" …>` prepended to that tool's result (the tool still runs!); prose match → hidden follow-up message after the reply.

**Repeat semantics** (`ttsr.repeatMode`): `once` (default) — a rule fires at most once per session (persisted as `ttsr_injection` entries, restored on resume); `after-gap` — re-fires after `ttsr.repeatGap` (default 10) completed turns. ⚠ With `once`, a *second* `.unwrap()` later in the session will NOT be caught. For hard project constraints set `repeatMode: after-gap` with a small gap in `.omp/config.yml` (see §6), and back it with clippy lints (TTSR is advisory steering, not a gate).

Registration is skipped (warning only) on invalid regex, duplicate name, or a scope that excludes all streams.

### Verified TTSR rules for `oh-my-clear`

All three verified with `omp ttsr test --rule <file> …` (outputs quoted below) and loaded as `[native]` from `.omp/rules/` in a throwaway repo.

**A) `.omp/rules/rs-no-panic.md`** — regex, catches unwrap/expect/panicking macros written into `*.rs` via edit/write:
```markdown
---
description: Never write panicking Rust (unwrap/expect/panic!/todo!/unimplemented!/unreachable!)
condition:
  - "\\.unwrap\\(\\)"
  - "\\.expect\\("
  - "\\.unwrap_err\\(\\)"
  - "\\.expect_err\\("
  - "\\.unwrap_unchecked\\(\\)"
  - "\\b(?:panic|todo|unimplemented|unreachable)!\\s*[\\(\\[\\{]"
scope: "tool:edit(*.rs), tool:write(*.rs)"
---

This project forbids panicking code. Propagate errors with `?` and the project error type;
use `ok_or`/`ok_or_else`, `unwrap_or`/`unwrap_or_else`/`unwrap_or_default`, `let … else`,
`.get(i)` instead of indexing, and return an error instead of `panic!`/`todo!`/`unreachable!`.
```
Observed:
- `let x = foo().unwrap();` (edit, src/lib.rs) → `✓ rs-no-panic condition: /\.unwrap\(\)/`
- `let x = foo().expect("boom");` (write) → `✓ … /\.expect\(/`
- `panic!("x")` → `✓ … /\b(?:panic|todo|unimplemented|unreachable)!\s*[\(\[\{]/`
- `foo().unwrap_or_default()` → No rules triggered
- `foo().unwrap()` with `--path README.md` → not triggered (path scope works); `--source text` → not triggered (prose explanations are allowed).
- Regex also fires inside comments/strings (accepted false positive). YAML note: backslashes are doubled inside double-quoted YAML strings (same as builtin `rs-parking-lot.md`).

**A′) AST variant (optional, fewer false positives)** `.omp/rules/rs-no-panic-ast.md`:
```markdown
---
description: Never write panicking Rust (AST)
astCondition:
  - "$X.unwrap()"
  - "$X.expect($$$A)"
  - "panic!($$$A)"
  - "todo!($$$A)"
  - "unimplemented!($$$A)"
  - "unreachable!($$$A)"
scope: "tool:edit(*.rs), tool:write(*.rs)"
---
```
Observed: fires on `foo().unwrap()`, `b.expect("x")`, `todo!()`; ignores `// foo().unwrap()` and `"a.unwrap()"`; **misses `println!("{}", x.unwrap())`** (macro token trees aren't parsed). ⇒ prefer the regex rule A. Don't mix both in one rule: regex and AST triggers are OR'ed, so the regex false positives remain.

**B) `.omp/rules/rs-no-index.md`** — AST, flags panicking indexing/slicing (non-interrupting reminder, since it's noisier):
```markdown
---
description: Avoid panicking index/slice expressions in Rust; use .get()/.get_mut()/iterators
astCondition: "$X[$I]"
scope: "tool:edit(*.rs), tool:write(*.rs)"
interruptMode: never
---

`v[i]` and `&s[a..b]` panic on out-of-bounds. Use `.get(i)`, `.get(a..b)`, `.first()`, `split_at_checked`, or iterators.
```
Observed: fires on `v[0]` and `&s[1..]`; does NOT fire on array type/repeat `[u8; 4] = [0; 4]` or `v.get(0)`.

**C) `.omp/rules/no-cargo-test.md`** — catches `cargo test` in bash tool args; allows `cargo test --doc` (nextest cannot run doctests):
```markdown
---
description: Run tests with cargo nextest, never cargo test
condition: "\\bcargo\\s+(?:\\+\\S+\\s+)?test\\b(?![^\\n\"]*--doc)"
scope: "tool:bash"
---

This repo runs tests with cargo-nextest only: `cargo nextest run` (add `--workspace`, `-p <crate>`, `-E '<filterset>'` as needed).
Doctests are the one exception: `cargo test --doc`.
```
Observed: `cargo test --workspace` → `✓ no-cargo-test`; `cargo nextest run` → not triggered; `cargo test --doc` → not triggered; prose "run cargo test" (`--source text`) → not triggered. (`scope: "tool:bash"` without a path glob is accepted — it appears as `scope: tool:bash` in `omp ttsr list`.) If you also want to catch it when the model *writes* it into docs/scripts, add `"tool:edit(*.md), tool:write(*.md), tool:edit(*.sh), tool:write(*.sh)"` or a separate rule.

Stronger alternative for C: a `.omp/hooks/pre/no-cargo-test.ts` factory with `pi.on("tool_call", …)` returning `{ block: true, reason }` for `event.toolName === "bash"` — actually blocks execution (hooks.md), whereas TTSR only interrupts/re-steers. Requires TS in repo; [UNVERIFIED] end-to-end here.

### Builtin rules relevant/conflicting (`omp ttsr list`, provider `builtin-defaults`)
`rs-box-leak`, `rs-future-prelude`, `rs-lazylock` (prefer `std::sync::LazyLock`), `rs-match-ergonomics`, `rs-result-type` (`type Result<T, E = …>`), **`rs-parking-lot`** (on `.lock().unwrap()` suggests adding `parking_lot` — a new dependency; conflicts with the "one reviewed dependency per category" policy unless parking_lot is approved). Disable per name via `ttsr.disabledRules: [rs-parking-lot]` or shadow it with a same-named project rule. Also note `rs-result-type` hard-codes `anyhow::Error` in its example — fine only if anyhow is the chosen error crate.

### CLI for maintaining rules
`omp ttsr list` · `omp ttsr test --rule .omp/rules/x.md --source tool --tool edit --path src/lib.rs '<snippet>'` · `omp ttsr scan [dir]` (scan the tree for existing violations; skips `question` rules) · `/omfg` in-session generates rules from the conversation.

---

## 4. Skills and custom agents (project-scoped)

### `.omp/skills/<skill-name>/SKILL.md`
Frontmatter (skills.md):
```markdown
---
name: memory-update
description: Use when finishing a task that produced a durable lesson, decision, or gotcha worth recording in the repo memory
---

# Steps
…
```
Fields: `name` (defaults to dir name), `description` (**required** for native `.omp`), `globs` (string[]), `alwaysApply` (bool), `hide` (bool: omit from prompt list but still reachable), `disableModelInvocation` / `disable-model-invocation`; other keys preserved. Exposed to the model as name+description in the system prompt (when `read` tool available), body read via `skill://<name>`, assets via `skill://<name>/<rel>`; user invokes `/skill:<name> [args]`. Dedup by name (native 100 wins). Layout is non-recursive. Settings gate: `skills.enabled` (true), `skills.enablePiProject` (true; gates native project skills), `skills.includeSkills`/`ignoredSkills`, `disabledExtensions: [skill:<name>]`.

### `.omp/agents/<name>.md` (task subagents)
Verified from bundled `src/prompts/agents/*.md` and `task-agent-discovery.md`:
```markdown
---
name: rust-reviewer
description: Reviews Rust changes for panics, dependency-gate violations, and nextest usage
tools: read, grep, glob, find, bash, lsp, ast_grep
spawns: scout
model: "@slow"
thinking-level: high
autoloadSkills: [rust-no-panic]
---

System prompt body…
```
Required: `name`, `description` (else skipped with warning). Optional: `tools` (CSV/array; `yield` auto-added), `spawns` (`*`/CSV/array), `model` (selector/CSV/array, `@role` aliases via `modelRoles`), `thinking-level`, `output` (JTD-like schema, see bundled reviewer), `blocking: true`, `autoloadSkills`, `read-summarize: false`, `prewalk`, `advisor`. Names `main` and `sub` are reserved. Project agents override user and bundled agents of the same name (so a project `reviewer.md` replaces bundled `reviewer`).

TTSR/rules can be restricted to such agents with `agents: [rust-reviewer]`.

---

## 5. Built-in memory features

| Feature | Stores | Where | Project-scoped? | Default | Repo-committable? |
|---|---|---|---|---|---|
| `memory.backend: local` | Per-session extraction → consolidated `MEMORY.md`, `memory_summary.md` (injected as "Memory Guidance"), `skills/`; plus `learned.md` from `learn` | `<agent-dir>/memories/<encoded-cwd>/…` (read via `memory://root`, `memory://root/MEMORY.md`, `memory://root/learned.md`) | yes (by cwd) | **off** | **No** — home dir |
| `learn` tool | One durable lesson (≤2000 chars, ≤100 entries, newest-first, redacted) + optional managed skill | local: `learned.md` above; managed skills `~/.omp/agent/managed-skills/<name>/SKILL.md` | project (local) / bank | needs `autolearn.enabled: true` **and** a backend | No |
| `retain`/`recall`/`reflect` | Facts in Hindsight server or Mnemopi SQLite | Hindsight remote; Mnemopi `<agent-dir>/memories/mnemopi/…db` | bank scoping (`per-project` default for mnemopi, `per-project-tagged` for hindsight) | only for `hindsight`/`mnemopi` | No |
| `memory_edit` | update/forget/invalidate Mnemopi rows | SQLite | — | only `mnemopi` | No |
| `sharpshooter` backend | `architecture.md`/`product.md`/`style.md` decision files | `<agentDir>/memories/sharpshooter/<bank>/` (source: `src/sharpshooter/paths.ts`: "never the project working tree") | yes | off | No |
| `context_notes` | ≤16 KiB per-branch notebook surviving context rollover | session journal (`experimental_context_notes` entries) | session/branch only | needs `compaction.experimentalContextManagement: true` (default false) | No |
| `autolearn.*` | Nudges agent after stop to `learn` / `manage_skill` | managed skills in home | — | `autolearn.enabled: false` | No |

Current machine: `memory.backend = off`, `autolearn.enabled = false`, `compaction.experimentalContextManagement = false` (`omp config list --json`); user config.yml sets none of these.

**Conclusion:** none of omp's memory backends write into the repo or can load a repo file as memory. They are per-user/per-machine and complementary. A committed, team-shared, self-iterating memory must be plain files that omp auto-loads through context mechanisms.

### Recommended repo-hosted self-iterating memory (auto-loaded by omp)
1. Files, e.g. `docs/memory/` (or `.agents/memory/`): `INDEX.md` (short curated summary, always loaded), plus topic files `decisions.md`, `lessons.md`, `gotchas.md`, `dependencies.md` (dependency review ledger) — each entry dated with a stable id.
2. **Auto-load** via `@` imports in root `AGENTS.md`: a line `Project memory: @docs/memory/INDEX.md` expands inline at session start (relative to the importing file; recursive ≤5 hops; cycles skipped; missing target leaves the literal token; tokens inside code spans/fences are NOT expanded — so don't wrap it in backticks). Keep `INDEX.md` small (it costs tokens every session) and let it name the topic files *without* `@` so they're read on demand — or `@`-import only the hot ones.
3. **Rulebook entry** for the protocol: `.omp/rules/memory-protocol.md` with `description: Read before finishing any task — how to record lessons/decisions in docs/memory` (listed every session, body on demand), or put a 2–3 line "update memory" requirement in `.omp/RULES.md` so it never scrolls out of context.
4. **Skill** `.omp/skills/memory-update/SKILL.md` encoding the write procedure (dedupe, prune, promote a lesson into a rule/TTSR rule when it recurs, keep INDEX.md ≤ N lines). Self-iteration loop: agent finishes task → skill appends/edits entry → recurring lesson gets promoted to `.omp/rules/*.md` (TTSR if mechanically detectable; validate with `omp ttsr test`) → INDEX.md updated.
5. Re-load semantics: `@` imports/AGENTS.md are read at session start; `RULES.md` also on `/clear`/`/new`. Memory edited mid-session is visible next session (or by explicit `read`).
6. Optionally also enable per-user `memory.backend: local` + `autolearn.enabled: true` in a personal config — orthogonal; don't put it in the committed project config unless the team agrees (it spends extra model tokens at startup on consolidation).

---

## 6. Project settings (`.omp/config.yml`) relevant keys (exact, from `omp config list --json`)

```yaml
# .omp/config.yml  — cwd-only, launch omp from repo root
ttsr:
  enabled: true            # default true
  interruptMode: always    # never | prose-only | tool-only | always (default always)
  contextMode: discard     # discard | keep (default discard)
  repeatMode: after-gap    # once (default) | after-gap  — after-gap so repeated violations are re-caught
  repeatGap: 2             # completed turns before a rule may fire again (default 10)
  builtinRules: true       # default true
  disabledRules:           # array REPLACES lower layers
    - rs-parking-lot       # only if parking_lot is not the approved lock crate
  judge: auto              # auto | on | off — only matters for `question` rules

# memory (all default off; per-user choice, see §5)
# memory:
#   backend: local          # off | local | hindsight | mnemopi | sharpshooter
# autolearn:
#   enabled: false
#   autoContinue: false
#   minToolCalls: 5
# memories:
#   summaryInjectionTokenLimit: 5000
# compaction:
#   experimentalContextManagement: false   # enables context_notes tool

skills:
  enabled: true
  enablePiProject: true    # gates native .omp project skills (legacy name)
```
Notes: `ttsr.*` enum values from ttsr-injection-lifecycle.md §1/§3/§5 and config list (enum values for `contextMode`/`repeatMode`/`interruptMode` confirmed by docs; `judge` values from lifecycle §10). Arrays replace, objects deep-merge. `omp config set` never writes project keys — edit the file by hand. Invalid project YAML is moved to `.broken-*` and startup fails. `ttsr.repeatGap: 2` is my suggestion, not a doc default.
