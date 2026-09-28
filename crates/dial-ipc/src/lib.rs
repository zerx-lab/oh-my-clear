//! UI<->daemon IPC: Unix socket / named pipe transport, length-prefixed frames (JSON control + raw binary data), handshake, auth token, endpoint discovery, daemon auto-spawn.
//!
//! Daemon side (server) and UI side (`EngineHandle` client); tests run the same codec over `tokio::io::duplex`. Allowed internal deps: `LAYERS` in xtask/src/layers.rs. Architecture: docs/memory/decisions/0008-process-split-daemon-ipc.md
