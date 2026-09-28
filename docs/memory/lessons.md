# Lessons learned
<!-- Newest first. ≤40 active entries. Grep by tag. Promote when hits ≥ 2. Delete (git keeps history) when obsolete. Template: skill://memory -->

## L-0013 Static-link libghostty-vt from a directory that holds only the `.a`
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [build, ghostty, macos, ffi]
- trigger: `cargo:rustc-link-lib=static=ghostty-vt` in `crates/dial-ghostty/build.rs`
- lesson: on macOS, when `libghostty-vt.dylib` sits in the same search dir, the link silently uses the dylib (runtime `@rpath/libghostty-vt.dylib` dyld error) and Apple ld rejects `+verbatim`. build.rs copies only the static archive into an isolated dir; keep it that way. `clippy::print_stdout` does not fire in build scripts, so an `#[expect]` for it there is unfulfilled and fails `-D warnings`.
- evidence: docs/research/2026-09-28-libghostty-vt.md probe P2; dial-ghostty implementation clippy run (2026-09-28)

## L-0012 Ghostty zig packages: `--fetch=all`, then offline via `--system` + `-fno-sys=`; always `-Dversion-string`
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [build, ghostty, zig, ci]
- trigger: libghostty-vt build fails with "lazy dependency package not found", needs network, or rebuilds slowly
- lesson: libghostty-vt's deps are lazy, so plain `zig build --fetch` misses them — run `zig build --build-file third_party/ghostty/build.zig --fetch=all` once. Offline builds pass `--system <zig-pkg>` plus `-fno-sys=` for all 11 integrations (`--system` alone turns every system integration on). Without `-Dversion-string`, any `git describe` change invalidates the zig cache (16.7 s vs 0.4 s). Ghostty's `requireZig` wants the exact major.minor (main = 0.16).
- evidence: reproduced lazy-dependency error, then offline build with an empty global cache (dial-ghostty build.rs, 2026-09-28)

## L-0011 cargo-deny `wrappers` can enforce internal layering, but warns on every edge not yet declared
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: promoted → xtask/src/layers.rs
- tags: [deps, cargo-deny, workspace]
- trigger: wanting to forbid crate edges (UI → engine, daemon → gpui)
- lesson: `bans.deny = [{ crate = "dial-x", wrappers = [...] }]` does apply to path/workspace crates and to dev-deps (`error[banned]`), and a root with no parents passes; but each listed wrapper that is not (yet) a parent emits `warning[unused-wrapper]`, so a planned-graph table is pure noise on a scaffold. Layering therefore lives in `cargo xtask layers` (`LAYERS`/`CONFINED`).
- evidence: /tmp copy of the workspace, cargo-deny 0.20.2 `check bans` exit 2 with ui→proto edge, `bans ok` without (2026-09-28)

## L-0010 `strip = "debuginfo"` silently discards `debug = "line-tables-only"` unless `split-debuginfo = "packed"`
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [build, profiles, debugging]
- trigger: tuning `[profile.release]` debuginfo/strip
- lesson: with line tables embedded, `strip = "debuginfo"` gives the same binary and backtraces (no file:line) as `debug = false`. `split-debuginfo = "packed"` keeps the binary equally small and writes a side file (.dSYM/.dwp/.pdb) from which panic backtraces resolve file:line — ship/keep it next to the binary or with symbol uploads.
- evidence: /tmp relprobe on macOS rustc 1.98: release 428128 B, no lines; packed 428128 B + .dSYM, lines resolved; strip none 461688 B (2026-09-28)

## L-0009 Zed gutter runnables run `cargo test`; override via tagged tasks
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: promoted → .zed/tasks.json
- tags: [zed, nextest, debugging]
- trigger: running/debugging tests from the Zed gutter
- lesson: Zed's built-in Rust tasks use `cargo test`; workspace tasks tagged `rust-test`/`rust-mod-test` replace them (vars `$ZED_CUSTOM_RUST_PACKAGE`, `$ZED_CUSTOM_RUST_TEST_NAME`, `$ZED_CUSTOM_RUST_TEST_FRAGMENT`). Zed's debug locator only understands cargo build/test/run, so test debugging builds with `cargo test --no-run`.
- evidence: zed `crates/languages/src/rust.rs` task templates + `runnables.scm` tags; https://zed.dev/docs/tasks.md

