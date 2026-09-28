---
status: accepted
date: 2026-09-28
tags: [workspace, tooling, debugging]
---
# 0007 Virtual multi-crate workspace, xtask, dev profiles, Zed debug/test wiring

## Context and Problem Statement
The user asked for a multi-crate workspace structure optimised for debugging and developer experience. ADR 0005 names the crates; this ADR fixes how the workspace is laid out and how developers build, test, run, and debug it.

## Considered Options
* Single crate with modules until code exists · crates created on demand · full ADR 0005 layout now
* Gate runner: shell script / `just` (not installed) / cargo aliases only / std-only `xtask`
* Editor wiring: none · VS Code (not installed) · Zed (installed: Zed Preview)

## Decision Outcome
- Virtual root manifest; members by glob `apps/*`, `crates/*`, `xtask`; `default-members = ["apps/dial"]`. All ADR 0005 library crates exist as `crates/dial-<area>` with a doc comment stating responsibility and allowed internal deps. Internal edges are declared (via `[workspace.dependencies]` path entries) only when code uses them — cargo-deny `workspace-dependencies.unused = "deny"` forbids unused entries.
- `xtask` (std + thiserror): `ci` runs the four gates in order with timings; `new-crate` scaffolds a library with inherited package fields, lints, `doctest = false`. Cargo aliases: `ci`, `lint`, `t` (shadows built-in `cargo t` → nextest), `dial`, `new-crate`, `xtask`.
- Profiles: dev = full debuginfo for workspace crates, `line-tables-only` for dependencies (backtraces keep line numbers, links stay fast), build scripts/proc-macros at opt-level 3; `profiling` = release + full symbols. gpui-specific `[profile.dev.package]` opt-levels are added together with gpui-kit (ADR 0002).
- `apps/dial/src/telemetry.rs`: tracing subscriber with `EnvFilter` (`RUST_LOG`, default `info`; `.cargo/config.toml [env]` sets `info,dial=debug` for dev, prefix covers every `dial_*` crate) + `log` bridge + panic hook logging through tracing with a forced backtrace.
- Zed: rust-analyzer runs clippy in `target/rust-analyzer`; tasks tagged `rust-test`/`rust-mod-test` replace the gutter `cargo test` runnables with nextest; CodeLLDB scenarios for the app, xtask, and tests. Exception to ADR 0006: the test debug scenario builds with `cargo test --no-run` (compile only) because Zed's locator cannot infer nextest binaries; terminal equivalent is `cargo nextest run --debugger`.
- Toolchain adds `rust-src` (std navigation, stepping into std).

### Consequences
* Good, because boundaries are physical (crate privacy, per-crate `cargo nextest -p`), editor diagnostics equal the clippy gate, and `cargo ci` is one command.
* Bad, because the library crates are empty until their milestone; bare `cargo check` covers only the app (gates always use `--workspace`).

## Evidence
- `cargo ci` green; `cargo dial` logs startup; bad `RUST_LOG` → exit 1 with message; `cargo new-crate probe …` produced a member that passed clippy; throwaway crate confirmed `dial=debug` matches `dial_engine` targets and the panic hook logs backtraces (2026-09-28)
- Zed docs: https://zed.dev/docs/languages/rust.md, https://zed.dev/docs/debugger.md, https://zed.dev/docs/tasks.md; rust runnable tags from zed `crates/languages/src/rust/runnables.scm`
