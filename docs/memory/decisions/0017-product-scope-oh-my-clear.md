---
status: accepted
date: 2026-09-28
tags: [product, architecture, memory]
---
# 0017 Product scope: oh-my-clear, a cross-platform system cleaner; agent-orchestration scope removed

## Context and Problem Statement
The user renamed the project from `dial` to **oh-my-clear** and changed its goal: it is no longer a multi-agent coding orchestration app but a system-cleaning desktop app for macOS, Windows and Linux. Code, memory and prompts written for the old goal would steer work toward the wrong product.

## Considered Options
* Rename only; keep the orchestration code, ADRs and rules for later reuse
* Rename and remove everything that serves only agent orchestration; keep development norms and the reusable foundation (**chosen**)

## Decision Outcome
- Names: crates `omc-<area>`, GUI binary/package `oh-my-clear`, daemon `oh-my-clear-daemon`, app id and runtime dir `dev.zerx.oh-my-clear`, cargo alias `cargo omc`.
- Kept: the development norms (ADRs 0001–0004, 0006, 0007, 0012), the GUI + daemon split and IPC (0008, rewritten for the cleaner by user direction), the UI design, motion and foundation (0011, 0013), `omc-telemetry`, and the `xtask` gates.
- Removed: agent/LLM/MCP/ACP/terminal/PTY/git/journal crates, the Ghostty submodule and its Zig build, the agent-IDE main-window layout, the daemon file explorer (fs watch/list), and ADRs 0005, 0009, 0010, 0014, 0015, 0016 with their research snapshots, lessons, glossary terms, dependency rows and rules. Kept ADRs were scrubbed in place instead of superseded: the user asked that no memory keep the old goal.
- No `unsafe` crate exists any more, so `unsafe_code = "forbid"` holds for every member; adding `unsafe` needs a new ADR.

### Consequences
* Good, because memory, rules and code describe one product; nothing steers toward the old goal.
* Bad, because the removed designs live only in git history (`git log -p docs/memory/`).

## Evidence
- User instruction 2026-09-28: rename to oh-my-clear, cross-platform system cleaner, remove all memory/prompts that are not development or language norms; keep the two-process architecture; `omc-*` crate prefix.
