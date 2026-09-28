---
description: unsafe Rust only inside unsafe-island crates (dial-ghostty), each block documented
condition:
  - "\\bunsafe\\s*\\{"
  - "\\bunsafe\\s+(?:fn|impl|extern|trait)\\b"
scope: "tool:edit(*.rs), tool:write(*.rs)"
interruptMode: never
---

`unsafe_code = "forbid"` in every crate except the islands listed in `UNSAFE_ISLANDS` (`xtask/src/layers.rs`), today only `crates/dial-ghostty` (ADR 0010).

- Outside an island: find a safe API or a dependency that owns the unsafe; a new island needs an ADR amendment + `UNSAFE_ISLANDS` entry + a lint table equal to `[workspace.lints]` except `unsafe_code = "deny"`.
- Inside an island: every `unsafe` block has a `// SAFETY:` comment (`undocumented_unsafe_blocks`) and exactly one unsafe operation (`multiple_unsafe_ops_per_block`); expose only safe, panic-free wrappers returning the crate's `Result`.
