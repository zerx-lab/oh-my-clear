---
description: Avoid panicking index/slice expressions in Rust; use .get()/iterators
astCondition: "$X[$I]"
scope: "tool:edit(*.rs), tool:write(*.rs)"
interruptMode: never
---

`v[i]`, `map[&k]` and `&s[a..b]` panic when out of bounds / missing (clippy `indexing_slicing`, `string_slice` are denied).
Use `.get(i)`, `.get(a..b)`, `.first()`/`.last()`, `split_at_checked`, `split_first`, iterators, or `str::get(a..b)` / `char_indices`.
Allowed only inside `#[cfg(test)]`.
