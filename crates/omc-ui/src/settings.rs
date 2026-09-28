//! UI preferences (theme and language) as one GPUI global. Every write goes through
//! [`UiSettings::update`], which applies side effects (locale, theme, native menus) and
//! refreshes all windows, so a view never has to know what depends on a preference.

use std::sync::LazyLock;

use gpui_kit::{App, Global};

use crate::actions;
use crate::i18n::{self, Language};
use crate::theme::{self, ThemePreferences};

/// The UI process's preferences.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UiSettings {
    /// Theme selection and style overrides.
    pub theme: ThemePreferences,
    /// UI language.
    pub language: Language,
}

impl Global for UiSettings {}

static DEFAULTS: LazyLock<UiSettings> = LazyLock::new(UiSettings::default);

impl UiSettings {
    /// Current preferences (defaults until [`crate::init`] installs the global).
    pub fn get(cx: &App) -> &Self {
        cx.try_global::<Self>().unwrap_or(&DEFAULTS)
    }

    /// Edits the preferences and applies whatever changed.
    pub fn update(cx: &mut App, edit: impl FnOnce(&mut Self)) {
        let before = Self::get(cx).clone();
        let mut next = before.clone();
        edit(&mut next);
        if next == before {
            return;
        }
        let language_changed = next.language != before.language;
        let theme_changed = next.theme != before.theme;
        let appearance_changed = next.theme.appearance != before.theme.appearance;
        cx.set_global(next);
        if language_changed {
            i18n::apply(Self::get(cx).language);
        }
        if theme_changed {
            theme::apply(cx.window_appearance(), cx);
        }
        if language_changed || appearance_changed {
            actions::set_menus(cx);
        }
        cx.refresh_windows();
    }
}
