---
status: accepted
date: 2026-09-28
tags: [gui, design]
---
# 0019 Main window: bare transparent titlebar over a full-height, spring-collapsed sidebar of cleaning areas

## Context and Problem Statement
The user asked to drop the titlebar's own content (product mark, appearance / language / settings toggles) and to give the main window a sidebar of disk-cleaning areas grouped like the best existing cleaners, collapsible with animation, with settings at its bottom. ADR 0013 had `AppTitleBar` carry a mark, a context title and global toggles. This amends ADR 0013's chrome only.

## Considered Options
* Native system titlebar (macOS/Windows caption, Linux SSD) above the sidebar
* Transparent titlebar kept only for window chrome; sidebar runs to the top edge, traffic lights over it (**chosen**, user pick)
* gpui-component `Sidebar` (collapse = 200 ms eased width transition) vs own clip + `gpui_kit::base::spring` (**own**: ADR 0011 requires retargetable springs)
* Collapse to an icon rail vs slide fully off (**slide off**: a 48 px rail cannot hold the ~70 px traffic lights)

## Decision Outcome
- **Titlebar**: `AppTitleBar` = gpui-component `TitleBar` with transparent background, optional divider, no mark/title/toggles. It still owns drag, double-click, macOS traffic-light inset, Windows caption buttons and Linux CSD controls. The main window lays it absolutely over sidebar + content; children start right after the traffic lights (the sidebar toggle). The settings window uses it in flow with a divider.
- **Sidebar** (`omc_ui::sidebar::Sidebar` entity): width springs between `layout::SIDEBAR_WIDTH` and 0 (`motion::PANEL` in, `PANEL_EXIT` = 0.7× out); the panel keeps full width pinned to the clip's right edge, so text never re-wraps; nothing renders once collapsed and settled. Rows reuse gpui-component `SidebarGroup`/`SidebarMenu`/`SidebarMenuItem`. `ToggleSidebar` (`secondary-b`, View menu) is handled by `MainView`.
- **Taxonomy** (`omc_ui::nav::NAV`): Overview · Cleanup (System Junk, Browser Data, Developer Junk, Trash/Recycle Bin) · Storage (Space Lens, Large & Old Files, Duplicates) · Applications (Uninstaller, Leftovers, Installers, Startup Items). Settings sits in the sidebar footer. Icons beyond gpui-kit's default bundle come from `omc_ui::assets::Assets`.
- Appearance and language stay reachable from the View menu and the settings window.

### Consequences
* Good, because the chrome is the platform's own behaviour with nothing custom to maintain, and the sidebar motion is interruptible without velocity jumps.
* Bad, because the sidebar clip width is a per-frame layout change (ADR 0011 prefers paint-only motion); it is limited to the one clip during the ~0.35 s spring.
* Bad, because `TitleBar`'s 80 px macOS inset is fixed in gpui-component; toggle placement depends on it.

## Evidence
- gpui-component 0.7.0 `src/title_bar.rs:16-19,319-420` (padding, drag, double-click, controls), `src/sidebar/mod.rs` (`SIDEBAR_TRANSITION_DURATION`, `EffectTransition`), `src/sidebar/{menu,group}.rs`; gpui-base 0.7.0 `src/motion.rs:573-650` (`spring`, reduced motion) — read 2026-09-28
- Taxonomy sources: CleanMyMac modules (https://9to5mac.com/2024/10/16/macpaw-releases-major-update-to-cleanmymac-with-fresh-design-and-new-features/), czkawka tools (https://czkawka.net/), Mole commands (https://github.com/tw93/mole) — 2026-09-28
- `main_view::tests::toggle_sidebar_shortcut_hides_and_shows_the_sidebar`, `nav::tests::*`, `assets::tests::every_sidebar_icon_is_bundled` (2026-09-28)
