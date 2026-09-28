---
description: Docs record rules and decisions only — no progress, milestones, schedules, roadmaps or session logs
condition:
  - "(?i)\\b(?:milestones?|roadmap|session log|next steps?|sprint|deadline|ETA)\\b"
  - "(?im)^\\s*(?:[-*]\\s*)?\\[[ x]\\]\\s*M\\d"
  - "(?i)\\bM\\d(?:\\.\\d)?\\s*[:：—-]"
  - "\\b(?:MVP|phase\\s*\\d|P[0-3])\\s*[:：]"
  - "(?:里程碑|排期|进度|路线图|下一步|阶段计划)"
scope: "tool:edit(*.md), tool:write(*.md)"
---

User rule: oh-my-clear's docs (`docs/memory/**`, ADRs, `AGENTS.md`, `.omp/**`, crate docs, research write-ups) record **development rules, decisions and their evidence only**.

- Do not write progress/status of work, milestones, schedules, roadmaps, phase plans (MVP/P1/…), next-step lists, or session logs. `git log` is the history.
- Undecided questions go to `docs/memory/open-questions.md` as a question with options — not as a plan.
- Describe designs as rules and constraints ("the daemon owns PTYs"), not as when/in what order they will be built.
