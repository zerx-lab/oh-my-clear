//! `AgentAdapter`/`AgentSession` trait and adapters: acp (third-party agents, own tokio JSON-RPC driver), pty (TUI fallback), mock (tests).
//!
//! The native agent implements the same trait in dial-native. The mock adapter goes behind a `test-support` feature. Allowed internal deps: `LAYERS` in xtask/src/layers.rs. Architecture: docs/memory/decisions/0009-native-agent-and-acp.md
