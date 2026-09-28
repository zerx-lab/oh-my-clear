# Repository Guidelines

## Project Overview
dial is a multi-agent orchestration desktop app: it runs coding agents in parallel, each in its own git worktree, and lets the user and the agents coordinate their work. It ships **its own native coding agent** (`dial-native`) for the tightest control over orchestration and task execution, and any **ACP-capable agent** (Claude Code, Codex, Gemini, OpenCode, Qwen, Copilot, …) plugs in through the same adapter trait. It is inspired by Orca (github.com/stablyai/orca) and Zeron (github.com/zeronsh/zeron). Targets: macOS, Windows (MSVC), Linux, all first-class, built in Rust on gpui-kit (https://gpui-kit.com/llms.txt). The UI is the product's first principle: refined, keyboard-first, Apple-style spring motion (ADR 0011).

Status: scaffolding stage. The ADR 0008 crate layout exists (see Key Directories); `dial-ghostty` builds libghostty-vt and wraps it, `dial-telemetry` is shared by both binaries, the other library crates are still empty. Both binaries only set up logging and exit. There is no gpui-kit window yet.

## Architecture & Data Flow
Accepted architecture: ADR 0008 (process split, IPC, layering), 0009 (native agent + ACP + MCP), 0010 (terminal), 0011 (UI/motion).
- **Two processes.** `dial-daemon` (headless, tokio, never links gpui) owns the engine, agents, PTYs, terminals, worktrees and the journal, and keeps running when the UI closes. `dial` (gpui-kit) is a viewport: it auto-spawns/attaches to the daemon over local IPC (Unix socket / Windows named pipe, token-authenticated) and can quit and reattach any time.
- **Crates.** Allowed internal edges live in `LAYERS` (`xtask/src/layers.rs`), checked by `cargo xtask layers` (first step of `cargo ci`); `gpui-kit` is confined to dial-ui/apps/dial, `alacritty_terminal` (PTY only) to dial-process. Leaf I/O crates never depend on dial-core; the UI never depends on the engine or adapters. Declare an internal edge only when code uses it:
  - add `dial-x = { path = "crates/dial-x" }` to root `[workspace.dependencies]`;
  - add `dial-x.workspace = true` to the member (and allow it in `LAYERS` if new).
- **Flow.** UI → `Command` (dial-ipc JSON frame) → daemon router → journal append → session actor → `AgentAdapter` (native in-process · ACP child over stdio · PTY fallback) → `AgentEvent` (upserts by id) → journal + fold → per-stream `EngineEvent` with `seq` → attached UIs (`cx.notify` at most once per frame). PTY bytes travel as raw binary frames; reattach = journal replay from `since`, or snapshot + live.
- **Orchestration model** (from Orca): Run / Task / Dispatch / Message / Gate. The native agent reaches it in-process via `OrchestratorPort`; third-party agents via the daemon's MCP server (loopback HTTP, or stdio through `dial-daemon mcp`). One tool-spec source in dial-proto. Terms are defined in `docs/memory/glossary.md`; use them exactly.
- **Runtimes.** Daemon: one tokio runtime for all I/O; each terminal is owned by its PTY thread (`dial_ghostty::Terminal` is `Send`, not `Sync`). UI: GPUI executors for UI/state; a tokio runtime in a GPUI `Global` only for the IPC client, bridged by bounded `async-channel`s or awaited `JoinHandle`s.

## Key Directories
- `apps/dial/`: the GUI binary (viewport + IPC client).
- `apps/dial-daemon/`: the headless execution daemon (planned subcommands `run|status|stop|logs|mcp|install-login-item`).
- `crates/dial-*/`: library crates, one per area: proto, core, ipc, telemetry, process, ghostty (the only `unsafe` crate), term, git, store, llm, agent, native, mcp, engine, ui. The `crates/*` glob adds members automatically. Create new ones with `cargo new-crate <area> "<purpose>"`, then add them to `LAYERS`.
- `third_party/ghostty/`: Ghostty source as a pinned, shallow git submodule; `dial-ghostty`'s `build.rs` builds libghostty-vt from it with Zig.
- `xtask/`: std-only dev automation (`cargo xtask ci | layers | new-crate`).
- `.github/workflows/ci.yml`: the gates on macOS, Windows (MSVC) and Linux.
- `.cargo/config.toml`: cargo aliases and dev env defaults (`RUST_LOG=info,dial=debug`, `RUST_BACKTRACE=1`; a value set in your shell wins).
- `.zed/`: Zed project config:
  - `settings.json`: rust-analyzer runs clippy, in its own `target/rust-analyzer` dir.
  - `tasks.json`: nextest tasks that replace the gutter `cargo test` runnables.
  - `debug.json`: CodeLLDB scenarios.
- `.omp/`: omp harness config. `RULES.md` holds the sticky hard rules. `rules/` holds TTSR rules, the always-apply rule, and rulebook rules. `skills/` has `memory` and `dep-review`. `config.yml` holds the TTSR settings.
- `docs/memory/`: project memory: development rules and their reasons (index, ADRs in `decisions/`, lessons, deps ledger, glossary, open questions) — never progress, milestones, schedules or session logs.
- `docs/research/`: dated research snapshots (gpui-kit, Zeron/Orca, Rust gates, memory, omp config, libghostty-vt, native agent, ACP, daemon/IPC, UI motion). Treat them as evidence, not as current truth.
- `.config/nextest.toml`: test-runner profiles.

## Development Commands
**Prerequisites:** Rust (pinned by `rust-toolchain.toml`), cargo-nextest, cargo-deny, **Zig 0.16.x** (exact minor; e.g. `brew install zig`), and the Ghostty submodule, once per clone: `git submodule update --init --depth 1 third_party/ghostty` then `zig build --build-file third_party/ghostty/build.zig --fetch=all` (after that, builds are offline; `build.rs` errors with these commands when something is missing). Windows also needs MSVC Build Tools; the MSVC libghostty-vt archive can only be built on Windows.

Run all gates with `cargo ci` (xtask). It runs `xtask layers` and then the four commands below in order, stopping at the first failure. Work is not done until they are all green:
```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings   # alias: cargo lint
cargo nextest run --workspace --all-features --locked                           # alias: cargo t
cargo deny --all-features check
```
Aliases and other commands:
- **Run**: `cargo dial` (GUI; also bare `cargo run`, since `default-members = ["apps/dial"]` — always pass `--workspace` for full coverage) and `cargo daemon` (the daemon in the foreground).
- **Log level**: `RUST_LOG=trace cargo dial`, or scope it, e.g. `RUST_LOG=dial_engine=trace`. The prefix `dial` matches every `dial_*` crate.
- **Tests for one crate**: `cargo nextest run -p dial-core`. Filter with `-E 'test(<name>)'` or pass a positional substring.
  - Nextest exits 4 when zero tests match (L-0002).
- **Debugging**:
  - Zed: F4 opens the `.zed/debug.json` scenarios.
  - Terminal: `cargo nextest run -p <crate> --debugger "rust-lldb --" <test>` or `rust-lldb target/debug/dial`.
  - Dependencies carry only line tables. To step into one, rebuild with `CARGO_PROFILE_DEV_PACKAGE_<NAME>_DEBUG=full`.
- **Profiling build**: `cargo build --profile profiling -p dial` gives release speed with full symbols (Instruments or samply).
- **CI tests**: `cargo nextest run --workspace --all-features --locked --profile ci` writes `target/nextest/ci/junit.xml`.
- **New crate**: `cargo new-crate <area> "<one-line purpose>"` creates `crates/dial-<area>` with workspace lints and `doctest = false`.
- **TTSR rules**: maintain them with `omp ttsr list` and `omp ttsr test --rule .omp/rules/<r>.md --source tool --tool edit --path crates/x/src/lib.rs '<snippet>'`.
- **Launch omp from the repo root.** `.omp/rules` and `.omp/config.yml` are only read from the cwd.

## Code Conventions & Common Patterns
- **No panics** (ADR 0003, enforced by `[workspace.lints]`). Outside `#[cfg(test)]` these are all forbidden:
  - `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!`, `unreachable!`
  - indexing and slicing: `v[i]`, `&s[a..b]`
  - unchecked arithmetic
  - `exit`, `mem_forget`

  Use these instead: `?`, `ok_or_else`, `let … else`, `.get()`, `checked_*`/`saturating_*`, `split_at_checked`.
- **Lint suppression** is only allowed as `#[expect(clippy::x, reason = "…")]` on the narrowest item. `#[allow]` fails the build.
- **Errors**:
  - `thiserror` is the only error crate. Each crate defines an `Error` enum and `pub type Result<T, E = Error> = std::result::Result<T, E>;`.
  - "Impossible" states return an error variant.
  - GPUI's `anyhow::Result` gets converted with `map_err` at the UI edge. anyhow is never a direct dependency.
- **Discarded results**: `let _ = must_use` and `.ok();` are denied. Handle the `Err` case, at minimum by logging it with `tracing`.
- **GPUI/gpui-kit**: read `rule://gpui-patterns` before writing UI code. Key points:
  - Use `KeyBinding::load`, not `new`.
  - Use `try_global`, not `global`.
  - Store the `Task` (dropping it cancels the work).
  - Never update an entity inside its own update or render.
  - Create child state entities in `new`, not in `render`.
- **Async**: never hold a lock or a `RefCell` borrow across `.await` (both are denied). Use `parking_lot` locks, which don't poison.
- **Statics**: use `std::sync::LazyLock`/`OnceLock`. `lazy_static` and `once_cell` are banned.
- **Logging**: use `tracing` only. `println!`, `eprintln!`, and `dbg!` are denied.
- **Formatting and naming**: `rustfmt.toml` sets style edition 2024. Crates are named `dial-<area>`. Files and modules use snake_case.
- **Unsafe**: `unsafe_code = "forbid"` everywhere except the unsafe islands in `UNSAFE_ISLANDS` (today `dial-ghostty`, ADR 0010), whose own lint table must equal `[workspace.lints]` except `unsafe_code = "deny"` (checked by `xtask layers`). Every `unsafe` block has a `// SAFETY:` comment and one unsafe op.
- **UI**: read `rule://ui-design-motion` before UI work: tokens only, springs from the `const` preset table, no layout animation, reduced motion, 4 ms/8 ms frame budgets.
- **Daemon boundary**: permission requests and Gates are durable journal entries (no UI may be attached). Daemon stdout is protocol-only (`READY` line, stdio MCP proxy); logs go to stderr/files via dial-telemetry.

## Important Files
- `Cargo.toml`: virtual workspace root. It holds:
  - the member globs;
  - `[workspace.package]`: version, edition, and `rust-version`, inherited by every member;
  - `[workspace.dependencies]`: every dependency is declared here, and members use `x.workspace = true`;
  - `[workspace.lints]`, inherited by every member through `[lints] workspace = true`;
  - the profiles: dev (full debuginfo for workspace crates, line tables for dependencies, optimized build scripts), release (`panic = "unwind"`, line tables in a packed side file), and profiling.
- `clippy.toml`: test-only escapes for unwrap, expect, panic, and indexing, plus the `arithmetic-side-effects-allowed` list.
- `deny.toml`: advisories, license allowlist, one-per-category bans (with gpui `wrappers`; also the rejected IPC/ACP/LLM/PTY/libghostty crates), `std-replacements`, crates.io-only sources.
- `xtask/src/layers.rs`: `LAYERS` (allowed internal edges), `CONFINED` external crates, `UNSAFE_ISLANDS`.
- `rust-toolchain.toml`: pins toolchain 1.98.0. Bump it together with `rust-version` and re-check the lints.
- `.omp/RULES.md`: non-negotiables sent with every request.
- `.omp/rules/*.md`: `rs-no-panic`, `rs-no-index`, `rs-expect-not-allow`, `rs-result-type`, `rs-unsafe-island`, `gpui-panicking-apis`, `term-no-alacritty-emulator`, `no-cargo-test`, `deps-banned-crates`, `deps-manifest-edit`, `deps-cargo-cli`, `docs-no-schedule` (TTSR); `memory-protocol` (always-apply); `gpui-patterns`, `ui-design-motion` (rulebook).
- `.omp/skills/dep-review/SKILL.md` and `.omp/skills/memory/SKILL.md`: the procedures for dependencies and memory.

## Runtime/Tooling Preferences
- **Rust toolchain**: stable 1.98.0 (pinned), edition 2024, resolver 3. Windows builds must use MSVC.
- **GUI**: `gpui-kit` only, pinned `=X.Y.Z` with `Cargo.lock` committed. Never add `gpui`, `gpui-pre*`, or `gpui-component` directly.
- **Dependencies**: one crate per category (ADR 0004; ledger in `docs/memory/deps.md`). Before any `Cargo.toml` dependency change, run `skill://dep-review`. Reuse the crates gpui already pulls in (parking_lot, chrono, uuid, async-channel, serde_json).
  - Chosen crates: tokio (+ tokio-util), reqwest (rustls), serde/serde_json, tracing (+ tracing-appender for daemon logs), thiserror, agent-client-protocol-schema (+ own JSON-RPC driver), rmcp, alacritty_terminal (`tty` only), process-wrap, rusqlite, keyring, ripgrep libs (ignore/globset/grep-*), and the `git` CLI instead of git2/gix. Provider clients and SSE are hand-rolled in dial-llm.
  - Non-cargo build dependency: Zig 0.16.x for libghostty-vt (ADR 0010).
- **Tools**: cargo-nextest ≥0.9.131, cargo-deny 0.20 and Zig 0.16.x are required. cargo-shear for unused dependencies is proposed but not installed.
- **Agent context files**: do not create `.omp/AGENTS.md` or `CLAUDE.md` (they shadow this file), and do not add `.cursor`, `.clinerules`, or copilot instruction duplicates.

## Testing & QA
- **Test runner**: cargo-nextest only (ADR 0006). `cargo test` is intercepted by TTSR. Set `doctest = false` in every `[lib]`. Rustdoc code blocks use `ignore` or `text`; examples live in nextest tests.
- **Unit tests** go in `#[cfg(test)] mod tests` next to the code. Clippy allows unwrap, expect, panic, and indexing in tests, but the `rs-no-panic` TTSR regex cannot see `cfg(test)` and fires anyway. So write assertions like `assert!(matches!(r, Err(Error::X(_))), "…")` or `assert!(r.is_ok_and(…), "…")`. `todo!` and `unreachable!` are never allowed. Every assertion needs a message (`missing_assert_message`).
- **Debugging tests in Zed**: the Zed debug scenario compiles test binaries with `cargo test --no-run`. This only builds them, because Zed cannot locate nextest binaries. Execution always goes through nextest.
- **UI tests**: `#[gpui_kit::test] fn t(cx: &mut TestAppContext)`, with `gpui-kit` features `["test-support"]` in dev-dependencies. Import explicitly in test modules, because `use gpui_kit::*` shadows `#[test]`.
- **Engine tests**: test engine and orchestration logic headless through the `mock` adapter (dial-agent `test-support` feature), the pure `dial-core` reducers, and dial-ipc over `tokio::io::duplex`.
- **Terminal tests**: `dial-ghostty`/`dial-term` tests feed bytes and assert on formatter output and snapshots; they need Zig and the submodule.
- **Coverage**: no numeric coverage target. Every behavior change needs a test that would fail without it (state transitions, error paths, boundaries).
- **CI profile**: retries with `flaky-result = "fail"`, so flaky tests fail CI.
- **GUI changes**: a passing build and passing tests are the bar. State explicitly that there was no visual verification.

## Memory
Project memory lives in docs/memory/. The index below is auto-loaded; read everything else on demand with read or grep.
- Before non-trivial work, check the index and read the ADRs and lessons for the area you are touching.
- Docs and memory record development rules, decisions and their evidence only. Never write progress, milestones, schedules, roadmaps, phase plans, next-step lists or session logs anywhere in the repo (user rule; enforced by TTSR `docs-no-schedule`; `git log` is the history).
- Memory is heuristic. The repo state and the user take precedence. Fix or delete stale entries in the same change.
- Write triggers are in rule://memory-protocol. Procedure, templates, consolidation, and promotion into lints and TTSR are in skill://memory.
- Subagents do not edit docs/memory/. They return "Memory candidates" instead.

@docs/memory/INDEX.md
