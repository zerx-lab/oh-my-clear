---
status: accepted
date: 2026-09-28
tags: [architecture, runtime, gui, deps]
---
# 0020 System tray lives in the daemon (tray-icon + tao); the UI exits with its last window and the tray brings it back

## Context and Problem Statement
The user wants oh-my-clear to stay resident in the system tray on macOS, Windows and Linux with Tauri's tray crate, bound to the daemon rather than the UI: closing the UI ends the UI process entirely (no hidden gpui process holding memory), and the tray summons it again at any time. The daemon was headless (ADR 0008) and exited after 10 idle minutes. This amends ADR 0008's lifecycle and gives the daemon its only UI surface.

## Considered Options
* Tray in the GUI process, hiding windows instead of quitting
* Tray in the daemon: tray-icon with a tao event loop on every OS (GTK on Linux)
* Tray in the daemon: tray-icon; tao on macOS/Windows only; tray-icon's `ksni` backend on Linux
* Tray-icon's default Linux backend (`libappindicator`, GTK)

## Decision Outcome
Chosen: **tray in the daemon; tray-icon 0.25 (`default-features = false, features = ["ksni"]`), tao 0.37 (`default-features = false`) on macOS/Windows only**.
- **Process roles.** The GUI quits when its last window closes (unchanged) — the tray keeps no gpui state alive. The daemon owns the tray; with a tray shown it stays resident without clients (no idle exit). Without a tray host (Linux session without a StatusNotifierItem watcher, D-Bus errors) the daemon keeps the ADR 0008 idle exit.
- **Host thread** (`apps/oh-my-clear-daemon/src/host.rs`): the tray needs the main thread's event loop on macOS (AppKit) and Windows (message pump), so `run` puts tao there (`ControlFlow::Wait`, `run_return`) and the daemon loop on the tokio runtime. macOS: `ActivationPolicy::Accessory`, `set_activate_ignoring_other_apps(false)` (no Dock icon, never steals focus when the GUI spawns it). Linux/BSD: `ksni` runs its StatusNotifierItem on its own D-Bus thread; the main thread just `block_on`s the daemon loop, keeping the `!Send` `TrayIcon` there. The tray appears only after `READY` (the daemon owns the lock) and is dropped before exit.
- **Tray → daemon**: menu/click handlers only enqueue a `TrayEvent` (tokio unbounded channel). **Open**: `Event::Activate` to every attached UI (main window forward, reopened if only settings is left); with no UI attached, launch the GUI detached (`omc_ipc::spawn_detached`, stderr → `ui.log`), at most one launch per 20 s until it attaches. **Quit**: `Event::Quit` to every UI, wait ≤3 s for them to detach, then stop. Windows/Linux: left click = Open; macOS: click = menu.
- **Wire**: `ServerFrame::Event { ev }`, pushed only to `ui` clients (engine broadcast). `PROTOCOL` stays 1: a UI replaces any daemon of another build before it could receive an unknown event. On `Quit` the client supervisor ends instead of reconnecting, so a quitting UI never respawns the daemon.
- **Executables** (`omc_ipc::layout`): side by side everywhere except the macOS bundle, where the daemon is a helper app `oh-my-clear.app/Contents/Helpers/oh-my-clear-daemon.app` (id `dev.zerx.oh-my-clear.daemon`, `LSUIElement`): as a Cocoa app inside the main bundle Launch Services would treat it as a running instance of oh-my-clear.
- **Icons**: macOS template image from `mark-mono.svg` (36 px = 18 pt @2x); Windows loads icon resource 1 embedded in the daemon by `build.rs` at the notification area size for the monitor scale; Linux uses the 64 px app icon. All rendered by `assets/brand/render.sh`. Menu strings: rust-i18n in the daemon, following the OS language.
- **Layering**: `tray-icon` and `tao` are `CONFINED` to `oh-my-clear-daemon` (`xtask layers`); gpui stays out of the daemon.

### Consequences
* Good, because a closed UI costs no memory, the tray survives UI crashes, Linux needs no GTK or system libraries (pure-Rust D-Bus, zbus is already in the graph), and the host thread sleeps until the OS or the daemon wakes it.
* Good, because the daemon now runs an AppKit/Win32 main loop in a bundle of its own — the place later system integrations (notifications) attach to.
* Bad, because the daemon is no longer headless on macOS/Windows (it links AppKit / user32 through tao), and GNOME shows the icon only with the AppIndicator extension (otherwise: no tray, idle exit).
* Bad, because cargo-deny unions targets: tao's Linux-only GTK deps enter the graph (never built), so RUSTSEC-2024-0429 (glib 0.18) is ignored in `deny.toml` and `gtk` is banned outside tao.

## Evidence
- tray-icon 0.25.1 `src/lib.rs:17-27` (event-loop requirements; KSNI needs none), `platform_impl/ksni/mod.rs:30-61` (`spawn()` returns `Result`); libappindicator-sys 0.9.0 `src/lib.rs:13-55` (`panic!` when the `.so` is missing); tao 0.37.1 `platform/macos.rs:310-360`, `platform/run_return.rs`; objc2-app-kit 0.3.2 `NSApplication::run` (verified 2026-09-28)
- crates.io 2026-09-28: tray-icon 0.25.1 (2026-09-16), tao 0.37.1 (2026-09-26), muda 0.20.0, owned by tauri-apps; ksni 0.3.6 (2026-07-15, Unlicense); no RustSec advisories for any of them
- Crate delta for `oh-my-clear-daemon` (normal deps): macOS 57 → 193, Windows 57 → 174, Linux 57 → 204, all already in the workspace graph except the tray crates and tao; `cargo deny --all-features check` clean with the glib ignore (2026-09-28)
