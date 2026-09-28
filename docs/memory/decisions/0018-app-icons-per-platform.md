---
status: accepted
date: 2026-09-28
tags: [gui, build, brand]
---
# 0018 App icons per platform: macOS bundle, Windows resource, Linux desktop entry + X11 icon, all from `assets/brand/`

## Context and Problem Statement
The logo must show in the Dock/taskbar/launcher on macOS, Windows and Linux and in the titlebar. Each OS resolves app icons differently, the workspace forbids `unsafe` (ADR 0017), and GPUI rasterises an SVG `img` at its intrinsic size (1024 px for the brand art).

## Considered Options
* macOS: `NSApplication.setApplicationIconImage` at runtime (objc2-app-kit), or run from an `.app` bundle with `CFBundleIconFile`
* Windows: `embed-resource` or `winresource` build script for icon resource 1
* Linux: `WindowOptions::icon` (X11 only) plus a freedesktop desktop entry named after `app_id` (Wayland)
* Titlebar: SVG `img`, or a pre-rendered PNG

## Decision Outcome
- **Sources**: `assets/brand/*.svg` are the only hand-edited art. `assets/brand/render.sh` (resvg, python3, `iconutil`) renders every derived raster; outputs are committed. macOS keeps Apple's 824/1024 tile with its shadow margin; Windows and Linux use the tile cropped to 904/1024 because their shells add no margin.
- **macOS**: bundle, because `setApplicationIconImage:` is an `unsafe fn` in objc2-app-kit 0.3. `apps/oh-my-clear/resources/macos/` holds `Info.plist` (`@VERSION@` filled in) and `AppIcon.icns`; `cargo omc` assembles `target/debug/oh-my-clear.app` with hard links to both binaries (hard links keep the daemon's mtime, i.e. its build id).
- **Windows**: `apps/oh-my-clear/build.rs` compiles `resources/windows/oh-my-clear.rc` (`1 ICON`) with embed-resource, because gpui-pre loads icon resource 1 for its window class and already depends on embed-resource 3 (`windows-manifest`). Required on Windows hosts; only a cargo warning when cross-checking without `llvm-rc`.
- **Linux**: `omc_ui::window::window_options` sets the 128 px X11 `_NET_WM_ICON` (decoded with `image`, the crate gpui already uses); Wayland resolves `app_id` `dev.zerx.oh-my-clear` through `resources/linux/share/applications/dev.zerx.oh-my-clear.desktop` and the hicolor PNGs beside it, laid out for copying into an install prefix.
- **Titlebar**: `omc_ui::brand::mark()`, a 64 px PNG of `mark.svg` shown at `chrome::ICON`.

### Consequences
* Good, because no `unsafe`, no new crates beyond ones already in the gpui graph, and one script regenerates every size.
* Bad, because a bare `cargo run -p oh-my-clear` on macOS shows the generic executable icon, and Wayland shows the logo only once the desktop entry is installed.

## Evidence
- gpui-pre-windows 0.3.7 `platform.rs` `load_icon` (`LoadImageW(.., PCWSTR(1), IMAGE_ICON, ..)`); gpui-pre 0.3.7 `platform.rs` `WindowOptions::icon` "(X11 only)"; `elements/img.rs` SVG → `render_single_frame(bytes, 1.0)`; objc2-app-kit 0.3.2 `NSApplication.rs` `pub unsafe fn setApplicationIconImage` (verified 2026-09-28)
- `llvm-readobj --coff-resources` on the cross-built resource: `GROUP_ICON` name 1 with 8 PNG `ICON` entries (verified 2026-09-28)
