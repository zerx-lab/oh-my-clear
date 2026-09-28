---
description: Do not add crates that duplicate an already-chosen category
condition: "(?m)^\\s*\"?(?:anyhow|eyre|color-eyre|snafu|failure|error-chain|async-std|smol|async-process|ureq|isahc|surf|lazy_static|once_cell|openssl|openssl-sys|native-tls|env_logger|simplelog|fern|log4rs|flume|crossbeam-channel|jiff|time|git2|portable-pty|interprocess|bincode|postcard|daemonize|agent-client-protocol|async-openai|genai|rig-core|eventsource-stream|libghostty-vt|libghostty-vt-sys|palette)\"?\\s*[.=]"
scope: "tool:edit(*Cargo.toml), tool:write(*Cargo.toml)"
---

This crate duplicates a category that already has a chosen crate (see `docs/memory/deps.md`):

| Category | Chosen |
|---|---|
| errors | `thiserror` only (no anyhow/eyre/snafu) |
| async runtime / processes | `tokio` (+ `tokio::process`) — smol/async-* only transitively via gpui |
| HTTP / TLS | `reqwest` (rustls) |
| logging | `tracing` + `tracing-subscriber` |
| lazy statics | `std::sync::LazyLock` / `OnceLock` |
| channels | `tokio::sync` in the runtime, `async-channel` (via gpui) for UI bridge |
| time | `chrono` (via gpui) |
| git | `git` CLI subprocess |
| IPC transport / wire | tokio UDS + named pipes; `u32` frames with serde_json control + raw binary data (ADR 0008) — no interprocess/bincode/postcard/daemonize |
| ACP | `agent-client-protocol-schema` + own tokio JSON-RPC driver (ADR 0009) — not the SDK |
| LLM providers / SSE | hand-rolled clients on reqwest in `dial-llm` (ADR 0009) |
| terminal emulation / PTY | libghostty-vt via `dial-ghostty`; PTY = `alacritty_terminal` `tty` in `dial-process` (ADR 0010) |
| color math | OKLCH→Hsla in `dial-ui` tokens (ADR 0011) |

If you believe a replacement is warranted, stop and run `skill://dep-review`; replacing a pick needs a superseding ADR.
