//! Domain state machines and pure reducers (Workspace, Worktree, Session, Run, Task, Dispatch, Message, Gate) plus the `OrchestratorPort` trait.
//!
//! `fn apply(state, event)` style, no I/O; unit-test everything here. Allowed internal deps: `LAYERS` in xtask/src/layers.rs. Architecture: docs/memory/decisions/0008-process-split-daemon-ipc.md
