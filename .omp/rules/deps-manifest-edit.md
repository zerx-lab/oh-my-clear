---
description: Cargo.toml dependency changes must pass the dependency gate and update the ledger
condition: "(?m)^\\s*\\[(?:workspace\\.)?(?:dev-|build-)?dependencies|^\\s*(?!(?:version|rust-version|edition|resolver)\\s*=)[A-Za-z0-9_-]+(?:\\.workspace)?\\s*=\\s*(?:\\{|\"[\\^=~<>]?\\d|true)"
scope: "tool:edit(*Cargo.toml), tool:write(*Cargo.toml)"
interruptMode: never
---

If this edit adds, removes, replaces, or re-versions a dependency:

1. Run `skill://dep-review` (category check, maintenance, security, cost). One crate per category.
2. Declare it in root `[workspace.dependencies]`; members use `name.workspace = true`.
3. Record/update the row in `docs/memory/deps.md`; new category or replacement → ADR in `docs/memory/decisions/`.
4. `cargo deny --all-features check` must pass.
