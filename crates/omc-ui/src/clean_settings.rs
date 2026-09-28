//! The daemon's stored [`Settings`] mirrored in a GPUI global ([`CleanPrefs`]).
//!
//! - Fetched on every connect and whenever the daemon reports `settings_changed`.
//! - [`CleanPrefs::update`] applies an edit at once and saves it (debounced) with
//!   `put_settings`; failures are logged and shown as a notification.
//! - UI preferences ([`UiSettings`]) persist in `Settings.ui` as stable string keys: every
//!   change through [`UiSettings::update`] is written back, and the stored map is applied
//!   once after the first successful fetch (without writing it back). Unknown or invalid
//!   values are ignored.

use std::collections::BTreeMap;
use std::sync::LazyLock;
use std::time::Duration;

use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ThemeMode, WindowExt as _};
use gpui_kit::{App, BorrowAppContext as _, Global, SharedString, Subscription, Task, WeakEntity};
use omc_ipc::client::{ClientEvent, ConnState};
use omc_proto::Event;
use omc_proto::settings::{CleanSettings, Settings};

use crate::engine::{self, Engine};
use crate::i18n::Language;
use crate::jobs;
use crate::settings::UiSettings;
use crate::theme::{
    self, Accent, Appearance, CornerStyle, MONO_FONT_SIZES, MonoFont, ScrollbarVisibility,
    UI_FONT_SIZES, UiFont,
};

/// Edits within this window are sent to the daemon as one `put_settings`.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(300);

/// Stable keys of [`UiSettings`] inside `Settings.ui`.
mod key {
    pub(super) const LANGUAGE: &str = "language";
    pub(super) const APPEARANCE: &str = "theme.appearance";
    pub(super) const LIGHT: &str = "theme.light";
    pub(super) const DARK: &str = "theme.dark";
    pub(super) const ACCENT: &str = "theme.accent";
    pub(super) const CORNERS: &str = "theme.corners";
    pub(super) const SHADOWS: &str = "theme.shadows";
    pub(super) const FOCUS_RING: &str = "theme.focus_ring";
    pub(super) const DIVIDERS: &str = "theme.dividers";
    pub(super) const UI_FONT: &str = "theme.ui_font";
    pub(super) const UI_FONT_SIZE: &str = "theme.ui_font_size";
    pub(super) const MONO_FONT: &str = "theme.mono_font";
    pub(super) const MONO_FONT_SIZE: &str = "theme.mono_font_size";
    pub(super) const SCROLLBAR: &str = "theme.scrollbar";
    /// Not part of [`super::UiSettings`]: read through [`super::CleanPrefs::auto_scan`].
    pub(super) const AUTO_SCAN: &str = "scan.auto_open";
}

static DEFAULT_CLEAN: LazyLock<CleanSettings> = LazyLock::new(CleanSettings::default);

/// Local mirror of the daemon's settings.
#[derive(Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent sync flags of one mirror; no state machine covers their combinations"
)]
pub struct CleanPrefs {
    /// The settings as last fetched, plus local edits.
    settings: Settings,
    /// A fetch succeeded on the current connection.
    loaded: bool,
    /// Pending debounced save.
    saving: Option<Task<()>>,
    /// Pending fetch.
    fetching: Option<Task<()>>,
    /// Local edits the daemon has not acknowledged yet (a fetch must not undo them).
    unsaved: bool,
    /// The stored UI map was applied (once per process).
    ui_restored: bool,
    /// UI preferences changed while nothing was loaded; written after the next fetch.
    ui_dirty: bool,
    /// Applying the stored UI map: its `UiSettings::update` must not write back.
    restoring_ui: bool,
    /// The engine entity followed by `subscription`.
    engine: Option<WeakEntity<Engine>>,
    subscription: Option<Subscription>,
}

impl Global for CleanPrefs {}

impl std::fmt::Debug for CleanPrefs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CleanPrefs")
            .field("loaded", &self.loaded)
            .field("unsaved", &self.unsaved)
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

/// What a successful fetch asks the caller to do.
#[derive(Debug, Default, PartialEq, Eq)]
struct Fetched {
    /// Apply this stored UI map to [`UiSettings`].
    restore_ui: Option<BTreeMap<String, String>>,
    /// Send the merged settings back.
    save: bool,
}

impl CleanPrefs {
    /// The scanning/cleaning settings (defaults until loaded).
    pub fn settings(cx: &App) -> &CleanSettings {
        cx.try_global::<Self>()
            .map_or(&*DEFAULT_CLEAN, |p| &p.settings.clean)
    }

