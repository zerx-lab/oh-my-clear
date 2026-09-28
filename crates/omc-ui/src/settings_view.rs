//! The settings window: every [`UiSettings`] field, grouped by page. A left navigation
//! (search field + one 28 px item per page, like the main sidebar) selects the page; the
//! page shows its groups as titled cards of setting rows, each with a 28 px control.
//! Fields read and write the global through [`UiSettings::update`], so a change applies to
//! every window immediately; each page can reset its fields to [`UiSettings::default`].
//!
//! The scanning/cleaning pages edit the daemon's stored `CleanSettings` through
//! [`CleanPrefs::update`]; they stay disabled until the daemon's settings are loaded and
//! reset to `CleanSettings::default()`.
//!
//! The layout is the app's own (built from [`crate::ui`]), not gpui-component's
//! `Settings`: that one fixes its sidebar items, search field, item labels and group
//! spacing to its own scale, which the app's control scale can't reach through its API.

use std::rc::Rc;

use gpui_kit::assets::IconName as AssetIcon;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, ThemeMode, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, AppContext as _, Context, Entity, FocusHandle, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Point, Render, ScrollHandle,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div,
};
use omc_proto::apps::Confidence;
use omc_proto::jobs::DeleteMethod;
use omc_proto::settings::CleanSettings;

use crate::clean_settings::{self, CleanPrefs};
use crate::format;
use crate::i18n::Language;
use crate::settings::UiSettings;
use crate::theme::{
    self, Accent, Appearance, CornerStyle, MONO_FONT_SIZES, MonoFont, ScrollbarVisibility,
    UI_FONT_SIZES, UiFont,
};
use crate::title_bar::AppTitleBar;
use crate::tokens::{card, chrome, control, layout, page, space, text};
use crate::ui;

/// Root view of the settings window.
pub(crate) struct SettingsView {
    focus_handle: FocusHandle,
    search: Entity<InputState>,
    /// Index of the selected page in [`all_pages`].
    selected: usize,
    scroll: ScrollHandle,
    exclude: Entity<PathEditor>,
    file_roots: Entity<PathEditor>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    /// Creates the view and focuses it, so window-level key bindings resolve.
    pub(crate) fn new(window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        clean_settings::attach(cx);
        let exclude = cx.new(|cx| PathEditor::new(PathList::Exclude, window, cx));
        let file_roots = cx.new(|cx| PathEditor::new(PathList::FileRoots, window, cx));
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr("settings.window.search")));
        let subscriptions = vec![
            cx.observe_window_appearance(window, |_, window, cx| {
                theme::system_appearance_changed(window.appearance(), cx);
            }),
            // Loaded state and values of the daemon's settings.
            cx.observe_global::<CleanPrefs>(|_, cx| cx.notify()),
            cx.subscribe(&search, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.scroll.set_offset(Point::default());
                    cx.notify();
                }
            }),
        ];
        Self {
            focus_handle,
            search,
            selected: 0,
            scroll: ScrollHandle::new(),
            exclude,
            file_roots,
            _subscriptions: subscriptions,
        }
    }

    fn select_page(&mut self, ix: usize, cx: &mut Context<'_, Self>) {
        if self.selected != ix {
            self.selected = ix;
            self.scroll.set_offset(Point::default());
            cx.notify();
        }
    }

    fn render_nav(
        &self,
        pages: &[(usize, Page)],
        active: Option<usize>,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (sidebar, foreground, muted) = (
            theme.sidebar,
            theme.sidebar_foreground,
            theme.muted_foreground,
        );
        let mut items = Vec::with_capacity(pages.len().saturating_add(1));
        let mut labelled = false;
        for (ix, page) in pages {
            let ix = *ix;
            if page.section == Section::Clean && !labelled {
                labelled = true;
                items.push(
                    div()
                        .px(space::MD)
                        .pt(space::MD)
                        .pb(space::XS)
                        .text_size(text::CAPTION)
                        .line_height(text::CAPTION_LINE_HEIGHT)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(muted)
                        .child(tr("settings.window.nav_clean"))
                        .into_any_element(),
                );
            }
            items.push(
                ui::NavItem::new(
                    SharedString::from(format!("settings-nav-{ix}")),
                    page.icon.clone(),
                    page.title.clone(),
                )
                .selected(active == Some(ix))
                .on_click(cx.listener(move |this, _, _, cx| this.select_page(ix, cx)))
                .into_any_element(),
            );
        }
        v_flex()
            .flex_none()
            .h_full()
            .w(layout::SIDEBAR_WIDTH)
            .bg(sidebar)
            .text_color(foreground)
            .border_r_1()
            .border_color(theme::divider_color(cx))
            // Under the titlebar: traffic lights.
            .child(div().flex_none().h(chrome::TITLE_BAR_HEIGHT))
            .child(
                div()
                    .flex_none()
                    .px(space::MD)
                    .pb(space::MD)
                    .child(
                        ui::TextInput::new(&self.search)
                            .search()
                            .w_full()
                            .label(tr("settings.window.search")),
                    ),
            )
            .child(
                v_flex()
                    .id("settings-nav")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(space::MD)
                    .pb(space::MD)
                    .gap(space::XXS)
                    .children(items),
            )
            .into_any_element()
    }

    fn render_page(page: &Page, window: &mut Window, cx: &mut App) -> AnyElement {
        let resettable = page
            .items()
            .filter(|item| !item.disabled)
            .collect::<Vec<_>>();
        let dirty = resettable.iter().any(|item| item.control.is_dirty(cx));
        let reset = dirty.then(|| {
            let controls = resettable
                .iter()
                .map(|item| item.control.clone())
                .collect::<Vec<_>>();
            ui::Button::new("settings-reset", tr("settings.window.reset"))
                .icon(IconName::Undo2)
                .on_click(move |_, _, cx| {
                    for control in &controls {
                        control.reset(cx);
                    }
                })
        });
        let mut header = ui::PageHeader::new(page.icon.clone(), page.title.clone());
        if let Some(description) = page.description.clone() {
            header = header.description(description);
        }
        let groups = page.groups.iter().map(|group| {
            v_flex()
                .w_full()
                .gap(space::MD)
                .child(ui::SectionHeader::new(group.title.clone()))
                .child(
                    ui::Card::new()
                        .flush()
                        .children(group.items.iter().map(|item| render_item(item, window, cx))),
                )
        });
        v_flex()
            .w_full()
            .gap(layout::SECTION_GAP)
            .child(header.children(reset))
            .children(groups)
            .into_any_element()
    }
}

