# Glossary
<!-- Alphabetical. 1–2 lines per term. Use these exact terms in code, docs, and UI. -->
- **Daemon** — `oh-my-clear-daemon`, the execution layer (engine, filesystem/system work) and owner of the tray; outlives UI processes. Not: the UI process `oh-my-clear`.
- **Endpoint** — `endpoint.json` in the per-user runtime dir: daemon address, protocol, build id, pid, epoch, auth token.
- **Engine** — the daemon's core (`omc-engine`): serves authenticated connections and routes requests; UIs are viewports over it.
- **EngineHandle** — the UI's omc-ipc client: requests, connection state, reconnect.
- **Epoch** — id of one daemon process lifetime; a new epoch tells clients to drop cached state.
- **Event** — an unsolicited daemon → UI frame (`omc_proto::Event`: `activate`, `quit`), pushed only to `ui` clients. Not: `ClientEvent`, the omc-ipc supervisor's channel item to the UI.
- **Helper bundle** — macOS `oh-my-clear.app/Contents/Helpers/oh-my-clear-daemon.app`, the daemon's own app bundle (id `dev.zerx.oh-my-clear.daemon`, `LSUIElement`). Code: `omc_ipc::layout`.
- **Host thread** — the daemon's main thread in `run`: tao event loop (macOS/Windows) or the daemon loop's `block_on` (Linux) that owns the tray. Code: `oh-my-clear-daemon/src/host.rs`.
- **Runtime dir** — private per-user dir (`0700`, owner-checked) holding the daemon socket, `daemon.lock`, `endpoint.json`, `daemon.log`, `ui.log` (a tray-launched UI's stderr). Code: `omc_ipc::RuntimeDir`.
- **Tray** — the daemon's system tray icon + menu (Open, Quit); keeps the daemon resident while no UI is attached. Code: `oh-my-clear-daemon/src/tray.rs`, ADR 0020.