    /// `true` once the daemon's settings arrived on the current connection.
    pub fn is_loaded(cx: &App) -> bool {
        cx.try_global::<Self>().is_some_and(|p| p.loaded)
    }

    /// Edits the settings: applies at once, saves to the daemon debounced. Ignored until
    /// the settings are loaded (the UI disables its controls meanwhile).
    pub fn update(cx: &mut App, edit: impl FnOnce(&mut CleanSettings)) {
        let changed = cx.update_default_global(|p: &mut Self, _| p.apply_edit(edit));
        if changed {
            schedule_save(cx);
        }
    }

    /// Whether opening an area may start its scan (UI preference `scan.auto_open`, on by
    /// default and until the settings are loaded).
    pub fn auto_scan(cx: &App) -> bool {
        cx.try_global::<Self>()
            .and_then(|p| p.settings.ui.get(key::AUTO_SCAN))
            .and_then(|v| v.parse::<bool>().ok())
            .unwrap_or(true)
    }

    /// Stores the `scan.auto_open` preference (ignored until the settings are loaded).
    pub fn set_auto_scan(cx: &mut App, on: bool) {
        let changed = cx.update_default_global(|p: &mut Self, _| {
            p.apply_ui_entry(key::AUTO_SCAN, on.to_string())
        });
        if changed {
            schedule_save(cx);
        }
    }

    /// Pure part of [`Self::set_auto_scan`]: `true` when the stored map changed.
    fn apply_ui_entry(&mut self, key: &str, value: String) -> bool {
        if !self.loaded {
            tracing::warn!(key, "UI preference ignored: settings not loaded");
            return false;
        }
        if self.settings.ui.get(key) == Some(&value) {
            return false;
        }
        self.settings.ui.insert(key.to_owned(), value);
        self.unsaved = true;
        true
    }

    /// Marks the settings loaded with `scan.auto_open = auto_scan` (no daemon).
    #[cfg(test)]
    pub(crate) fn force_loaded(cx: &mut App, auto_scan: bool) {
        cx.update_default_global(|p: &mut Self, _| {
            p.loaded = true;
            p.settings
                .ui
                .insert(key::AUTO_SCAN.to_owned(), auto_scan.to_string());
        });
    }

    /// Pure part of [`Self::update`]: `true` when an edit changed something.
    fn apply_edit(&mut self, edit: impl FnOnce(&mut CleanSettings)) -> bool {
        if !self.loaded {
            tracing::warn!("settings edit ignored: settings not loaded");
            return false;
        }
        let before = self.settings.clean.clone();
        edit(&mut self.settings.clean);
        let changed = self.settings.clean != before;
        self.unsaved |= changed;
        changed
    }

    /// Pure part of a UI preference change: `true` when the stored map changed and must
    /// be saved.
    fn apply_ui(&mut self, local: &BTreeMap<String, String>) -> bool {
        if !self.loaded {
            self.ui_dirty = true;
            return false;
        }
        let changed = merge_map(&mut self.settings.ui, local);
        self.unsaved |= changed;
        changed
    }

    /// Pure part of a successful fetch. `local_ui` is the current [`UiSettings`] map.
    fn merge_fetched(&mut self, fetched: Settings, local_ui: &BTreeMap<String, String>) -> Fetched {
        let mut out = Fetched::default();
        if self.unsaved {
            // Local edits win. A save in flight already carries them (re-saving here
            // would echo `settings_changed` back and forth); otherwise send them.
            out.save = self.saving.is_none();
        } else {
            self.settings = fetched;
        }
        self.loaded = true;
        if self.ui_dirty {
            self.ui_dirty = false;
            if merge_map(&mut self.settings.ui, local_ui) {
                self.unsaved = true;
                out.save = true;
            }
        } else if !self.ui_restored {
            out.restore_ui = Some(self.settings.ui.clone());
        }
        self.ui_restored = true;
        out
    }
}

/// Copies `local` over `stored`, keeping keys it does not know; `true` if anything changed.
fn merge_map(stored: &mut BTreeMap<String, String>, local: &BTreeMap<String, String>) -> bool {
    let mut changed = false;
    for (k, v) in local {
        if stored.get(k) != Some(v) {
            stored.insert(k.clone(), v.clone());
            changed = true;
        }
    }
    changed
}

/// Installs the global. Call from [`crate::init`].
pub(crate) fn init(cx: &mut App) {
    if !cx.has_global::<CleanPrefs>() {
        cx.set_global(CleanPrefs::default());
    }
}