impl std::fmt::Debug for SettingsView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsView")
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let query = self.search.read(cx).value().trim().to_lowercase();
        let pages = all_pages(&self.exclude, &self.file_roots, cx)
            .into_iter()
            .enumerate()
            .filter_map(|(ix, page)| page.filtered(&query).map(|page| (ix, page)))
            .collect::<Vec<_>>();
        let active = pages
            .iter()
            .find(|(ix, _)| *ix == self.selected)
            .or_else(|| pages.first());
        let active_ix = active.map(|(ix, _)| *ix);
        let body = match active {
            Some((_, page)) => Self::render_page(page, window, cx),
            None => ui::EmptyState::new(IconName::Search, tr("settings.window.no_results.title"))
                .description(tr("settings.window.no_results.description"))
                .into_any_element(),
        };
        let nav = self.render_nav(&pages, active_ix, cx);
        let background = cx.theme().background;

        div()
            .id("settings-window")
            .key_context("Settings")
            .track_focus(&self.focus_handle)
            .relative()
            .size_full()
            .child(
                h_flex().size_full().child(nav).child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .bg(background)
                        // Under the titlebar.
                        .child(div().flex_none().h(chrome::TITLE_BAR_HEIGHT))
                        .child(
                            div()
                                .id("settings-content")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .track_scroll(&self.scroll)
                                .child(
                                    div()
                                        .w_full()
                                        .max_w(page::CONTENT_MAX_WIDTH)
                                        .mx_auto()
                                        .px(layout::PAGE_PAD_X)
                                        .pt(layout::PAGE_PAD_TOP)
                                        .pb(layout::SECTION_GAP)
                                        .child(body),
                                ),
                        ),
                ),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .child(AppTitleBar::new()),
            )
    }
}

/// The title (13/500) and description (12 muted) of a setting; dimmed while disabled.
fn setting_label(
    title: SharedString,
    description: Option<SharedString>,
    disabled: bool,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    v_flex()
        .flex_1()
        .min_w_0()
        .gap(space::XXS)
        .when(disabled, |this| this.opacity(control::DISABLED_OPACITY))
        .child(
            div()
                .text_size(text::BODY)
                .line_height(text::BODY_LINE_HEIGHT)
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.foreground)
                .child(title),
        )
        .when_some(description, |this, description| {
            this.child(
                div()
                    .text_size(text::SMALL)
                    .line_height(text::SMALL_LINE_HEIGHT)
                    .text_color(theme.muted_foreground)
                    .child(description),
            )
        })
}

/// One row of a group card: label on the left, the control right-aligned.
fn render_item(item: &Item, window: &mut Window, cx: &mut App) -> AnyElement {
    let row = div().w_full().px(card::PADDING).py(space::LG);
    if let Control::Folders { editor, .. } = &item.control {
        return row.child(editor.clone()).into_any_element();
    }
    row.flex()
        .items_center()
        .gap(space::XL)
        .child(setting_label(
            item.title.clone(),
            item.description.clone(),
            item.disabled,
            cx,
        ))
        .child(div().flex_none().child(render_control(item, window, cx)))
        .into_any_element()
}

fn render_control(item: &Item, _window: &mut Window, cx: &mut App) -> AnyElement {
    match &item.control {
        Control::Switch { get, set, .. } => {
            let set = set.clone();
            ui::Switch::new(item.id.clone())
                .checked(get(cx))
                .disabled(item.disabled)
                .on_click(move |on, _, cx| set(*on, cx))
                .into_any_element()
        }
        Control::Choice {
            options, get, set, ..
        } => {
            let set = set.clone();
            ui::Select::new(item.id.clone(), options.iter().cloned())
                .selected(get(cx))
                .disabled(item.disabled)
                .anchor(Anchor::TopRight)
                .on_change(move |value, _, cx| set(value.clone(), cx))
                .into_any_element()
        }
        Control::Folders { editor, .. } => editor.clone().into_any_element(),
    }
}

// ---- The settings model ------------------------------------------------------------------

type Getter<T> = Rc<dyn Fn(&App) -> T>;
type Setter<T> = Rc<dyn Fn(T, &mut App)>;

/// The control of a setting, with how to read, write and reset it.
#[derive(Clone)]
enum Control {
    /// An on/off switch.
    Switch {
        get: Getter<bool>,
        set: Setter<bool>,
        default: bool,
    },
    /// A dropdown over `(key, label)` options, stored by key.
    Choice {
        options: Vec<(SharedString, SharedString)>,
        get: Getter<SharedString>,
        set: Setter<SharedString>,
        default: SharedString,
    },
    /// An editable folder list (renders its own label); resets to empty.
    Folders {
        editor: Entity<PathEditor>,
        list: PathList,
    },
}

impl Control {
    fn switch(
        get: impl Fn(&App) -> bool + 'static,
        set: impl Fn(bool, &mut App) + 'static,
        default: bool,
    ) -> Self {
        Self::Switch {
            get: Rc::new(get),
            set: Rc::new(set),
            default,
        }
    }

