---
description: Result type aliases must default to the crate's own error type
condition: "type\\s+Result<[A-Za-z_]\\w*>\\s*="
scope: "tool:edit(*.rs), tool:write(*.rs)"
interruptMode: never
---

Expose the error as a defaulted parameter and default it to the crate's `thiserror` enum (anyhow is not a dial dependency):

```rust
pub type Result<T, E = Error> = std::result::Result<T, E>;
```

Never write `type Result<T> = std::result::Result<T, Error>;` or use `anyhow::Error`.
