# Lessons learned
<!-- Newest first. ≤40 active entries. Grep by tag. Promote when hits ≥ 2. Delete (git keeps history) when obsolete. Template: skill://memory -->

## L-0024 cargo-deny unions targets: a platform-gated dependency drags in its other platforms' deps
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [deps, deny]
- trigger: adding a dependency under `[target.'cfg(…)'.dependencies]` whose own deps are gated to another OS (tao on macOS/Windows → gtk/glib on Linux)
- lesson: `cargo deny` builds one graph for all `[graph] targets`, so those never-built crates still hit advisories/bans (RUSTSEC-2024-0429 glib 0.18 failed `unsound = "all"`). Confirm with `cargo tree --target <t> -p <member> -i <crate>` that nothing builds it, then add a reasoned `ignore` (kept honest by `unused-ignored-advisory = "deny"`) and a `wrappers` ban.
- evidence: `cargo deny --all-features check advisories` path `glib ← gtk ← tao ← oh-my-clear-daemon`, while `cargo tree --target x86_64-unknown-linux-gnu -p oh-my-clear-daemon -i gtk` prints nothing (2026-09-28)

## L-0023 App-level action handlers must `cx.defer` before updating the dispatching window
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [gpui, actions]
- trigger: a `cx.on_action` (global) handler that closes/minimizes/zooms or otherwise updates `cx.active_window()`
- lesson: key and menu dispatch run global listeners inside `window.update` of that same window, which is leased out, so `handle.update` fails and the command is silently dropped (secondary-w did nothing). Wrap the window work in `cx.defer`. The test platform has no key window: call `window.activate_window()` before `simulate_keystrokes`.
- evidence: gpui-pre-0.3.7 `src/window.rs:2442-2455,6277-6300`; `omc_ui::tests::close_window_shortcut_closes_the_settings_window` fails without the defer (2026-09-28)

## L-0022 SVG filters on brand art need `color-interpolation-filters="sRGB"`
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [brand, svg]
- trigger: a `<filter>` (drop shadow, bloom) wraps or sits over a dark gradient in `assets/brand/*.svg`
- lesson: filters default to `linearRGB` and round-trip through 8-bit, so dark gradients band in 4–6-level steps (visible stripes across the icon tile). Set `color-interpolation-filters="sRGB"` on every filter.
- evidence: `resvg` render of `assets/brand/app-icon.svg`: column R jumped `1C→16` under `filter="url(#drop)"`, 1-level steps with sRGB.

## L-0021 Wire newtypes with invariants deserialize via `try_from`, never `serde(transparent)`
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [ipc, serde, security]
- trigger: an omc-proto type whose constructor validates (e.g. a relative-path newtype)
- lesson: `#[serde(transparent)]` skips the constructor, so a peer could send `..` and every consumer had to re-validate. Use `#[serde(try_from = "String", into = "String")]`; a malformed request then fails to decode and the daemon closes the connection.
- evidence: a daemon e2e test decoded `".."` into a validated `RelPath` newtype before the fix; the newtype was removed with the file explorer (ADR 0017), see git history (2026-09-28)

## L-0019 Key-context bindings shadow global ones — put the fallback in the handler
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [gpui, actions]
- trigger: binding a key in a view's key context that also has a global binding (e.g. secondary-w closing a tab vs. the window)
- lesson: the context binding wins whenever that view has focus, so the global action never fires there; the handler must fall back itself (e.g. close-tab with no tab left → `window.remove_window()`).
- evidence: former omc-ui test `close_binding_closes_tabs_before_the_window`, removed with the tabbed layout (ADR 0017) (2026-09-28)

## L-0018 Fixed-width side panels: `base::resize_handle`, not gpui-kit `Resizable`
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [gpui, layout]
- trigger: sidebar or dock that keeps its width when the window resizes, and can be hidden
- lesson: `ResizableState::adjust_to_container_size` rescales every panel proportionally on window resize, and a `ResizablePanel::visible(false)` panel leaves the neighbour's drag handle behind. Use `gpui_kit::base::resize_handle` + `.inside(HandleEdge)` + `on_drag_move` and clamp so the centre keeps its minimum width.
- evidence: gpui-base-0.7.0 `src/resizable/{mod,panel}.rs` (2026-09-28)

