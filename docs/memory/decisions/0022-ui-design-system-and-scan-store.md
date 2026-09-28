---
status: accepted
date: 2026-09-28
tags: [gui, design, architecture]
---
# 0022 UI v2: app-owned components on gpui-base primitives, a 24/28/32 control scale, and one shared scan store

## Context and Problem Statement
The user reviewed the first cleaning UI: controls felt oversized (gpui-component's Medium = 32 px buttons/inputs with 14–16 px text, 44 px rows with three bordered badges), collapse chevrons did nothing, results found on the overview were missing on the area pages, and every area needed a manual scan. They asked for a principled redesign "at shadcn quality that still looks current in five years" rather than a blanket scale-down.

## Considered Options
* Scale everything down via the root rem (rejected by the user: shrinks text and targets alike, no hierarchy)
* Keep gpui-component's styled widgets and override sizes per call site
* **App-owned component library (`omc_ui::ui`) on gpui-base headless primitives, tokens-only styling (the shadcn model)** (chosen)

## Decision Outcome
- **Components**: `crates/omc-ui/src/ui/` (catalogue in `ui/mod.rs`) builds Button, IconButton, Checkbox (tri-state), Switch, Segmented, Badge/Dot/FactIcon, Card, PageHeader, SectionHeader, CollapsibleHeader, ListRow, Stat, ProgressBar, EmptyState, Kbd on `gpui_kit::base` primitives (they own focus, keyboard, a11y); gpui-component is used only for Input, Select, Dialog, Tooltip, Notification and the settings framework, sized to the same scale. Pages compose `ui` + `pages/widgets/parts.rs`; they never style controls ad hoc.
- **Scale** (`tokens.rs`, rems so the UI-font setting scales type and controls together): type 11/12/13/14/20/28; controls `sm` 24 / `md` 28 (default: 13 px text line 18 + 2×5 padding) / `lg` 32 (at most one per screen); inputs share button heights; radius 6 (controls) / 10 (cards); rows 32 single-line, 40 two-line; badges 18 px tinted without border, at most one per row (other facts as dots/icons).
- **Theme**: default preset pair "oh-my-clear Light/Dark" (OKLCH-authored neutral greys, hairline borders; dark background L 0.16, cards 0.19); other presets stay selectable.
- **Scan store** (`omc_ui::scans`): one flow/result per scan area shared by the overview and area pages; per-area freshness and auto-scan policy (cheap areas — Trash, browser, system junk, installers, leftovers, app list, startup — scan when opened if stale; developer junk, large/old, duplicates and space lens never auto-scan); preference `scan.auto_open` in `Settings.ui`; a new daemon epoch invalidates everything.
- Design spec text lives with the components (`ui/mod.rs`, `tokens.rs` docs); ADR 0011's motion/token rules still apply.

### Consequences
* Good, because sizes and hierarchy change in one place (tokens/components), controls align on one baseline, and interaction fixes (click propagation, whole-row toggles) live in components.
* Bad, because the app now maintains its own component styling instead of inheriting gpui-component updates, and a few gpui-component widgets (Input, settings pages) are held to the scale by overrides.

## Evidence
- User feedback with screenshots of the Browser Data page (2026-09-28); gpui-component-0.7.0 `sizing.rs` (`input_h` Medium = `h_8`, `input_text_size` = `text_sm`), `button/button.rs` sizes; gpui-base-0.7.0 `button.rs`/`checkbox.rs` ("unstyled … styles supplied by the application")
