//! dial terminal model over libghostty-vt: byte seq, attach snapshots, OSC 7/133/title events, screen reads for agents.
//!
//! Used by the daemon (authoritative terminal per PTY) and the UI (render copy). No unsafe; FFI lives in dial-ghostty. Allowed internal deps: `LAYERS` in xtask/src/layers.rs. Architecture: docs/memory/decisions/0010-terminal-libghostty-vt-unsafe-island.md
