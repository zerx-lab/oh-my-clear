//! Window creation. Every dial window draws its own titlebar ([`crate::AppTitleBar`]):
//! transparent titlebar with inset traffic lights on macOS, caption-less window on
//! Windows, client-side decorations (shadow + resize edges from gpui-component's window
//! border) on Linux.

use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Global, Pixels, Size, TitlebarOptions,
    WindowBounds, WindowDecorations, WindowOptions, point,
};

use crate::settings_view::SettingsView;
use crate::tokens::chrome;
use crate::workspace::Workspace;
use crate::{Error, Result};

/// Application id (Wayland `app_id`, X11 `WM_CLASS`).
const APP_ID: &str = "dev.zerx.dial";

/// Options for a dial window of `size`, never smaller than `min_size`, centred on the
/// primary display.
pub fn window_options(size: Size<Pixels>, min_size: Size<Pixels>, cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size, cx))),
        window_min_size: Some(min_size),
        titlebar: Some(TitlebarOptions {
            title: Some("dial".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(
                chrome::TRAFFIC_LIGHT_INSET,
                chrome::TRAFFIC_LIGHT_INSET,
            )),
        }),
        // The titlebar moves the window itself; AppKit must not also treat it as a drag
        // region (it would delay clicks to disambiguate double clicks).
        app_owns_titlebar_drag: true,
        window_decorations: Some(WindowDecorations::Client),
        app_id: Some(APP_ID.to_owned()),
        ..WindowOptions::default()
    }
}

/// Opens the main window.
pub fn open_main_window(cx: &mut App) -> Result<AnyWindowHandle> {
    let options = window_options(chrome::MAIN_WINDOW, chrome::MAIN_WINDOW_MIN, cx);
    gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| Workspace::new(window, cx))
    })
    .map(|(handle, _)| handle)
    .map_err(|err| Error::Window {
        window: "main",
        message: format!("{err:#}"),
    })
}

/// The open settings window, so a second request focuses it instead of opening another.
struct SettingsWindow(AnyWindowHandle);

impl Global for SettingsWindow {}

/// Opens the settings window, or brings the existing one forward.
pub(crate) fn open_settings_window(cx: &mut App) {
    let existing = cx.try_global::<SettingsWindow>().map(|w| w.0);
    if let Some(handle) = existing
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    let options = window_options(chrome::SETTINGS_WINDOW, chrome::SETTINGS_WINDOW_MIN, cx);
    match gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| SettingsView::new(window, cx))
    }) {
        Ok((handle, _)) => cx.set_global(SettingsWindow(handle)),
        Err(err) => tracing::error!("failed to open the settings window: {err:#}"),
    }
}
