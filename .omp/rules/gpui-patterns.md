---
description: gpui-kit/GPUI UI code patterns and pitfalls — read before writing or reviewing UI code
globs: ["**/*.rs"]
---

# gpui-kit / GPUI patterns for dial

Dependency: `gpui-kit` only (umbrella; re-exports GPUI as `use gpui_kit::*;`). Never add `gpui`, `gpui-pre*`, or `gpui-component` separately — gpui-kit pins the exact `gpui-pre` snapshot. Pin `gpui-kit = "=X.Y.Z"`; dev-dep adds `features = ["test-support"]`. Only `dial-ui` and `apps/dial` may depend on it (`cargo xtask layers`); the daemon never links gpui. Docs: https://gpui-kit.com/llms.txt, per page `https://gpui-kit.com/<path>.md` (e.g. `/component/dock.md`).

Design, tokens and motion: follow `rule://ui-design-motion` (ADR 0011).

## Startup
`application().with_assets(assets::Assets).run(|cx| { init(cx); … open_window(opts, cx, |w, cx| cx.new(|cx| View::new(w, cx))) })`.
`open_window` returns `Result` — handle it (`if let Err(e) = … { tracing::error!(…); cx.quit(); }`), never `.expect`. `init(cx)` once, before windows. Return content view, not a `Root`. Quit/close actions + keymaps are the app's job.

## State
- `Entity<T>` owns state; `cx.new`, `entity.update(cx, |t, cx| …)`, `read(cx)`. `WeakEntity::update` returns `Result` — handle it (no `let _ =`; `let_underscore_must_use` is denied).
- `cx.notify()` to re-render; `cx.emit(E)` + `impl EventEmitter<E>`; `cx.subscribe`/`observe` return `Subscription` → store in `_subscriptions: Vec<Subscription>`.
- Create child state entities (e.g. `InputState`) in `new`, never in `render`.
- Globals: `try_global`, never `global` (panics when unset).
- Re-entrancy panic: never update/read an entity inside its own `update`/`render`/listener; use the provided `&mut self` + `cx`.

## Async
- `Task` cancels on drop: store it (`Option<Task<()>>`), await it, or `.detach()`. Never call `cx.spawn(..);` bare. Never spawn unconditionally in `render`.
- `cx.spawn(async move |this, cx| …)` foreground; `cx.background_spawn` for CPU work on owned `Send` data. Guard stale results with a revision/request id.
- GPUI executors are not tokio. The UI process's only I/O is the dial-ipc connection to `dial-daemon` (ADR 0008): its tokio runtime lives in a `Global` (built fallibly at startup); bridge via bounded `async-channel` or by awaiting tokio `JoinHandle`s inside GPUI tasks. Never spawn agents, PTYs, git or HTTP from the UI — send a `Command` through `EngineHandle`.
- Streams (tokens, terminal bytes): at most one `cx.notify()` per frame per view, coalescing capped at 33 ms; heavy parsing via `background_spawn`.
- The daemon may be absent or restarting: every view renders a disconnected/reconnecting state, and resyncs from `subscribe{since}` / snapshots after reconnect.

## Actions / keys
`actions!(ns, [A, B])`; unique names (duplicates panic at startup). Bind with `KeyBinding::load` + `KeyBindingContextPredicate::parse` (not `KeyBinding::new`). Dispatch needs a focused element with matching `key_context`. `secondary` = Cmd on macOS / Ctrl elsewhere.

## Components worth reusing (gpui_kit::component::…)
Dock (persisted panel layout), Resizable, MessageScroller (virtualized tail-follow transcript), TextView (streaming markdown), VirtualList, List/ListDelegate, DataTable, Tree, Input/Textarea/Editor, Tabs, Notification, Dialog/Sheet, Command palette, Sidebar/TitleBar/StatusBar. No terminal component — render `dial-term` (libghostty-vt RenderState) in a custom element; key/mouse input goes through libghostty-vt encoders (ADR 0010).

## Tests
`#[gpui_kit::test] fn t(cx: &mut TestAppContext)` expands to `#[test]` → runs under nextest. In test modules import explicitly; `use gpui_kit::*;` shadows `#[test]`.

## Arithmetic lint
`clippy::arithmetic_side_effects` flags `Pixels` ops. Prefer allowlisting the type in `clippy.toml` (`arithmetic-side-effects-allowed`, crate-internal path, verify it) over crate-level `#![expect(…, reason = "…")]` in the UI crate.
