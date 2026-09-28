//! Raw libghostty-vt bindings: checked-in bindgen output, so builds need no libclang.
//!
//! `bindings.rs` must match the Ghostty commit pinned by `GHOSTTY_COMMIT` in
//! `build.rs` (the build script refuses any other submodule HEAD). After bumping the
//! submodule and the pin, regenerate from the repo root with bindgen-cli 0.72.1 (a
//! developer tool, not a dependency; `cargo binstall bindgen-cli@0.72.1`):
//!
//! ```text
//! bindgen third_party/ghostty/include/ghostty/vt.h \
//!   --rust-target 1.85 --rust-edition 2024 --use-core --no-doc-comments \
//!   --no-layout-tests --sort-semantically \
//!   --allowlist-function 'ghostty_(free|terminal_(new|free|vt_write|resize|set|get|grid_ref)|formatter_(terminal_new|format_alloc|free)|snapshot_(encode_alloc|decoder_(new_buf|decode|free)))' \
//!   --allowlist-type 'GhosttyString' \
//!   -o crates/dial-ghostty/src/ffi/bindings.rs \
//!   -- -Ithird_party/ghostty/include -DGHOSTTY_STATIC
//! ```
//!
//! Widen the allowlist when the wrapper needs more of the C API. Layout tests are off
//! because every versioned struct carries a `size` field the library validates.

include!("ffi/bindings.rs");
