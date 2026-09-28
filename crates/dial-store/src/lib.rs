//! SQLite persistence: append-only journal with per-stream seq, projections cache, settings, migrations.
//!
//! The journal is the source of truth; UI state is a fold over it. Allowed internal deps: `LAYERS` in xtask/src/layers.rs. Architecture: docs/memory/decisions/0008-process-split-daemon-ipc.md
