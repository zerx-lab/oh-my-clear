//! Orchestration MCP server hosted by the daemon: per-session loopback HTTP, stdio via `dial-daemon mcp`; specs from dial-proto.
//!
//! Same tool specs as the native agent's `OrchestratorPort` tools, so both surfaces cannot drift. Allowed internal deps: `LAYERS` in xtask/src/layers.rs. Architecture: docs/memory/decisions/0009-native-agent-and-acp.md
