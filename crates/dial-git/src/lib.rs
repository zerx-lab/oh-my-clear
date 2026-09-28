//! git CLI wrapper: worktree add/list/remove/prune, status, diff/numstat, commit; typed output parsers.
//!
//! Shells out through dial-process; no libgit2/gix. Allowed internal deps: `LAYERS` in xtask/src/layers.rs. Architecture: docs/memory/decisions/0008-process-split-daemon-ipc.md
