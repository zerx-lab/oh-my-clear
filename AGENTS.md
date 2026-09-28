# Repository Guidelines

## Project Overview
oh-my-clear is a cross-platform system-cleaning desktop app (ADR 0017). Targets: macOS, Windows (MSVC), Linux, all first-class, built in Rust on gpui-kit (https://gpui-kit.com/llms.txt). The UI is the product's first principle: refined, keyboard-first, Apple-style spring motion (ADR 0011).

Crates: `omc-telemetry` (tracing setup shared by both binaries), `omc-proto` (wire types: control frames, meta requests, daemon → UI events, errors), `omc-ipc` (runtime dir, lock, framing, token handshake, `EngineHandle` supervisor that spawns/reattaches/restarts the daemon, detached spawn, executable layout), `omc-scan` (parallel walker, junk catalogues, space lens, large/old files, duplicates, deletion, removal guard), `omc-apps` (per-OS app inventory, related files/registry, uninstall, leftovers, startup items, removal executor, elevated helper), `omc-engine` (the daemon's connection router, job manager, settings store and UI event broadcast), `omc-ui` (ADR 0013 foundation: theme, i18n, titlebar, settings window, actions, plus the main window (ADR 0019): collapsible sidebar of cleaning areas, area pages, status bar with the daemon connection). `oh-my-clear-daemon` serves `run` and `stop` and owns the system tray (ADR 0020).

## Architecture & Data Flow
Accepted architecture: ADR 0008 (process split, IPC, layering), 0011 (UI/motion), 0013 (theme, i18n, chrome), 0017 (product scope), 0020 (tray in the daemon), 0021 (cleaning jobs, removal, elevation), 0022 (UI v2 components, control scale, scan store).
- **Two processes.** `oh-my-clear-daemon` (tokio, never links gpui) owns the engine, all filesystem/system work and the system tray, and keeps running when the UI closes. `oh-my-clear` (gpui-kit) is a viewport: it auto-spawns/attaches to the daemon over local IPC (Unix socket / Windows named pipe, token-authenticated) and quits entirely with its last window; the tray's Open reactivates or relaunches it, the tray's Quit ends both.
- **Tray (ADR 0020).** `host.rs` puts a tao event loop on the daemon's main thread on macOS/Windows (accessory app on macOS) and runs the daemon loop on tokio; Linux uses tray-icon's pure-Rust `ksni` backend (no GTK, no event loop). The tray only enqueues `TrayEvent`s; with a tray the daemon has no idle exit. On macOS the daemon lives in a helper bundle (`omc_ipc::layout`).
- **Crates.** Allowed internal edges live in `LAYERS` (`xtask/src/layers.rs`), checked by `cargo xtask layers` (first step of `cargo ci`); `gpui-kit` is confined to omc-ui/apps/oh-my-clear, `tray-icon`/`tao` to oh-my-clear-daemon. The UI never depends on the engine. Declare an internal edge only when code uses it:
  - add `omc-x = { path = "crates/omc-x" }` to root `[workspace.dependencies]`;
  - add `omc-x.workspace = true` to the member (and allow it in `LAYERS` if new).
- **Flow.** UI → `Request` (omc-ipc JSON control frame) → daemon `Engine` router → `Response` on the same connection → UI (`cx.notify` at most once per frame). The daemon pushes `Event`s (`activate`, `quit`, `job` progress, `settings_changed`) to `ui` clients unsolicited. A new daemon `epoch` means every client cache is stale.
- **Jobs (ADR 0021).** Scans, cleans, app inventory, uninstall and startup changes are daemon jobs: `start_job` → `job` events → `job_result` → `release_job`. Removal only takes item ids of a retained scan (never paths from the client), every path passes `omc_scan::Guard`, and admin-only items go to one `oh-my-clear-daemon elevated <manifest>` run. Platform code lives in `omc-apps/src/{macos,windows,linux}/`; Windows/Linux code is compile-checked here with `cargo clippy --target x86_64-pc-windows-msvc|x86_64-unknown-linux-gnu`. Settings are daemon-owned (`settings.toml`).
- **Runtimes.** Daemon: one tokio runtime for all I/O. UI: GPUI executors for UI/state; a tokio runtime in a GPUI `Global` (`omc_ui::engine`) only for the IPC client. Its `tokio::sync` channel/oneshot futures are awaited directly in GPUI tasks.

## Key Directories
- `apps/oh-my-clear/`: the GUI binary (viewport + IPC client).
  - `resources/`: platform app icons (ADR 0018): macOS `Info.plist` + `AppIcon.icns`, the Windows `.rc`/`.ico` embedded by `build.rs`, the Linux desktop entry + hicolor icons (`share/`, copied into an install prefix).
- `assets/brand/`: logo sources (SVG). `render.sh` regenerates every derived raster (app icons, the in-app mark); commit its output.
- `apps/oh-my-clear-daemon/`: the execution daemon (`run` (default) and `stop`) with the tray (`src/host.rs`, `src/tray.rs`, `src/ui.rs`).
  - `assets/`: tray icons rendered by `assets/brand/render.sh`; `locales/app.yml`: tray menu strings; `resources/`: the macOS helper-bundle `Info.plist` and the Windows `.rc` embedding the app icon (`build.rs`).
- `crates/omc-*/`: library crates, one per area: proto, ipc, telemetry, scan, apps, engine, ui. The `crates/*` glob adds members automatically. Create new ones with `cargo new-crate <area> "<purpose>"`, then add them to `LAYERS`.
- `xtask/`: std-only dev automation (`cargo xtask ci | layers | new-crate | omc`).
- `.github/workflows/ci.yml`: the gates on macOS, Windows (MSVC) and Linux.
- `.cargo/config.toml`: cargo aliases and dev env defaults (`RUST_LOG=info,omc=debug,oh_my_clear=debug`, `RUST_BACKTRACE=1`; a value set in your shell wins).
- `.zed/`: Zed project config:
  - `settings.json`: rust-analyzer runs clippy, in its own `target/rust-analyzer` dir.
  - `tasks.json`: nextest tasks that replace the gutter `cargo test` runnables.
  - `debug.json`: CodeLLDB scenarios.
- `.omp/`: omp harness config. `RULES.md` holds the sticky hard rules. `rules/` holds TTSR rules, the always-apply rule, and rulebook rules. `skills/` has `memory` and `dep-review`. `config.yml` holds the TTSR settings.
- `docs/memory/`: project memory: development rules and their reasons (index, ADRs in `decisions/`, lessons, deps ledger, glossary, open questions) — never progress, milestones, schedules or session logs.
- `docs/research/`: dated research snapshots (gpui-kit, Rust gates, memory, omp config, daemon/IPC, UI motion). Treat them as evidence, not as current truth.
- `.config/nextest.toml`: test-runner profiles.

## Development Commands
**Prerequisites:** Rust (pinned by `rust-toolchain.toml`), cargo-nextest, cargo-deny. Windows also needs MSVC Build Tools.

Run all gates with `cargo ci` (xtask). It runs `xtask layers` and then the four commands below in order, stopping at the first failure. Work is not done until they are all green:
```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings   # alias: cargo lint
cargo nextest run --workspace --all-features --locked                           # alias: cargo t
cargo deny --all-features check
```
Aliases and other commands:
- **Run**: `cargo omc` (xtask: builds `oh-my-clear-daemon`, then runs the GUI, which spawns that daemon from its own target dir or restarts a running daemon of another build; on macOS it runs from `target/debug/oh-my-clear.app` so the Dock shows the icon, with the daemon in `Contents/Helpers/oh-my-clear-daemon.app`) and `cargo daemon` (the daemon in the foreground, with its tray). Bare `cargo run` builds only the GUI (`default-members = ["apps/oh-my-clear"]`); the GUI then runs `cargo build -p oh-my-clear-daemon` in the same profile itself before connecting (only when `cargo run` launched it). Always pass `--workspace` to other commands for full coverage. The tray's Quit or `target/debug/oh-my-clear-daemon stop` stops a running daemon; its log is `daemon.log` in the runtime dir (macOS `~/Library/Application Support/dev.zerx.oh-my-clear/run/`), and a tray-launched UI logs to `ui.log` there.
- **Log level**: `RUST_LOG=trace cargo omc`, or scope it, e.g. `RUST_LOG=omc_engine=trace`. The prefix `omc` matches every `omc_*` crate; `oh_my_clear` matches both binaries.
- **Tests for one crate**: `cargo nextest run -p omc-ipc`. Filter with `-E 'test(<name>)'` or pass a positional substring.
  - Nextest exits 4 when zero tests match (L-0002).
- **Debugging**:
  - Zed: F4 opens the `.zed/debug.json` scenarios.
  - Terminal: `cargo nextest run -p <crate> --debugger "rust-lldb --" <test>` or `rust-lldb target/debug/oh-my-clear`.
  - Dependencies carry only line tables. To step into one, rebuild with `CARGO_PROFILE_DEV_PACKAGE_<NAME>_DEBUG=full`.
- **Profiling build**: `cargo build --profile profiling -p oh-my-clear` gives release speed with full symbols (Instruments or samply).
- **CI tests**: `cargo nextest run --workspace --all-features --locked --profile ci` writes `target/nextest/ci/junit.xml`.
- **New crate**: `cargo new-crate <area> "<one-line purpose>"` creates `crates/omc-<area>` with workspace lints and `doctest = false`.
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
- **Formatting and naming**: `rustfmt.toml` sets style edition 2024. Crates are named `omc-<area>`; the binaries are `oh-my-clear` and `oh-my-clear-daemon`. Files and modules use snake_case.
- **Unsafe**: `unsafe_code = "forbid"` in every crate (inherited `[workspace.lints]`, checked by `xtask layers`). Prefer safe wrappers gpui already brings; introducing `unsafe` needs a new ADR (ADR 0017).
- **UI**: build screens from `omc_ui::ui` components (catalogue in `crates/omc-ui/src/ui/mod.rs`, ADR 0022); read `rule://ui-design-motion` before UI work: tokens only, springs from the `const` preset table, no layout animation, reduced motion, 4 ms/8 ms frame budgets.
- **Daemon boundary**: work that must survive the UI runs in the daemon (no UI may be attached). Daemon stdout is protocol-only (`READY` line); logs go to stderr/files via omc-telemetry.

## Important Files
- `Cargo.toml`: virtual workspace root. It holds:
  - the member globs;
  - `[workspace.package]`: version, edition, and `rust-version`, inherited by every member;
  - `[workspace.dependencies]`: every dependency is declared here, and members use `x.workspace = true`;
  - `[workspace.lints]`, inherited by every member through `[lints] workspace = true`;
  - the profiles: dev (full debuginfo for workspace crates, line tables for dependencies, optimized build scripts), release (`panic = "unwind"`, line tables in a packed side file), and profiling.
- `clippy.toml`: test-only escapes for unwrap, expect, panic, and indexing, plus the `arithmetic-side-effects-allowed` list.
- `deny.toml`: advisories, license allowlist, one-per-category bans (with gpui `wrappers`; also the rejected IPC crates), `std-replacements`, crates.io-only sources.
- `xtask/src/layers.rs`: `LAYERS` (allowed internal edges), `CONFINED` external crates, lint inheritance.
- `rust-toolchain.toml`: pins toolchain 1.98.0. Bump it together with `rust-version` and re-check the lints.
- `.omp/RULES.md`: non-negotiables sent with every request.
- `.omp/rules/*.md`: `rs-no-panic`, `rs-no-index`, `rs-expect-not-allow`, `rs-result-type`, `rs-no-unsafe`, `gpui-panicking-apis`, `no-cargo-test`, `deps-banned-crates`, `deps-manifest-edit`, `deps-cargo-cli`, `docs-no-schedule` (TTSR); `memory-protocol` (always-apply); `gpui-patterns`, `ui-design-motion` (rulebook).
- `.omp/skills/dep-review/SKILL.md` and `.omp/skills/memory/SKILL.md`: the procedures for dependencies and memory.

## Runtime/Tooling Preferences
- **Rust toolchain**: stable 1.98.0 (pinned), edition 2024, resolver 3. Windows builds must use MSVC.
- **GUI**: `gpui-kit` only, pinned `=X.Y.Z` with `Cargo.lock` committed. Never add `gpui`, `gpui-pre*`, or `gpui-component` directly.
- **Dependencies**: one crate per category (ADR 0004; ledger in `docs/memory/deps.md`). Before any `Cargo.toml` dependency change, run `skill://dep-review`. Reuse the crates gpui already pulls in (parking_lot, chrono, uuid, async-channel, serde_json).
  - Chosen crates: tokio, reqwest (rustls), serde/serde_json, tracing (+ tracing-appender for daemon logs), thiserror, rust-i18n, sys-locale, toml. IPC framing is hand-rolled in omc-ipc.
- **Tools**: cargo-nextest ≥0.9.131 and cargo-deny 0.20 are required. cargo-shear for unused dependencies is proposed but not installed.
- **Agent context files**: do not create `.omp/AGENTS.md` or `CLAUDE.md` (they shadow this file), and do not add `.cursor`, `.clinerules`, or copilot instruction duplicates.

## Testing & QA
- **Test runner**: cargo-nextest only (ADR 0006). `cargo test` is intercepted by TTSR. Set `doctest = false` in every `[lib]`. Rustdoc code blocks use `ignore` or `text`; examples live in nextest tests.
- **Unit tests** go in `#[cfg(test)] mod tests` next to the code. Clippy allows unwrap, expect, panic, and indexing in tests, but the `rs-no-panic` TTSR regex cannot see `cfg(test)` and fires anyway. So write assertions like `assert!(matches!(r, Err(Error::X(_))), "…")` or `assert!(r.is_ok_and(…), "…")`. `todo!` and `unreachable!` are never allowed. Every assertion needs a message (`missing_assert_message`).
- **Debugging tests in Zed**: the Zed debug scenario compiles test binaries with `cargo test --no-run`. This only builds them, because Zed cannot locate nextest binaries. Execution always goes through nextest.
- **UI tests**: `#[gpui_kit::test] fn t(cx: &mut TestAppContext)`, with `gpui-kit` features `["test-support"]` in dev-dependencies. Import explicitly in test modules, because `use gpui_kit::*` shadows `#[test]`. They cannot talk to a real daemon (any tokio-thread activity fails the test scheduler, L-0016): keep UI-side IPC logic in pure models.
- **Engine tests**: test the engine and omc-ipc headless over `tokio::io::duplex`; filesystem tests use unique temp dirs and never touch real user data.
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
