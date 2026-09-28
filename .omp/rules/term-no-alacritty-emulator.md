---
description: Terminal emulation is libghostty-vt (dial-term/dial-ghostty); alacritty_terminal is PTY-only
condition:
  - "alacritty_terminal::(?:term|grid|vte|event_loop|index|selection|event)\\b"
  - "\\b(?:vt100|vte|termwiz|wezterm-term)\\s*[.=]"
scope: "tool:edit(*.rs), tool:write(*.rs), tool:edit(*Cargo.toml), tool:write(*Cargo.toml)"
---

dial's terminal emulator is **libghostty-vt** (ADR 0010):

- Parse/render/snapshot terminals through `dial-term` (safe model) → `dial-ghostty` (the only crate with `unsafe`, FFI to libghostty-vt).
- `alacritty_terminal` is allowed **only** for its `tty` module (PTY spawn/resize/IO) and **only** in `dial-process` (`cargo xtask layers` enforces the crate boundary). Do not use its `Term`, grid, `vte` parser or event loop; do not call `tty::setup_env()` (set `TERM` via the spawn options).
- Do not add another VT parser/emulator crate (vte, vt100, termwiz, wezterm-term).
- Key/mouse/paste input to a PTY is encoded with libghostty-vt's encoders, not hand-built escape sequences.
