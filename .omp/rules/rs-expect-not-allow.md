---
description: Suppress lints only with #[expect(lint, reason = "...")], never #[allow]
condition: "#!?\\[allow\\("
scope: "tool:edit(*.rs), tool:write(*.rs)"
---

`#[allow(..)]` fails the build (`clippy::allow_attributes`). First try to fix the code.
If suppression is truly justified, use `#[expect(clippy::lint_name, reason = "why this is sound")]` on the narrowest item.
`#[expect]` errors when the lint stops firing, so stale suppressions get removed.
Never suppress the no-panic lints (`unwrap_used`, `expect_used`, `panic`, `indexing_slicing`, …) to get code through.