## L-0017 Icons outside the default set need `omc_ui::assets::Assets`: `gpui_kit::assets::Assets` bundles only 104
- date: 2026-09-28 · verified: 2026-09-28 · hits: 2 · status: active
- tags: [gpui, assets]
- trigger: picking an icon for a view
- lesson: `gpui_kit::assets::Assets` embeds the 104 icons of `default-icons.txt` (`AllAssets` has all of Lucide). Other `gpui_kit_assets::IconName` variants compile but render nothing at runtime. Use `gpui_kit::component::IconName` when it has the icon; otherwise add the variant to `icon_assets!` in `crates/omc-ui/src/assets.rs` (the app's source, layered over the default set) and keep it covered by a load test.
- evidence: gpui-kit-assets-0.7.0 `build.rs`, `src/lib.rs` (`icon_assets!`), `src/native_assets.rs`; `assets::tests::every_sidebar_icon_is_bundled` (2026-09-28)

## L-0016 gpui `TestAppContext` panics on activity from other threads — test IPC below the view
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [gpui, testing, ipc]
- trigger: a `#[gpui_kit::test]` that talks to a real daemon or any tokio thread
- lesson: the test scheduler fails with "Detected activity on thread … Your test is not deterministic" when a tokio thread (e.g. the `omc-ipc` runtime) completes a oneshot. Keep UI-side IPC logic in pure models and test the wire with `tokio::io::duplex` in omc-ipc/omc-engine.
- evidence: gpui-pre-scheduler-0.3.7 `src/test_scheduler.rs:193`; throwaway real-daemon gpui test (2026-09-28)

## L-0015 Apply the app's theme style after every preset reload, never before
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [gpui, theme]
- trigger: editing `Theme` fields (radius, fonts, colours) in omc-ui
- lesson: `Theme::change` (and gpui-component's own `ThemeRegistry` observer, which fires on any `ThemeRegistry::global_mut`) re-applies the bare preset and resets colours; a style edited earlier is lost silently. Write overrides with `Theme::update` after `Theme::change`, and keep the app's second registry observer (registered after `gpui_kit::init`) that re-runs `theme::apply`.
- evidence: gpui-component 0.7.0 `src/theme/mod.rs` `Theme::edit` / `src/theme/registry.rs:46-72`; test `style_overrides_survive_mode_switches_and_registry_reloads` (2026-09-28)

## L-0014 gpui-component theme JSON silently drops unknown keys — check keys against `schema.rs`
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [gpui, theme]
- trigger: adding or editing a preset in `crates/omc-ui/themes/`
- lesson: `ThemeConfigColors` has no `deny_unknown_fields`, so a misspelled key parses fine and the colour falls back to the default. The upstream longbridge/gpui-kit presets use `window_border` (schema: `window.border`), `link.foreground` (schema: `link`), `panel.background` (no such key) and syntax `comment.doc` (serde expects `comment_doc`); the app's copies rename/drop those. Validate new presets against the `#[serde(rename)]` keys of gpui-component's `src/theme/schema.rs`.
- evidence: gpui-component-0.7.0/src/theme/schema.rs; `src/highlighter/registry.rs` `SyntaxColors.comment_doc`; a key check over the 6 upstream files found 292 unknown keys (2026-09-28)

## L-0011 cargo-deny `wrappers` can enforce internal layering, but warns on every edge not yet declared
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: promoted → xtask/src/layers.rs
- tags: [deps, cargo-deny, workspace]
- trigger: wanting to forbid crate edges (UI → engine, daemon → gpui)
- lesson: `bans.deny = [{ crate = "omc-x", wrappers = [...] }]` does apply to path/workspace crates and to dev-deps (`error[banned]`), and a root with no parents passes; but each listed wrapper that is not (yet) a parent emits `warning[unused-wrapper]`, so a planned-graph table is pure noise on a scaffold. Layering therefore lives in `cargo xtask layers` (`LAYERS`/`CONFINED`).
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
