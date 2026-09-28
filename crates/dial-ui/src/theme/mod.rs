//! Theme system: one source of colour and shape for every component, gpui-kit's and dial's.
//!
//! Layers, applied in this order by [`apply`] (any change re-runs all of them):
//! 1. **Preset** — a gpui-component `ThemeConfig` from [`presets`] (colours, syntax
//!    highlight). The user picks one light and one dark preset.
//! 2. **Mode** — [`Appearance`] chooses light, dark, or follows the OS.
//! 3. **Style** — [`ThemeStyle`] overrides every knob gpui-kit exposes on `Theme`
//!    (radii, shadows, focus ring, fonts, scrollbar) plus dial's own chrome dividers and an
//!    optional OKLCH [`Accent`] that replaces the preset's accent family.
//!
//! Views read colours through `cx.theme()` and dial-only switches through
//! [`UiSettings::get`]; nothing caches a colour across renders.

mod color;
pub mod presets;

use gpui_kit::base::ScrollbarMode;
use gpui_kit::component::{ActiveTheme as _, Theme, ThemeConfig, ThemeMode, ThemeRegistry};
use gpui_kit::{App, Hsla, SharedString, WindowAppearance, px};
use std::rc::Rc;

use crate::settings::UiSettings;
use crate::tokens::chrome;
use crate::{Error, Result, fonts};

/// Light/dark selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Appearance {
    /// Follow the OS appearance, live.
    #[default]
    System,
    /// Always the light preset.
    Light,
    /// Always the dark preset.
    Dark,
}

impl Appearance {
    /// Every choice, in menu order.
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    /// Stable identifier (settings keys, dropdown values).
    pub const fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    /// Inverse of [`Self::key`].
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.key() == key)
    }

    /// The theme mode this choice resolves to under the given OS appearance.
    pub fn mode(self, system: WindowAppearance) -> ThemeMode {
        match self {
            Self::System => ThemeMode::from(system),
            Self::Light => ThemeMode::Light,
            Self::Dark => ThemeMode::Dark,
        }
    }
}

/// Corner treatment for every component. Radii follow the 4/6/8/12 scale (ADR 0011);
/// `Square` removes rounding everywhere, including pills and avatars.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CornerStyle {
    /// 0 px everywhere.
    Square,
    /// 4 px controls, 6 px surfaces.
    Compact,
    /// 6 px controls, 8 px surfaces.
    #[default]
    Standard,
    /// 8 px controls, 12 px surfaces.
    Round,
}

impl CornerStyle {
    /// Every choice, in menu order.
    pub const ALL: [Self; 4] = [Self::Square, Self::Compact, Self::Standard, Self::Round];

    /// Stable identifier.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Square => "square",
            Self::Compact => "compact",
            Self::Standard => "standard",
            Self::Round => "round",
        }
    }

    /// Inverse of [`Self::key`].
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.key() == key)
    }

    /// `(control radius, surface radius)` in px.
    pub const fn radii(self) -> (f32, f32) {
        match self {
            Self::Square => (0., 0.),
            Self::Compact => (4., 6.),
            Self::Standard => (6., 8.),
            Self::Round => (8., 12.),
        }
    }
}

/// Accent family. `Theme` keeps the preset's own accent; the others replace it with an
/// OKLCH-authored ramp tuned per mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Accent {
    /// The preset's accent.
    #[default]
    Theme,
    /// Blue.
    Blue,
    /// Violet.
    Violet,
    /// Green.
    Green,
    /// Orange.
    Orange,
    /// Pink.
    Pink,
}

/// Resolved accent ramp for one mode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AccentRamp {
    pub(crate) base: Hsla,
    pub(crate) hover: Hsla,
    pub(crate) active: Hsla,
    pub(crate) foreground: Hsla,
}

impl Accent {
    /// Every choice, in menu order.
    pub const ALL: [Self; 6] = [
        Self::Theme,
        Self::Blue,
        Self::Violet,
        Self::Green,
        Self::Orange,
        Self::Pink,
    ];

