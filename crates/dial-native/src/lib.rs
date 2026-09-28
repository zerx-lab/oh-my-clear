//! dial's own coding agent: turn loop, tools, edit engine, permissions, context compaction, checkpoints; an `AgentAdapter` like ACP.
//!
//! Runs in-process in the daemon and emits the same `AgentEvent` stream as ACP agents; orchestration via `OrchestratorPort`. Allowed internal deps: `LAYERS` in xtask/src/layers.rs. Architecture: docs/memory/decisions/0009-native-agent-and-acp.md
