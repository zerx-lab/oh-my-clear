<!-- Research snapshot 2026-09-28 (bootstrap session). Point-in-time evidence: versions/dates/activity go stale; /tmp paths mentioned below no longer exist. Decisions derived from this live in docs/memory/decisions/. -->

# Rust quality gates for `oh-my-clear` (no-panic, one-crate-per-category)

Toolchain these were checked against: rustc/clippy **1.98.0 (2026-08-18)**, cargo-nextest **0.9.146**, cargo-deny **0.20.2**, rustup 1.29.1.
Scratch crate: `/tmp/gatecheck`, with `gpui-kit 0.7` + tokio + reqwest 0.13 + serde + thiserror + anyhow + tracing added. **Every config block below ran clean there**: `cargo clippy … -D warnings` exited 0, `cargo nextest run --profile ci` wrote a JUnit file, `cargo deny check` printed `advisories ok, bans ok, licenses ok, sources ok`, and `cargo fmt --check` exited 0. Anything not tested that way is marked [UNVERIFIED].

How lint facts were checked: I ran `cargo clippy --explain <lint>` for each lint (this shows the description plus every `clippy.toml` option it has), and read `cargo clippy -- -W help` for the default level and group.

---

## 1. Lints (`[workspace.lints]` in the root Cargo.toml)

