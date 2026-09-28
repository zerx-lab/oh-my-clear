# Memory index
<!-- CORE memory, auto-loaded via AGENTS.md. Budget ≤80 lines / ≤6 KB. One line per entry; detail lives in linked files. Procedure: skill://memory -->
Last consolidated: 2026-09-28

Memory records development rules and their reasons — never progress, milestones, schedules, roadmaps or session logs. It is heuristic context, not ground truth: verify against the repo before acting; if the repo or the user contradicts it, they win — fix or delete the stale entry.

## Files (docs/memory/, read on demand)
- open-questions.md — undecided development questions (no progress/plans)
- lessons.md — non-obvious bugs, gotchas, errata (grep by tag)
- deps.md — dependency ledger: one crate per category + review evidence
- glossary.md — domain vocabulary (Daemon/Engine/Endpoint/Runtime dir/…)
- decisions/ — ADRs; research snapshots in docs/research/

## Decisions
- 0001 Record decisions as ADRs; file-based self-iterating memory in docs/memory (accepted)
- 0002 GUI: gpui-kit only, exact pin, no direct gpui crates (accepted)
- 0003 No-panic policy enforced by clippy restriction lints + TTSR (accepted)
- 0004 One crate per category; dependency gate = deny.toml + skill://dep-review; tokio, thiserror-only (accepted)
- 0006 cargo-nextest is the only test runner; doctests disabled (accepted)
- 0007 Virtual workspace (apps/*, crates/*, xtask), `cargo ci`, dev/profiling profiles, tracing+panic hook, Zed nextest/CodeLLDB wiring (accepted)
- 0008 Two processes (`oh-my-clear` GUI + headless `oh-my-clear-daemon`), tokio UDS/named-pipe IPC with token auth, JSON control frames, epoch resync; crate layering in xtask (accepted)
- 0011 UI first: OKLCH tokens, Inter/JetBrains Mono, Apple spring presets, reduced motion, frame budgets (accepted)
- 0012 Docs/memory record development rules only — no progress, milestones, schedules, roadmaps, session logs; TTSR `docs-no-schedule` (accepted; amends 0001)
- 0013 UI foundation: gpui-component JSON theme presets + app style layer (OKLCH accent), `UiSettings` global, rust-i18n en/zh-CN following the OS, TitleBar-based chrome, every command an Action (accepted; amends 0011 colour authoring; chrome amended by 0019)
- 0018 App icons from `assets/brand/` (render.sh): macOS `.app` via `cargo omc` (no `unsafe` Dock API), Windows icon resource 1 via embed-resource, Linux X11 `WindowOptions::icon` + desktop entry for Wayland (accepted)
- 0017 Product = oh-my-clear, a cross-platform (macOS/Windows/Linux) system cleaner; agent-orchestration scope, crates and ADRs removed; `omc-*` crates, no `unsafe` crate (accepted)
- 0019 Main window: bare transparent titlebar (window chrome only) over a full-height sidebar of cleaning areas (`nav::NAV`), spring-collapsed off-canvas, settings in its footer (accepted; amends 0013 chrome)

## Hot lessons
- L-0001 Never create .omp/AGENTS.md or CLAUDE.md — they shadow/compete with root AGENTS.md (omp)
- L-0002 `cargo nextest run` exits 4 when zero tests match (e.g. `-p` on an empty crate); use `--no-tests=warn` there (nextest)
- L-0003 TTSR default repeatMode `once` misses repeat violations; project config uses after-gap (omp)
- L-0010 `strip = "debuginfo"` drops line tables unless `split-debuginfo = "packed"` (build)
- L-0007 rs-no-panic TTSR fires inside `#[cfg(test)]` too; write tests with `assert!(matches!(..))`/`is_ok_and` (omp, testing)
- L-0016 gpui `TestAppContext` panics on activity from other threads — test IPC in pure models / `tokio::io::duplex` (gpui, testing)
- L-0017 Icons outside gpui-kit's 104 default set render blank unless added to `omc_ui::assets` (gpui, assets)
- L-0023 Global action handlers must `cx.defer` before updating the dispatching window (gpui, actions)