    /// Stable identifier.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Theme => "theme",
            Self::Blue => "blue",
            Self::Violet => "violet",
            Self::Green => "green",
            Self::Orange => "orange",
            Self::Pink => "pink",
        }
    }

    /// Inverse of [`Self::key`].
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.key() == key)
    }

    /// `(hue°, chroma)`; chroma is trimmed per hue to stay near the sRGB gamut.
    const fn hue_chroma(self) -> Option<(f32, f32)> {
        match self {
            Self::Theme => None,
            Self::Blue => Some((256., 0.19)),
            Self::Violet => Some((293., 0.2)),
            Self::Green => Some((150., 0.15)),
            Self::Orange => Some((48., 0.17)),
            Self::Pink => Some((352., 0.19)),
        }
    }

    /// The ramp for `mode`; `None` keeps the preset's colours. Light mode uses a dark
    /// accent under white text, dark mode a light accent under near-black text, both
    /// ≥ 4.5:1 (checked by tests).
    pub(crate) fn ramp(self, mode: ThemeMode) -> Option<AccentRamp> {
        let (hue, chroma) = self.hue_chroma()?;
        Some(if mode.is_dark() {
            AccentRamp {
                base: color::oklch(0.74, chroma * 0.8, hue),
                hover: color::oklch(0.79, chroma * 0.75, hue),
                active: color::oklch(0.69, chroma * 0.8, hue),
                foreground: color::oklch(0.18, 0.02, hue),
            }
        } else {
            AccentRamp {
                base: color::oklch(0.5, chroma, hue),
                hover: color::oklch(0.45, chroma, hue),
                active: color::oklch(0.41, chroma, hue),
                foreground: color::oklch(1., 0., 0.),
            }
        })
    }
}

/// UI typeface choice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UiFont {
    /// Bundled Inter.
    #[default]
    Inter,
    /// The platform UI font.
    System,
}

impl UiFont {
    /// Every choice, in menu order.
    pub const ALL: [Self; 2] = [Self::Inter, Self::System];

    /// The font family name, which doubles as the stable identifier.
    pub const fn family(self) -> &'static str {
        match self {
            Self::Inter => fonts::UI_FONT_FAMILY,
            Self::System => fonts::SYSTEM_UI_FONT_FAMILY,
        }
    }

    /// Inverse of [`Self::family`].
    pub fn from_family(family: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.family() == family)
    }
}

/// Code typeface choice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MonoFont {
    /// Bundled JetBrains Mono.
    #[default]
    JetBrainsMono,
    /// The platform's default monospace font.
    System,
}

impl MonoFont {
    /// Every choice, in menu order.
    pub const ALL: [Self; 2] = [Self::JetBrainsMono, Self::System];

    /// The font family name, which doubles as the stable identifier.
    pub const fn family(self) -> &'static str {
        match self {
            Self::JetBrainsMono => fonts::MONO_FONT_FAMILY,
            Self::System => fonts::SYSTEM_MONO_FONT_FAMILY,
        }
    }

    /// Inverse of [`Self::family`].
    pub fn from_family(family: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.family() == family)
    }
}

/// Scrollbar visibility.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScrollbarVisibility {
    /// Follow the OS "show scroll bars" preference.
    #[default]
    System,
    /// Show while scrolling, then fade.
    Scrolling,
    /// Show on hover.
    Hover,
    /// Always show.
    Always,
}

impl ScrollbarVisibility {
    /// Every choice, in menu order.
    pub const ALL: [Self; 4] = [Self::System, Self::Scrolling, Self::Hover, Self::Always];

    /// Stable identifier.
    pub const fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Scrolling => "scrolling",
            Self::Hover => "hover",
            Self::Always => "always",
        }
    }

    /// Inverse of [`Self::key`].
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.key() == key)
    }

    fn mode(self, os_auto_hides: bool) -> ScrollbarMode {
        match self {
            Self::System if os_auto_hides => ScrollbarMode::Scrolling,
            Self::Scrolling => ScrollbarMode::Scrolling,
            Self::System | Self::Hover => ScrollbarMode::Hover,
            Self::Always => ScrollbarMode::Always,
        }
    }
}

