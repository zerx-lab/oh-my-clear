---
description: cargo add/remove must go through the dependency gate
condition: "\\bcargo\\s+(?:add|remove|rm)\\b"
scope: "tool:bash"
---

Do not add or remove dependencies ad hoc. First run `skill://dep-review`, then edit root `[workspace.dependencies]` (members use `name.workspace = true`), update `docs/memory/deps.md`, and verify with `cargo deny --all-features check`.