    fn choice(
        options: Vec<(SharedString, SharedString)>,
        get: impl Fn(&App) -> SharedString + 'static,
        set: impl Fn(SharedString, &mut App) + 'static,
        default: impl Into<SharedString>,
    ) -> Self {
        Self::Choice {
            options,
            get: Rc::new(get),
            set: Rc::new(set),
            default: default.into(),
        }
    }

    /// Whether the value differs from its default (the page offers a reset).
    fn is_dirty(&self, cx: &App) -> bool {
        match self {
            Self::Switch { get, default, .. } => get(cx) != *default,
            Self::Choice { get, default, .. } => get(cx) != *default,
            Self::Folders { list, .. } => !list.get(CleanPrefs::settings(cx)).is_empty(),
        }
    }

    fn reset(&self, cx: &mut App) {
        match self {
            Self::Switch { set, default, .. } => set(*default, cx),
            Self::Choice { set, default, .. } => set(default.clone(), cx),
            Self::Folders { list, .. } => {
                let list = *list;
                CleanPrefs::update(cx, |s| list.get_mut(s).clear());
            }
        }
    }
}

/// One setting: title, optional description and its control.
#[derive(Clone)]
struct Item {
    /// Element id of the control (stable per position in [`all_pages`]).
    id: SharedString,
    title: SharedString,
    description: Option<SharedString>,
    control: Control,
    disabled: bool,
}

impl Item {
    fn new(title: impl Into<SharedString>, control: Control) -> Self {
        Self {
            id: SharedString::default(),
            title: title.into(),
            description: None,
            control,
            disabled: false,
        }
    }

    fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Whether the title or description contains `query` (already lower-cased).
    fn matches(&self, query: &str) -> bool {
        query.is_empty()
            || self.title.to_lowercase().contains(query)
            || self
                .description
                .as_ref()
                .is_some_and(|d| d.to_lowercase().contains(query))
    }
}

/// A titled card of settings.
struct Group {
    title: SharedString,
    items: Vec<Item>,
}

impl Group {
    fn new(title: impl Into<SharedString>, items: Vec<Item>) -> Self {
        Self {
            title: title.into(),
            items,
        }
    }
}

/// Navigation section of a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Section {
    /// The app's own preferences.
    App,
    /// The daemon's scanning and cleaning settings.
    Clean,
}

/// One settings page.
struct Page {
    title: SharedString,
    icon: Icon,
    section: Section,
    description: Option<SharedString>,
    groups: Vec<Group>,
}

impl Page {
    fn new(title: impl Into<SharedString>, icon: impl Into<Icon>, section: Section) -> Self {
        Self {
            title: title.into(),
            icon: icon.into(),
            section,
            description: None,
            groups: Vec::new(),
        }
    }

    fn groups(mut self, groups: Vec<Group>) -> Self {
        self.groups = groups;
        self
    }

    fn items(&self) -> impl Iterator<Item = &Item> {
        self.groups.iter().flat_map(|group| group.items.iter())
    }

    /// The page with only the settings matching `query` (lower-cased); `None` when
    /// nothing matches. Empty groups are dropped.
    fn filtered(mut self, query: &str) -> Option<Self> {
        for group in &mut self.groups {
            group.items.retain(|item| item.matches(query));
        }
        self.groups.retain(|group| !group.items.is_empty());
        (!self.groups.is_empty()).then_some(self)
    }
}

fn tr(key: &str) -> SharedString {
    rust_i18n::t!(key).to_string().into()
}

/// Every page, with a stable element id per control.
fn all_pages(exclude: &Entity<PathEditor>, file_roots: &Entity<PathEditor>, cx: &App) -> Vec<Page> {
    let mut pages = vec![appearance_page(cx), language_page()];
    pages.extend(clean_pages(exclude, file_roots, cx));
    for (p, page) in pages.iter_mut().enumerate() {
        for (g, group) in page.groups.iter_mut().enumerate() {
            for (i, item) in group.items.iter_mut().enumerate() {
                item.id = format!("setting-{p}-{g}-{i}").into();
            }
        }
    }
    pages
}

/// A dropdown over a closed set of choices, stored in [`UiSettings`] and identified by a
/// stable string key.
fn choice<T>(
    all: &[T],
    key: fn(T) -> &'static str,
    label: fn(T) -> SharedString,
    get: fn(&UiSettings) -> T,
    set: fn(&mut UiSettings, T),
) -> Control
where
    T: Copy + PartialEq + 'static,
{
    let options = all
        .iter()
        .map(|&value| (SharedString::from(key(value)), label(value)))
        .collect();
    let values = all.to_vec();
    Control::choice(
        options,
        move |cx: &App| key(get(UiSettings::get(cx))).into(),
        move |picked: SharedString, cx: &mut App| {
            if let Some(&value) = values.iter().find(|&&v| key(v) == picked.as_ref()) {
                UiSettings::update(cx, |s| set(s, value));
            }
        },
        key(get(&UiSettings::default())),
    )
}

/// A dropdown of font sizes in px.
fn size_choice(
    sizes: &'static [u8],
    get: fn(&UiSettings) -> u8,
    set: fn(&mut UiSettings, u8),
) -> Control {
    let options = sizes
        .iter()
        .map(|size| {
            let value = SharedString::from(size.to_string());
            (value.clone(), value)
        })
        .collect();
    Control::choice(
        options,
        move |cx: &App| get(UiSettings::get(cx)).to_string().into(),
        move |picked: SharedString, cx: &mut App| match picked.parse::<u8>() {
            Ok(size) if sizes.contains(&size) => UiSettings::update(cx, |s| set(s, size)),
            _ => tracing::warn!(%picked, "ignoring unknown font size"),
        },
        get(&UiSettings::default()).to_string(),
    )
}

