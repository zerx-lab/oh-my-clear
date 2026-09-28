---
description: When agents must write to docs/memory (lessons, ADRs, deps ledger, glossary, open questions)
alwaysApply: true
---

Memory records development rules and their reasons — never progress, status of work, milestones, schedules, roadmaps, next steps or session logs (user rule; `git log` is the history).

Memory write triggers (procedure + templates: read `skill://memory` before writing):
- Fixed a non-obvious bug → lesson in docs/memory/lessons.md (or bump `hits`/`verified` on the existing one).
- Architectural/cross-cutting decision → new ADR in docs/memory/decisions/ + one line in INDEX.md.
- Dependency added/removed/replaced → row in docs/memory/deps.md (+ ADR for a new category or replacement).
- User correction or durable preference → lesson; hard constraint → promote (lint > TTSR rule > skill > AGENTS.md).
- New domain term → docs/memory/glossary.md.
- Decision needed but not taken → docs/memory/open-questions.md; delete the line once an ADR/lesson answers it.
- Any file over budget → consolidate per `skill://memory`.
Memory is heuristic: repo state and the user win; fix or delete stale entries in the same change.
Subagents: never edit docs/memory/**; end your result with a "Memory candidates" list (with evidence) for the main agent.
Never record what code, Cargo.toml, or git log already say; never record secrets.
