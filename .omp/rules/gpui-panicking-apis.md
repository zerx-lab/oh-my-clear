---
description: GPUI APIs that panic at runtime — use the fallible variants
condition:
  - "KeyBinding::new\\("
  - "\\.global::<"
  - "\\.global_mut::<"
  - "\\.update_global::<"
  - "ReqwestClient::new\\("
  - "\\.with_damping\\("
  - "\\.with_epsilon\\("
  - "\\.with_tracking\\("
scope: "tool:edit(*.rs), tool:write(*.rs)"
---

These GPUI calls panic (verified in gpui-pre 0.3.7 / gpui-kit 0.7):

- `KeyBinding::new` unwraps keystroke/context parsing → `KeyBinding::load(keys, Box::new(action), ctx.map(Rc::new), false, None, &DummyKeyboardMapper)?` with `KeyBindingContextPredicate::parse(ctx)?`.
- `cx.global::<T>()` / `global_mut` / `update_global` panic when unset → `cx.try_global::<T>()`, `has_global`, `default_global`, `update_default_global`.
- `ReqwestClient::new()` expects → build the HTTP client fallibly.
- Motion `with_damping`/`with_epsilon` panic on invalid input → at runtime use `try_with_damping`/`try_with_epsilon`. Exception: the `const` spring presets in `omc_ui::motion` (ADR 0011) — a bad literal there is a compile error, not a runtime panic.
- `ScrollBounceMotion::with_tracking` asserts on non-finite/≤0 → pass only `const` literals, never config/user input.

Also: never `entity.update`/`read` an entity inside its own `update`/`render`/listener (lease panic) — use the provided `&mut self` and `cx`. See `rule://gpui-patterns`.
