//! Every UI command is an `Action` (ADR 0011): bound to keys here, listed in the native
//! menus, and dispatchable from any view. Handlers are application-level, so they work
//! from every window regardless of focus; [`ToggleSidebar`] is the exception, handled by
//! the main window's view.

use gpui_kit::component::kbd::Kbd;
use gpui_kit::{
    Action, App, AsKeystroke as _, DummyKeyboardMapper, KeyBinding, Menu, MenuItem, SharedString,
    Window,
};

use crate::i18n::Language;
use crate::settings::UiSettings;
use crate::theme::Appearance;
use crate::{Error, Result, window};

gpui_kit::actions!(
    oh_my_clear,
    [
        /// Quit oh-my-clear: the UI and the daemon with its tray. Closing the last window
        /// (`CloseWindow`) only ends the UI.
        Quit,
        /// Close the focused window.
        CloseWindow,
        /// Minimize the focused window.
        Minimize,
        /// Zoom (maximize / restore) the focused window.
        Zoom,
        /// Open, or bring forward, the settings window.
        OpenSettings,
        /// Show or hide the main window's sidebar.
        ToggleSidebar,
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

/// A key binding: keystrokes and the action they dispatch.
type Binding = (&'static str, Box<dyn Action>);

/// Global key bindings. `secondary` is Cmd on macOS and Ctrl elsewhere.
fn bindings() -> Vec<Binding> {
    let mut bindings: Vec<Binding> = vec![
        ("secondary-q", Box::new(Quit)),
        ("secondary-w", Box::new(CloseWindow)),
        ("secondary-,", Box::new(OpenSettings)),
        ("secondary-b", Box::new(ToggleSidebar)),
        ("secondary-k secondary-t", Box::new(ToggleAppearance)),
    ];
    if cfg!(target_os = "macos") {
        bindings.push(("cmd-m", Box::new(Minimize)));
    }
    bindings
}

fn load_binding((keys, action): Binding) -> Result<KeyBinding> {
    KeyBinding::load(keys, action, None, false, None, &DummyKeyboardMapper).map_err(|err| {
        Error::KeyBinding {
            keys,
            message: err.to_string(),
        }
    })
}

fn set_appearance(appearance: Appearance, cx: &mut App) {
    UiSettings::update(cx, |s| s.theme.appearance = appearance);
}

fn set_language(language: Language, cx: &mut App) {
    UiSettings::update(cx, |s| s.language = language);
}

/// Registers handlers and key bindings.
pub(crate) fn init(cx: &mut App) -> Result<()> {
    cx.on_action(|_: &Quit, cx| crate::engine::quit_all(cx));
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
        .map(load_binding)
        .collect::<Result<Vec<_>>>()?;
    cx.bind_keys(bindings);
    Ok(())
}

/// Runs `f` on the active window once the current dispatch returns. Actions dispatched by a
/// keystroke or a menu item reach these app-level handlers while GPUI is still updating that
/// same window, and a window cannot be updated re-entrantly: `handle.update` would fail and
/// the command would be dropped.
fn with_active_window(cx: &mut App, f: impl FnOnce(&mut Window) + 'static) {
    let Some(handle) = cx.active_window() else {
        return;
    };
    cx.defer(move |cx| {
        if let Err(err) = handle.update(cx, |_, window, _| f(window)) {
            tracing::warn!("active window is gone: {err:#}");
        }
    });
}

/// The action that selects `appearance`.
fn appearance_action(appearance: Appearance) -> Box<dyn Action> {
    match appearance {
        Appearance::System => Box::new(UseSystemAppearance),
        Appearance::Light => Box::new(UseLightAppearance),
        Appearance::Dark => Box::new(UseDarkAppearance),
    }
}

/// The action that selects `language`.
fn language_action(language: Language) -> Box<dyn Action> {
    match language {
        Language::System => Box::new(UseSystemLanguage),
        Language::English => Box::new(UseEnglish),
        Language::SimplifiedChinese => Box::new(UseSimplifiedChinese),
    }
}

/// Key hints for `action` as bound where the focus is.
pub(crate) fn key_hints(action: &dyn Action, window: &Window) -> Vec<Kbd> {
    window
        .highest_precedence_binding_for_action(action)
        .map(|binding| {
            binding
                .keystrokes()
                .iter()
                .map(|stroke| Kbd::new(stroke.as_keystroke().clone()))
                .collect()
        })
        .unwrap_or_default()
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
        Menu::new("oh-my-clear").items([
            MenuItem::action(tr("menu.settings"), OpenSettings),
            MenuItem::separator(),
            MenuItem::action(tr("menu.quit"), Quit),
        ]),
        Menu::new(tr("menu.view")).items([
            MenuItem::action(tr("menu.toggle_sidebar"), ToggleSidebar),
            MenuItem::separator(),
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
        for binding in bindings() {
            let keys = binding.0;
            let loaded = load_binding(binding);
            assert!(loaded.is_ok(), "binding `{keys}` must parse: {loaded:?}");
        }
    }
}
