---
status: accepted
date: 2026-09-28
tags: [uninstall, apps, ui, deps]
---
# 0023 App attribution by evidence (incl. paths compiled into the app), friendly names for ids, treemap Space Lens

## Context and Problem Statement
The user found the uninstaller missing `~/.config/<app>` and home dotfiles, saw raw bundle ids (`com.soda.music.helper`) where beginners expect app names, and noted that Space Lens was only a sorted folder list. Name-only matching of XDG/dot folders is either too timid (misses) or too greedy (claims a CLI tool's `~/.claude` for the Claude desktop app).

## Considered Options
* Name matching only, with lower confidence for home folders
* Curated per-app rule tables (Pearcleaner / Homebrew zap style) only
* **Evidence first: paths compiled into the app's own binaries, open files of the running app, receipts/launchd references; names only as supporting or weak evidence; rival apps and foreign same-named CLI commands lower confidence** (chosen)

## Decision Outcome
- **Home config discovery** (`omc_apps::userconf`, shared by all OSes): children of `~/.config`, `~/.local/share|state`, `~/.cache` (XDG env honoured) and home dot entries (Windows `%USERPROFILE%\.x`, `.config`) are candidates; ~200 shared tool names (git, ssh, npm, cargo, shells, editors…) are never candidates. Evidence: the candidate's relative path (`.config/<n>`, `$XDG_CONFIG_HOME/<n>`, `.<n>rc`, Windows backslash and UTF-16 forms) found by `memchr::memmem` in the app's executables, bundled CLIs and `app.asar` (chunked with overlap, ≤256 MiB per app) or among the running app's open files → High; exact name of the app / its own CLI without a rival → Medium–High; a same-named command outside the app (not a shim into the bundle) caps at Low.
- **Friendly names** (`omc_apps::names`): the daemon resolves ids/labels/folder names to installed apps' display names and icons (exact id, parent/family ids, bundle paths, Electron product names via `platform::app_aliases`), falls back to a pure prettifier (vendor/noise components dropped) plus ~47 curated system-service labels; `JunkItem`/`StartupItem` carry the raw value in `ident` and the icon in `icon` (additive wire fields). Applied to System Junk, Leftovers and Startup items; ids and ordering never change.
- **Space Lens** renders a two-level squarified treemap (`ui::Treemap`, one canvas paint pass, OKLCH palette per level-1 child) linked to a ranked list; level-2 listings are prefetched with `space_children`.
- New dependency `memchr` (reused from the gpui graph) for the byte search.

### Consequences
* Good, because attribution is explainable (every item has a reason), catches XDG apps without guessing, and beginners see app names/icons.
* Bad, because reading app binaries costs up to a few hundred ms per uninstall preview, and the curated name table needs occasional upkeep.

## Evidence
- User screenshots 2026-09-28 (Space Lens list, raw ids in System Junk/Leftovers/Startup, uninstaller indentation); `strings /Applications/Ghostty.app/Contents/MacOS/ghostty` contains `ghostty/config`, `XDG_CONFIG_HOME`; `/opt/homebrew/bin/ghostex` is a bash shim exec'ing into `Ghostex.app`; memchr 2.8.3 (crates.io 2026-07-08, 369 M recent downloads)