/// UI font sizes offered (root rem, px).
pub const UI_FONT_SIZES: [u8; 5] = [14, 15, 16, 17, 18];
/// Code font sizes offered (px).
pub const MONO_FONT_SIZES: [u8; 8] = [11, 12, 13, 14, 15, 16, 17, 18];

/// Everything a user can tune on top of a preset.
#[derive(Clone, Debug, PartialEq)]
pub struct ThemeStyle {
    /// Corner radii.
    pub corners: CornerStyle,
    /// Accent family.
    pub accent: Accent,
    /// Drop shadows on controls and overlays.
    pub shadows: bool,
    /// Focus ring outside focused controls (the tinted border remains when off).
    pub focus_ring: bool,
    /// Hairline dividers between chrome regions (titlebar, panes).
    pub dividers: bool,
    /// UI typeface.
    pub ui_font: UiFont,
    /// Root UI font size in px; every rem-based size scales with it.
    pub ui_font_size: u8,
    /// Code typeface.
    pub mono_font: MonoFont,
    /// Code font size in px.
    pub mono_font_size: u8,
    /// Scrollbar visibility.
    pub scrollbar: ScrollbarVisibility,
}

impl Default for ThemeStyle {
    fn default() -> Self {
        Self {
            corners: CornerStyle::default(),
            accent: Accent::default(),
            shadows: true,
            focus_ring: true,
            dividers: true,
            ui_font: UiFont::default(),
            ui_font_size: 16,
            mono_font: MonoFont::default(),
            mono_font_size: 13,
            scrollbar: ScrollbarVisibility::default(),
        }
    }
}

/// Theme choice: mode, one preset per mode, and style overrides.
#[derive(Clone, Debug, PartialEq)]
pub struct ThemePreferences {
    /// Light/dark selection.
    pub appearance: Appearance,
    /// Preset name used in light mode.
    pub light_theme: SharedString,
    /// Preset name used in dark mode.
    pub dark_theme: SharedString,
    /// Style overrides.
    pub style: ThemeStyle,
}

impl Default for ThemePreferences {
    fn default() -> Self {
        Self {
            appearance: Appearance::default(),
            light_theme: presets::DEFAULT_LIGHT.into(),
            dark_theme: presets::DEFAULT_DARK.into(),
            style: ThemeStyle::default(),
        }
    }
}

/// Registers the bundled presets and keeps the style layer applied when the registry
/// reloads. Call after `gpui_kit::init`: gpui-component's own registry observer reloads the
/// bare preset, and observers run in registration order, so ours re-applies the style
/// right after it.
pub(crate) fn init(cx: &mut App) -> Result<()> {
    if cx.try_global::<ThemeRegistry>().is_none() {
        return Err(Error::ThemePreset {
            file: "<registry>",
            message: "gpui_kit::init must run before dial_ui::init".to_owned(),
        });
    }
    let registry = ThemeRegistry::global_mut(cx);
    for (file, json) in presets::FILES {
        registry
            .load_themes_from_str(json)
            .map_err(|err| Error::ThemePreset {
                file,
                message: format!("{err:#}"),
            })?;
    }
    cx.observe_global::<ThemeRegistry>(|cx| apply(cx.window_appearance(), cx))
        .detach();
    Ok(())
}

/// Re-applies the whole theme after the OS appearance changed; a no-op unless the user
/// follows the system.
pub(crate) fn system_appearance_changed(system: WindowAppearance, cx: &mut App) {
    if UiSettings::get(cx).theme.appearance == Appearance::System {
        apply(system, cx);
    }
}

