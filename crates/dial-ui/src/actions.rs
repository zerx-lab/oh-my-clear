//! Every UI command is an `Action` (ADR 0011): bound to keys here, listed in the native
//! menus, and dispatchable from any view. Handlers are application-level, so they work
//! from every window regardless of focus.

use gpui_kit::{
    Action, App, DummyKeyboardMapper, KeyBinding, Menu, MenuItem, SharedString, Window,
};

use crate::i18n::Language;
use crate::settings::UiSettings;
use crate::theme::Appearance;
use crate::{Error, Result, window};

gpui_kit::actions!(
    dial,
    [
        /// Quit the UI process (the daemon keeps running).
        Quit,
        /// Close the focused window.
        CloseWindow,
        /// Minimize the focused window.
        Minimize,
        /// Zoom (maximize / restore) the focused window.
        Zoom,
        /// Open, or bring forward, the settings window.
        OpenSettings,
        /// Switch between the light and dark theme.
        ToggleAppearance,
        /// Follow the OS appearance.
        UseSystemAppearance,
        /// Use the light theme.
        UseLightAppearance,
        /// Use the dark theme.
        UseDarkAppearance,
        /// Follow the OS language.
        UseSystemLanguage,
        /// Use English.
        UseEnglish,
        /// Use Simplified Chinese.
        UseSimplifiedChinese,
    ]
);

/// Global key bindings. `secondary` is Cmd on macOS and Ctrl elsewhere.
fn bindings() -> Vec<(&'static str, Box<dyn Action>)> {
    let mut bindings: Vec<(&'static str, Box<dyn Action>)> = vec![
        ("secondary-q", Box::new(Quit)),
        ("secondary-w", Box::new(CloseWindow)),
        ("secondary-,", Box::new(OpenSettings)),
        ("secondary-k secondary-t", Box::new(ToggleAppearance)),
    ];
    if cfg!(target_os = "macos") {
        bindings.push(("cmd-m", Box::new(Minimize)));
    }
    bindings
}

fn set_appearance(appearance: Appearance, cx: &mut App) {
    UiSettings::update(cx, |s| s.theme.appearance = appearance);
}

fn set_language(language: Language, cx: &mut App) {
    UiSettings::update(cx, |s| s.language = language);
}

/// Registers handlers and key bindings.
pub(crate) fn init(cx: &mut App) -> Result<()> {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &OpenSettings, cx| window::open_settings_window(cx));
    cx.on_action(|_: &CloseWindow, cx| with_active_window(cx, Window::remove_window));
    cx.on_action(|_: &Minimize, cx| with_active_window(cx, |w| w.minimize_window()));
    cx.on_action(|_: &Zoom, cx| with_active_window(cx, |w| w.zoom_window()));
    cx.on_action(|_: &ToggleAppearance, cx| {
        let dark = cx
            .try_global::<gpui_kit::component::Theme>()
            .is_some_and(|theme| theme.mode.is_dark());
        let next = if dark {
            Appearance::Light
        } else {
            Appearance::Dark
        };
        set_appearance(next, cx);
    });
    cx.on_action(|_: &UseSystemAppearance, cx| set_appearance(Appearance::System, cx));
    cx.on_action(|_: &UseLightAppearance, cx| set_appearance(Appearance::Light, cx));
    cx.on_action(|_: &UseDarkAppearance, cx| set_appearance(Appearance::Dark, cx));
    cx.on_action(|_: &UseSystemLanguage, cx| set_language(Language::System, cx));
    cx.on_action(|_: &UseEnglish, cx| set_language(Language::English, cx));
    cx.on_action(|_: &UseSimplifiedChinese, cx| {
        set_language(Language::SimplifiedChinese, cx);
    });

    let bindings = bindings()
        .into_iter()
        .map(|(keys, action)| {
            KeyBinding::load(keys, action, None, false, None, &DummyKeyboardMapper).map_err(|err| {
                Error::KeyBinding {
                    keys,
                    message: err.to_string(),
                }
            })
        })
        .collect::<Result<Vec<_>>>()?;
    cx.bind_keys(bindings);
    Ok(())
}

fn with_active_window(cx: &mut App, f: impl FnOnce(&mut Window)) {
    let Some(handle) = cx.active_window() else {
        return;
    };
    if let Err(err) = handle.update(cx, |_, window, _| f(window)) {
        tracing::warn!("active window is gone: {err:#}");
    }
}

/// The action that selects `appearance`.
pub fn appearance_action(appearance: Appearance) -> Box<dyn Action> {
    match appearance {
        Appearance::System => Box::new(UseSystemAppearance),
        Appearance::Light => Box::new(UseLightAppearance),
        Appearance::Dark => Box::new(UseDarkAppearance),
    }
}

/// The action that selects `language`.
pub fn language_action(language: Language) -> Box<dyn Action> {
    match language {
        Language::System => Box::new(UseSystemLanguage),
        Language::English => Box::new(UseEnglish),
        Language::SimplifiedChinese => Box::new(UseSimplifiedChinese),
    }
}

fn tr(key: &str) -> SharedString {
    rust_i18n::t!(key).to_string().into()
}

fn checked_item(name: SharedString, action: Box<dyn Action>, checked: bool) -> MenuItem {
    MenuItem::Action {
        name,
        action,
        os_action: None,
        checked,
        disabled: false,
    }
}

/// Installs the native menu bar (macOS) in the current language. Rebuilt whenever the
/// language or appearance changes, because labels and check marks are baked in.
pub(crate) fn set_menus(cx: &mut App) {
    let settings = UiSettings::get(cx);
    let (appearance, language) = (settings.theme.appearance, settings.language);
    let appearance_menu = Menu::new(tr("menu.appearance")).items(
        Appearance::ALL.map(|a| checked_item(a.label(), appearance_action(a), a == appearance)),
    );
    let language_menu = Menu::new(tr("menu.language"))
        .items(Language::ALL.map(|l| checked_item(l.label(), language_action(l), l == language)));
    cx.set_menus([
        Menu::new("dial").items([
            MenuItem::action(tr("menu.settings"), OpenSettings),
            MenuItem::separator(),
            MenuItem::action(tr("menu.quit"), Quit),
        ]),
        Menu::new(tr("menu.view")).items([
            MenuItem::action(tr("appearance.toggle"), ToggleAppearance),
            MenuItem::separator(),
            MenuItem::submenu(appearance_menu),
            MenuItem::submenu(language_menu),
        ]),
        Menu::new(tr("menu.window")).items([
            MenuItem::action(tr("menu.minimize"), Minimize),
            MenuItem::action(tr("menu.zoom"), Zoom),
            MenuItem::separator(),
            MenuItem::action(tr("menu.close_window"), CloseWindow),
        ]),
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bindings_parse() {
        for (keys, action) in bindings() {
            let binding = KeyBinding::load(keys, action, None, false, None, &DummyKeyboardMapper);
            assert!(binding.is_ok(), "binding `{keys}` must parse: {binding:?}");
        }
    }
}
