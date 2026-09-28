//! The settings window: every [`UiSettings`] field, grouped by page, built on
//! gpui-component's searchable `Settings`. Fields read and write the global through
//! [`UiSettings::update`], so a change applies to every window immediately; each page can
//! reset its fields to [`UiSettings::default`].

use gpui_kit::component::setting::{
    SettingField, SettingGroup, SettingItem, SettingPage, Settings,
};
use gpui_kit::component::{IconName, Sizable as _, ThemeMode, v_flex};
use gpui_kit::{
    App, Context, FocusHandle, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Window, div,
};

use crate::i18n::Language;
use crate::settings::UiSettings;
use crate::theme::{
    self, Accent, Appearance, CornerStyle, MONO_FONT_SIZES, MonoFont, ScrollbarVisibility,
    UI_FONT_SIZES, UiFont,
};
use crate::title_bar::AppTitleBar;

/// Root view of the settings window.
pub(crate) struct SettingsView {
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    /// Creates the view and focuses it, so window-level key bindings resolve.
    pub(crate) fn new(window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let subscriptions = vec![cx.observe_window_appearance(window, |_, window, cx| {
            theme::system_appearance_changed(window.appearance(), cx);
        })];
        Self {
            focus_handle,
            _subscriptions: subscriptions,
        }
    }
}

impl std::fmt::Debug for SettingsView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsView").finish_non_exhaustive()
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        v_flex()
            .id("settings-window")
            .key_context("Settings")
            .track_focus(&self.focus_handle)
            .size_full()
            .child(AppTitleBar::new().divider(true))
            .child(
                div().flex_1().min_h_0().child(
                    // Small controls: 24 px dropdowns with `text_sm` labels and 28×16 switches
                    // match the `text_sm` item titles; the Medium default (32 px, `text_base`)
                    // outweighs the text it annotates.
                    Settings::new("settings")
                        .small()
                        .pages(vec![appearance_page(cx), language_page()]),
                ),
            )
    }
}

fn tr(key: &str) -> SharedString {
    rust_i18n::t!(key).to_string().into()
}

/// A dropdown over a closed set of choices, stored in [`UiSettings`] and identified by a
/// stable string key.
fn choice<T>(
    all: &[T],
    key: fn(T) -> &'static str,
    label: fn(T) -> SharedString,
    get: fn(&UiSettings) -> T,
    set: fn(&mut UiSettings, T),
) -> SettingField<SharedString>
where
    T: Copy + PartialEq + 'static,
{
    let options = all
        .iter()
        .map(|&value| (SharedString::from(key(value)), label(value)))
        .collect();
    let values = all.to_vec();
    SettingField::dropdown(
        options,
        move |cx: &App| key(get(UiSettings::get(cx))).into(),
        move |picked: SharedString, cx: &mut App| {
            if let Some(&value) = values.iter().find(|&&v| key(v) == picked.as_ref()) {
                UiSettings::update(cx, |s| set(s, value));
            }
        },
    )
    .default_value(key(get(&UiSettings::default())))
}

/// A dropdown of font sizes in px.
fn size_choice(
    sizes: &'static [u8],
    get: fn(&UiSettings) -> u8,
    set: fn(&mut UiSettings, u8),
) -> SettingField<SharedString> {
    let options = sizes
        .iter()
        .map(|size| {
            let value = SharedString::from(size.to_string());
            (value.clone(), value)
        })
        .collect();
    SettingField::dropdown(
        options,
        move |cx: &App| get(UiSettings::get(cx)).to_string().into(),
        move |picked: SharedString, cx: &mut App| match picked.parse::<u8>() {
            Ok(size) if sizes.contains(&size) => UiSettings::update(cx, |s| set(s, size)),
            _ => tracing::warn!(%picked, "ignoring unknown font size"),
        },
    )
    .default_value(get(&UiSettings::default()).to_string())
}

/// A dropdown of the registered presets for `mode`.
fn preset_choice(
    mode: ThemeMode,
    get: fn(&UiSettings) -> SharedString,
    set: fn(&mut UiSettings, SharedString),
    cx: &App,
) -> SettingField<SharedString> {
    let options = theme::preset_names(mode, cx)
        .into_iter()
        .map(|name| (name.clone(), name))
        .collect();
    SettingField::scrollable_dropdown(
        options,
        move |cx: &App| get(UiSettings::get(cx)),
        move |picked: SharedString, cx: &mut App| UiSettings::update(cx, |s| set(s, picked)),
    )
    .default_value(get(&UiSettings::default()))
}

/// A switch stored in [`UiSettings`].
fn toggle(get: fn(&UiSettings) -> bool, set: fn(&mut UiSettings, bool)) -> SettingField<bool> {
    SettingField::switch(
        move |cx: &App| get(UiSettings::get(cx)),
        move |on: bool, cx: &mut App| UiSettings::update(cx, |s| set(s, on)),
    )
    .default_value(get(&UiSettings::default()))
}

