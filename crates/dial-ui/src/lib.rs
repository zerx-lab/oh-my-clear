//! gpui-kit views (sidebar, transcript + composer, terminal, diff review, task board), design tokens and spring motion.
//!
//! Viewport only: talks to the daemon through the dial-ipc `EngineHandle`. Design/motion rules: `rule://ui-design-motion`. Allowed internal deps: `LAYERS` in xtask/src/layers.rs. Architecture: docs/memory/decisions/0011-ui-design-language-and-motion.md
//!
//! Foundation modules:
//! - [`theme`]: presets + appearance + style overrides → gpui-kit's global `Theme`;
//! - [`i18n`]: English / Simplified Chinese, following the OS by default;
//! - [`tokens`]: spacing, type scale and chrome geometry;
//! - [`title_bar`]: the cross-platform client-side titlebar;
//! - [`actions`]: every command as an `Action`, key bindings and native menus.

rust_i18n::i18n!("locales", fallback = "en");

pub mod actions;
mod error;
pub mod fonts;
pub mod i18n;
pub mod settings;
mod settings_view;
pub mod theme;
pub mod title_bar;
pub mod tokens;
pub mod window;
mod workspace;

use gpui_kit::App;

pub use error::{Error, Result};
pub use settings::UiSettings;
pub use title_bar::AppTitleBar;
pub use window::{open_main_window, window_options};
pub use workspace::Workspace;

/// Initialises gpui-kit and dial's UI layer: fonts, theme presets, preferences, locale,
/// actions, key bindings and menus. Call once, before opening any window.
pub fn init(cx: &mut App) -> Result<()> {
    gpui_kit::init(cx);
    fonts::register(cx)?;
    theme::init(cx)?;
    cx.set_global(UiSettings::default());
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
        assert_eq!(name, "One Dark", "default dark preset applies");
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
        assert_eq!(name, "One Light", "default light preset applies");
        assert!(
            radius.abs() < f32::EPSILON,
            "corners survive the mode switch"
        );
        assert_eq!(
            Some(light_primary),
            Accent::Pink.ramp(ThemeMode::Light).map(|r| r.base),
            "accent is re-derived for the new mode"
        );

        // gpui-component reloads the bare preset whenever the registry changes; dial must
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
}
