# Active context
Updated: 2026-09-28 by main (architecture v2 session)

## Focus
- Architecture v2 accepted: UI process + `dial-daemon` over local IPC (ADR 0008), native agent + ACP client + daemon-hosted MCP (0009), libghostty-vt terminal in the `dial-ghostty` unsafe island with alacritty `tty` PTYs (0010), UI design language + Apple-style spring motion (0011). ADR 0005 superseded.
- Layering is mechanical: `LAYERS`/`CONFINED`/`UNSAFE_ISLANDS` in `xtask/src/layers.rs`, run first by `cargo ci`.
- Code so far: `dial-telemetry` (shared tracing, stderr sink), `dial-ghostty` (libghostty-vt build + safe wrapper), both binaries log and exit; other crates are doc-only.

## Open questions
- Windows MSVC libghostty-vt link is proven only by upstream/third-party CI; first dial CI run on `windows-latest` must confirm (aarch64-windows unproven) — owner: agent, since 2026-09-28
- Is Node 22 bundled (managed runtime) or required on PATH for npx-distributed ACP agents? — owner: user, since 2026-09-28
- Streaming coalescing changed from ~100 ms to one notify per frame (≤33 ms) per UiMotion research — confirm when the transcript view exists — owner: agent, since 2026-09-28
- Unused-dep tool: cargo-shear proposed, not installed — owner: user, since 2026-09-28

## Next steps
1. M1: gpui-kit window in `dial-ui`/`apps/dial` (dep-review gpui-kit `=0.7.0` + gpui `[profile.dev.package]` opt-levels), tokens + `dial_ui::motion` presets, bundled fonts; CI Linux apt packages for gpui.
2. M1.5 daemon skeleton: `dial-ipc` framing/handshake/token auth over UDS + named pipes, endpoint discovery + `File::try_lock`, detached spawn + `READY`, `dial-daemon run|status|stop`, tracing-appender logs; UI connection-state pill.
3. Terminal vertical slice: `dial-process` PTY (alacritty tty) → daemon `dial-term` thread → raw `0x02` frames → UI render via RenderState + key encoder.
4. Verify the `arithmetic-side-effects-allowed` path for gpui `Pixels` (L-0004) before UI arithmetic spreads.

## Recent changes
- 2026-09-28 Architecture v2 (ADRs 0008–0011), crates dial-ipc/llm/native/telemetry/ghostty + apps/dial-daemon, xtask layers check, CI matrix, rules ui-design-motion / term-no-alacritty-emulator / rs-unsafe-island, release split-debuginfo.
- 2026-09-28 Workspace split into apps/*, crates/*, xtask; dev/profiling profiles; `.cargo/config.toml` aliases; `.zed/` configs.
- 2026-09-28 Bootstrap: workspace lints, gates, omp config, memory system, research snapshots.
