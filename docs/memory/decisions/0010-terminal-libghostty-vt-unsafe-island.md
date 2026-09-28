---
status: accepted
date: 2026-09-28
tags: [terminal, ffi, unsafe, build, platform]
---
# 0010 Terminal emulation = libghostty-vt in a single unsafe-island crate; PTY = alacritty_terminal `tty` only

## Context and Problem Statement
The user switched terminal emulation from `alacritty_terminal` (ADR 0005) to libghostty-vt, Ghostty's VT library, which has a C ABI and is built with Zig. ADR 0003 sets `unsafe_code = "forbid"` everywhere and keeps FFI inside dependencies. There is no usable Rust binding: crates.io `libghostty-vt` 0.2.1 pins zig 0.15.2 and `git clone`s in `build.rs`. libghostty-vt has no PTY support, so a PTY must come from elsewhere.

## Considered Options
* crates.io `libghostty-vt` 0.2.1: rejected. It fails on zig 0.16, needs the network at build time, and binds someone else's commit.
* An own crate, built with `zig build` from a pinned git submodule, with checked-in bindings (**chosen**)
* PTY: `portable-pty` 0.9.0 (release 2025-02 fails dep-review criterion 1; released Windows `kill()` bug) · own ConPTY/openpty code (~500 LOC of unsafe) · `alacritty_terminal` 0.26 `tty` module only (**chosen**)

## Decision Outcome
- **`crates/dial-ghostty`** is the only crate allowed to contain `unsafe` (the unsafe island).
  - Contents: checked-in bindgen output (allowlisted, with the regeneration command in the file; no libclang at build time) plus a safe, panic-free wrapper (Terminal, formatter, snapshot; later RenderState and key/mouse encoders).
  - Lints: it carries its own `[lints]` table, identical to `[workspace.lints]` except `unsafe_code = "deny"`. `cargo xtask layers` fails on any drift, and `UNSAFE_ISLANDS` lists the islands.
  - Every `unsafe` block has a `// SAFETY:` comment and exactly one unsafe op.
  - A new island (Windows sandbox, ConPTY job glue) requires an ADR amendment plus an entry in `UNSAFE_ISLANDS`. Every other crate keeps `forbid`.
- **Source and build:**
  - `third_party/ghostty` is a shallow git submodule pinned to a Ghostty `main` commit. The v1.3.1 tag lacks the Terminal, formatter and snapshot APIs.
  - `build.rs` checks for **zig 0.16.x** (exact minor, required by upstream `requireZig`) and runs `zig build -Demit-lib-vt -Doptimize=ReleaseFast -Demit-xcframework=false -Dversion-string=<pin>` offline (Zig packages are pre-fetched once). It links `libghostty-vt.a` from an isolated directory, because on macOS a `.dylib` in the same directory silently wins.
  - Cost: cold build ≈35–45 s, no-op ≈0.4 s, +1.3 MB to the binary.
- **Platforms:**
  - macOS and Linux builds are verified.
  - Windows MSVC must build on a Windows host with MSVC Build Tools; cross-builds cannot produce the MSVC-ABI archive. Upstream CI and libghostty-rs build `x86_64-pc-windows-msvc`; aarch64-windows is unproven.
  - CI runs the gates on all three OSes with `mlugg/setup-zig` at 0.16.0.
- **`dial-term`** (no unsafe; ghostty + proto) is dial's terminal model: seq bookkeeping, attach/snapshot, OSC 7/133/title → proto events, and screen reads for agents.
- **Daemon/UI split (ADR 0008):**
  - The daemon owns the authoritative `Terminal` per PTY on the thread that owns the PTY (`Terminal` is `Send` but not `Sync`; verified: libc malloc, no thread-locals, no callbacks). That thread also answers DA/DSR queries back to the PTY.
  - The daemon streams **raw PTY bytes** as `0x02` frames with `seq`.
  - Attach = `ghostty_snapshot_encode` blob + the `seq` it covers; the UI then applies frames with a greater `seq` to its own `Terminal`, which it renders via RenderState. Render diffs are not streamed.
  - Measured: parse 442–791 MB/s; a 10k-row snapshot is 2.9 MB, encodes in ≈1 ms and decodes in 3.3 ms.
- **PTY:** `alacritty_terminal = { version = "0.26", default-features = false }`, `tty` module only, confined to `dial-process` by `xtask layers`. Using its emulator (`term`, `grid`, `vte`, `event_loop`) is banned by the TTSR rule `term-no-alacritty-emulator`. Set `TERM` via options; never call `setup_env()`.
- **Bump procedure:** move the submodule, re-fetch Zig packages, regenerate bindings, run nextest on all 3 OSes. Expect C API breaks: the API is unversioned; `GhosttySearchOptions` was removed 2026-08-31. Ship the Ghostty MIT license and the bundled simdutf/highway/wuffs/uucode notices (cargo-deny cannot see them).

### Consequences
* Good: a modern VT core (kitty keyboard/graphics, OSC 8/52/133, fast SIMD parsing) with cheap reattach snapshots. `unsafe` is confined to one audited crate, and the no-panic lints still apply there.
* Bad: Zig becomes a required toolchain for every developer and CI runner, with an exact minor version. Every Ghostty bump is a manual port. The Windows link is proven only by CI. `alacritty_terminal` still compiles its unused emulator code (≈4 s).

## Evidence
- docs/research/2026-09-28-libghostty-vt.md: macOS probe with zig 0.16.0 and Rust 1.98 (cells, SGR, OSC 7/133, snapshot round-trip), cross-target results, PTY crate table (crates.io + RustSec, 2026-09-28)
- https://github.com/ghostty-org/ghostty/actions/runs/34894281701 (Windows libghostty-vt CI), https://github.com/Uzaaft/libghostty-rs/actions/runs/33512181744