## L-0008 Cargo aliases can shadow built-ins and chain other aliases
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [cargo, nextest]
- trigger: adding shortcuts in .cargo/config.toml
- lesson: `t = "nextest run …"` overrides the built-in `cargo t` (= cargo test); `ci = "xtask ci"` resolves through the `xtask` alias. `[env]` values apply to `cargo run`/nextest and lose to shell-set vars (no `force`).
- evidence: /tmp probe with cargo 1.98.0 (2026-09-28)

## L-0007 rs-no-panic TTSR fires inside `#[cfg(test)]` too
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [omp, ttsr, testing]
- trigger: writing unit tests with unwrap/expect/panic!
- lesson: clippy.toml allows them in tests, but the regex can't see cfg(test) and interrupts. Write tests without them: `assert!(matches!(r, Err(Error::X(_))), "msg")`, `assert!(r.is_ok_and(..), "msg")`, `let Ok(v) = r else { .. }`. Rule body now says so.
- evidence: TTSR interrupt while writing xtask tests (2026-09-28); .omp/rules/rs-no-panic.md

## L-0006 In AGENTS.md, `@path` imports expand only outside code spans/fences
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [omp, memory]
- trigger: editing AGENTS.md or adding always-loaded memory
- lesson: write `@docs/memory/INDEX.md` as plain text on its own line; backticked or fenced tokens stay literal. Imports resolve relative to the importing file, ≤5 hops.
- evidence: omp://context-files.md "@ imports"

## L-0005 `.omp/rules` and `.omp/config.yml` load only from the cwd's `.omp/`
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [omp]
- trigger: rules/TTSR/settings "not applying"
- lesson: launch omp from the repo root. Skills and agents walk ancestors; rules and config.yml do not.
- evidence: omp://rulebook-matching-pipeline.md §2, omp://settings.md

## L-0004 clippy `arithmetic-side-effects-allowed` takes crate-internal paths
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [clippy, gpui]
- trigger: `arithmetic_side_effects` flags third-party operator impls (gpui `Pixels`)
- lesson: list the path inside the defining crate without the crate name (`"geo::Px"` worked; `"Px"` and `"mycrate::geo::Px"` did not). Exact gpui path unverified — test before relying on it.
- evidence: docs/research/2026-09-28-rust-gates.md §1.3 (scratch-crate test)

## L-0003 TTSR rules fire once per session unless repeatMode is after-gap
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: promoted → .omp/config.yml
- tags: [omp, ttsr]
- trigger: writing or debugging TTSR rules
- lesson: default `ttsr.repeatMode: once` lets a second violation through; `.omp/config.yml` sets `after-gap`/`repeatGap: 2`. TTSR steers, lints enforce — keep both. Validate rules with `omp ttsr test` (positive + negative).
- evidence: omp://ttsr-injection-lifecycle.md; `omp ttsr test` runs 2026-09-28

## L-0002 `cargo nextest run` fails (exit 4) when there are no tests
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [nextest]
- trigger: running the test gate in a crate/workspace without tests
- lesson: no config key exists; pass `--no-tests=warn` (or `NEXTEST_NO_TESTS=warn`). Drop the flag once tests exist.
- evidence: nextest 0.9.146 local run; https://nexte.st/docs/configuration/reference/

## L-0001 Never create `.omp/AGENTS.md` or a divergent `CLAUDE.md`
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [omp, memory]
- trigger: adding agent context files
- lesson: `.omp/AGENTS.md` (native, priority 100) shadows root `AGENTS.md` at the same depth; `CLAUDE.md` competes at equal priority. Keep root `AGENTS.md` canonical; sticky rules go in `.omp/RULES.md`.
- evidence: omp://context-files.md "Load order and shadowing"
