---
status: accepted
date: 2026-09-28
tags: [architecture, runtime, ipc]
---
# 0008 UI process + background daemon over local IPC; enforced crate layering

## Context and Problem Statement
The execution layer (scanning and cleaning) must be decoupled from the UI: closing the UI ends only the UI process, while long-running work keeps going and any UI can reattach later. The app must behave equally well on macOS, Windows (MSVC) and Linux.

## Considered Options
* In-process engine in the GUI
* One binary with a GUI mode and a `daemon` subcommand
* Two binaries: `oh-my-clear` (gpui-kit GUI) + `oh-my-clear-daemon` (headless, no gpui)

## Decision Outcome
Chosen: **two binaries**. A daemon that links gpui cannot start on a headless Linux box, and it would keep windowing/GPU libraries resident in a long-lived process.
- **`oh-my-clear-daemon`** owns the engine and all filesystem/system work on one tokio runtime. Subcommands: `run` (default), `stop`.
- **`oh-my-clear`** is a viewport: gpui-kit views plus an `omc-ipc` client (`EngineHandle`), with a tokio runtime in a GPUI `Global` used only for IPC.
- **Layering**, enforced by `cargo xtask layers` (the first step of `cargo ci`; the table is `LAYERS` in `xtask/src/layers.rs`):

  | Crate | Allowed internal deps |
  |---|---|
  | proto, telemetry | — |
  | ipc | proto |
  | engine | proto, ipc |
  | ui | proto, ipc |
  | apps/oh-my-clear | ui, ipc, proto, telemetry |
  | apps/oh-my-clear-daemon | engine, ipc, proto, telemetry |

  Confined external crates: `gpui-kit` only in ui and apps/oh-my-clear. The UI never depends on the engine; the daemon never links gpui. Every member inherits `[workspace.lints]`.
- **Transport:** `tokio::net::UnixListener` on macOS/Linux and `tokio::net::windows::named_pipe` on Windows. No TCP, and no `interprocess` crate.
- **Security:** a private `0700` runtime dir, verified tmux-style, plus a `peer_cred` uid check. On Windows: a random pipe name, `first_pipe_instance`, and `reject_remote_clients`. On every OS, a 256-bit token from the user-private `endpoint.json` must be the first frame, and the daemon writes nothing before it has checked it.
- **Discovery / single instance:** `endpoint.json` (`protocol, build_id, pid, epoch, address, token`) is written atomically. `File::try_lock` on `daemon.lock` is taken first, and only the lock holder may (un)bind the socket. macOS socket paths must stay under 104 bytes.
- **Spawn:** the UI starts the daemon when it can't connect, and replaces a daemon of another build. On Unix it uses `process_group(0)`; on Windows `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` (plus `CREATE_BREAKAWAY_FROM_JOB` with a retry without it). The daemon prints one `READY` line on stdout; logs never go to stdout. No `unsafe`, no `daemonize`.
- **Wire:** `u32 LE len | u8 kind | payload`, hand-rolled in `omc-ipc`. Kind `0x01` is a serde_json control frame; other kinds are reserved for raw binary data.
  - A frozen meta subset (`hello`, `ping`, `shutdown`) never changes shape, so any client build can greet, probe and stop any daemon build.
  - A new daemon `epoch` drops client caches. The engine never blocks on a slow client: every outgoing frame goes through one bounded per-connection queue.
- **Lifecycle:** the daemon exits on a `shutdown` request, SIGTERM/SIGINT, or 10 minutes with no client and no work.

### Consequences
* Good: work survives UI crashes and restarts. The daemon runs headless. Engine changes do not rebuild the gpui crate, and the layering is checked mechanically instead of by comments.
* Bad: two artifacts to sign and ship. Protocol versioning and upgrade handoff are permanent work. Engine tests need an in-memory transport (`tokio::io::duplex`) to keep the boundary honest.

## Evidence
- docs/research/2026-09-28-daemon-ipc.md (tmux, zellij, wezterm, Zed remote_server, VS Code read in source). Probes: UDS ≈1.5 GB/s; a 146-byte socket path fails on macOS; a `process_group(0)` child survives its parent; `try_lock` → `WouldBlock`. bincode RUSTSEC-2025-0141, daemonize RUSTSEC-2025-0069 (verified 2026-09-28)
- `cargo xtask layers` unit tests (UI→engine, daemon→gpui-kit, unlisted member, missing lint inheritance) (2026-09-28)
