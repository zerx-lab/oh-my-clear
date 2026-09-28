---
description: When agents must write to docs/memory (lessons, ADRs, deps ledger, session log)
alwaysApply: true
---

Memory write triggers (procedure + templates: read `skill://memory` before writing):
- Fixed a non-obvious bug → lesson in docs/memory/lessons.md (or bump `hits`/`verified` on the existing one).
- Architectural/cross-cutting decision → new ADR in docs/memory/decisions/ + one line in INDEX.md.
- Dependency added/removed/replaced → row in docs/memory/deps.md (+ ADR for a new category or replacement).
- User correction or durable preference → lesson; hard constraint → promote (lint > TTSR rule > skill > AGENTS.md).
- New domain term → docs/memory/glossary.md.
- End of a top-level task that changed the repo → update active-context.md, add one progress.md session-log line, bump the INDEX session counter.
- Any file over budget or ≥10 sessions since consolidation → consolidate per `skill://memory`.
Memory is heuristic: repo state and the user win; fix or delete stale entries in the same change.
Subagents: never edit docs/memory/**; end your result with a "Memory candidates" list (with evidence) for the main agent.
Never record what code, Cargo.toml, or git log already say; never record secrets.
