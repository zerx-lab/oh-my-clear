//! Child process lifecycle: spawn, kill-tree (process groups / Job Objects), PTYs via `alacritty_terminal` tty, stderr tail, env/PATH.
//!
//! Daemon-only, runs on tokio. Never uses `alacritty_terminal`'s emulator (ADR 0010). Allowed internal deps: `LAYERS` in xtask/src/layers.rs. Architecture: docs/memory/decisions/0008-process-split-daemon-ipc.md