/// Applies preset, mode and style from [`UiSettings`] to the global `Theme` and refreshes
/// every window.
pub(crate) fn apply(system: WindowAppearance, cx: &mut App) {
    let prefs = UiSettings::get(cx).theme.clone();
    let mode = prefs.appearance.mode(system);
    let light = preset(&prefs.light_theme, ThemeMode::Light, cx);
    let dark = preset(&prefs.dark_theme, ThemeMode::Dark, cx);
    {
        // Direct write: `Theme::change` below reloads the mode's preset and refreshes.
        let theme = Theme::global_mut(cx);
        if let Some(light) = light {
            theme.light_theme = light;
        }
        if let Some(dark) = dark {
            theme.dark_theme = dark;
        }
    }
    Theme::change(mode, None, cx);
    let os_auto_hides = cx.should_auto_hide_scrollbars();
    Theme::update(cx, |theme| apply_style(theme, &prefs.style, os_auto_hides));
}

/// The registered preset called `name` if it has the right mode; otherwise gpui-kit's
/// default for that mode (logged, since the name came from settings).
fn preset(name: &SharedString, mode: ThemeMode, cx: &App) -> Option<Rc<ThemeConfig>> {
    let registry = cx.try_global::<ThemeRegistry>()?;
    match registry.themes().get(name) {
        Some(config) if config.mode == mode => Some(config.clone()),
        _ => {
            tracing::warn!(theme = %name, mode = mode.name(), "unknown theme preset, using the default");
            registry.default_themes().get(&mode).cloned()
        }
    }
}

/// Registered preset names for `mode`, sorted case-insensitively.
pub fn preset_names(mode: ThemeMode, cx: &App) -> Vec<SharedString> {
    let Some(registry) = cx.try_global::<ThemeRegistry>() else {
        return Vec::new();
    };
    let mut names: Vec<SharedString> = registry
        .themes()
        .values()
        .filter(|config| config.mode == mode)
        .map(|config| config.name.clone())
        .collect();
    names.sort_by_key(|name| name.to_lowercase());
    names
}

fn apply_style(theme: &mut Theme, style: &ThemeStyle, os_auto_hides_scrollbars: bool) {
    let (radius, radius_lg) = style.corners.radii();
    theme.radius = px(radius);
    theme.radius_lg = px(radius_lg);
    theme.shadow = style.shadows;
    theme.focus_ring = style.focus_ring;
    theme.font_family = style.ui_font.family().into();
    theme.font_size = px(f32::from(style.ui_font_size));
    theme.mono_font_family = style.mono_font.family().into();
    theme.mono_font_size = px(f32::from(style.mono_font_size));
    theme.scrollbar_mode = style.scrollbar.mode(os_auto_hides_scrollbars);
    // Sheets open below dial's titlebar, not gpui-kit's default 34 px one.
    theme.sheet.margin_top = chrome::TITLE_BAR_HEIGHT;
    if let Some(ramp) = style.accent.ramp(theme.mode) {
        apply_accent(theme, ramp);
    }
}

fn apply_accent(theme: &mut Theme, ramp: AccentRamp) {
    let AccentRamp {
        base,
        hover,
        active,
        foreground,
    } = ramp;
    let c = &mut theme.colors;
    c.primary = base;
    c.primary_hover = hover;
    c.primary_active = active;
    c.primary_foreground = foreground;
    c.button_primary = base;
    c.button_primary_hover = hover;
    c.button_primary_active = active;
    c.button_primary_foreground = foreground;
    c.sidebar_primary = base;
    c.sidebar_primary_foreground = foreground;
    c.ring = base;
    c.caret = base;
    c.link = base;
    c.link_hover = hover;
    c.link_active = active;
    c.drag_border = base;
    c.list_active_border = base;
    c.table_active_border = base;
    c.progress_bar = base;
    c.slider_bar = base;
    c.list_active = base.alpha(0.14);
    c.table_active = base.alpha(0.14);
    c.selection = base.alpha(0.28);
    c.drop_target = base.alpha(0.2);
}

fn tr(key: &str) -> SharedString {
    rust_i18n::t!(key).to_string().into()
}

impl Appearance {
    /// Localised label.
    pub fn label(self) -> SharedString {
        tr(&format!("appearance.{}", self.key()))
    }
}

