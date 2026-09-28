---
status: accepted
date: 2026-09-28
tags: [memory, process]
---
# 0001 Record decisions as ADRs; file-based self-iterating memory in docs/memory

## Context and Problem Statement
oh-my-clear is built mostly by omp agents across many sessions and parallel subagents. Knowledge (decisions, gotchas, dependency evidence, progress) must survive sessions, be shared through git, cost little always-loaded context, and improve itself without installing software.

## Considered Options
* omp built-in memory (`memory.backend: local`, `learn`, hindsight/mnemopi)
* Cline/Roo "memory bank" (read all files every task)
* Tiered Markdown memory: small auto-loaded index + on-demand topic files + ADRs + promotion into rules

## Decision Outcome
Chosen option: tiered Markdown memory in `docs/memory/`, because omp's backends are machine-local, off by default, unavailable to subagents and not in git, while "read everything" wastes context. Design follows Claude Code auto-memory (index + topic files + budgets), Letta core/archival tiers, MADR for decisions, and adds a single-writer rule for multi-agent work.

- Core: `INDEX.md` imported into `AGENTS.md` (≤80 lines). Everything else on demand; git history is the archive.
- Triggers: `.omp/rules/memory-protocol.md` (always-apply). Procedure/templates/consolidation: `.omp/skills/memory/SKILL.md`.
- Self-iteration: lessons with hits ≥2 are promoted to the strongest enforcement available (lint/deny > TTSR > skill > AGENTS.md line); budgets force consolidation.

### Consequences
* Good, because memory is reviewable in PRs, shared by every tool that reads AGENTS.md, and enforced knowledge migrates into lints/TTSR.
* Bad, because it depends on agents honoring the protocol; consolidation is manual (triggered by counters/budgets).

## Evidence
- docs/research/2026-09-28-memory-system.md, docs/research/2026-09-28-omp-config.md (verified 2026-09-28)
