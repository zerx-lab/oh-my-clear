---
description: Run tests with cargo nextest, never cargo test
condition: "\\bcargo\\s+(?:\\+\\S+\\s+)?test\\b"
scope: "tool:bash"
---

dial runs tests with cargo-nextest only:

- `cargo nextest run --workspace --all-features` (narrow with `-p <crate>` or `-E 'test(name)'`)
- CI profile: `--profile ci`; while the workspace has no tests add `--no-tests=warn`.

Doctests are disabled by policy (`doctest = false` in `[lib]`); write examples as nextest tests. There is no `cargo test --doc` exception.