/// A dropdown of the registered presets for `mode`.
fn preset_choice(
    mode: ThemeMode,
    get: fn(&UiSettings) -> SharedString,
    set: fn(&mut UiSettings, SharedString),
    cx: &App,
) -> Control {
    let options = theme::preset_names(mode, cx)
        .into_iter()
        .map(|name| (name.clone(), name))
        .collect();
    Control::choice(
        options,
        move |cx: &App| get(UiSettings::get(cx)),
        move |picked: SharedString, cx: &mut App| UiSettings::update(cx, |s| set(s, picked)),
        get(&UiSettings::default()),
    )
}

/// A switch stored in [`UiSettings`].
fn toggle(get: fn(&UiSettings) -> bool, set: fn(&mut UiSettings, bool)) -> Control {
    Control::switch(
        move |cx: &App| get(UiSettings::get(cx)),
        move |on: bool, cx: &mut App| UiSettings::update(cx, |s| set(s, on)),
        get(&UiSettings::default()),
    )
}

fn theme_group(cx: &App) -> Group {
    Group::new(
        tr("settings.group.theme"),
        vec![
            Item::new(
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
            Item::new(
                tr("settings.light_theme.title"),
                preset_choice(
                    ThemeMode::Light,
                    |s| s.theme.light_theme.clone(),
                    |s, v| s.theme.light_theme = v,
                    cx,
                ),
            )
            .description(tr("settings.light_theme.description")),
            Item::new(
                tr("settings.dark_theme.title"),
                preset_choice(
                    ThemeMode::Dark,
                    |s| s.theme.dark_theme.clone(),
                    |s, v| s.theme.dark_theme = v,
                    cx,
                ),
            )
            .description(tr("settings.dark_theme.description")),
            Item::new(
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
        ],
    )
}

fn shape_group() -> Group {
    Group::new(
        tr("settings.group.shape"),
        vec![
            Item::new(
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
            Item::new(
                tr("settings.shadows.title"),
                toggle(|s| s.theme.style.shadows, |s, v| s.theme.style.shadows = v),
            )
            .description(tr("settings.shadows.description")),
            Item::new(
                tr("settings.focus_ring.title"),
                toggle(
                    |s| s.theme.style.focus_ring,
                    |s, v| s.theme.style.focus_ring = v,
                ),
            )
            .description(tr("settings.focus_ring.description")),
            Item::new(
                tr("settings.dividers.title"),
                toggle(
                    |s| s.theme.style.dividers,
                    |s, v| s.theme.style.dividers = v,
                ),
            )
            .description(tr("settings.dividers.description")),
        ],
    )
}

fn typography_group() -> Group {
    Group::new(
        tr("settings.group.typography"),
        vec![
            Item::new(
                tr("settings.ui_font.title"),
                choice(
                    &UiFont::ALL,
                    UiFont::family,
                    UiFont::label,
                    |s| s.theme.style.ui_font,
                    |s, v| s.theme.style.ui_font = v,
                ),
            ),
            Item::new(
                tr("settings.ui_font_size.title"),
                size_choice(
                    &UI_FONT_SIZES,
                    |s| s.theme.style.ui_font_size,
                    |s, v| s.theme.style.ui_font_size = v,
                ),
            )
            .description(tr("settings.ui_font_size.description")),
            Item::new(
                tr("settings.mono_font.title"),
                choice(
                    &MonoFont::ALL,
                    MonoFont::family,
                    MonoFont::label,
                    |s| s.theme.style.mono_font,
                    |s, v| s.theme.style.mono_font = v,
                ),
            ),
            Item::new(
                tr("settings.mono_font_size.title"),
                size_choice(
                    &MONO_FONT_SIZES,
                    |s| s.theme.style.mono_font_size,
                    |s, v| s.theme.style.mono_font_size = v,
                ),
            ),
        ],
    )
}

fn scrolling_group() -> Group {
    Group::new(
        tr("settings.group.scrolling"),
        vec![Item::new(
            tr("settings.scrollbar.title"),
            choice(
                &ScrollbarVisibility::ALL,
                ScrollbarVisibility::key,
                ScrollbarVisibility::label,
                |s| s.theme.style.scrollbar,
                |s, v| s.theme.style.scrollbar = v,
            ),
        )],
    )
}

fn appearance_page(cx: &App) -> Page {
    Page::new(
        tr("settings.page.appearance"),
        IconName::Palette,
        Section::App,
    )
    .groups(vec![
        theme_group(cx),
        shape_group(),
        typography_group(),
        scrolling_group(),
    ])
}

fn language_page() -> Page {
    Page::new(tr("settings.page.language"), IconName::Globe, Section::App).groups(vec![Group::new(
        tr("settings.group.language"),
        vec![
            Item::new(
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
        ],
    )])
}

// ---- Scanning & cleaning (the daemon's `CleanSettings`) --------------------------------

const KIB: u64 = 1 << 10;
const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

fn clean_tr(key: &str) -> SharedString {
    tr(&format!("settings.clean.{key}"))
}

/// A titled, described item of a clean page; disabled until the settings are loaded.
fn clean_item(key: &str, control: Control, loaded: bool) -> Item {
    Item::new(clean_tr(&format!("{key}.title")), control)
        .description(clean_tr(&format!("{key}.description")))
        .disabled(!loaded)
}

/// A switch stored in [`CleanSettings`].
fn clean_toggle(get: fn(&CleanSettings) -> bool, set: fn(&mut CleanSettings, bool)) -> Control {
    Control::switch(
        move |cx: &App| get(CleanPrefs::settings(cx)),
        move |on: bool, cx: &mut App| CleanPrefs::update(cx, |s| set(s, on)),
        get(&CleanSettings::default()),
    )
}

/// A dropdown over `choices` stored in [`CleanSettings`]. A stored value outside the
/// choices (edited by hand) is offered too, so the dropdown always shows the truth.
fn clean_choice<T>(
    choices: Vec<T>,
    key: fn(T) -> String,
    label: fn(T) -> SharedString,
    get: fn(&CleanSettings) -> T,
    set: fn(&mut CleanSettings, T),
    cx: &App,
) -> Control
where
    T: Copy + PartialEq + 'static,
{
    let mut values = choices;
    let current = get(CleanPrefs::settings(cx));
    if !values.contains(&current) {
        values.push(current);
    }
    let options = values
        .iter()
        .map(|&v| (SharedString::from(key(v)), label(v)))
        .collect::<Vec<_>>();
    Control::choice(
        options,
        move |cx: &App| SharedString::from(key(get(CleanPrefs::settings(cx)))),
        move |picked: SharedString, cx: &mut App| {
            if let Some(&value) = values.iter().find(|&&v| key(v) == picked.as_ref()) {
                CleanPrefs::update(cx, |s| set(s, value));
            }
        },
        key(get(&CleanSettings::default())),
    )
}

/// `100 MB`-style label of a power-of-two size (the thresholds are binary multiples).
fn size_label(bytes: u64) -> SharedString {
    if bytes == 0 {
        return clean_tr("value.any_size");
    }
    let (unit, name) = if bytes.is_multiple_of(GIB) {
        (GIB, "GB")
    } else if bytes.is_multiple_of(MIB) {
        (MIB, "MB")
    } else if bytes.is_multiple_of(KIB) {
        (KIB, "KB")
    } else {
        return format::bytes(bytes);
    };
    format!("{} {name}", bytes.checked_div(unit).unwrap_or(0)).into()
}

fn hours_label(hours: u32) -> SharedString {
    match hours {
        0 => clean_tr("value.no_limit"),
        1 => clean_tr("value.one_hour"),
        n => rust_i18n::t!("settings.clean.value.hours", n = n)
            .to_string()
            .into(),
    }
}

fn days_label(days: u32) -> SharedString {
    rust_i18n::t!("settings.clean.value.days", n = days)
        .to_string()
        .into()
}

fn threads_label(threads: u16) -> SharedString {
    match threads {
        0 => clean_tr("scan_threads.auto"),
        1 => clean_tr("value.one_thread"),
        n => rust_i18n::t!("settings.clean.value.threads", n = n)
            .to_string()
            .into(),
    }
}

fn delete_key(method: DeleteMethod) -> String {
    match method {
        DeleteMethod::Permanent => "permanent",
        DeleteMethod::Trash => "trash",
    }
    .to_owned()
}

fn delete_label(method: DeleteMethod) -> SharedString {
    clean_tr(&format!("delete.{}", delete_key(method)))
}

/// Stable key of a confidence level (also the `apps.confidence.*` label key).
pub(crate) fn confidence_key(level: Confidence) -> &'static str {
    match level {
        Confidence::High => "high",
        Confidence::Medium => "medium",
        Confidence::Low => "low",
    }
}

fn confidence_label(level: Confidence) -> SharedString {
    tr(&format!("apps.confidence.{}", confidence_key(level)))
}

/// Logical CPUs, for the thread choices.
fn logical_cpus() -> u16 {
    std::thread::available_parallelism().map_or(1, |n| u16::try_from(n.get()).unwrap_or(u16::MAX))
}

fn clean_page(key: &str, icon: impl Into<Icon>, loaded: bool) -> Page {
    let mut page = Page::new(clean_tr(&format!("page.{key}")), icon, Section::Clean);
    if !loaded {
        page.description = Some(clean_tr("loading"));
    }
    page
}

fn clean_pages(
    exclude: &Entity<PathEditor>,
    file_roots: &Entity<PathEditor>,
    cx: &App,
) -> Vec<Page> {
    let loaded = CleanPrefs::is_loaded(cx);
    vec![
        scanning_page(exclude, file_roots, loaded, cx),
        cleaning_page(loaded, cx),
        browsers_page(loaded),
        developer_page(loaded, cx),
        large_old_page(loaded, cx),
        duplicates_page(loaded, cx),
        uninstaller_page(loaded, cx),
    ]
}

fn path_list_item(editor: &Entity<PathEditor>, list: PathList, loaded: bool) -> Item {
    Item::new(
        clean_tr(&format!("{}.title", list.key())),
        Control::Folders {
            editor: editor.clone(),
            list,
        },
    )
    .description(clean_tr(&format!("{}.description", list.key())))
    .disabled(!loaded)
}

fn scanning_page(
    exclude: &Entity<PathEditor>,
    file_roots: &Entity<PathEditor>,
    loaded: bool,
    cx: &App,
) -> Page {
    let threads = (0..=logical_cpus()).collect();
    clean_page("scanning", IconName::Search, loaded).groups(vec![
        Group::new(
            tr("scans.settings.group"),
            vec![
                Item::new(
                    tr("scans.auto_open.title"),
                    Control::switch(
                        CleanPrefs::auto_scan,
                        |on: bool, cx: &mut App| CleanPrefs::set_auto_scan(cx, on),
                        true,
                    ),
                )
                .description(tr("scans.auto_open.description"))
                .disabled(!loaded),
            ],
        ),
        Group::new(
            clean_tr("group.scope"),
            vec![
                path_list_item(exclude, PathList::Exclude, loaded),
                path_list_item(file_roots, PathList::FileRoots, loaded),
                clean_item(
                    "one_file_system",
                    clean_toggle(|s| s.one_file_system, |s, v| s.one_file_system = v),
                    loaded,
                ),
                clean_item(
                    "skip_hidden",
                    clean_toggle(|s| s.skip_hidden, |s, v| s.skip_hidden = v),
                    loaded,
                ),
            ],
        ),
        Group::new(
            clean_tr("group.performance"),
            vec![clean_item(
                "scan_threads",
                clean_choice(
                    threads,
                    |v| v.to_string(),
                    threads_label,
                    |s| s.scan_threads,
                    |s, v| s.scan_threads = v,
                    cx,
                ),
                loaded,
            )],
        ),
    ])
}

fn cleaning_page(loaded: bool, cx: &App) -> Page {
    let methods = vec![DeleteMethod::Trash, DeleteMethod::Permanent];
    clean_page("cleaning", Icon::new(AssetIcon::Broom), loaded).groups(vec![
        Group::new(
            clean_tr("group.safety"),
            vec![
                clean_item(
                    "junk_min_age",
                    clean_choice(
                        vec![0, 1, 6, 24, 72, 168],
                        |v| v.to_string(),
                        hours_label,
                        |s| s.junk_min_age_hours,
                        |s, v| s.junk_min_age_hours = v,
                        cx,
                    ),
                    loaded,
                ),
                clean_item(
                    "skip_running_apps",
                    clean_toggle(|s| s.skip_running_apps, |s, v| s.skip_running_apps = v),
                    loaded,
                ),
                clean_item(
                    "include_system",
                    clean_toggle(|s| s.include_system, |s, v| s.include_system = v),
                    loaded,
                ),
            ],
        ),
        Group::new(
            clean_tr("group.removal"),
            vec![
                clean_item(
                    "junk_delete",
                    clean_choice(
                        methods.clone(),
                        delete_key,
                        delete_label,
                        |s| s.junk_delete,
                        |s, v| s.junk_delete = v,
                        cx,
                    ),
                    loaded,
                ),
                clean_item(
                    "files_delete",
                    clean_choice(
                        methods,
                        delete_key,
                        delete_label,
                        |s| s.files_delete,
                        |s, v| s.files_delete = v,
                        cx,
                    ),
                    loaded,
                ),
                clean_item(
                    "elevate",
                    clean_toggle(|s| s.elevate, |s, v| s.elevate = v),
                    loaded,
                ),
            ],
        ),
    ])
}

fn browsers_page(loaded: bool) -> Page {
    clean_page("browsers", IconName::Globe, loaded).groups(vec![Group::new(
        clean_tr("group.privacy"),
        vec![
            clean_item(
                "browser_cookies",
                clean_toggle(|s| s.browser_cookies, |s, v| s.browser_cookies = v),
                loaded,
            ),
            clean_item(
                "browser_history",
                clean_toggle(|s| s.browser_history, |s, v| s.browser_history = v),
                loaded,
            ),
            clean_item(
                "browser_site_data",
                clean_toggle(|s| s.browser_site_data, |s, v| s.browser_site_data = v),
                loaded,
            ),
            clean_item(
                "browser_sessions",
                clean_toggle(|s| s.browser_sessions, |s, v| s.browser_sessions = v),
                loaded,
            ),
        ],
    )])
}

fn developer_page(loaded: bool, cx: &App) -> Page {
    clean_page("developer", Icon::new(AssetIcon::CodeXml), loaded).groups(vec![Group::new(
        clean_tr("group.projects"),
        vec![
            clean_item(
                "dev_min_age",
                clean_choice(
                    vec![0, 7, 14, 30, 90, 180],
                    |v| v.to_string(),
                    |v| {
                        if v == 0 {
                            clean_tr("value.no_limit")
                        } else {
                            days_label(v)
                        }
                    },
                    |s| s.dev_project_min_age_days,
                    |s, v| s.dev_project_min_age_days = v,
                    cx,
                ),
                loaded,
            ),
            clean_item(
                "dev_max_depth",
                clean_choice(
                    (2..=16).collect(),
                    |v| v.to_string(),
                    |v| {
                        rust_i18n::t!("settings.clean.value.levels", n = v)
                            .to_string()
                            .into()
                    },
                    |s| s.dev_project_max_depth,
                    |s, v| s.dev_project_max_depth = v,
                    cx,
                ),
                loaded,
            ),
        ],
    )])
}

fn large_old_page(loaded: bool, cx: &App) -> Page {
    let large = vec![
        10 * MIB,
        50 * MIB,
        100 * MIB,
        250 * MIB,
        500 * MIB,
        GIB,
        2 * GIB,
        5 * GIB,
        10 * GIB,
    ];
    clean_page("large_old", Icon::new(AssetIcon::FileClock), loaded).groups(vec![
        Group::new(
            clean_tr("group.large"),
            vec![clean_item(
                "large_min",
                clean_choice(
                    large,
                    |v| v.to_string(),
                    size_label,
                    |s| s.large_min_bytes,
                    |s, v| s.large_min_bytes = v,
                    cx,
                ),
                loaded,
            )],
        ),
        Group::new(
            clean_tr("group.old"),
            vec![
                clean_item(
                    "old_days",
                    clean_choice(
                        vec![0, 90, 180, 365, 730],
                        |v| v.to_string(),
                        |v| {
                            if v == 0 {
                                clean_tr("value.off")
                            } else {
                                days_label(v)
                            }
                        },
                        |s| s.old_days,
                        |s, v| s.old_days = v,
                        cx,
                    ),
                    loaded,
                ),
                clean_item(
                    "old_min",
                    clean_choice(
                        vec![0, MIB, 10 * MIB, 50 * MIB, 100 * MIB, GIB],
                        |v| v.to_string(),
                        size_label,
                        |s| s.old_min_bytes,
                        |s, v| s.old_min_bytes = v,
                        cx,
                    ),
                    loaded,
                ),
            ],
        ),
    ])
}

fn duplicates_page(loaded: bool, cx: &App) -> Page {
    clean_page("duplicates", IconName::Copy, loaded).groups(vec![Group::new(
        clean_tr("group.duplicates"),
        vec![clean_item(
            "dup_min",
            clean_choice(
                vec![0, KIB, 100 * KIB, MIB, 10 * MIB, 100 * MIB],
                |v| v.to_string(),
                size_label,
                |s| s.dup_min_bytes,
                |s, v| s.dup_min_bytes = v,
                cx,
            ),
            loaded,
        )],
    )])
}

fn uninstaller_page(loaded: bool, cx: &App) -> Page {
    let mut groups = vec![
        Group::new(
            clean_tr("group.apps"),
            vec![
                clean_item(
                    "show_system_apps",
                    clean_toggle(|s| s.show_system_apps, |s, v| s.show_system_apps = v),
                    loaded,
                ),
                clean_item(
                    "leftover_confidence",
                    clean_choice(
                        vec![Confidence::High, Confidence::Medium, Confidence::Low],
                        |v| confidence_key(v).to_owned(),
                        confidence_label,
                        |s| s.leftover_confidence,
                        |s, v| s.leftover_confidence = v,
                        cx,
                    ),
                    loaded,
                ),
            ],
        ),
        Group::new(
            clean_tr("group.uninstall"),
            vec![
                clean_item(
                    "run_vendor_uninstaller",
                    clean_toggle(
                        |s| s.run_vendor_uninstaller,
                        |s, v| s.run_vendor_uninstaller = v,
                    ),
                    loaded,
                ),
                clean_item(
                    "quit_running_apps",
                    clean_toggle(|s| s.quit_running_apps, |s, v| s.quit_running_apps = v),
                    loaded,
                ),
            ],
        ),
    ];
    if cfg!(windows) {
        groups.push(Group::new(
            clean_tr("group.windows"),
            vec![
                clean_item(
                    "restore_point",
                    clean_toggle(|s| s.restore_point, |s, v| s.restore_point = v),
                    loaded,
                ),
                clean_item(
                    "backup_registry",
                    clean_toggle(|s| s.backup_registry, |s, v| s.backup_registry = v),
                    loaded,
                ),
            ],
        ));
    }
    clean_page("uninstaller", Icon::new(AssetIcon::PackageX), loaded).groups(groups)
}

/// The two editable folder lists of [`CleanSettings`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PathList {
    Exclude,
    FileRoots,
}

impl PathList {
    const fn key(self) -> &'static str {
        match self {
            Self::Exclude => "exclude",
            Self::FileRoots => "file_roots",
        }
    }

    fn get(self, s: &CleanSettings) -> &Vec<String> {
        match self {
            Self::Exclude => &s.exclude,
            Self::FileRoots => &s.file_roots,
        }
    }

    fn get_mut(self, s: &mut CleanSettings) -> &mut Vec<String> {
        match self {
            Self::Exclude => &mut s.exclude,
            Self::FileRoots => &mut s.file_roots,
        }
    }
}

/// Checks a typed folder: absolute, or `~` / `~/…` (the daemon expands it); not listed
/// yet. `Err` carries the `settings.clean.list.*` message key.
fn validate_path(raw: &str, existing: &[String]) -> Result<String, &'static str> {
    let path = raw.trim();
    let home_relative =
        path == "~" || path.starts_with("~/") || (cfg!(windows) && path.starts_with("~\\"));
    if path.is_empty() || !(home_relative || std::path::Path::new(path).is_absolute()) {
        return Err("invalid");
    }
    let trimmed = path.trim_end_matches(['/', '\\']);
    let path = if trimmed.is_empty() || trimmed.ends_with(':') {
        path
    } else {
        trimmed
    };
    if existing.iter().any(|p| p == path) {
        return Err("duplicate");
    }
    Ok(path.to_owned())
}

