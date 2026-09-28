//! Daemon core: per-session actors, command router, journal recovery, per-stream event bus, IPC server, agent installs.
//!
//! Implements `OrchestratorPort`. Linked only by apps/dial-daemon; never by the UI. Allowed internal deps: `LAYERS` in xtask/src/layers.rs. Architecture: docs/memory/decisions/0008-process-split-daemon-ipc.md
