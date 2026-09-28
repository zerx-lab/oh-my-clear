# dial — non-negotiables (sent every request)

- **No panics in Rust** outside `#[cfg(test)]`: no `unwrap`/`expect`/`panic!`/`todo!`/`unimplemented!`/`unreachable!`, no `v[i]`/`&s[a..b]`, no unchecked integer arithmetic. Use `?` + the crate's `thiserror` error enum, `.get()`, `checked_*`/`saturating_*`, `let … else`, `ok_or_else`. Lint suppression only as `#[expect(lint, reason = "…")]`; `#[allow]` fails the build.
- **Tests run with `cargo nextest run`**, never `cargo test` (doctests are disabled; examples are nextest tests).
- **One crate per category.** Adding/replacing a dependency requires `skill://dep-review`, a row in `docs/memory/deps.md`, and a passing `cargo deny --all-features check`. Reuse crates gpui-kit already pulls in before adding new ones.
- **Architecture** (ADRs 0008–0011): `dial` (GUI) and `dial-daemon` (execution) are separate processes; crate edges follow `LAYERS` in `xtask/src/layers.rs` (UI never links engine/adapters, daemon never links gpui). `unsafe` only in `UNSAFE_ISLANDS` (today `dial-ghostty`, libghostty-vt FFI). UI code follows `rule://ui-design-motion`.
- **Done = all gates green** (`cargo ci`): `cargo xtask layers`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`, `cargo nextest run --workspace --all-features --locked`, `cargo deny --all-features check`.
- **Memory**: `docs/memory/` is project memory; follow `rule://memory-protocol`. Subagents never edit it — they return "memory candidates".
