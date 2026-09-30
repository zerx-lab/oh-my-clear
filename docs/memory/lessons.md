# Lessons learned
<!-- Newest first. ≤40 active entries. Grep by tag. Promote when hits ≥ 2. Delete (git keeps history) when obsolete. Template: skill://memory -->

## L-0041 Engine tests that scan developer junk must pass `file_roots`/`exclude` for a temp fixture
- date: 2026-09-30 · verified: 2026-09-30 · hits: 1 · status: active
- tags: [testing, scanning, engine]
- trigger: an engine/automation test that runs a `developer_junk` (or other home-rooted) scan
- lesson: without injected roots the scan walks the real home (≈10 s, nondeterministic, touches user data); with them it takes ≈0.1 s.
- evidence: `crates/omc-engine/src/engine/tests/automation.rs` (`scan_only`)

## L-0040 After moving/renaming the repo, `cargo clean -p xtask`: `env!("CARGO_MANIFEST_DIR")` stays baked in
- date: 2026-09-30 · verified: 2026-09-30 · hits: 1 · status: active
- tags: [build, xtask]
- trigger: `cargo xtask layers` fails with `…/<old-dir>/apps: No such file or directory`
- lesson: the cached xtask binary keeps the old workspace path (`workspace_root()`); cargo does not rebuild it on a directory move. `cargo clean -p xtask` fixes it.
- evidence: `xtask/src/main.rs:121`; error `zerx-lab/dial/apps` after the rename to oh-my-clear, gone after `cargo clean -p xtask`

## L-0039 Poll `job_status` right after `start_job`: job updates can beat the id to the UI
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [ipc, ui, jobs]
- trigger: UI code that starts a daemon job and follows its pushed `Event::Job` updates
- lesson: the daemon samples progress from the job's first tick and only sends changes; events and responses reach the UI on separate paths, so updates arriving before `start_job` answers are dropped (unknown id). A job whose progress changes once (one large item removed) then shows zeros until it ends. Poll once after the id arrives (`slots::started`, `Flow::scan`/`clean`); removal must also count freed bytes per file, not per target.
- evidence: crates/omc-engine/src/jobs.rs `drive` (interval + dedup); crates/omc-ui/src/pages/widgets/flow.rs `Flow::poll`; trash clean stuck at "正在准备… 0 B" then done

## L-0038 A PATH command can be the app's own shim: check scripts before treating a same-named CLI as a rival
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [uninstall, macos, attribution]
- trigger: judging whether `~/.config/<n>` belongs to an app when a `<n>` command exists outside the bundle
- lesson: apps install CLIs as small `#!` scripts that `exec` into the bundle (`/opt/homebrew/bin/ghostex` → `Ghostex.app/Contents/Resources/CLI/ghostex`), not only as symlinks; a script referencing the bundle path counts for the app. Only a real foreign command (e.g. the `claude` CLI in `~/.local/bin`) lowers confidence.
- evidence: smoke of `app_files(Ghostex)` gave Low for `~/.config/ghostex` before the fix; `cat /opt/homebrew/bin/ghostex` (2026-09-28)

## L-0037 GPUI clicks bubble: an inner control inside a clickable row must `stop_propagation`
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [gpui, ui, interaction]
- trigger: a checkbox, chevron or button inside a clickable row/header/tile
- lesson: mouse-up click handlers run inner → outer and neither gpui-base Button/Checkbox nor gpui-component's Checkbox (only `prevent_default`) stops it, so a chevron button plus a row handler toggled twice (collapse did nothing). Give a row exactly one handler and call `cx.stop_propagation()` in inner handlers (`ui::Checkbox` does); cover it with a one-click-toggles-once test.
- evidence: gpui-pre-0.3.7 `elements/div.rs` mouse-up dispatch; `crates/omc-ui/src/ui/collapsible.rs` and its tests (2026-09-28)

## L-0036 `uniform_list` rows must share one height; `Input::h` is multi-line only
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [gpui, ui]
- trigger: mixing group headers and item rows in a virtual list; sizing a single-line text input
- lesson: `uniform_list` measures one row and assumes all match, so a 32 px header among 40 px rows must be wrapped to 40 px. For a single-line gpui-component `Input`, height comes from `Styled::h` applied after its own sizing (`Input::h` sets the multi-line height). gpui-kit also has two different `IconName` types (assets vs component): components take `impl Into<Icon>`.
- evidence: `crates/omc-ui/src/ui/input.rs`, `pages/junk.rs` header wrapper (2026-09-28)

