//! gpui-kit views of oh-my-clear, design tokens and spring motion.
//!
//! Viewport only: talks to the daemon through the omc-ipc `EngineHandle`. Design/motion rules: `rule://ui-design-motion`. Allowed internal deps: `LAYERS` in xtask/src/layers.rs. Architecture: docs/memory/decisions/0011-ui-design-language-and-motion.md
//!
//! Foundation modules:
//! - [`theme`]: presets + appearance + style overrides → gpui-kit's global `Theme`;
//! - [`i18n`]: English / Simplified Chinese, following the OS by default;
//! - [`tokens`]: spacing, type scale and chrome geometry; [`motion`]: spring presets;
//! - [`ui`]: the app's component library (buttons, inputs, lists, cards…) — pages compose
//!   these instead of styling controls themselves;
//! - [`title_bar`]: the cross-platform titlebar (window chrome only);
//! - [`actions`]: every command as an `Action`, key bindings and native menus;
//! - [`assets`]: gpui-kit's icons plus the app's own;
//! - [`nav`]: the cleaning areas shown in the main window's sidebar;
//! - [`engine`]: the omc-ipc client bridge (tokio runtime + `EngineHandle` in a Global);
//! - [`rules`] / [`prompt`] / [`palette`]: automation (ADR 0024) — the shared rules store,
//!   the prompt window of a run that needs a decision, and the ⌘K command palette.

rust_i18n::i18n!("locales", fallback = "en");

pub mod actions;
pub mod assets;
mod brand;
pub mod clean_settings;
pub mod engine;
mod error;
pub mod fonts;
pub mod format;
pub mod i18n;
pub mod jobs;
mod main_view;
pub mod motion;
pub mod nav;
mod pages;
mod palette;
pub mod prompt;
mod rules;
pub(crate) mod scans;
pub mod settings;
mod settings_view;
mod sidebar;
pub mod theme;
pub mod title_bar;
pub mod tokens;
pub mod ui;
pub mod window;

use gpui_kit::App;

pub use error::{Error, Result};
pub use main_view::MainView;
pub use settings::UiSettings;
pub use title_bar::AppTitleBar;
pub use window::{open_main_window, window_options};

/// Initialises gpui-kit and the app's UI layer: fonts, theme presets, preferences, locale,
/// actions, key bindings and menus. Call once, before opening any window.
pub fn init(cx: &mut App) -> Result<()> {
    gpui_kit::init(cx);
    fonts::register(cx)?;
    theme::init(cx)?;
    cx.set_global(UiSettings::default());
    clean_settings::init(cx);
    i18n::apply(UiSettings::get(cx).language);
    theme::apply(cx.window_appearance(), cx);
    actions::init(cx)?;
    actions::set_menus(cx);
    Ok(())
}

#[cfg(test)]
mod tests {
    use gpui_kit::TestAppContext;
    use gpui_kit::component::{Theme, ThemeMode, ThemeRegistry};

    use crate::UiSettings;
    use crate::theme::{Accent, Appearance, CornerStyle};

    fn snapshot(cx: &mut TestAppContext) -> (ThemeMode, f32, gpui_kit::Hsla, String) {
        cx.update(|cx| {
            let theme = Theme::global(cx);
            (
                theme.mode,
                f32::from(theme.radius),
                theme.primary,
                theme.theme_name().to_string(),
            )
        })
    }

    #[gpui_kit::test]
    fn style_overrides_survive_mode_switches_and_registry_reloads(cx: &mut TestAppContext) {
        let init = cx.update(super::init);
        assert!(init.is_ok(), "init must succeed headless: {init:?}");

        cx.update(|cx| {
            UiSettings::update(cx, |s| {
                s.theme.appearance = Appearance::Dark;
                s.theme.style.corners = CornerStyle::Square;
                s.theme.style.accent = Accent::Pink;
            });
        });
        let (mode, radius, dark_primary, name) = snapshot(cx);
        assert_eq!(mode, ThemeMode::Dark, "explicit dark applies");
        assert_eq!(
            name,
            crate::theme::presets::DEFAULT_DARK,
            "default dark preset applies"
        );
        assert!(
            radius.abs() < f32::EPSILON,
            "square corners apply: {radius}"
        );
        assert_eq!(
            Some(dark_primary),
            Accent::Pink.ramp(ThemeMode::Dark).map(|r| r.base),
            "accent overrides the preset primary"
        );

        cx.update(|cx| UiSettings::update(cx, |s| s.theme.appearance = Appearance::Light));
        let (mode, radius, light_primary, name) = snapshot(cx);
        assert_eq!(
            mode,
            ThemeMode::Light,
            "switching mode loads the light preset"
        );
        assert_eq!(
            name,
            crate::theme::presets::DEFAULT_LIGHT,
            "default light preset applies"
        );
        assert!(
            radius.abs() < f32::EPSILON,
            "corners survive the mode switch"
        );
        assert_eq!(
            Some(light_primary),
            Accent::Pink.ramp(ThemeMode::Light).map(|r| r.base),
            "accent is re-derived for the new mode"
        );

        // gpui-component reloads the bare preset whenever the registry changes; the app must
        // put its style back on top.
        cx.update(|cx| {
            ThemeRegistry::global_mut(cx);
        });
        cx.run_until_parked();
        let (_, radius, primary, _) = snapshot(cx);
        assert!(
            radius.abs() < f32::EPSILON,
            "corners survive a registry reload"
        );
        assert_eq!(primary, light_primary, "accent survives a registry reload");
    }

    #[gpui_kit::test]
    fn settings_window_is_a_singleton(cx: &mut TestAppContext) {
        let opened = cx.update(|cx| super::init(cx).and_then(|()| super::open_main_window(cx)));
        assert!(opened.is_ok(), "main window opens and renders: {opened:?}");
        cx.run_until_parked();

        for _ in 0..2 {
            cx.update(|cx| cx.dispatch_action(&crate::actions::OpenSettings));
            cx.run_until_parked();
        }
        let windows = cx.update(|cx| cx.windows().len());
        assert_eq!(
            windows, 2,
            "a second OpenSettings focuses the open settings window"
        );
    }

    #[gpui_kit::test]
    fn close_window_shortcut_closes_the_settings_window(cx: &mut TestAppContext) {
        let opened = cx.update(|cx| super::init(cx).and_then(|()| super::open_main_window(cx)));
        assert!(opened.is_ok(), "main window opens and renders: {opened:?}");
        cx.run_until_parked();
        cx.update(|cx| cx.dispatch_action(&crate::actions::OpenSettings));
        cx.run_until_parked();
        let main = opened.ok();
        let settings = cx
            .update(|cx| cx.windows())
            .into_iter()
            .find(|w| Some(*w) != main);
        assert!(settings.is_some(), "the settings window is open");

        if let Some(settings) = settings {
            // The test platform has no key window of its own; a real one makes the newly
            // opened settings window key.
            let activated = settings.update(cx, |_, window, _| window.activate_window());
            assert!(
                activated.is_ok(),
                "settings window activates: {activated:?}"
            );
            cx.simulate_keystrokes(settings, "secondary-w");
        }
        let windows = cx.update(|cx| cx.windows());
        assert_eq!(
            (windows.len(), windows.first().copied()),
            (1, main),
            "secondary-w closes the settings window and keeps the main window"
        );
    }
}
