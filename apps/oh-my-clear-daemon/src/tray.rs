//! The system tray (ADR 0020): icon, menu (Open, Quit) and clicks. Built on the host
//! thread ([`crate::host`]); every user action is forwarded as a [`TrayEvent`] to the
//! daemon loop ([`crate::serve`]), which never blocks the thread that draws the tray.
//!
//! - macOS: a monochrome template image in the menu bar, tinted by `AppKit` for light/dark
//!   menu bars and the highlight; a click opens the menu (platform convention).
//! - Windows: the app icon resource at the notification area's size for the monitor's
//!   scale; left click opens the UI, right click the menu.
//! - Linux/BSD: a `StatusNotifierItem` over D-Bus (KDE, GNOME with the `AppIndicator`
//!   extension, …); activating the item opens the UI, the menu has both commands.

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
    /// Quit oh-my-clear: every UI, then the daemon.
    Quit,
}

const TRAY_ID: &str = "oh-my-clear";
const TOOLTIP: &str = "oh-my-clear";
const OPEN_ID: &str = "open";
const QUIT_ID: &str = "quit";

/// A shown tray icon; dropping it removes the icon. Not `Send`: it stays on the host
/// thread that built it.
pub(crate) struct Tray {
    _icon: TrayIcon,
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
        let open = MenuItem::with_id(OPEN_ID, t!("tray.open"), true, None);
        let quit = MenuItem::with_id(QUIT_ID, t!("tray.quit"), true, None);
        let menu = Menu::with_items(&[&open, &PredefinedMenuItem::separator(), &quit])?;
        let icon = TrayIconBuilder::new()
            .with_id(TRAY_ID)
            .with_tooltip(TOOLTIP)
            .with_menu(Box::new(menu))
            .with_icon(icon)
            .with_icon_as_template(cfg!(target_os = "macos"))
            .with_menu_on_left_click(cfg!(target_os = "macos"))
            .build()?;
        send(events, TrayEvent::Shown);
        Ok(Self { _icon: icon })
    }
}

/// Routes menu picks and icon clicks to `events`. The handlers run on the thread that
/// receives them from the OS (host thread, or ksni's D-Bus thread) and only enqueue.
fn forward_events(events: &mpsc::UnboundedSender<TrayEvent>) {
    let menu_events = events.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| match event.id.as_ref() {
        OPEN_ID => send(&menu_events, TrayEvent::Open),
        QUIT_ID => send(&menu_events, TrayEvent::Quit),
        other => tracing::debug!(id = other, "unknown tray menu item"),
    }));
    let click_events = events.clone();
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if opens_ui(&event) {
            send(&click_events, TrayEvent::Open);
        }
    }));
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
