---
status: accepted
date: 2026-09-28
tags: [gui, deps]
---
# 0002 GUI: gpui-kit as the single GUI dependency, exact pin

## Context and Problem Statement
dial first ships as a cross-platform desktop client (macOS, Windows, Linux). The user chose gpui-kit (https://gpui-kit.com) as the GUI foundation.

## Considered Options
* gpui-kit umbrella crate (Longbridge; re-exports GPUI + gpui-base + gpui-component)
* official `gpui` crate + hand-written components
* gpui-component directly

## Decision Outcome
Chosen option: `gpui-kit` only, pinned `=X.Y.Z` with `Cargo.lock` committed, because it bundles a matching GPUI snapshot (`gpui-pre =0.3.7` for 0.7.0), 75+ components (Dock, MessageScroller, TextView streaming markdown, Editor, VirtualList), and the official `gpui` crate is stale (0.2.2, 2025-10).

- Never list `gpui`, `gpui-pre*`, `gpui-component` directly; upgrades are deliberate PRs following upstream release notes (pre-1.0, weekly, breaking minors).
- Dev-dependency adds `features = ["test-support"]`; `#[gpui_kit::test]` runs under nextest.
- Dev profile: `[profile.dev.package]` opt-level 3 list from gpui-kit installation docs.
- Toolchain: MSRV ≥1.92 (we pin 1.98.0); Windows needs MSVC + CMake; Linux needs Vulkan/Wayland/X11 dev packages; macOS 15+ with CLT.

### Consequences
* Good, because one dependency covers runtime + components + theming, and upstream is very active (324 commits/30 days).
* Bad, because `gpui-pre` snapshots are published by one person and APIs break every minor; GPUI brings anyhow/smol/thiserror 1+2 transitively and ~85 duplicate crates (deny.toml uses `multiple-versions = "warn"`).
* Bad, because some GPUI APIs panic (`KeyBinding::new`, `cx.global`, entity re-entrancy) — guarded by `rule://gpui-patterns` and the `gpui-panicking-apis` TTSR rule.

## Evidence
- docs/research/2026-09-28-gpui-kit.md (probe compiled clippy-clean under no-panic lints, nextest test passed; verified 2026-09-28)
