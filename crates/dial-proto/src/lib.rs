//! Wire types shared by daemon and UI: ids, `Command`, `EngineEvent`, `AgentEvent` (upserts by id), orchestration tool specs.
//!
//! Pure data + serde, no I/O. IPC framing lives in dial-ipc. Allowed internal deps: `LAYERS` in xtask/src/layers.rs. Architecture: docs/memory/decisions/0008-process-split-daemon-ipc.md