/// Follows the current engine entity (idempotent): fetches on every connect and on
/// `settings_changed`. Views call this when they are created, which is after
/// `engine::start`.
pub(crate) fn attach(cx: &mut App) {
    let engine = engine::entity(cx);
    let same = cx
        .try_global::<CleanPrefs>()
        .and_then(|p| p.engine.as_ref())
        .and_then(WeakEntity::upgrade)
        .is_some_and(|e| e == engine);
    if same {
        return;
    }
    let subscription = cx.subscribe(&engine, |_, event: &ClientEvent, cx| match event {
        ClientEvent::State(ConnState::Connected { .. })
        | ClientEvent::Daemon(Event::SettingsChanged) => fetch(cx),
        ClientEvent::State(_) => {
            cx.update_default_global(|p: &mut CleanPrefs, _| {
                p.loaded = false;
                p.fetching = None;
            });
        }
        ClientEvent::Daemon(_) => {}
    });
    let connected = matches!(engine.read(cx).state(), ConnState::Connected { .. });
    let weak = engine.downgrade();
    cx.update_default_global(|p: &mut CleanPrefs, _| {
        p.engine = Some(weak);
        p.subscription = Some(subscription);
        p.loaded = false;
    });
    if connected {
        fetch(cx);
    }
}

fn fetch(cx: &mut App) {
    let answer = jobs::get_settings(cx);
    let task = cx.spawn(async move |cx| {
        let result = answer.await;
        cx.update(|cx| fetched(result, cx));
    });
    cx.update_default_global(|p: &mut CleanPrefs, _| p.fetching = Some(task));
}

fn fetched(result: Result<Settings, jobs::Failure>, cx: &mut App) {
    let settings = match result {
        Ok(settings) => settings,
        Err(err) => {
            tracing::warn!("cannot load settings: {err}");
            return;
        }
    };
    let local_ui = ui_map(UiSettings::get(cx));
    let out = cx.update_default_global(|p: &mut CleanPrefs, _| {
        p.fetching = None;
        p.merge_fetched(settings, &local_ui)
    });
    if let Some(stored) = out.restore_ui {
        let presets = [ThemeMode::Light, ThemeMode::Dark].map(|m| theme::preset_names(m, cx));
        cx.update_default_global(|p: &mut CleanPrefs, _| p.restoring_ui = true);
        UiSettings::update(cx, |s| {
            apply_ui_map(&stored, s, |mode, name| {
                let [light, dark] = &presets;
                let names = if mode == ThemeMode::Dark { dark } else { light };
                names.iter().any(|n| n.as_ref() == name)
            });
        });
        cx.update_default_global(|p: &mut CleanPrefs, _| p.restoring_ui = false);
    }
    if out.save {
        schedule_save(cx);
    }
}

/// Hook of [`UiSettings::update`]: stores the changed UI preferences.
pub(crate) fn ui_changed(cx: &mut App) {
    if cx.try_global::<CleanPrefs>().is_none_or(|p| p.restoring_ui) {
        return;
    }
    let local = ui_map(UiSettings::get(cx));
    let changed = cx.update_default_global(|p: &mut CleanPrefs, _| p.apply_ui(&local));
    if changed {
        schedule_save(cx);
    }
}

fn schedule_save(cx: &mut App) {
    let timer = cx.background_executor().timer(SAVE_DEBOUNCE);
    let task = cx.spawn(async move |cx| {
        timer.await;
        let answer = cx.update(|cx| {
            let settings = cx
                .try_global::<CleanPrefs>()
                .map(|p| p.settings.clone())
                .unwrap_or_default();
            jobs::put_settings(settings, cx)
        });
        let result = answer.await;
        cx.update(|cx| saved(result, cx));
    });
    cx.update_default_global(|p: &mut CleanPrefs, _| p.saving = Some(task));
}

fn saved(result: Result<(), jobs::Failure>, cx: &mut App) {
    cx.update_default_global(|p: &mut CleanPrefs, _| {
        p.unsaved = false;
        p.saving = None;
    });
    if let Err(err) = result {
        tracing::warn!("cannot save settings: {err}");
        let message: SharedString = rust_i18n::t!("settings.clean.save_failed", error = err)
            .to_string()
            .into();
        notify_error(message, cx);
    }
}

/// Shows `message` in the active (or first) window.
fn notify_error(message: SharedString, cx: &mut App) {
    let Some(window) = cx
        .active_window()
        .or_else(|| cx.windows().into_iter().next())
    else {
        return;
    };
    if let Err(err) = window.update(cx, |_, window, cx| {
        window.push_notification(Notification::error(message), cx);
    }) {
        tracing::debug!("cannot show a notification: {err:#}");
    }
}

