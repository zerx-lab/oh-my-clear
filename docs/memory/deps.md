# Dependency ledger
<!-- Invariant: exactly one crate per category (ADR 0004). Adding/replacing requires skill://dep-review, a row here, and an ADR for a new category or replacement. Re-verify rows older than 90 days at consolidation. -->
<!-- Status: added = in Cargo.toml · chosen = decided, add when first needed (still run dep-review §2 then) · open = undecided -->

Evidence source: crates.io / GitHub / RustSec, 2026-09-28 (docs/research/2026-09-28-*.md).

| Category | Crate | Version req | Status | ADR | Verified | Evidence | Rejected |
|---|---|---|---|---|---|---|---|
| GUI framework | gpui-kit (umbrella; brings gpui-pre =0.3.7) | `=0.7.0`; dev: `features=["test-support"]` | chosen (dial-ui, apps/dial only) | 0002, 0008 | 2026-09-28 | 0.7.0 2026-09-28, ~weekly releases, ~161 contributors, Apache-2.0; 0 vulns, 5 transitive unmaintained (instant, paste, rustls-pemfile, rustybuzz, ttf-parser) | gpui 0.2.2 (stale 2025-10); gpui-component direct (covered by kit) |
| Errors | thiserror | `2.0.21` | added (dial-telemetry, xtask) | 0004 | 2026-09-28 | 2.0.21 2026-09-23, MSRV 1.77; dtolnay; already in gpui graph | anyhow (gpui's `anyhow::Result` handled at the UI edge), snafu, eyre (banned) |
| Async runtime (I/O, processes) | tokio (+ tokio-util: codec, CancellationToken) | `1` / `0.7` | chosen | 0004, 0008 | 2026-09-28 | tokio 1.53.1 2026-07-20; tokio-util 0.7.19 2026-07-21 (tokio-rs); UDS + named pipes native | smol/async-std as direct deps |
| HTTP client | reqwest (rustls) | `0.13`, `default-features=false`, features rustls/http2/json/stream | chosen | 0004, 0009 | 2026-09-28 | 0.13.5 2026-09-08; SSE streaming for dial-llm | ureq, isahc, surf, gpui-pre-reqwest-client (0.12 fork) |
| Serialization | serde + serde_json (IPC control frames too) | `1` | chosen | 0004, 0008 | 2026-09-28 | already in gpui graph; ACP payloads carry `serde_json::Value` (postcard/bitcode fail on it); control msg 70–300 ns | bincode (RUSTSEC-2025-0141), postcard, rmp-serde |
| Logging | tracing + tracing-subscriber (`env-filter`) + tracing-appender (daemon files, daily, 14 kept) | `0.1.44` / `0.3.23` / `0.2` | added (dial-telemetry); appender chosen | 0004, 0007, 0008 | 2026-09-28 | tokio-rs org; appender 0.2.5; `cargo deny` clean | env_logger, fern, simplelog (banned) |
| IPC transport | tokio `UnixListener` / `named_pipe` (no crate) | — | chosen | 0008 | 2026-09-28 | UDS probe ≈1.5 GB/s; Windows AF_UNIX not in std/mio | interprocess (zellij; unneeded), daemonize (RUSTSEC-2025-0069) |
| Locks | parking_lot | `0.12` (reuse gpui's) | chosen | 0004 | 2026-09-28 | no poisoning → no `lock().unwrap()` | std::sync::Mutex for new code |
| Channels | tokio::sync (daemon) + async-channel (UI bridge, via gpui) | — | chosen | 0004 | 2026-09-28 | both already present | flume, crossbeam-channel |
| Time / IDs | chrono / uuid (via gpui) | reuse gpui's | chosen | 0004 | 2026-09-28 | chrono 0.4.45, uuid 1.26.1 | time, jiff |
| Lazy statics / file lock | std `LazyLock`/`OnceLock`; std `File::try_lock` (1.89+) | std | chosen | 0004, 0008 | 2026-09-28 | deny.toml std-replacements; try_lock = flock on Unix | lazy_static, once_cell, fs4 |
| Config format | toml | `1` | chosen | 0008 | 2026-09-28 | 1.1.6 2026-09-10 | — |
| Git | `git` CLI subprocess (no crate) | — | chosen | 0008 | 2026-09-28 | Zeron + Orca both shell out; worktree lifecycle parity | git2 (3 unsound advisories 2026), gix (no worktree remove/repair) |
| Child processes | tokio::process + process-wrap | `10` | chosen | 0008 | 2026-09-28 | process-wrap 10.0.1 2026-09-23 (watchexec org); kill-tree via pgroups/Job Objects | command-group (superseded) |
| Terminal emulation | libghostty-vt (C ABI, Ghostty submodule `third_party/ghostty`, built by `dial-ghostty` build.rs) | pinned submodule rev | added (dial-ghostty) | 0010 | 2026-09-28 | MIT; parse 442–791 MB/s; snapshot 10k rows 2.9 MB / 1 ms; +1.3 MB binary | alacritty_terminal emulator, crates.io libghostty-vt 0.2.1 (zig 0.15 pin, git clone in build.rs) |
| Build toolchain (non-cargo) | Zig | `0.16.x` exact minor | required | 0010 | 2026-09-28 | Ghostty `requireZig` exact major.minor; CI `mlugg/setup-zig@v2` | — |
| PTY | alacritty_terminal, `tty` module only (dial-process) | `0.26`, `default-features=false` | chosen | 0010 | 2026-09-28 | 0.26.0 2026-04-06; Zed-proven ConPTY + openpty; 29–36 crates, 4.1 s build | portable-pty (last release 2025-02, Windows kill bug), pty-process (Unix only), teletypewriter (single owner) |
| Agent protocol types | agent-client-protocol-schema (+ own tokio NDJSON JSON-RPC driver) | `=1.9.1`, `default-features=false`, `unstable_session_fork` | chosen (dial-agent) | 0009 | 2026-09-28 | 2026-09-18; 39 crates, no runtime | agent-client-protocol SDK 2.2.0 (async-io/async-process second I/O stack; banned) |
| MCP (server + native-agent client) | rmcp | `3.5` | chosen | 0009 | 2026-09-28 | 3.5.0 2026-09-28, official, tokio, streamable-HTTP server; own Host/Origin checks (RUSTSEC-2026-0189 class) | hand-rolled |
| LLM provider clients / SSE | hand-rolled in dial-llm (no crate) | — | chosen | 0009 | 2026-09-28 | codex + goose hand-roll on reqwest; must round-trip opaque reasoning state | async-openai, genai, rig-core, eventsource-stream (2022) — banned |
| Credentials | keyring (`v1`; Linux zbus `rt-tokio-crypto-rust`) | `4.2` | chosen | 0009 | 2026-09-28 | Keychain / Credential Manager / Secret Service; no openssl | — |
| Code search | ignore, globset, grep-searcher, grep-regex (ripgrep libs) | latest | chosen (dial-native) | 0009 | 2026-09-28 | BurntSushi; in-process, no `rg` binary | shelling out to rg |
| Tool schemas | schemars (reuse gpui's) | `1` | chosen | 0009 | 2026-09-28 | already in gpui graph | — |
| Persistence | rusqlite (`bundled`) | `0.40` | chosen | 0008 | 2026-09-28 | 0.40.2 2026-08-08; only old patched advisories | sqlx (heavy), redb (no SQL) |
| Text diff | similar | `3` | chosen | 0009 | 2026-09-28 | 3.2.0 2026-08-17 | — |
| File watching | notify | `7` (match gpui-component's `^7`) | chosen | 0009 | 2026-09-28 | notify 8 would duplicate in the UI binary | — |
| System prefs (accent, reduce transparency/contrast) | objc2-app-kit / windows / ashpd (target-specific, reuse gpui's versions) | `0.3` / `0.62` / `0.13` | chosen (dial-ui) | 0011 | 2026-09-28 | all safe fns; already in gpui graph | palette (color math is 25 lines; banned) |
| Fonts (assets, not crates) | Inter 4.1 + JetBrains Mono 2.304, bundled unmodified | — | chosen | 0011 | 2026-09-28 | OFL-1.1; ship license texts; never rename (RFN) | Geist Mono (box-drawing unverified) |
| CLI args | none (std::env::args); clap 4 if ever needed | — | chosen | 0004 | 2026-09-28 | daemon subcommands are few | argh |
