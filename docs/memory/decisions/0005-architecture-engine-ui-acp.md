---
status: superseded
superseded-by: 0008
date: 2026-09-28
tags: [architecture, orchestration]
---
# 0005 Engine/UI split, ACP-first adapters, git-CLI worktrees, journal-backed state

## Context and Problem Statement
dial orchestrates multiple coding agents (like Orca and Zeron). It needs a structure that keeps the gpui UI thin, isolates agents, survives crashes, and lets agents coordinate.

## Considered Options
* Orca style: every agent is a TUI in a PTY, status via injected hooks, orchestration via a CLI called from agent shells
* Zeron style: headless engine + typed RPC, agents over ACP (JSON-RPC stdio), MCP server injected for inter-agent messages
* Hybrid: Zeron structure + ACP first, PTY adapter as fallback, Orca's Run/Task/Dispatch/Message/Gate domain model

## Decision Outcome
Proposed: hybrid.

Planned workspace (`proto ← core ← {agent, process, term, git, store, mcp} ← engine ← ui ← apps/dial`):
`crates/dial-proto` (ids, Command, AgentEvent, EngineEvent; serde, no I/O) · `dial-core` (pure domain state machines/reducers) · `dial-agent` (adapter trait; `acp/`, `pty/`, `mock/`) · `dial-process` (spawn, kill-tree, stderr tail) · `dial-term` (alacritty_terminal) · `dial-git` (`git` CLI wrapper + parsers) · `dial-store` (SQLite journal) · `dial-mcp` (stdio MCP server for orchestration tools) · `dial-engine` (composition root, actors, event bus, recovery) · `dial-ui` (gpui-kit views; depends only on proto/core + `EngineHandle`) · `apps/dial` (binary).

Data flow: UI → `Command` → engine router → journal append → session actor → adapter ↔ agent child (in its worktree) → `AgentEvent` → journal + fold → `EngineEvent` broadcast → UI entities (`cx.notify`, coalesced ~100 ms). Permission requests round-trip as Commands. Startup replays the journal and marks in-flight runs aborted.

Open: ACP schema crate + own tokio driver (preferred) vs full SDK; rmcp vs hand-rolled MCP (resolved by ADR 0009).

### Consequences
* Good, because the engine is testable headless (mock adapter + nextest), UI state is a pure fold, and a daemon/remote mode becomes a transport swap.
* Bad, because more crates up front, and ACP adapters for Claude/Codex are Node packages that must be installed/pinned.

## Evidence
- docs/research/2026-09-28-orchestrators.md (Zeron ARCHITECTURE.md, Orca orchestration docs; verified 2026-09-28)
