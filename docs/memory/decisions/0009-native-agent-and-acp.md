---
status: accepted
date: 2026-09-28
tags: [agents, acp, mcp, orchestration, llm]
---
# 0009 Native dial agent + ACP client behind one adapter trait; daemon-hosted MCP for orchestration

## Context and Problem Statement
The user wants dial's own coding agent, for the tightest control over orchestration and task execution. Any ACP-capable third-party agent must also plug in. Both kinds run inside `dial-daemon` (ADR 0008), and both must look the same to the engine, the journal and the UI.

## Considered Options
* ACP-only (ADR 0005): every agent is an external process
* A native agent served over ACP to ourselves (a child process speaking ACP)
* A native agent in-process behind the same `AgentAdapter`/`AgentSession` trait as ACP (Zed's design)

## Decision Outcome
Chosen: **one trait, two families** (plus `pty` and a `mock` adapter behind `test-support`).
- **Trait (`dial-agent`):**
  - `AgentAdapter::start(SessionSpec) -> Box<dyn AgentSession>`.
  - `AgentSession`: `prompt`, `cancel`, `respond_permission`, `set_config_option` (mode/model/effort; replaces `set_mode`, to match ACP v2), and an `events()` stream.
  - `AgentEvent` values are **upserts keyed by id** (message, tool call, plan).
- **Native agent (`dial-native`, in-process in the daemon):**
  - Turn loop with a per-turn `CancellationToken`.
  - Parallel tool calls: read-only tools share access, everything else runs exclusively.
  - Tools: read/write/edit/grep/glob/list/shell/todo/ask. `edit` is an exact match with bounded fuzzy tiers; `apply_patch` for GPT models.
  - In-process search with the ripgrep libraries.
  - Compaction: first prune old tool outputs, then summarize at 80 % of the context.
  - Shadow-git checkpoints.
  - Tool results are model-readable data, never exceptions.
- **Orchestration:**
  - `dial-core` defines `OrchestratorPort` (task_create, dispatch, message_send, gate_open/wait, task_list); `dial-engine` implements it.
  - The tool specs are data in `dial-proto`. The same specs are served to third-party agents by `dial-mcp`, so the two surfaces cannot drift.
  - A subagent is a Dispatch of a child Task, so a subagent may be native or any ACP agent.
- **Providers (`dial-llm`):**
  - Hand-rolled typed clients on reqwest for four wire formats: Anthropic Messages, OpenAI Responses, Gemini `generateContent`, and OpenAI-compatible Chat (DeepSeek/Qwen/Kimi/GLM/OpenRouter/Ollama/LM Studio).
  - Hand-rolled SSE framer.
  - Must round-trip opaque provider state (thinking `signature`, `encrypted_content`, `thoughtSignature`) and place cache breakpoints exactly.
  - API keys live in the OS keychain (`keyring`).
  - **No subscription OAuth in the native agent**: Anthropic forbids it in third-party products. Subscription users run Claude Code/Codex/Gemini through ACP.
- **Sandbox policy:**
  - Always: approvals, worktree scoping and a path policy in the file tools.
  - macOS: `sandbox-exec` profiles. Linux: `bwrap` when installed, otherwise a Landlock/seccomp self-re-exec helper using safe APIs.
  - Windows: restricted token, which requires a new unsafe island under ADR 0010's policy (ADR amendment first).
- **ACP client (`dial-agent/acp`):**
  - Protocol v1 now, with the data model shaped for v2.
  - Uses `agent-client-protocol-schema =1.9.1` (`default-features = false`) plus our own tokio NDJSON JSON-RPC driver, reusable later to serve `dial-native` as an ACP agent.
  - Rejected: the 2.2.0 SDK, which always pulls async-io/async-process/blocking, i.e. a second I/O stack.
  - Client capabilities: `fs.*`; `terminal` (backed by daemon PTYs + libghostty-vt); `auth.terminal`; opt-in `_meta.terminal_output(_delta)`, because claude-agent-acp streams shell output that way instead of calling `terminal/create`.
- **Agent installs:**
  - The daemon is the only installer.
  - `registry.json` from the ACP CDN, fetched at most hourly, with a bundled fallback.
  - Versioned install dirs, committed atomically, recorded in an `agents.lock`.
  - Launch as `node <bin>` or the native binary, **never npx**.
  - Node ≥ 22. uvx agents are listed only when `uv` exists. An update takes effect at the next session start.
- **MCP injection:**
  - When the agent advertises `mcpCapabilities.http`, the daemon serves a loopback streamable-HTTP MCP endpoint with a per-session bearer token and Host/Origin checks (RUSTSEC-2026-0189).
  - Otherwise, a stdio proxy: `dial-daemon mcp`, over the daemon socket.
  - `rmcp` provides the single tool handler behind both.
- **Auth:** `agent`-type auth calls `authenticate`. `terminal`-type auth relaunches the agent in a daemon PTY rendered by libghostty-vt. While no UI is attached, the session parks in `needs_auth`.

### Consequences
* Good:
  - Full control of prompts, tools, context and orchestration.
  - Cross-agent subagents, which none of the references can do.
  - One event model for journal replay and UI.
  - One async runtime.
* Bad:
  - Harness quality is mostly per-model prompt work and needs an eval loop.
  - Hand-rolled provider clients need recorded fixtures and fast releases when vendors drift.
  - Unversioned ACP `_meta` extensions.
  - Node 22 is required for about half of the registry.

## Evidence
- docs/research/2026-09-28-native-agent.md (codex-rs 44fe510, zed d3ccd57 [GPL: study only], goose 98c626d, opencode 03e6717; https://code.claude.com/docs/en/legal-and-compliance)
- docs/research/2026-09-28-acp.md (schema v1 meta.json, v2 migration guide, live `initialize` probes of claude-agent-acp 0.81.2 / codex-acp 1.13.1 / gemini / qwen / copilot / omp, registry of 41 agents; verified 2026-09-28)
