---
description: Never write panicking Rust (unwrap/expect/panic!/todo!/unimplemented!/unreachable!)
condition:
  - "\\.unwrap\\(\\)"
  - "\\.expect\\("
  - "\\.unwrap_err\\(\\)"
  - "\\.expect_err\\("
  - "\\.unwrap_unchecked\\(\\)"
  - "\\b(?:panic|todo|unimplemented|unreachable)!\\s*[\\(\\[\\{]"
scope: "tool:edit(*.rs), tool:write(*.rs)"
---

oh-my-clear forbids panicking code outside `#[cfg(test)]` (clippy denies it; the build will fail).

- Propagate with `?` into the crate's `thiserror` error enum; add a variant instead of panicking.
- `Option` → `ok_or(Error::X)?` / `ok_or_else(..)?` / `let Some(x) = .. else { return Err(..) };`
- Defaults → `unwrap_or`, `unwrap_or_else`, `unwrap_or_default`.
- "Impossible" branches → return an error variant (e.g. `Error::Invariant("…")`), not `unreachable!`.
- Unfinished work → do not stub with `todo!`; finish it or leave the code path out.
- Tests: clippy allows unwrap/expect/panic inside `#[cfg(test)]`, but this regex cannot see `cfg(test)` and fires anyway. Write tests without them: `assert!(matches!(r, Err(Error::X(_))), "msg")`, `assert!(r.is_ok_and(|v| …), "msg")`, `let Ok(v) = r else { … }`.