fn theme_group(cx: &App) -> SettingGroup {
    SettingGroup::new()
        .title(tr("settings.group.theme"))
        .items(vec![
            SettingItem::new(
                tr("settings.appearance.title"),
                choice(
                    &Appearance::ALL,
                    Appearance::key,
                    Appearance::label,
                    |s| s.theme.appearance,
                    |s, v| s.theme.appearance = v,
                ),
            )
            .description(tr("settings.appearance.description")),
            SettingItem::new(
                tr("settings.light_theme.title"),
                preset_choice(
                    ThemeMode::Light,
                    |s| s.theme.light_theme.clone(),
                    |s, v| s.theme.light_theme = v,
                    cx,
                ),
            )
            .description(tr("settings.light_theme.description")),
            SettingItem::new(
                tr("settings.dark_theme.title"),
                preset_choice(
                    ThemeMode::Dark,
                    |s| s.theme.dark_theme.clone(),
                    |s, v| s.theme.dark_theme = v,
                    cx,
                ),
            )
            .description(tr("settings.dark_theme.description")),
            SettingItem::new(
                tr("settings.accent.title"),
                choice(
                    &Accent::ALL,
                    Accent::key,
                    Accent::label,
                    |s| s.theme.style.accent,
                    |s, v| s.theme.style.accent = v,
                ),
            )
            .description(tr("settings.accent.description")),
        ])
}

fn shape_group() -> SettingGroup {
    SettingGroup::new()
        .title(tr("settings.group.shape"))
        .items(vec![
            SettingItem::new(
                tr("settings.corners.title"),
                choice(
                    &CornerStyle::ALL,
                    CornerStyle::key,
                    CornerStyle::label,
                    |s| s.theme.style.corners,
                    |s, v| s.theme.style.corners = v,
                ),
            )
            .description(tr("settings.corners.description")),
            SettingItem::new(
                tr("settings.shadows.title"),
                toggle(|s| s.theme.style.shadows, |s, v| s.theme.style.shadows = v),
            )
            .description(tr("settings.shadows.description")),
            SettingItem::new(
                tr("settings.focus_ring.title"),
                toggle(
                    |s| s.theme.style.focus_ring,
                    |s, v| s.theme.style.focus_ring = v,
                ),
            )
            .description(tr("settings.focus_ring.description")),
            SettingItem::new(
                tr("settings.dividers.title"),
                toggle(
                    |s| s.theme.style.dividers,
                    |s, v| s.theme.style.dividers = v,
                ),
            )
            .description(tr("settings.dividers.description")),
        ])
}

fn typography_group() -> SettingGroup {
    SettingGroup::new()
        .title(tr("settings.group.typography"))
        .items(vec![
            SettingItem::new(
                tr("settings.ui_font.title"),
                choice(
                    &UiFont::ALL,
                    UiFont::family,
                    UiFont::label,
                    |s| s.theme.style.ui_font,
                    |s, v| s.theme.style.ui_font = v,
                ),
            ),
            SettingItem::new(
                tr("settings.ui_font_size.title"),
                size_choice(
                    &UI_FONT_SIZES,
                    |s| s.theme.style.ui_font_size,
                    |s, v| s.theme.style.ui_font_size = v,
                ),
            )
            .description(tr("settings.ui_font_size.description")),
            SettingItem::new(
                tr("settings.mono_font.title"),
                choice(
                    &MonoFont::ALL,
                    MonoFont::family,
                    MonoFont::label,
                    |s| s.theme.style.mono_font,
                    |s, v| s.theme.style.mono_font = v,
                ),
            ),
            SettingItem::new(
                tr("settings.mono_font_size.title"),
                size_choice(
                    &MONO_FONT_SIZES,
                    |s| s.theme.style.mono_font_size,
                    |s, v| s.theme.style.mono_font_size = v,
                ),
            ),
        ])
}

fn scrolling_group() -> SettingGroup {
    SettingGroup::new()
        .title(tr("settings.group.scrolling"))
        .item(SettingItem::new(
            tr("settings.scrollbar.title"),
            choice(
                &ScrollbarVisibility::ALL,
                ScrollbarVisibility::key,
                ScrollbarVisibility::label,
                |s| s.theme.style.scrollbar,
                |s, v| s.theme.style.scrollbar = v,
            ),
        ))
}

fn appearance_page(cx: &App) -> SettingPage {
    SettingPage::new(tr("settings.page.appearance"))
        .icon(IconName::Palette)
        .default_open(true)
        .resettable(true)
        .groups(vec![
            theme_group(cx),
            shape_group(),
            typography_group(),
            scrolling_group(),
        ])
}

fn language_page() -> SettingPage {
    SettingPage::new(tr("settings.page.language"))
        .icon(IconName::Globe)
        .resettable(true)
        .group(
            SettingGroup::new()
                .title(tr("settings.group.language"))
                .item(
                    SettingItem::new(
                        tr("settings.language.title"),
                        choice(
                            &Language::ALL,
                            Language::key,
                            Language::label,
                            |s| s.language,
                            |s, v| s.language = v,
                        ),
                    )
                    .description(tr("settings.language.description")),
                ),
        )
}
