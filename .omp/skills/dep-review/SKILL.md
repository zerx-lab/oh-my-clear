---
name: dep-review
description: Use before adding, removing, replacing, or re-versioning any Rust dependency — one-crate-per-category gate with maintenance/security/performance review
---

# Dependency review gate

Policy (ADR 0004): exactly one crate per category; every direct dependency is actively maintained, secure, and performant. `deny.toml` is the machine half of this gate; this checklist is the human half. Record the result in `docs/memory/deps.md`.

## 1. Category check (stop early)
1. Read `docs/memory/deps.md`. If the category already has a pick, use it. Replacing a pick = superseding ADR.
2. Check if gpui-kit already pulls a suitable crate: `cargo tree -i <crate> -e normal --target all`. Reuse that version instead of adding an alternative (e.g. parking_lot, chrono, uuid, async-channel, serde_json).
3. Could `std` do it? (`LazyLock`/`OnceLock`, `std::process`, `std::fs`) → no dependency.

## 2. Evidence (record every item with a date)
Collect via crates.io API (`https://crates.io/api/v1/crates/<name>`), GitHub API, `cargo owner --list <name>`, RustSec (`https://rustsec.org/packages/<name>.html`).

| # | Criterion | Pass bar |
|---|---|---|
| 1 | Maintenance | release ≤12 months ago (or small + explicitly "done"); commits ≤6 months; issues triaged |
| 2 | Bus factor | >1 publisher/owner or an org/team (tokio-rs, rust-lang, dtolnay-level track record) |
| 3 | Security | no open RustSec advisory; `unsafe` justified/minimal; prefer `#![forbid(unsafe_code)]` crates |
| 4 | Supply chain | note `build.rs`, proc-macros, bundled C/native code, network at build time |
| 5 | Adoption | meaningful recent downloads; used by tokio/Zed/rust-lang ecosystem is a plus |
| 6 | License | in `deny.toml` `[licenses] allow`, else explicit exception + reason |
| 7 | MSRV | `rust-version` ≤ workspace `rust-version` (1.98) |
| 8 | Cost | crate-count delta (`cargo tree -e normal --prefix none \| sort -u \| wc -l` before/after), new duplicates (`cargo deny check bans`), compile-time delta (`cargo build --timings`), release binary size delta |
| 9 | Features | `default-features = false` + explicit features when defaults are heavy |
| 10 | Panic surface | API returns `Result` on bad input; panicking constructors noted with fallible alternatives |

Any hard fail (1, 3, 6, or an existing pick in the category) → reject, or escalate to the user with evidence.

## 3. Apply
1. Add to root `[workspace.dependencies]` with an exact-enough requirement (pre-1.0 fast movers such as gpui-kit / ACP schema: `=X.Y.Z`). Members: `name.workspace = true`.
2. If it creates a new category or replaces a crate: add a `deny.toml` `[bans] deny` entry for the obvious alternatives (with `wrappers` if gpui pulls them transitively), and write an ADR (`skill://memory`).
3. Update the `docs/memory/deps.md` row (category, crate, version req, ADR, dates, evidence, rejected alternatives).
4. Run the gates:
   ```sh
   cargo deny --all-features check
   cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
   cargo nextest run --workspace --all-features --locked
   ```
5. Removal: delete from `[workspace.dependencies]` (cargo-deny `unused = "deny"` catches leftovers) and the ledger row.

## 4. Periodic re-verification
During memory consolidation, re-check rows whose `Verified` date is >90 days old (criteria 1–3) and update the date or open a replacement ADR.