Use `[workspace.lints]` in the root file and put `[lints] workspace = true` in every member crate. A member that says `workspace = true` cannot also list its own lints ([Cargo docs](https://doc.rust-lang.org/cargo/reference/workspaces.html#the-lints-table)). So if one crate needs a different level, set it with a crate-level `#![expect(lint, reason = "…")]`.

### 1.1 Lint name, group and default level (from the local clippy 1.98)

| lint | group | default | test-escape option in clippy.toml |
|---|---|---|---|
| `unwrap_used` | restriction | allow | `allow-unwrap-in-tests` (default false); also `allow-unwrap-in-consts` (default **true**), `allow-unwrap-types` |
| `expect_used` | restriction | allow | `allow-expect-in-tests` (default false); `allow-expect-in-consts` (default true) |
| `panic` | restriction | allow | `allow-panic-in-tests` (default false) |
| `todo` | restriction | allow | **none** |
| `unimplemented` | restriction | allow | **none** |
| `unreachable` | restriction | allow | **none** |
| `indexing_slicing` | restriction | allow | `allow-indexing-slicing-in-tests` (default false); `suppress-restriction-lint-in-const` |
| `string_slice` | restriction | allow | none |
| `get_unwrap` | restriction | allow | none |
| `panic_in_result_fn` | restriction | allow | none. It **also flags `assert!` inside test fns that return `Result`** (seen in the scratch crate) |
| `unwrap_in_result` | restriction | allow | none |
| `arithmetic_side_effects` | restriction | allow | `arithmetic-side-effects-allowed`, `-allowed-binary`, `-allowed-unary` |
| `exit` | restriction | allow | none (it already allows `exit` inside `main`) |
| `mem_forget` | restriction | allow | none |
| `missing_assert_message` | restriction | allow | none |
| `allow_attributes` | restriction | allow | none |
| `allow_attributes_without_reason` | restriction | allow | none |
| `dbg_macro` | restriction | allow | `allow-dbg-in-tests` |
| `print_stdout` / `print_stderr` | restriction | allow | `allow-print-in-tests` |
| `undocumented_unsafe_blocks`, `multiple_unsafe_ops_per_block` | restriction | allow | (comment-placement options) |
| `let_underscore_must_use`, `unused_result_ok`, `infinite_loop`, `tests_outside_test_module`, `rc_buffer` | restriction | allow | – |
| `fallible_impl_from` | **nursery** | allow | – |
| `missing_panics_doc`, `manual_assert`, `unchecked_time_subtraction`, `large_futures`, `large_stack_arrays` | **pedantic** | allow | – |
| `panicking_unwrap`, `option_env_unwrap`, `panicking_overflow_checks` | correctness | **deny** | – (already on by default) |
| `await_holding_lock`, `await_holding_refcell_ref` | suspicious | warn | – |

Name corrections:
- `unchecked_duration_subtraction` is **no longer a lint** (`unknown lint`). Its current name is `unchecked_time_subtraction`, and it is in pedantic.
- `option_map_unwrap_or` does not exist. The current name is `map_unwrap_or`.

rustc lints that already exist and are **deny by default**: `arithmetic_overflow` and `unconditional_panic`. These only catch overflow and out-of-bounds cases that are provable at compile time.

### 1.2 Recommended block (verified: no unknown-lint warnings, clean code passes `-D warnings`)

```toml
[workspace.lints.rust]
unsafe_code = "forbid"
missing_debug_implementations = "warn"
unreachable_pub = "warn"
unused_qualifications = "warn"
elided_lifetimes_in_paths = "deny"
let_underscore_drop = "deny"
redundant_lifetimes = "warn"
single_use_lifetimes = "warn"
trivial_numeric_casts = "warn"
unit_bindings = "warn"
unnameable_types = "warn"
meta_variable_misuse = "warn"
rust_2018_idioms = { level = "deny", priority = -1 }
unexpected_cfgs = "deny"

[workspace.lints.clippy]
# Groups first (priority -1 so individual entries below override them).
all = { level = "deny", priority = -1 }
pedantic = { level = "warn", priority = -1 }
cargo = { level = "warn", priority = -1 }

# ---- No-panic policy (restriction group, all default-allow) ----
unwrap_used = "deny"
expect_used = "deny"
panic = "deny"
todo = "deny"
unimplemented = "deny"
unreachable = "deny"
indexing_slicing = "deny"
string_slice = "deny"
get_unwrap = "deny"
unwrap_in_result = "deny"
panic_in_result_fn = "deny"
fallible_impl_from = "deny"
arithmetic_side_effects = "deny"
unchecked_time_subtraction = "deny"
exit = "deny"
mem_forget = "deny"
missing_assert_message = "deny"
manual_assert = "deny"

# ---- Suppression hygiene: #[expect(lint, reason = "...")] only ----
allow_attributes = "deny"
allow_attributes_without_reason = "deny"

# ---- Debug leftovers / hygiene ----
dbg_macro = "deny"
print_stdout = "deny"
print_stderr = "deny"
let_underscore_must_use = "deny"
unused_result_ok = "deny"
undocumented_unsafe_blocks = "deny"
multiple_unsafe_ops_per_block = "deny"
large_futures = "deny"
await_holding_lock = "deny"
await_holding_refcell_ref = "deny"
rc_buffer = "warn"
infinite_loop = "deny"
tests_outside_test_module = "deny"

# ---- Noise from pedantic/cargo we deliberately turn off ----
module_name_repetitions = "allow"
must_use_candidate = "allow"
missing_errors_doc = "allow"
multiple_crate_versions = "allow"   # gpui brings ~85 duplicated crates; cargo-deny covers this instead
cargo_common_metadata = "allow"     # publish = false app
```

Each member crate then contains:
```toml
[lints]
workspace = true
```

`clippy.toml` at the workspace root. Clippy rejects unknown keys, and all of these were accepted:
```toml
allow-unwrap-in-tests = true
allow-expect-in-tests = true
allow-panic-in-tests = true
allow-indexing-slicing-in-tests = true
allow-print-in-tests = true
```
What the scratch crate showed: `.unwrap()`, `.expect()` and `v[0]` inside `#[cfg(test)] mod tests` passed. The same calls in non-test code failed. There is **no** test escape for `todo`, `unimplemented`, `unreachable`, `panic_in_result_fn` or `manual_assert`, so tests must use `assert!`/`assert_eq!` with a message and must not use `todo!`. `allow-dbg-in-tests` is left off on purpose.

### 1.3 Decisions

- **Suppressing a lint**: `allow_attributes` combined with `allow_attributes_without_reason` means `#[allow(..)]` fails the build (tested: `error: #[allow] attribute found`). The only allowed form is `#[expect(clippy::x, reason = "…")]`, which was tested and works. `#[expect]` also breaks the build when the lint stops firing (`unfulfilled_lint_expectations`), so stale suppressions get cleaned up automatically.
- **`arithmetic_side_effects = "deny"`**: this is the only lint that catches runtime `/` or `%` by zero and debug-build overflow panics, so it is kept on.
  - Floats, `Wrapping`, `Saturating` and divisions by a non-zero constant are ignored. Tested: `n / 2` is accepted, while `n + 1` and `10 / n.max(1)` are flagged.
  - It **also flags third-party operator impls**, for example gpui `Pixels + Pixels`. Tested with a local newtype: `arithmetic-side-effects-allowed = ["geo::Px"]` works, i.e. the path *inside the defining crate* without the crate name. `["Px"]` and `["arith::geo::Px"]` did **not** work.
  - For gpui geometry types the entry would be something like `"geometry::Pixels"` [UNVERIFIED — confirm the path against the gpui-pre source before adding].
  - If it is too noisy in the UI crate, put a crate-level `#![expect(clippy::arithmetic_side_effects, reason = "gpui f32 geometry newtypes")]` there only.
- **pedantic = warn, nursery = off**: pedantic is still `-D warnings` in CI and has real signal. The noisy pedantic lints are set to allow above. Nursery lints are unstable and churn between releases; only `fallible_impl_from` is taken from it.
- **`unsafe_code = "forbid"`**: correct for an app built on gpui-kit, since the platform FFI lives in dependencies. If a crate ever needs unsafe (Windows API or objc glue), put that crate outside the lint inheritance. Don't downgrade the workspace level. `undocumented_unsafe_blocks` is already on for that case.
- **`missing_docs`: off.** This is an app, not a public library. Turn on `missing_docs = "warn"` only in crates you designate as internal APIs. `missing_panics_doc` isn't needed because panics are banned.
- **`unused_crate_dependencies`: off.** It gives false positives with dev-deps and multiple targets. Use cargo-shear or cargo-machete instead (§5).
- **CI command**: `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`.

### 1.4 What lints cannot catch (put this in the docs)

- Dependencies panic freely: slice ops in serde_json, gpui's own `unwrap`/`expect`, tokio runtime misuse such as `block_on` inside the runtime.
- `RefCell::borrow_mut` double borrows. The same applies to gpui `Entity::update` re-entrancy, which panics at runtime [UNVERIFIED for the exact gpui behaviour]. Code that uses `RefCell` directly should use `try_borrow_mut`.
- Integer overflow panics in debug builds and silently wraps in release, unless `arithmetic_side_effects` is on.
- Allocation failure aborts the process.
- Stack overflow from deep recursion aborts the process.
- `Duration`/`Instant` subtraction panics; `unchecked_time_subtraction` covers only part of this.
- `std` APIs that panic by contract, e.g. `Vec::remove(i)`, `split_at`, `copy_from_slice` with a length mismatch, `chunks(0)`, `Instant + Duration` overflow.
- `format!` with a `Display` impl that panics; `thread::spawn`, which panics when the OS refuses a thread (`Builder::spawn` returns a `Result`).
- Code generated by derive macros is not linted.
- Clippy's `unwrap_used` does not catch `Option::unwrap_unchecked`, which is unsafe and already forbidden.

Remaining risk at runtime: a panic hook plus crash reporting (§2).

---

## 2. Panic strategy in release

**Recommendation: keep `panic = "unwind"`, the default, in release.** Install a panic hook at startup that logs through `tracing::error!` and writes a crash file. Don't use `panic = "abort"` for this app.

| | unwind (recommended) | abort |
|---|---|---|
| Panic in a tokio task | Isolated. `JoinHandle` returns a `JoinError` (panic); that task fails, but the UI and the other tasks keep running | Whole app dies, taking every in-flight task with it |
| Cleanup (kill child processes, flush state) | `Drop` impls run while unwinding | No `Drop` runs, so orphaned child processes are likely |
| Binary size / speed | Slightly larger, with landing pads | Smaller and slightly faster |
| FFI safety | Since Rust 1.81 an unwind that reaches an `extern "C"` boundary aborts anyway, so objc/win32 callbacks are safe | Same |
| `catch_unwind` | Works | Does nothing |

For a desktop app running long background jobs, containing failures matters more than a few percent of binary size. The hook still runs under `abort`, so crash logging works either way.

Suggested release profile [partly UNVERIFIED — tune after measuring]:
```toml
[profile.release]
panic = "unwind"
lto = "thin"
codegen-units = 1
strip = "debuginfo"   # keep symbols for crash reports; use "symbols" only if you ship split debuginfo
debug = "line-tables-only"
overflow-checks = false  # default; arithmetic_side_effects lint is the guard

[profile.dev.package]  # from gpui-kit docs "Improve development runtime performance"
gpui-pre = { opt-level = 3 }
gpui-component = { opt-level = 3 }
gpui-kit = { opt-level = 3 }
gpui-kit-assets = { opt-level = 3 }
gpui-pre-macros = { opt-level = 3 }
gpui-pre-platform = { opt-level = 3 }
rustybuzz = { opt-level = 3 }
taffy = { opt-level = 3 }
ttf-parser = { opt-level = 3 }
```
The `[profile.dev.package]` list is copied verbatim from https://gpui-kit.com/llms-full.txt ("Improve development runtime performance").

---

## 3. nextest

`.config/nextest.toml` (tested with 0.9.146: default and ci profiles both ran, and ci wrote `target/nextest/ci/junit.xml`):

```toml
nextest-version = { required = "0.9.146" }

[profile.default]
retries = 0
fail-fast = true
slow-timeout = { period = "30s", terminate-after = 4, grace-period = "5s" }
leak-timeout = "200ms"
status-level = "pass"
final-status-level = "flaky"
failure-output = "immediate-final"
success-output = "never"

[profile.ci]
retries = { backoff = "exponential", count = 2, delay = "1s", max-delay = "5s", jitter = true }
flaky-result = "fail"          # retries surface flakes but still fail the run
fail-fast = false
slow-timeout = { period = "60s", terminate-after = 3, grace-period = "10s" }
global-timeout = "30m"
status-level = "fail"
final-status-level = "flaky"
failure-output = "immediate-final"

[profile.ci.junit]
path = "junit.xml"             # relative to <store.dir>/<profile> => target/nextest/ci/junit.xml
store-success-output = false
store-failure-output = true
```

Keys were checked against https://nexte.st/docs/configuration/reference/, including the embedded default config. Relevant details:
- `retries` accepts an object form with `backoff`, `count`, `delay`, `max-delay` and `jitter`.
- `flaky-result` needs nextest 0.9.131 or later.
- `slow-timeout.on-timeout` needs 0.9.115 or later.
- `fail-fast = { max-fail = N, terminate = "wait"|"immediate" }` needs 0.9.111 or later.
- The JUnit `path` is relative to `store.dir/<profile>`.
- JSON schema: https://nexte.st/schemas/repo-config.json. Locally: `cargo nextest self schema repo-config`.

**Zero tests**: this is a **CLI flag or env var only, not a config key**. The reference has no such key.
- `--no-tests=<auto|pass|warn|fail>`, or `NEXTEST_NO_TESTS`. `auto` defaults to fail.
- Checked locally: a filter matching nothing prints `error: no tests to run` and exits **4**. With `--no-tests=warn` it exits 0.
- While the repo has no tests, CI should pass `--no-tests=warn`. Remove the flag once tests exist.

**Commands**
```sh
cargo nextest run --workspace --all-features --locked                  # local
cargo nextest run --workspace --all-features --locked --profile ci     # CI (or NEXTEST_PROFILE=ci)
```

**Doctests**: nextest doesn't run them. Its docs say: "Doctests are currently not supported … run doctests in a separate step with `cargo test --doc`" (https://nexte.st/docs/running/, footnote).

Doctest code also bypasses the workspace lints. So an `unwrap()` in a doc example would slip past the no-panic policy.

Recommended policy: put `doctest = false` in every lib crate's `[lib]` section, mark rustdoc code blocks as ```` ```ignore ```` or ```` ```text ````, and write examples as nextest unit or integration tests. That keeps "never `cargo test`" true with no exceptions.

---

## 4. `deny.toml` (cargo-deny 0.20.2 — verified `advisories ok, bans ok, licenses ok, sources ok` on the gpui-kit 0.7 graph)

Schema notes (docs at https://embarkstudios.github.io/cargo-deny/checks/…/cfg.html):
- **advisories**: `vulnerability`, `notice` and `severity-threshold` were removed; these advisories now always error. The docs page lists `unsound` as removed too, yet also documents `unsound = "all"|"workspace"|"transitive"|"none"` (default `workspace`). 0.20.2 accepted `unsound = "all"`.
- **licenses**: `unlicensed`, `deny`, `copyleft`, `allow-osi-fsf-free` and `default` were removed. Any license not in `allow` is denied.
- `cargo deny init` writes a template whose `[graph]`, `[output]`, `[licenses.private]`, `[sources.allow-org]` and similar sections match the block below.

```toml
# cargo-deny 0.20 configuration. Docs: https://embarkstudios.github.io/cargo-deny/
[graph]
targets = [
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
]
all-features = true

[output]
feature-depth = 1

[advisories]
yanked = "deny"
# "all" fails today on gpui transitive deps: instant (RUSTSEC-2024-0384), paste (RUSTSEC-2024-0436),
# rustybuzz (RUSTSEC-2026-0206), ttf-parser (RUSTSEC-2026-0192). Gate our direct deps only.
unmaintained = "workspace"
unsound = "all"
unused-ignored-advisory = "deny"
ignore = [
    # { id = "RUSTSEC-YYYY-NNNN", reason = "<why unaffected> — <tracking link>" },
]

[licenses]
confidence-threshold = 0.93
unused-allowed-license = "warn"
include-dev = false
allow = [
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "MIT",
    "MIT-0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Zlib",
    "0BSD",
    "BSL-1.0",
    "CC0-1.0",
    "Unlicense",
    "Unicode-3.0",
    "MPL-2.0",          # option-ext, dwrote, cbindgen (file-level copyleft; OK for an app)
    "bzip2-1.0.6",      # libbz2-rs-sys
]
exceptions = [
    # { crate = "some-crate", allow = ["LGPL-2.1-or-later"], reason = "..." },
]

[licenses.private]
ignore = true

[bans]
# gpui brings ~85 duplicated crates (windows-*, objc2*, thiserror 1+2, toml, syn, hashbrown…).
# Global "deny" would need a huge skip list that churns on every gpui bump → warn + review.
multiple-versions = "warn"
wildcards = "deny"
allow-wildcard-paths = true
highlight = "simplest-path"
workspace-default-features = "allow"
external-default-features = "allow"
deny = [
    # One crate per category. `wrappers` = third-party parents allowed to pull it transitively.
    { crate = "smol", wrappers = ["gpui-base", "gpui-component", "gpui-pre-linux"], reason = "async runtime is tokio; smol only via gpui" },
    { crate = "async-std", reason = "async runtime is tokio" },
    { crate = "ureq", reason = "HTTP client is reqwest" },
    { crate = "isahc", reason = "HTTP client is reqwest" },
    { crate = "surf", reason = "HTTP client is reqwest" },
    { crate = "snafu", reason = "errors: thiserror (typed) + anyhow (app edge)" },
    { crate = "eyre", reason = "errors: thiserror (typed) + anyhow (app edge)" },
    { crate = "color-eyre", reason = "errors: thiserror (typed) + anyhow (app edge)" },
    { crate = "failure", reason = "deprecated; use thiserror" },
    { crate = "openssl", reason = "TLS is rustls" },
    { crate = "openssl-sys", reason = "TLS is rustls" },
    { crate = "native-tls", reason = "TLS is rustls" },
    { crate = "env_logger", reason = "logging is tracing-subscriber" },
    { crate = "simplelog", reason = "logging is tracing-subscriber" },
    { crate = "fern", reason = "logging is tracing-subscriber" },
]
skip = []
skip-tree = []

[bans.workspace-dependencies]
duplicates = "deny"            # every member must use `dep.workspace = true`
include-path-dependencies = true
unused = "deny"                # no dead entries in [workspace.dependencies]

[bans.std-replacements]        # bans direct use of lazy_static / once_cell etc. (tested: both error)
scope = "workspace"
level = "deny"

[sources]
unknown-registry = "deny"
unknown-git = "deny"
required-git-spec = "rev"      # any future git dep must pin a commit
allow-registry = ["https://github.com/rust-lang/crates.io-index"]
allow-git = []                 # gpui-kit / gpui-pre are on crates.io now; add exact URLs if a git dep is ever needed
unused-allowed-source = "warn"
```

Things tested in the scratch crate:
- `smol` or `lazy_static` as a *direct* dependency fails. Output: `error[banned]: crate 'smol = 2.0.2' is explicitly banned`, `error[replaced-in-std]: crate 'lazy_static …'`, `… 'once_cell …'`.
- `lazy_static` must **not** be in `bans.deny`. It arrives transitively through dwrote, zed-font-kit and sharded-slab (tracing-subscriber), so `std-replacements` with `scope = "workspace"` is the right tool.
- The `wrappers` list for `smol` must list every gpui parent. I found them with `cargo tree -i smol --target all`. cargo-deny prints `unmatched-wrapper` when a new one shows up.
- The list may shift when gpui is bumped; re-run that command then.

Other deny rules:
- **git sources**: gpui-kit docs now use crates.io, e.g. `gpui-kit = "0.6"` and `gpui-pre = "=0.3.x"`, so `allow-git` stays empty. If you ever pin Zed or gpui-kit from git, add the exact URL (e.g. `"https://github.com/longbridge/gpui-kit"`) with `rev = "<sha>"`.
- **CI**: run `cargo deny --all-features check` (advisories, bans, licenses, sources). `cargo-audit` is covered by the advisories check, so it is not needed.
- `cargo clippy` warns: `the following packages contain code that will be rejected by a future version of Rust: block v0.1.6`. This is a gpui transitive dependency; track it, don't act.

---

## 5. Dependency review gate

### 5.1 Checklist (every new direct dependency; record the answers in the PR description)

1. **Category**: does a crate already chosen for this category cover the need? If so, stop. Also check `cargo tree -i <crate>`: if gpui already brings it in, reuse that version.
2. **Maintenance**: last release within 12 months, *or* the crate is explicitly "done" and small. Commits within 6 months. More than one maintainer with publish rights (`cargo owner --list <crate>`). Open issues are being triaged.
3. **Security**: no open RustSec advisory (`cargo deny check advisories`). Review its `unsafe` usage (`cargo geiger` 0.13.0 is optional). Prefer crates that use `#![forbid(unsafe_code)]`.
4. **Supply chain**: `build.rs` or proc-macro? Native binaries? cargo-deny `[bans.build]` can flag these; enable it later if wanted.
5. **Adoption**: meaningful recent downloads on crates.io; used by tokio, Zed or rust-lang crates is a plus.
6. **License**: already in the `deny.toml` allow list; otherwise it needs an explicit `exceptions` entry.
7. **MSRV**: its `rust-version` is at or below our pin (1.98).
8. **Cost**: added crate count (`cargo tree -e normal --prefix none | sort -u | wc -l` before and after), duplicates it adds (`cargo deny check bans`), compile-time delta (`cargo build --timings`), binary delta (`cargo bloat` 0.12.1 is stale, last release 2024-05; better to diff the stripped release binary size).
9. **Features**: `default-features = false` plus an explicit feature list for anything with heavy defaults.
10. **Panic surface**: does the API return `Result` instead of panicking on bad input?

### 5.2 Tools
- **Required**: `cargo-deny` 0.20.2 (advisories, licenses, bans, sources), clippy, nextest.
- **Unused deps**: `cargo-shear` 1.14.0 (2026-09-22, actively released, stable toolchain, workspace-aware, can `--fix`) or `cargo-machete` 0.9.2 (2026-04-15, stable, regex-based, more false positives). Pick **one**; I recommend **cargo-shear**. `cargo-udeps` (0.1.61) needs nightly, so skip it. None of these is installed.
- **Optional**: `cargo-vet` 0.10.2 (last release 2026-01, heavy process to run) and cargo-crev. Both are too heavy for a new solo repo; revisit if the dependency count grows.
- `cargo-audit` 0.22.2: fully covered by cargo-deny advisories, so don't add it.

### 5.3 One pick per category (crates.io data as of 2026-09-28)

**Already in the graph through gpui-kit 0.7.0 → gpui-pre 0.3.7 / gpui-component 0.7.0**, checked with the crates.io dependencies API: anyhow, thiserror 2, log, tracing (gpui-component), futures, async-task, async-channel, flume, parking_lot, serde, serde_json, schemars, smol (gpui-component, gpui-base, gpui-pre-linux), uuid, chrono, regex, smallvec. Picks below reuse these wherever possible, so they add nothing new.

| Category | Pick | Version (released) | Why / notes |
|---|---|---|---|
| Errors (library/domain crates) | **thiserror** | 2.0.21 (2026-09-23), MSRV 1.77 | Typed `enum` errors callers can match on. gpui already depends on it. |
| Errors (app edge / gpui glue) | **anyhow** | 1.0.104 (2026-07-18), MSRV 1.68 | **gpui's API uses `anyhow::Result`** (`HttpClient`, `Task<anyhow::Result<_>>`), so it is unavoidable at the UI edge. Rule: anyhow only in the app/UI crate; domain crates expose thiserror types. snafu (0.9.2) and eyre/color-eyre are banned in deny.toml. |
| Async runtime (I/O, processes, network) | **tokio** | 1.53.1 (2026-07-20), MSRV 1.71 | gpui's executor runs the UI and does not drive tokio I/O. The gpui-kit docs say so: "reqwest needs a tokio runtime; GPUI's executors are not one" and build a `LazyLock<tokio::runtime::Runtime>`. reqwest and the gpui-pre-reqwest-client both require tokio. smol stays transitive-only, enforced by the `wrappers` ban. Pattern: one owned multi-thread runtime; bridge results to gpui through channels or by awaiting the `JoinHandle` inside `cx.background_spawn`. |
| HTTP client | **reqwest 0.13** | 0.13.5 (2026-09-08), MSRV 1.85 | Streaming, HTTP/2, proxy support. Default TLS in 0.13 = rustls with aws-lc-rs (`default-tls → rustls → __rustls-aws-lc-rs`). Use `default-features = false, features = ["rustls", "http2", "json", "stream", "charset", "system-proxy"]`. ureq 3.4.2 is sync-only, so it would mean a second blocking I/O model → rejected. Note that `gpui-pre-reqwest-client` pulls a *fork* `gpui-pre-reqwest ^0.12.15`; prefer writing a small `HttpClient` adapter over upstream reqwest 0.13 (as the gpui-kit caching example does) to avoid a second reqwest. [UNVERIFIED] aws-lc-sys build requirements on Windows (cmake/NASM); `rustls-no-provider` plus an explicit provider is the fallback. |
| Serialization | **serde** + **serde_json** | 1.0.229 (2026-07-18) / 1.0.151 (2026-07-20) | Already brought in by gpui. Config files: **toml** 1.1.6 (2026-09-10), only if a TOML config is needed. |
| Logging / diagnostics | **tracing** + **tracing-subscriber** (+ **tracing-appender** for file rotation) | 0.1.44 (2025-12-18) / 0.3.23 (2026-03-13) / 0.2.5 (2026-04-17), all MIT | Spans per job and per async task. gpui itself logs through `log`, so enable tracing-subscriber's `tracing-log` bridge (default feature [UNVERIFIED for 0.3.23 defaults]) rather than a second logger. env_logger, fern and simplelog are banned. |
| CLI args | **none for now**; if needed, **clap** 4.6.7 (2026-09-14, MSRV 1.85) with `derive` | – | A GUI app usually needs nothing, and `std::env::args` is enough for `--profile`-style flags. argh 0.1.19 (BSD-3) is smaller but less maintained. |
| Locks | **parking_lot** 0.12.5 (already via gpui) or `std::sync` | – | Pick one; parking_lot matches gpui and has no lock poisoning, so no `lock().unwrap()`. |
| Channels | **tokio::sync** inside the runtime; **async-channel** (via gpui) for UI↔worker | – | Don't add flume or crossbeam-channel directly. |
| Time | **chrono** (already via gpui) | 0.4.45 | Avoid adding jiff or time as a second time crate. |
| IDs | **uuid** (already via gpui) | 1.26.1 | – |
| Global/lazy statics | `std::sync::LazyLock` / `OnceLock` | std | lazy_static and once_cell are blocked by `std-replacements`. |

Not decided here (future categories that need their own gate):
- Persistence: `rusqlite` 0.40.2 vs files.

---

## 6. rustfmt.toml / rust-toolchain.toml

`rust-toolchain.toml`: pin an exact stable version so every contributor and CI get identical clippy lints. The lint set changes between releases (e.g. the `unchecked_duration_subtraction` rename). Bump deliberately.
```toml
[toolchain]
channel = "1.98.0"
components = ["rustfmt", "clippy"]
profile = "minimal"
targets = ["aarch64-apple-darwin", "x86_64-apple-darwin", "x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"]
```
Also add `rust-version = "1.98"` to `[workspace.package]`. Clippy's `msrv` defaults to it.

`rustfmt.toml`: stable options only. Tested: `imports_granularity` and `group_imports` print "unstable features are only available in nightly channel" on stable, so leave them out.
```toml
style_edition = "2024"
edition = "2024"          # standalone rustfmt/editor runs default to 2015 otherwise (seen via --print-config)
newline_style = "Unix"
use_field_init_shorthand = true
use_try_shorthand = true
```

## Gate summary (CI order)
```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo nextest run --workspace --all-features --locked --profile ci   # add --no-tests=warn until first test lands
cargo deny --all-features check
cargo shear            # once installed
```