/// Editor of one folder list: its label, the entries (32 px rows with a remove button),
/// and a field to add one.
struct PathEditor {
    list: PathList,
    input: Entity<InputState>,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for PathEditor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PathEditor")
            .field("list", &self.list)
            .finish_non_exhaustive()
    }
}

impl PathEditor {
    fn new(list: PathList, window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let placeholder = clean_tr(&format!("{}.placeholder", list.key()));
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let subscriptions =
            vec![
                cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                    match event {
                        InputEvent::PressEnter { .. } => this.add(window, cx),
                        InputEvent::Change if this.error.is_some() => {
                            this.error = None;
                            cx.notify();
                        }
                        _ => {}
                    }
                }),
                cx.observe_global::<CleanPrefs>(|_, cx| cx.notify()),
            ];
        Self {
            list,
            input,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    fn add(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if !CleanPrefs::is_loaded(cx) {
            return;
        }
        let raw = self.input.read(cx).value();
        let list = self.list;
        match validate_path(&raw, list.get(CleanPrefs::settings(cx))) {
            Ok(path) => {
                CleanPrefs::update(cx, |s| list.get_mut(s).push(path));
                self.error = None;
                self.input
                    .update(cx, |input, cx| input.set_value("", window, cx));
            }
            Err(key) => self.error = Some(clean_tr(&format!("list.{key}"))),
        }
        cx.notify();
    }
}

