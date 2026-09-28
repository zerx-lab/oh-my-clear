---
status: accepted
date: 2026-09-28
supersedes: 0005
tags: [architecture, runtime, ipc, orchestration]
---
# 0008 UI process + background daemon over local IPC; enforced crate layering

## Context and Problem Statement
The user requires the execution layer to be decoupled from the UI: closing the UI ends only the UI process, while agents and terminals keep running and any UI can reattach later. The app must behave equally well on macOS, Windows (MSVC) and Linux. ADR 0005 put the engine in the UI process and let `dial-ui` depend on `dial-engine`, which contradicted its own "UI talks only via messages" boundary.

## Considered Options
* In-process engine (ADR 0005), with the daemon added later
* One binary with a GUI mode and a `daemon` subcommand
* Two binaries: `dial` (gpui-kit GUI) + `dial-daemon` (headless, no gpui)

## Decision Outcome
Chosen: **two binaries**. A daemon that links gpui cannot start on a headless Linux box, and it would keep windowing/GPU libraries resident in a long-lived process.
- **`dial-daemon`** owns the engine, agents (native and ACP), PTYs, the libghostty-vt terminals, worktrees and the journal, all on one tokio runtime. Planned subcommands: `run` (default), `status`, `stop`, `logs`, `install-login-item`, and `mcp` (the stdio MCP proxy for injected agents, ADR 0009).
- **`dial`** is a viewport: it holds gpui-kit views plus a `dial-ipc` client (`EngineHandle`), with a tokio runtime in a GPUI `Global` used only for IPC.
- **Layering**, enforced by `cargo xtask layers` (the first step of `cargo ci`; the table is `LAYERS` in `xtask/src/layers.rs`):

  | Crate | Allowed internal deps |
  |---|---|
  | proto, telemetry, process, ghostty, llm | — |
  | core, ipc, store | proto |
  | term | ghostty, proto |
  | git | process |
  | agent | proto, core, process, term |
  | native | proto, core, agent, llm, process, git |
  | mcp | proto, core |
  | engine | every daemon-side crate + ipc |
  | ui | proto, core, term, ipc |
  | apps/dial | ui, ipc, proto, telemetry |
  | apps/dial-daemon | engine, ipc, proto, telemetry |

  Confined external crates: `gpui-kit` only in ui/dial; `alacritty_terminal` only in process; `agent-client-protocol-schema` only in agent. Leaf I/O crates do not depend on `dial-core`, so a domain change does not rebuild them.
- **Kept from 0005:** `Command`/`EngineEvent`/`AgentEvent` in `dial-proto`; an append-only journal as the source of truth, with UI state as a fold; `git` CLI worktrees; the Orca domain model (Run/Task/Dispatch/Message/Gate). Permission requests and Gates are durable journal entries, because no UI may be attached when they arise.
- **Transport:** `tokio::net::UnixListener` on macOS/Linux and `tokio::net::windows::named_pipe` on Windows. No TCP, and no `interprocess` crate.
- **Security:** a private `0700` runtime dir, verified tmux-style, plus a `peer_cred` uid check. On Windows: a random pipe name, `first_pipe_instance`, and `reject_remote_clients`. On every OS, a 256-bit token from the user-private `endpoint.json` must be the first frame, and the daemon writes nothing before it has checked it.
- **Discovery / single instance:** `endpoint.json` (`protocol, build_id, pid, epoch, address, token`) is written atomically. `File::try_lock` on `daemon.lock` is taken before the journal opens, and only the lock holder may (un)bind the socket. macOS socket paths must stay under 104 bytes.
- **Spawn:** the UI starts the daemon when it can't connect. On Unix it uses `process_group(0)`; on Windows `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` (plus `CREATE_BREAKAWAY_FROM_JOB` with a retry without it). The daemon prints one `READY` line on stdout; logs never go to stdout. No `unsafe`, no `daemonize`.
- **Wire:** `u32 LE len | u8 kind | payload`, via tokio-util `LengthDelimitedCodec`. Kind `0x01` is a serde_json control envelope; `0x02` is `u64 stream | u64 seq | raw bytes` for PTY data.
  - A frozen meta subset (`hello`, `status`, `drain`, `shutdown`, `ping`) never changes. On a version mismatch the old daemon is drained, then restarted.
  - Every stream has a journal-persisted `seq`. `subscribe{since}` replays from the journal when it still can, otherwise sends snapshot + live. A new daemon `epoch` drops client caches. A slow client gets `lagged` and resyncs; the engine never blocks on a client.
- **Lifecycle:** the daemon exits only with 0 clients, 0 active runs/terminals, and 10 minutes idle, or on an explicit "Quit and stop agents". Logs go to `tracing-appender` with daily rotation, 14 files. Start at login is opt-in and shells out to `launchctl` / `systemctl --user` / HKCU `Run`. Packaging: on macOS the helper is `Contents/MacOS/dial-daemon`; on Windows and for AppImage, the daemon exe is copied to a per-version dir before spawning.

### Consequences
* Good: agents survive UI crashes, restarts and upgrades. The daemon runs headless (Linux servers, CI). A remote or second UI is only a transport swap. Engine changes no longer rebuild the gpui crate, and the layering is checked mechanically instead of by comments.
* Bad: two artifacts to sign and ship. Protocol versioning and upgrade handoff are permanent work (Orca's daemon is on protocol version 36). Engine tests need an in-memory transport (`tokio::io::duplex`) to keep the boundary honest.

## Evidence
- docs/research/2026-09-28-daemon-ipc.md (tmux, zellij, wezterm, Zed remote_server, VS Code, Orca, and Zeron read in source). Probes: UDS ≈1.5 GB/s; a 146-byte socket path fails on macOS; a `process_group(0)` child survives its parent; `try_lock` → `WouldBlock`. bincode RUSTSEC-2025-0141, daemonize RUSTSEC-2025-0069 (verified 2026-09-28)
- `cargo xtask layers` unit tests (UI→engine, daemon→gpui-kit, unlisted member, lint drift) (2026-09-28)
