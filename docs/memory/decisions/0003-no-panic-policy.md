---
status: accepted
date: 2026-09-28
tags: [rust, quality]
---
# 0003 No-panic policy enforced by clippy restriction lints and TTSR

## Context and Problem Statement
The user forbids panicking functions in Rust. A system cleaner must survive failures of individual scans, file operations, and parsers without leaving work half-done.

## Considered Options
* Convention only (review)
* Clippy restriction lints in `[workspace.lints]` + test escapes + TTSR steering
* `panic = "abort"` to fail fast

## Decision Outcome
Chosen option: lints + TTSR. `Cargo.toml [workspace.lints.clippy]` denies `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `unreachable`, `indexing_slicing`, `string_slice`, `get_unwrap`, `unwrap_in_result`, `panic_in_result_fn`, `fallible_impl_from`, `arithmetic_side_effects`, `unchecked_time_subtraction`, `exit`, `mem_forget`, `manual_assert`, plus `allow_attributes(_without_reason)` so suppression is only `#[expect(lint, reason = "…")]`. `clippy.toml` allows unwrap/expect/panic/indexing inside tests. TTSR rules (`rs-no-panic`, `rs-no-index`, `rs-expect-not-allow`, `gpui-panicking-apis`) catch violations while the model is writing. `unsafe_code = "forbid"`.

Release keeps `panic = "unwind"`: a panic inside a dependency then fails one tokio task instead of the app, and `Drop` still releases resources. Install a panic hook that logs via tracing and writes a crash file.

### Consequences
* Good, because violations fail `cargo clippy -D warnings`, and `#[expect]` self-cleans stale suppressions.
* Bad, because `arithmetic_side_effects` is noisy on gpui geometry types (allowlist via `clippy.toml`, see L-0004); lints cannot see panics inside dependencies, `RefCell`/entity re-entrancy, std contract panics (`Vec::remove`, `split_at`), stack overflow, OOM.

## Evidence
- docs/research/2026-09-28-rust-gates.md §1–2 (each lint verified with clippy 1.98 `--explain`; scratch crate tests 2026-09-28)