/// [`UiSettings`] as stable string keys.
pub(crate) fn ui_map(s: &UiSettings) -> BTreeMap<String, String> {
    let style = &s.theme.style;
    [
        (key::LANGUAGE, s.language.key().to_owned()),
        (key::APPEARANCE, s.theme.appearance.key().to_owned()),
        (key::LIGHT, s.theme.light_theme.to_string()),
        (key::DARK, s.theme.dark_theme.to_string()),
        (key::ACCENT, style.accent.key().to_owned()),
        (key::CORNERS, style.corners.key().to_owned()),
        (key::SHADOWS, style.shadows.to_string()),
        (key::FOCUS_RING, style.focus_ring.to_string()),
        (key::DIVIDERS, style.dividers.to_string()),
        (key::UI_FONT, style.ui_font.family().to_owned()),
        (key::UI_FONT_SIZE, style.ui_font_size.to_string()),
        (key::MONO_FONT, style.mono_font.family().to_owned()),
        (key::MONO_FONT_SIZE, style.mono_font_size.to_string()),
        (key::SCROLLBAR, style.scrollbar.key().to_owned()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect()
}

/// Applies a stored map onto `s`; missing, unknown or invalid values keep the current
/// value. `is_preset(mode, name)` tells whether a theme preset exists.
pub(crate) fn apply_ui_map(
    map: &BTreeMap<String, String>,
    s: &mut UiSettings,
    is_preset: impl Fn(ThemeMode, &str) -> bool,
) {
    let get = |k: &str| map.get(k).map(String::as_str);
    let flag = |k: &str| get(k).and_then(|v| v.parse::<bool>().ok());
    let size = |k: &str, allowed: &[u8]| {
        get(k)
            .and_then(|v| v.parse::<u8>().ok())
            .filter(|v| allowed.contains(v))
    };
    if let Some(v) = get(key::LANGUAGE).and_then(Language::from_key) {
        s.language = v;
    }
    let theme = &mut s.theme;
    if let Some(v) = get(key::APPEARANCE).and_then(Appearance::from_key) {
        theme.appearance = v;
    }
    if let Some(v) = get(key::LIGHT).filter(|v| is_preset(ThemeMode::Light, v)) {
        theme.light_theme = SharedString::from(v.to_owned());
    }
    if let Some(v) = get(key::DARK).filter(|v| is_preset(ThemeMode::Dark, v)) {
        theme.dark_theme = SharedString::from(v.to_owned());
    }
    let style = &mut theme.style;
    if let Some(v) = get(key::ACCENT).and_then(Accent::from_key) {
        style.accent = v;
    }
    if let Some(v) = get(key::CORNERS).and_then(CornerStyle::from_key) {
        style.corners = v;
    }
    if let Some(v) = flag(key::SHADOWS) {
        style.shadows = v;
    }
    if let Some(v) = flag(key::FOCUS_RING) {
        style.focus_ring = v;
    }
    if let Some(v) = flag(key::DIVIDERS) {
        style.dividers = v;
    }
    if let Some(v) = get(key::UI_FONT).and_then(UiFont::from_family) {
        style.ui_font = v;
    }
    if let Some(v) = size(key::UI_FONT_SIZE, &UI_FONT_SIZES) {
        style.ui_font_size = v;
    }
    if let Some(v) = get(key::MONO_FONT).and_then(MonoFont::from_family) {
        style.mono_font = v;
    }
    if let Some(v) = size(key::MONO_FONT_SIZE, &MONO_FONT_SIZES) {
        style.mono_font_size = v;
    }
    if let Some(v) = get(key::SCROLLBAR).and_then(ScrollbarVisibility::from_key) {
        style.scrollbar = v;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Accent;

    fn loaded() -> CleanPrefs {
        CleanPrefs {
            loaded: true,
            ui_restored: true,
            ..CleanPrefs::default()
        }
    }

    #[test]
    fn auto_scan_pref_waits_for_load_and_survives_ui_writes() {
        let mut prefs = CleanPrefs::default();
        assert!(
            !prefs.apply_ui_entry(key::AUTO_SCAN, "false".into()),
            "ignored before the settings are loaded"
        );
        let mut prefs = loaded();
        assert!(
            prefs.apply_ui_entry(key::AUTO_SCAN, "false".into()),
            "stored once loaded"
        );
        assert!(prefs.unsaved, "marked for saving");
        assert!(
            !prefs.apply_ui_entry(key::AUTO_SCAN, "false".into()),
            "the same value changes nothing"
        );
        prefs.apply_ui(&ui_map(&UiSettings::default()));
        assert_eq!(
            prefs.settings.ui.get(key::AUTO_SCAN).map(String::as_str),
            Some("false"),
            "writing the theme/language map keeps the scan preference"
        );
    }

    #[test]
    fn ui_prefs_round_trip_through_the_string_map() {
        let mut prefs = UiSettings {
            language: Language::SimplifiedChinese,
            ..UiSettings::default()
        };
        prefs.theme.appearance = Appearance::Dark;
        prefs.theme.dark_theme = "Custom Dark".into();
        prefs.theme.style.accent = Accent::Green;
        prefs.theme.style.corners = CornerStyle::Round;
        prefs.theme.style.shadows = false;
        prefs.theme.style.ui_font_size = 18;
        prefs.theme.style.mono_font_size = 11;
        prefs.theme.style.scrollbar = ScrollbarVisibility::Always;
        let map = ui_map(&prefs);
        let mut restored = UiSettings::default();
        apply_ui_map(&map, &mut restored, |_, _| true);
        assert_eq!(restored, prefs, "every preference survives the map");
    }

    #[test]
    fn invalid_ui_values_are_ignored() {
        let map: BTreeMap<String, String> = [
            ("theme.appearance", "sepia"),
            ("theme.ui_font_size", "99"),
            ("theme.shadows", "maybe"),
            ("theme.dark", "Missing Preset"),
            ("language", "zh-CN"),
            ("future.key", "x"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        let mut s = UiSettings::default();
        apply_ui_map(&map, &mut s, |_, name| name != "Missing Preset");
        let expected = UiSettings {
            language: Language::SimplifiedChinese,
            ..UiSettings::default()
        };
        assert_eq!(s, expected, "only the valid language applies");
    }

    #[test]
    fn edits_are_ignored_until_loaded_and_mark_unsaved_after() {
        let mut prefs = CleanPrefs::default();
        assert!(
            !prefs.apply_edit(|s| s.skip_hidden = false),
            "no edit before the settings are loaded"
        );
        assert!(prefs.settings.clean.skip_hidden, "value unchanged");

        let mut prefs = loaded();
        assert!(
            !prefs.apply_edit(|s| s.skip_hidden = true),
            "setting the same value is no change"
        );
        assert!(!prefs.unsaved, "no-op edits are not saved");
        assert!(prefs.apply_edit(|s| s.old_days = 90), "a real edit changes");
        assert!(prefs.unsaved, "a real edit waits for its save");
    }

    #[test]
    fn a_fetch_never_undoes_unsaved_edits() {
        let mut prefs = loaded();
        assert!(prefs.apply_edit(|s| s.old_days = 90), "edit applies");
        let out = prefs.merge_fetched(Settings::default(), &BTreeMap::new());
        assert_eq!(prefs.settings.clean.old_days, 90, "local edit wins");
        assert!(out.save, "the pending save still goes out");

        let mut prefs = loaded();
        let mut stored = Settings::default();
        stored.clean.old_days = 730;
        let out = prefs.merge_fetched(stored, &BTreeMap::new());
        assert_eq!(prefs.settings.clean.old_days, 730, "stored value applies");
        assert_eq!(out, Fetched::default(), "nothing to save or restore");
    }

    #[test]
    fn the_stored_ui_map_is_restored_once_unless_changed_locally() {
        let mut stored = Settings::default();
        stored.ui.insert("theme.appearance".into(), "dark".into());
        let mut prefs = CleanPrefs::default();
        let out = prefs.merge_fetched(stored.clone(), &BTreeMap::new());
        assert_eq!(
            out.restore_ui,
            Some(stored.ui.clone()),
            "first fetch restores"
        );
        let out = prefs.merge_fetched(stored.clone(), &BTreeMap::new());
        assert_eq!(out.restore_ui, None, "later fetches do not");

        let local = ui_map(&UiSettings::default());
        let mut prefs = CleanPrefs::default();
        assert!(!prefs.apply_ui(&local), "not saved before load");
        let out = prefs.merge_fetched(stored, &local);
        assert_eq!(
            out.restore_ui, None,
            "a local change wins over the stored map"
        );
        assert!(out.save, "and is written back");
        assert_eq!(
            prefs
                .settings
                .ui
                .get("theme.appearance")
                .map(String::as_str),
            Some("system"),
            "local value replaces the stored one"
        );
    }
}
