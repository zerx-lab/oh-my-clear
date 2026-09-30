//! The system tray (ADR 0020): icon, menu (pending automation runs, Open, Quit) and clicks.
//! Built on the host thread ([`crate::host`]); every user action is forwarded as a
//! [`TrayEvent`] to the daemon loop ([`crate::serve`]), which never blocks the thread that
//! draws the tray. The menu is rebuilt whenever the pending runs change
//! ([`Tray::set_pending`]); picking one asks the daemon to prompt for it.
//!
//! - macOS: a monochrome template image in the menu bar, tinted by `AppKit` for light/dark
//!   menu bars and the highlight; a click opens the menu (platform convention).
//! - Windows: the app icon resource at the notification area's size for the monitor's
//!   scale; left click opens the UI, right click the menu.
//! - Linux/BSD: a `StatusNotifierItem` over D-Bus (KDE, GNOME with the `AppIndicator`
//!   extension, …); activating the item opens the UI, the menu has both commands.

use omc_engine::PendingRun;
use omc_proto::rules::RunId;
use rust_i18n::t;
use tokio::sync::mpsc;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::Result;

/// Tray lifecycle and user commands, for the daemon loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrayEvent {
    /// The icon is up: the daemon stays resident while no UI is attached.
    Shown,
    /// Show the UI.
    Open,
    /// Ask the user about this pending automation run.
    Prompt(RunId),
    /// Quit oh-my-clear: every UI, then the daemon.
    Quit,
}

const TRAY_ID: &str = "oh-my-clear";
const TOOLTIP: &str = "oh-my-clear";
const OPEN_ID: &str = "open";
const QUIT_ID: &str = "quit";
/// Menu id prefix of a pending run: `run:<id>`.
const RUN_ID_PREFIX: &str = "run:";
/// Pending runs listed in the menu; the rest is summarised.
const MAX_LISTED: usize = 5;

/// A shown tray icon; dropping it removes the icon. Not `Send`: it stays on the host
/// thread that built it.
pub(crate) struct Tray {
    icon: TrayIcon,
}

impl std::fmt::Debug for Tray {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tray").finish_non_exhaustive()
    }
}

impl Tray {
    /// Shows the tray with `icon`, sending user actions (and [`TrayEvent::Shown`]) to
    /// `events`. Call once per process: the menu and click handlers are process-global.
    pub(crate) fn show(events: &mpsc::UnboundedSender<TrayEvent>, icon: Icon) -> Result<Self> {
        forward_events(events);
        let menu = build_menu(&[])?;
        let icon = TrayIconBuilder::new()
            .with_id(TRAY_ID)
            .with_tooltip(TOOLTIP)
            .with_menu(Box::new(menu))
            .with_icon(icon)
            .with_icon_as_template(cfg!(target_os = "macos"))
            .with_menu_on_left_click(cfg!(target_os = "macos"))
            .build()?;
        send(events, TrayEvent::Shown);
        Ok(Self { icon })
    }

    /// Rebuilds the menu for the runs waiting for the user. Call on the host thread.
    pub(crate) fn set_pending(&self, pending: &[PendingRun]) {
        match build_menu(pending) {
            Ok(menu) => self.icon.set_menu(Some(Box::new(menu))),
            Err(err) => tracing::warn!(%err, "cannot rebuild the tray menu"),
        }
    }
}

/// The menu: a line per pending run (at most [`MAX_LISTED`], then a summary), then Open and
/// Quit.
fn build_menu(pending: &[PendingRun]) -> Result<Menu> {
    let menu = Menu::new();
    for run in pending.iter().take(MAX_LISTED) {
        let label = t!(
            "tray.pending",
            name = run.rule_name,
            size = human_size(run.bytes)
        );
        menu.append(&MenuItem::with_id(
            format!("{RUN_ID_PREFIX}{}", run.id),
            label,
            true,
            None,
        ))?;
    }
    if let Some(more) = pending
        .len()
        .checked_sub(MAX_LISTED)
        .filter(|more| *more > 0)
    {
        menu.append(&MenuItem::new(
            t!("tray.pending_more", count = more),
            false,
            None,
        ))?;
    }
    if !pending.is_empty() {
        menu.append(&PredefinedMenuItem::separator())?;
    }
    menu.append(&MenuItem::with_id(OPEN_ID, t!("tray.open"), true, None))?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&MenuItem::with_id(QUIT_ID, t!("tray.quit"), true, None))?;
    Ok(menu)
}

/// `1.5 GB`-style size for a menu line (binary units, one decimal from KB up).
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    // Tenths of the current unit, so no float is needed.
    let mut tenths = bytes.saturating_mul(10);
    let mut unit = 0_usize;
    while tenths >= 10_240 && unit < UNITS.len().saturating_sub(1) {
        tenths /= 1024;
        unit = unit.saturating_add(1);
    }
    let name = UNITS.get(unit).copied().unwrap_or("TB");
    if unit == 0 {
        format!("{} {name}", tenths / 10)
    } else {
        format!("{}.{} {name}", tenths / 10, tenths % 10)
    }
}

