# Glossary
<!-- Alphabetical. 1–2 lines per term. Use these exact terms in code, docs, and UI. -->
- **Daemon** — `oh-my-clear-daemon`, the headless execution layer (engine, filesystem/system work); outlives UI processes. Not: the UI process `oh-my-clear`.
- **Endpoint** — `endpoint.json` in the per-user runtime dir: daemon address, protocol, build id, pid, epoch, auth token.
- **Engine** — the daemon's core (`omc-engine`): serves authenticated connections and routes requests; UIs are viewports over it.
- **EngineHandle** — the UI's omc-ipc client: requests, connection state, reconnect.
- **Epoch** — id of one daemon process lifetime; a new epoch tells clients to drop cached state.
- **Runtime dir** — private per-user dir (`0700`, owner-checked) holding the daemon socket, `daemon.lock`, `endpoint.json`, `daemon.log`. Code: `omc_ipc::RuntimeDir`.
