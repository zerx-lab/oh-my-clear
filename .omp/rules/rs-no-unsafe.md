---
description: unsafe Rust is forbidden in every crate
condition:
  - "\\bunsafe\\s*\\{"
  - "\\bunsafe\\s+(?:fn|impl|extern|trait)\\b"
scope: "tool:edit(*.rs), tool:write(*.rs)"
interruptMode: never
---

`unsafe_code = "forbid"` holds in every crate (`[workspace.lints]`, inheritance checked by `cargo xtask layers`); no crate may contain `unsafe` (ADR 0017).

- Find a safe API or a maintained dependency that owns the unsafe (gpui already brings `objc2-app-kit` / `windows` / `ashpd` safe wrappers).
- If no safe route exists, stop: introducing `unsafe` needs a new ADR that defines an isolated FFI crate, its lint table and review rules. Inside such a crate every `unsafe` block would carry a `// SAFETY:` comment and exactly one unsafe operation (`undocumented_unsafe_blocks`, `multiple_unsafe_ops_per_block` are already denied).