/// Routes menu picks and icon clicks to `events`. The handlers run on the thread that
/// receives them from the OS (host thread, or ksni's D-Bus thread) and only enqueue.
fn forward_events(events: &mpsc::UnboundedSender<TrayEvent>) {
    let menu_events = events.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if let Some(command) = menu_command(event.id.as_ref()) {
            send(&menu_events, command);
        } else {
            tracing::debug!(id = event.id.as_ref(), "unknown tray menu item");
        }
    }));
    let click_events = events.clone();
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if opens_ui(&event) {
            send(&click_events, TrayEvent::Open);
        }
    }));
}

/// The command a menu item id stands for.
fn menu_command(id: &str) -> Option<TrayEvent> {
    match id {
        OPEN_ID => Some(TrayEvent::Open),
        QUIT_ID => Some(TrayEvent::Quit),
        other => other
            .strip_prefix(RUN_ID_PREFIX)
            .and_then(|run| run.parse::<RunId>().ok())
            .map(TrayEvent::Prompt),
    }
}

/// A primary click on the icon opens the UI where the icon is not a menu button (macOS
/// shows the menu on every click instead).
fn opens_ui(event: &TrayIconEvent) -> bool {
    use tray_icon::{MouseButton, MouseButtonState};
    !cfg!(target_os = "macos")
        && matches!(
            event,
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }
        )
}

fn send(events: &mpsc::UnboundedSender<TrayEvent>, event: TrayEvent) {
    if events.send(event).is_err() {
        tracing::debug!(?event, "daemon loop is gone; tray event dropped");
    }
}

/// The menu bar template image (macOS) or the app icon (Linux/BSD), 18 pt / 64 px.
#[cfg(not(windows))]
pub(crate) fn icon() -> Result<Icon> {
    #[cfg(target_os = "macos")]
    const PNG: &[u8] = include_bytes!("../assets/tray-template.png");
    #[cfg(not(target_os = "macos"))]
    const PNG: &[u8] = include_bytes!("../assets/tray.png");
    let image = image::load_from_memory_with_format(PNG, image::ImageFormat::Png)?.into_rgba8();
    let (width, height) = image.dimensions();
    Ok(Icon::from_rgba(image.into_raw(), width, height)?)
}

/// The executable's icon resource 1 (`build.rs`) at the small-icon size for a monitor
/// scale of `scale_factor`, so Windows picks the matching `.ico` entry instead of scaling.
#[cfg(windows)]
pub(crate) fn icon(scale_factor: f64) -> Result<Icon> {
    let size = small_icon_size(scale_factor);
    Ok(Icon::from_resource(1, Some((size, size)))?)
}

/// `SM_CXSMICON` for a scale factor: 16 px at 100 %, one `.ico` entry per step.
#[cfg_attr(
    not(any(windows, test)),
    expect(dead_code, reason = "Windows-only; unit-tested on every host")
)]
fn small_icon_size(scale_factor: f64) -> u32 {
    const SIZES: [(f64, u32); 6] = [
        (1.0, 16),
        (1.25, 20),
        (1.5, 24),
        (2.0, 32),
        (2.5, 40),
        (3.0, 48),
    ];
    SIZES
        .iter()
        .find(|(max, _)| scale_factor <= max + 0.01)
        .map_or(64, |(_, size)| *size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_ids_map_to_commands() {
        assert_eq!(menu_command(OPEN_ID), Some(TrayEvent::Open), "open");
        assert_eq!(menu_command(QUIT_ID), Some(TrayEvent::Quit), "quit");
        assert_eq!(
            menu_command("run:42"),
            Some(TrayEvent::Prompt(42)),
            "a pending run"
        );
        assert_eq!(menu_command("run:x"), None, "a malformed run id");
        assert_eq!(menu_command("other"), None, "an unknown item");
    }

    #[test]
    fn sizes_read_like_a_file_manager() {
        for (bytes, expected) in [
            (0, "0 B"),
            (999, "999 B"),
            (1_023, "1023 B"),
            (1_024, "1.0 KB"),
            (1_536, "1.5 KB"),
            (5_000_000_000, "4.6 GB"),
            (u64::MAX, "1677721.5 TB"),
        ] {
            assert_eq!(human_size(bytes), expected, "{bytes} bytes");
        }
    }

    #[test]
    fn small_icon_follows_the_scale_factor() {
        for (scale, expected) in [
            (1.0, 16),
            (1.25, 20),
            (1.5, 24),
            (1.75, 32),
            (2.0, 32),
            (3.0, 48),
            (4.0, 64),
        ] {
            assert_eq!(small_icon_size(scale), expected, "scale {scale}");
        }
    }
}