impl Render for PathEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let loaded = CleanPrefs::is_loaded(cx);
        let list = self.list;
        let key = list.key();
        let title = clean_tr(&format!("{key}.title"));
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let rows = list
            .get(CleanPrefs::settings(cx))
            .iter()
            .enumerate()
            .map(|(ix, path)| folder_row(list, ix, path, loaded, cx))
            .collect::<Vec<_>>();
        let empty = rows.is_empty();
        v_flex()
            .w_full()
            .gap(space::MD)
            .child(setting_label(
                title.clone(),
                Some(clean_tr(&format!("{key}.description"))),
                !loaded,
                cx,
            ))
            .when(empty, |this| {
                this.child(
                    div()
                        .text_size(text::SMALL)
                        .line_height(text::SMALL_LINE_HEIGHT)
                        .text_color(muted)
                        .child(clean_tr(&format!("{key}.empty"))),
                )
            })
            .when(!empty, |this| this.child(v_flex().w_full().children(rows)))
            .child(
                h_flex()
                    .w_full()
                    .gap(space::MD)
                    .child(
                        ui::TextInput::new(&self.input)
                            .flex_1()
                            .disabled(!loaded)
                            .label(title),
                    )
                    .child(
                        ui::Button::new(
                            SharedString::from(format!("{key}-add")),
                            clean_tr("list.add"),
                        )
                        .icon(IconName::Plus)
                        .disabled(!loaded)
                        .on_click(cx.listener(|this, _, window, cx| this.add(window, cx))),
                    ),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .text_size(text::SMALL)
                        .line_height(text::SMALL_LINE_HEIGHT)
                        .text_color(danger)
                        .child(error),
                )
            })
    }
}