impl CornerStyle {
    /// Localised label.
    pub fn label(self) -> SharedString {
        tr(&format!("corners.{}", self.key()))
    }
}

impl Accent {
    /// Localised label.
    pub fn label(self) -> SharedString {
        tr(&format!("accent.{}", self.key()))
    }
}

impl ScrollbarVisibility {
    /// Localised label.
    pub fn label(self) -> SharedString {
        tr(&format!("scrollbar.{}", self.key()))
    }
}

impl UiFont {
    /// Label: the family name, or the localised "system font".
    pub fn label(self) -> SharedString {
        match self {
            Self::Inter => fonts::UI_FONT_FAMILY.into(),
            Self::System => tr("font.system_ui"),
        }
    }
}

impl MonoFont {
    /// Label: the family name, or the localised "system monospace".
    pub fn label(self) -> SharedString {
        match self {
            Self::JetBrainsMono => fonts::MONO_FONT_FAMILY.into(),
            Self::System => tr("font.system_mono"),
        }
    }
}

/// Colour of chrome dividers: the theme border, or nothing when the user turned them off.
pub fn divider_color(cx: &App) -> Hsla {
    if UiSettings::get(cx).theme.style.dividers {
        cx.theme().border
    } else {
        cx.theme().transparent
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// `name → mode` of every theme in one preset file, parsed the way `init` loads it.
    fn themes_in(json: &str) -> Result<Vec<(String, ThemeMode)>, String> {
        let mut registry = ThemeRegistry::default();
        registry
            .load_themes_from_str(json)
            .map_err(|err| format!("{err:#}"))?;
        Ok(registry
            .themes()
            .values()
            .map(|config| (config.name.to_string(), config.mode))
            .collect())
    }

    #[test]
    fn every_bundled_preset_parses_with_unique_names() {
        let mut modes = BTreeMap::new();
        for (file, json) in presets::FILES {
            let themes = themes_in(json);
            assert!(themes.is_ok(), "{file} must parse: {themes:?}");
            for (name, mode) in themes.unwrap_or_default() {
                assert!(
                    modes.insert(name.clone(), mode).is_none(),
                    "theme name {name} in {file} is already bundled"
                );
            }
        }
        assert_eq!(
            modes.get(presets::DEFAULT_LIGHT),
            Some(&ThemeMode::Light),
            "default light preset is bundled as a light theme"
        );
        assert_eq!(
            modes.get(presets::DEFAULT_DARK),
            Some(&ThemeMode::Dark),
            "default dark preset is bundled as a dark theme"
        );
    }

    #[test]
    fn accent_ramps_keep_text_readable_in_both_modes() {
        for accent in Accent::ALL {
            for mode in [ThemeMode::Light, ThemeMode::Dark] {
                let Some(ramp) = accent.ramp(mode) else {
                    assert_eq!(accent, Accent::Theme, "only `Theme` defers to the preset");
                    continue;
                };
                for (state, fill) in [
                    ("base", ramp.base),
                    ("hover", ramp.hover),
                    ("active", ramp.active),
                ] {
                    let ratio = color::contrast(ramp.foreground, fill);
                    assert!(
                        ratio >= 4.5,
                        "{accent:?} {mode:?} {state}: text contrast {ratio:.2} < 4.5"
                    );
                }
            }
        }
    }

    #[test]
    fn appearance_follows_the_os_only_when_asked() {
        assert_eq!(
            Appearance::System.mode(WindowAppearance::Dark),
            ThemeMode::Dark,
            "system follows a dark OS"
        );
        assert_eq!(
            Appearance::System.mode(WindowAppearance::VibrantLight),
            ThemeMode::Light,
            "system follows a light OS"
        );
        assert_eq!(
            Appearance::Light.mode(WindowAppearance::Dark),
            ThemeMode::Light,
            "explicit light ignores the OS"
        );
        assert_eq!(
            Appearance::Dark.mode(WindowAppearance::Light),
            ThemeMode::Dark,
            "explicit dark ignores the OS"
        );
    }
}
