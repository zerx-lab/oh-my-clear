//! Model provider clients: Anthropic Messages, OpenAI Responses, Gemini, OpenAI-compatible chat; hand-rolled SSE, normalized stream types, credentials.
//!
//! Must round-trip provider-opaque state (thinking signatures, encrypted reasoning) and control cache breakpoints exactly. Allowed internal deps: `LAYERS` in xtask/src/layers.rs. Architecture: docs/memory/decisions/0009-native-agent-and-acp.md