/// One 32 px entry of a folder list: folder icon, `~` path and a ghost remove button. Not
/// clickable itself, so it has no hover fill (list rows follow `tokens::row`).
fn folder_row(list: PathList, ix: usize, path: &str, loaded: bool, cx: &App) -> impl IntoElement {
    let key = list.key();
    let path = path.to_owned();
    ui::ListRow::new(
        SharedString::from(format!("{key}-entry-{ix}")),
        format::tilde(&path),
    )
    .icon(ui::row_icon(None, IconName::Folder, cx))
    .trailing(
        ui::IconButton::new(
            SharedString::from(format!("{key}-remove-{ix}")),
            IconName::Close,
            clean_tr("list.remove"),
        )
        .small()
        .disabled(!loaded)
        .on_click(move |_, _, cx| {
            CleanPrefs::update(cx, |s| {
                list.get_mut(s).retain(|p| *p != path);
            });
        }),
    )
    .pr(space::XS)
}

#[cfg(test)]
mod tests {
    use gpui_kit::TestAppContext;

    use super::{Control, Group, Item, Page, Section, validate_path};

    #[test]
    fn typed_folders_must_be_absolute_or_home_relative_and_new() {
        let existing = vec!["~/Projects".to_owned()];
        assert_eq!(
            validate_path("  ~/Movies/ ", &existing),
            Ok("~/Movies".to_owned()),
            "trimmed"
        );
        assert_eq!(
            validate_path("~/Projects", &existing),
            Err("duplicate"),
            "no duplicates"
        );
        assert_eq!(
            validate_path("relative/dir", &existing),
            Err("invalid"),
            "relative"
        );
        assert_eq!(validate_path("", &existing), Err("invalid"), "empty");
        let root = if cfg!(windows) {
            "C:\\Data"
        } else {
            "/Volumes/Data"
        };
        assert_eq!(
            validate_path(root, &existing),
            Ok(root.to_owned()),
            "absolute"
        );
    }

