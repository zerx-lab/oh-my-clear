---
status: accepted
date: 2026-09-28
tags: [deps]
---
# 0004 One crate per category; dependency gate; tokio + thiserror-only

## Context and Problem Statement
The user requires exactly one best crate per category (no parallel error-handling or API crates) and a review gate ensuring each dependency is actively maintained, secure, and performant.

## Considered Options
* Errors: thiserror + anyhow (common split) · thiserror only · anyhow only
* Runtime: tokio · smol (already transitive via gpui)
* Gate tooling: cargo-deny only · cargo-deny + cargo-vet/crev

## Decision Outcome
- Gate = `deny.toml` (advisories incl. `unmaintained = "workspace"`, `unsound = "all"`, `yanked = "deny"`; license allowlist; category bans with gpui `wrappers`; `std-replacements`; `workspace-dependencies` dedup; crates.io only, git deps need `rev`) + `skill://dep-review` checklist + `docs/memory/deps.md` ledger. cargo-vet/crev: too heavy for now. All deps declared in root `[workspace.dependencies]`.
- Errors: **thiserror only** as a direct dependency. GPUI's `anyhow::Result` is converted at the UI edge (`map_err` into our enum) — no direct anyhow. Chosen over thiserror+anyhow because the user explicitly wants a single error-handling crate; typed enums are needed anyway to tell I/O, permission and protocol failures apart.
- Runtime: **tokio** for all I/O (daemon, IPC); the UI owns one tokio runtime in a GPUI `Global` for its IPC client, bridged to GPUI executors via channels/awaited `JoinHandle`s. smol is banned as a direct dep (allowed only through gpui wrappers). Chosen because UDS/named pipes, processes and HTTP are tokio-native and gpui-kit docs prescribe a separate tokio runtime for tokio-based clients.
- Other picks are listed in `docs/memory/deps.md` (status `chosen`); reuse crates gpui already brings (parking_lot, chrono, uuid, async-channel, serde_json).

### Consequences
* Good, because category duplication fails `cargo deny` (or TTSR `deps-banned-crates` for anyhow, which cannot be banned transitively).
* Bad, because anyhow/smol remain in the transitive graph via gpui; the rule governs direct dependencies. `multiple-versions` is only `warn`.

## Evidence
- docs/research/2026-09-28-rust-gates.md §4–5 (crates.io/GitHub/RustSec data 2026-09-28)