## L-0035 macOS attribution: executable names are weak evidence; don't read other apps' containers
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [macos, apps, uninstall]
- trigger: matching Library items to an app in `omc-apps/src/macos/attribution.rs`
- lesson: helper bundles reuse executable names (`Claude Code URL Handler.app` has `CFBundleExecutable=claude`), so the executable only adds weight on top of stronger evidence; vendor folders (`Application Support/Google`) hold several products — attribute only `<vendor>/<product>`; sibling ids (`.canary`, `.for.testing`, `.pro`) belong to other products. Reading inside `~/Library/Containers/*` for corroboration triggers macOS 14's "access data from other apps" prompt — match names only. `lsof` prints `/private/var/…`, `getconf` `/var/…`: normalize before prefix checks. Run `codesign`/System Events lookups in parallel (≈0.5 s / 0.27 s each).
- evidence: MacUninstall smoke on real apps (Chrome, Xcode, Claude); `macos/files.rs` `unprivate` (2026-09-28)

## L-0034 rustfmt on a `mod.rs` also formats its child modules
- date: 2026-09-28 · verified: 2026-09-28 · hits: 2 · status: active
- tags: [tooling, agents]
- trigger: formatting only your own files while other agents edit siblings
- lesson: `rustfmt <path>/mod.rs` (or `main.rs`) follows `mod` declarations and rewrites child files other agents own (`skip_children` is unstable); format leaf files explicitly, or run `cargo fmt --all` once at integration.
- evidence: Removal (daemon main.rs → serve.rs/host.rs) and ScanState (pages/mod.rs → widgets/*) reports (2026-09-28)

## L-0033 macOS: opening another app's sandbox container can hang 5–6 s and fail with EINTR
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [macos, scanning, performance]
- trigger: scanning inside `~/Library/Containers` / `~/Library/Group Containers` (junk caches, app files, space lens)
- lesson: `open()` of a folder in another app's container sometimes blocks in the kernel for 5.0–6.0 s, then returns errno 4 (an immediate retry works); one such call held a whole parallel scan (System Junk 5–6 s instead of ~0.1 s). Junk scanning skips empty APFS folders without opening them (size 64, nlink 2) and walks container candidates on detached threads it waits for only 300 ms. Other walks that cross containers can still see one 5 s stall per hit.
- evidence: `sample` of the stalled pool thread in `__open_nocancel` from `read_dir`; plain parallel `fs::read_dir` of 742 container Caches folders stalled in ~1 of 4 runs; `crates/omc-scan/src/junk/measure.rs` (2026-09-28)

## L-0032 Filesystem tests that clean must set both delete methods to Permanent
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [testing, scanning]
- trigger: a test that runs a clean/uninstall job or `omc_apps::remove` on temp files
- lesson: `CleanSettings::default()` sends user files to the Trash (`files_delete = Trash`), so a test using defaults moves fixtures into the developer's real Trash. Put `Permanent` for `files_delete` and `junk_delete` (engine tests: through `PutSettings`) and remove every temp dir, also on early returns.
- evidence: omc-proto `settings.rs` `CleanSettings::default`; omc-engine tests; stale `$TMPDIR/omc-*` fixtures showed up in a real System Junk scan (2026-09-28)

## L-0031 The elevated helper gets root's environment: pass HOME and absolute exclusions in
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [security, macos, linux, elevation]
- trigger: code that runs inside `oh-my-clear-daemon elevated` (osascript `with administrator privileges`, pkexec)
- lesson: neither mechanism keeps the invoking user's `HOME`, and `Guard::new` derives its essential folders from `HOME`; the helper is started with `HOME=<user home>` (sh prefix / `/usr/bin/env`) and the manifest carries exclusions already expanded to absolute paths. Anything else read from the environment there is root's.
- evidence: `crates/omc-apps/src/elevate.rs` (helper command, manifest), `omc_scan::paths::home` (2026-09-28)

## L-0030 Windows platform quirks: `Key` is `!Send`, HRESULT error codes, localized `schtasks`, PowerShell encoding
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [windows, registry, apps]
- trigger: registry or tool calls in `omc-apps/src/windows/`
- lesson: `windows_registry::Key` wraps a raw HKEY (not `Send`/`Sync`): parallel scans reopen keys per thread from a `Copy` hive enum. Its errors turned into `io::Error` carry the HRESULT (0x8007xxxx), so compare `err.code()` with HRESULT_FROM_WIN32(2/3/5), never `io::ErrorKind`. List scheduled tasks with `Get-ScheduledTask | ConvertTo-Json` (trigger CIM class names), not `schtasks /query /fo csv` (translated headers). PowerShell scripts set `[Console]::OutputEncoding=UTF8` first. `Clear-RecycleBin` throws on an empty bin (treat as success).
- evidence: `crates/omc-apps/src/windows/{known,reg,tasks,system}.rs`; windows-registry-0.6.1 `key.rs` (2026-09-28)

## L-0029 macOS app inventory: batch `mdls` for sizes/last use, follow `.app` symlinks, match ids exactly
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [macos, apps, performance]
- trigger: listing apps or attributing Library files to an app
- lesson: one `mdls -name kMDItemPhysicalSize -name kMDItemLastUsedDate -raw <all bundles>` (NUL-separated, `(null)` for unset) replaces a walk per bundle (54 apps: 3.6 s → 0.2 s, sizes identical). `/Applications/Safari.app` is a symlink into `/System/Cryptexes/App`. Only the exact bundle id or one extra component is High confidence (`com.google.Chrome` must not claim `com.google.chrome.for.testing`); `pkgutil --file-info <bundle>` gives the installing pkg id, Homebrew's `INSTALL_RECEIPT.json` the cask's app name and zap paths.
- evidence: `crates/omc-apps/src/macos/{inventory,ident,files}.rs`; smoke run of `list_apps`/`app_files` (2026-09-28)

## L-0028 Never lock a shared mutex per file in a parallel walk
- date: 2026-09-28 · verified: 2026-09-28 · hits: 2 · status: active
- tags: [scanning, performance, rayon]
- trigger: writing a `walk::Visitor` or anything called per file from the walker's pool
- lesson: a `Mutex` taken per file serializes the pool: `Walker::measure`'s summer was mutex-bound (junk measurement 2–4× faster lock-free), and a global hard-link `Mutex<HashSet>` cost ~15% of a `/` walk (306k files with nlink > 1). Use atomics or per-thread buffers merged at the end, and shard any shared set. Also: rayon `for_each_init` runs init per split job, not per thread.
- evidence: `crates/omc-scan/src/walk.rs` `Summer` (atomics), `space.rs` sharded link set, ScanJunk/ScanFiles timings (2026-09-28)

## L-0027 `#[cfg(all(test, unix))] mod tests` fails `tests_outside_test_module`; gate the fn instead
- date: 2026-09-28 · verified: 2026-09-28 · hits: 2 · status: active
- tags: [clippy, testing, cross-platform]
- trigger: a test module that only applies to some OSes
- lesson: clippy only recognizes a plain `#[cfg(test)] mod tests`; put `#[cfg(unix)]` (etc.) on the test functions and move their `use super::*` inside them so other targets see no unused import.
- evidence: `crates/omc-apps/src/cmd.rs` tests; `cargo clippy --all-targets` on host and `x86_64-unknown-linux-gnu` (2026-09-28)

## L-0026 Check Windows/Linux code from macOS with `cargo clippy --target …`; linking is not possible
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [cross-platform, build]
- trigger: changing `cfg(windows)` / Linux-only code on the macOS development host
- lesson: `x86_64-pc-windows-msvc` and `x86_64-unknown-linux-gnu` are installed; `cargo clippy -p omc-proto -p omc-scan -p omc-apps -p omc-engine --all-targets --target <t> -- -D warnings` type-checks and lints that code (tests included). omc-ui cannot be checked for Linux here (wayland-backend's build script needs `x86_64-linux-gnu-gcc`). Such code only runs in CI on its OS; keep parsers pure so their tests carry the behaviour.
- evidence: cross-target clippy runs of omc-apps/omc-scan/omc-engine; UiScanPages report on the Linux omc-ui failure (2026-09-28)

## L-0025 `trash` on Windows needs a `coinit_*` feature
- date: 2026-09-28 · verified: 2026-09-28 · hits: 1 · status: active
- tags: [deps, windows]
- trigger: depending on `trash` with `default-features = false`
- lesson: without `coinit_apartmentthreaded` or `coinit_multithreaded` the crate fails to compile on Windows on purpose; the daemon uses `coinit_multithreaded` (blocking threads are not STA).
- evidence: trash-5.2.9 `src/windows.rs:323` compile error on `--target x86_64-pc-windows-msvc` (2026-09-28)

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
- date: 2026-09-28 · verified: 2026-09-28 · hits: 3 · status: active
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