    fn item(title: &str, description: &str) -> Item {
        Item::new(
            title.to_owned(),
            Control::switch(|_| false, |_, _| {}, false),
        )
        .description(description.to_owned())
    }

    #[test]
    fn search_keeps_matching_settings_and_drops_empty_groups() {
        let page = || {
            Page::new("Page", gpui_kit::component::IconName::Globe, Section::App).groups(vec![
                Group::new(
                    "A",
                    vec![item("Shadows", "Soft depth"), item("Corners", "Radius")],
                ),
                Group::new("B", vec![item("Language", "Interface language")]),
            ])
        };
        let all = page().filtered("").map(|p| p.items().count());
        assert_eq!(all, Some(3), "an empty query keeps everything");

        let by_description = page().filtered("depth");
        let titles = by_description
            .as_ref()
            .map(|p| p.items().map(|i| i.title.to_string()).collect::<Vec<_>>());
        assert_eq!(
            titles,
            Some(vec!["Shadows".to_owned()]),
            "description matches"
        );
        assert_eq!(
            by_description.map(|p| p.groups.len()),
            Some(1),
            "groups without a match are dropped"
        );
        assert!(
            page().filtered("nothing").is_none(),
            "no match hides the page"
        );
    }

    #[gpui_kit::test]
    fn settings_window_renders_clean_pages_without_a_daemon(cx: &mut TestAppContext) {
        let init = cx.update(crate::init);
        assert!(init.is_ok(), "init: {init:?}");
        cx.update(crate::window::open_settings_window);
        cx.run_until_parked();
        let windows = cx.update(|cx| cx.windows().len());
        assert_eq!(windows, 1, "the settings window opens and renders");
        assert!(
            !cx.update(|cx| crate::clean_settings::CleanPrefs::is_loaded(cx)),
            "without a daemon the clean pages stay in their loading state"
        );
        let Some(window) = cx.update(|cx| cx.windows().first().copied()) else {
            return;
        };
        // Render every page, including the clean pages' loading state and folder editors.
        for page in 0..9 {
            let selected = cx.update(|cx| {
                window
                    .update(cx, |root, _, cx| {
                        let view = root
                            .downcast::<gpui_kit::component::Root>()
                            .ok()?
                            .read(cx)
                            .view()
                            .clone()
                            .downcast::<super::SettingsView>()
                            .ok()?;
                        view.update(cx, |view, cx| view.select_page(page, cx));
                        Some(())
                    })
                    .ok()
                    .flatten()
            });
            assert!(selected.is_some(), "page {page} is selectable");
            cx.run_until_parked();
        }
    }
}
