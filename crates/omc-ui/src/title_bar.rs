//! The app's titlebar: gpui-component's `TitleBar` reduced to window chrome. It shows no
//! product mark, title or toggles; it only owns the per-platform window behaviour:
//! - macOS: room for the native traffic lights (positioned by [`crate::window::window_options`]),
//!   window drag, and double-click following the system "zoom/minimize" preference;
//! - Windows: drawn min/max/close buttons mapped to `WindowControlArea`, so snap layouts
//!   and caption hit-testing stay native;
//! - Linux: drawn controls only under client-side decorations, limited to what the
//!   compositor supports; right-click opens the window menu.
//!
//! The bar is transparent, so the content beneath it (the main window's sidebar) runs to
//! the top edge of the window. Children sit right after the traffic-light area.

use gpui_kit::component::{ActiveTheme as _, TitleBar};
use gpui_kit::{AnyElement, App, IntoElement, ParentElement, RenderOnce, Styled as _, Window};

use crate::theme;
use crate::tokens::chrome;

/// Window titlebar: drag region and platform window controls over transparent background.
#[derive(IntoElement)]
pub struct AppTitleBar {
    children: Vec<AnyElement>,
    divider: bool,
}

impl std::fmt::Debug for AppTitleBar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppTitleBar")
            .field("children", &self.children.len())
            .field("divider", &self.divider)
            .finish()
    }
}

impl AppTitleBar {
    /// A bare titlebar with no bottom divider.
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
            divider: false,
        }
    }

    /// Draws the chrome divider under the bar (hidden when the user turned dividers off).
    #[must_use]
    pub fn divider(mut self, divider: bool) -> Self {
        self.divider = divider;
        self
    }
}

impl Default for AppTitleBar {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for AppTitleBar {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for AppTitleBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let transparent = cx.theme().transparent;
        let border = if self.divider {
            theme::divider_color(cx)
        } else {
            transparent
        };
        TitleBar::new()
            .h(chrome::TITLE_BAR_HEIGHT)
            .bg(transparent)
            .border_color(border)
            .children(self.children)
    }
}
