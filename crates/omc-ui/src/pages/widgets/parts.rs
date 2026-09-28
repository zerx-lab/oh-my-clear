//! Stateless building blocks of the area pages: header, notices, job progress, size bars,
//! row cells, the clean confirmation and the clean report. Everything takes its colours
//! from the theme and its geometry from [`crate::tokens`].

use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, ElementId, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, Stateful, Styled as _, Window, div, relative,
};
use omc_proto::jobs::{CleanReport, DeleteMethod, Denied, FailReason, Failure, Phase, Progress};

use crate::clean_settings::CleanPrefs;
use crate::format;
use crate::nav::Category;
use crate::tokens::{card, chrome, control, layout, page, row, space, text};
use crate::ui;

/// A click handler as the page's `cx.listener(..)` produces it.
pub(crate) type OnClick = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// Localised text for `key`.
pub(crate) fn tr(key: &str) -> SharedString {
    rust_i18n::t!(key).to_string().into()
}

/// Page header: the area's icon tile and title, the page toolbar (`actions`) on the right
/// of the title line, and its one-sentence description below.
pub(crate) fn area_header(category: Category, actions: Vec<AnyElement>, _cx: &App) -> AnyElement {
    ui::PageHeader::new(category.icon(), category.title())
        .when_some(category.description(), ui::PageHeader::description)
        .children(actions)
        .into_any_element()
}

/// The scrolling column every page lays its content in: centred, at most 960 px wide,
/// 24 px side and 20 px top padding, 16 px between blocks.
pub(crate) fn page_column(id: &'static str) -> Stateful<gpui_kit::Div> {
    v_flex()
        .id(id)
        .flex_1()
        .min_h_0()
        .w_full()
        .max_w(page::CONTENT_MAX_WIDTH)
        .mx_auto()
        .px(layout::PAGE_PAD_X)
        .pt(layout::PAGE_PAD_TOP)
        .pb(layout::PAGE_PAD_X)
        .gap(layout::BLOCK_GAP)
}

/// Colour family of a notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone {
    /// Neutral information.
    Info,
    /// Something limits the result (permissions, connection).
    Warning,
    /// Something failed.
    Danger,
    /// Something succeeded.
    Success,
}

impl Tone {
    /// The component library's tone.
    const fn ui(self) -> ui::Tone {
        match self {
            Self::Info => ui::Tone::Accent,
            Self::Warning => ui::Tone::Warning,
            Self::Danger => ui::Tone::Danger,
            Self::Success => ui::Tone::Success,
        }
    }

    fn color(self, cx: &App) -> Hsla {
        self.ui().color(cx)
    }

    fn icon(self) -> IconName {
        match self {
            Self::Info => IconName::Info,
            Self::Warning => IconName::TriangleAlert,
            Self::Danger => IconName::CircleX,
            Self::Success => IconName::CircleCheck,
        }
    }
}

/// A banner on the card surface: tone icon, title, optional body, optional trailing
/// actions. The tone shows in the icon and a faint tint, not in a loud border.
pub(crate) fn notice(
    tone: Tone,
    title: SharedString,
    body: Option<SharedString>,
    actions: Vec<AnyElement>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let color = tone.color(cx);
    h_flex()
        .w_full()
        .items_start()
        .gap(space::MD)
        .px(card::PADDING)
        .py(space::LG)
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.border.alpha(theme.border.a * card::BORDER_ALPHA))
        .bg(theme.group_box)
        .child(
            div()
                .flex_none()
                .pt(space::XXS)
                .text_color(color)
                .child(Icon::new(tone.icon()).size(chrome::ICON)),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(space::XXS)
                .child(
                    div()
                        .text_size(text::BODY)
                        .line_height(text::BODY_LINE_HEIGHT)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child(title),
                )
                .when_some(body, |this, body| {
                    this.child(
                        div()
                            .text_size(text::SMALL)
                            .line_height(text::SMALL_LINE_HEIGHT)
                            .text_color(theme.muted_foreground)
                            .child(body),
                    )
                }),
        )
        .when(!actions.is_empty(), |this| {
            this.child(
                h_flex()
                    .flex_none()
                    .items_center()
                    .gap(space::MD)
                    .children(actions),
            )
        })
        .into_any_element()
}

/// Notice shown while the daemon is not reachable; `None` when connected.
pub(crate) fn connection_notice(connected: bool, cx: &App) -> Option<AnyElement> {
    (!connected).then(|| {
        notice(
            Tone::Warning,
            tr("scan.disconnected.title"),
            Some(tr("scan.disconnected.body")),
            Vec::new(),
            cx,
        )
    })
}

/// An error banner with a dismiss button.
pub(crate) fn error_notice(message: SharedString, on_dismiss: OnClick, cx: &App) -> AnyElement {
    notice(
        Tone::Danger,
        tr("scan.error"),
        Some(message),
        vec![
            ui::IconButton::new("dismiss-error", IconName::Close, tr("scan.dismiss"))
                .small()
                .on_click(on_dismiss)
                .into_any_element(),
        ],
        cx,
    )
}

/// Live state of a running job, as a card: phase and cancel on the header line, a 4 px
/// progress bar (determinate when the job counts its work), tabular counters and the
/// current path.
pub(crate) fn job_progress(
    id: &'static str,
    progress: &Progress,
    on_cancel: OnClick,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let determinate = progress.total > 0;
    let counters = if determinate {
        rust_i18n::t!(
            "scan.progress.counted",
            done = format::count(progress.done),
            total = format::count(progress.total),
            bytes = format::bytes(progress.bytes)
        )
    } else {
        rust_i18n::t!(
            "scan.progress.found",
            items = format::count(progress.items),
            bytes = format::bytes(progress.bytes)
        )
    };
    ui::Card::new()
        .child(
            v_flex()
                .w_full()
                .gap(space::MD)
                .child(
                    h_flex()
                        .w_full()
                        .gap(space::MD)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(text::SECTION)
                                .line_height(text::SECTION_LINE_HEIGHT)
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(phase_label(progress.phase)),
                        )
                        .child(
                            ui::Button::new(
                                ElementId::from(SharedString::from(format!("{id}-cancel"))),
                                tr("scan.cancel"),
                            )
                            .small()
                            .on_click(on_cancel),
                        ),
                )
                .child(
                    ui::ProgressBar::new(ElementId::from(SharedString::from(format!("{id}-bar"))))
                        .value(determinate.then(|| fraction(progress.done, progress.total))),
                )
                .child(
                    h_flex()
                        .w_full()
                        .gap(space::LG)
                        .child(
                            div()
                                .flex_none()
                                .text_size(text::SMALL)
                                .line_height(text::SMALL_LINE_HEIGHT)
                                .font_features(ui::tabular())
                                .text_color(theme.foreground)
                                .child(counters.to_string()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis_middle()
                                .text_size(text::CAPTION)
                                .line_height(text::SMALL_LINE_HEIGHT)
                                .text_color(theme.muted_foreground)
                                .child(progress.current.clone().unwrap_or_default()),
                        ),
                ),
        )
        .into_any_element()
}

/// Localised name of a job phase.
pub(crate) fn phase_label(phase: Phase) -> SharedString {
    tr(match phase {
        Phase::Starting => "scan.phase.starting",
        Phase::Scanning => "scan.phase.scanning",
        Phase::Measuring => "scan.phase.measuring",
        Phase::Hashing => "scan.phase.hashing",
        Phase::Quitting => "scan.phase.quitting",
        Phase::Uninstalling => "scan.phase.uninstalling",
        Phase::Elevating => "scan.phase.elevating",
        Phase::Removing => "scan.phase.removing",
        Phase::Finishing => "scan.phase.finishing",
    })
}

/// `part / whole` in `0.0..=1.0` (0 when `whole` is 0).
pub(crate) fn fraction(part: u64, whole: u64) -> f32 {
    if whole == 0 {
        return 0.;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "a display ratio; f64 keeps 52 bits, far beyond what a bar can show"
    )]
    let ratio = part as f64 / whole as f64;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "clamped to 0..=1 before narrowing"
    )]
    let ratio = ratio.clamp(0., 1.) as f32;
    ratio
}

/// A horizontal bar filled to `fraction` (0..=1) in `color`, on a faint track.
pub(crate) fn size_bar(fraction: f32, color: Hsla, cx: &App) -> AnyElement {
    let theme = cx.theme();
    div()
        .w_full()
        .h(page::SIZE_BAR_HEIGHT)
        .rounded(page::SIZE_BAR_HEIGHT)
        .bg(theme.muted)
        .overflow_hidden()
        .child(
            div()
                .h_full()
                .w(relative(fraction.clamp(0., 1.)))
                .rounded(page::SIZE_BAR_HEIGHT)
                .bg(color),
        )
        .into_any_element()
}

/// A 16 px selection checkbox (focusable; Space toggles it); its click never reaches the
/// row around it.
pub(crate) fn check(
    id: impl Into<ElementId>,
    checked: bool,
    on_toggle: impl Fn(&bool, &mut Window, &mut App) + 'static,
) -> ui::Checkbox {
    ui::Checkbox::new(id).checked(checked).on_click(on_toggle)
}

/// The technical id (bundle id, package, launchd label) a row shows before its detail
/// line (`ident · ~/path`): `None` when there is none, or when the detail's last component
/// already contains it (a middle-ellipsised path keeps its end visible, so the id would
/// only repeat).
pub(crate) fn detail_ident(ident: Option<&str>, detail: &str) -> Option<SharedString> {
    let ident = ident.map(str::trim).filter(|i| !i.is_empty())?;
    let leaf = detail
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(detail);
    let repeated = leaf.to_lowercase().contains(&ident.to_lowercase());
    (!repeated).then(|| SharedString::from(ident.to_owned()))
}

/// Right-aligned tabular size text (13/500).
pub(crate) fn size_cell(bytes: SharedString, cx: &App) -> AnyElement {
    div()
        .flex_none()
        .w(row::SIZE_COLUMN)
        .text_right()
        .whitespace_nowrap()
        .text_size(text::BODY)
        .font_weight(FontWeight::MEDIUM)
        .font_features(ui::tabular())
        .text_color(cx.theme().foreground)
        .child(bytes)
        .into_any_element()
}

/// Muted fixed-width text (ages, dates; 12 px).
pub(crate) fn muted_cell(value: SharedString, cx: &App) -> AnyElement {
    div()
        .flex_none()
        .w(row::AGE_COLUMN)
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .text_size(text::SMALL)
        .text_color(cx.theme().muted_foreground)
        .child(value)
        .into_any_element()
}

/// A compact toggle button for filters, tabs and sort keys (24 px; the active one raised).
pub(crate) fn chip(
    id: impl Into<ElementId>,
    label: SharedString,
    active: bool,
    on_click: OnClick,
) -> ui::Button {
    ui::Button::new(id, label)
        .small()
        .ghost()
        .selected(active)
        .on_click(on_click)
}

/// How the confirmed items are removed, for the confirmation's wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Removal {
    /// Caches, logs, build output: removed as the settings say for junk.
    Junk,
    /// The user's own files: removed as the settings say for user files.
    UserFiles,
}

/// What a clean confirmation shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CleanConfirm {
    /// Items to remove.
    pub(crate) count: usize,
    /// Their total size.
    pub(crate) bytes: u64,
    /// How they go.
    pub(crate) removal: Removal,
}

/// Asks before cleaning; `on_ok` runs only when the user confirms.
pub(crate) fn confirm_clean(
    confirm: CleanConfirm,
    on_ok: impl Fn(&mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let on_ok = std::rc::Rc::new(on_ok);
    let title = rust_i18n::t!(
        "clean.confirm.title",
        count = format::count(u64::try_from(confirm.count).unwrap_or(u64::MAX)),
        bytes = format::bytes(confirm.bytes)
    )
    .to_string();
    let method = tr(method_key(confirm.removal, cx));
    window.open_alert_dialog(cx, move |alert, _, _| {
        let on_ok = on_ok.clone();
        alert
            .confirm()
            .width(page::DIALOG_WIDTH)
            .title(title.clone())
            .description(method.clone())
            .ok_text(tr("clean.confirm.ok"))
            .ok_variant(ButtonVariant::Danger)
            .cancel_text(tr("clean.confirm.cancel"))
            .on_ok(move |_, window, cx| {
                on_ok(window, cx);
                true
            })
    });
}

/// Confirmation wording for `removal`: the configured deletion method once the daemon's
/// settings are loaded, a neutral description before.
fn method_key(removal: Removal, cx: &App) -> &'static str {
    if !CleanPrefs::is_loaded(cx) {
        return match removal {
            Removal::Junk => "clean.confirm.method_junk",
            Removal::UserFiles => "clean.confirm.method_files",
        };
    }
    let settings = CleanPrefs::settings(cx);
    let method = match removal {
        Removal::Junk => settings.junk_delete,
        Removal::UserFiles => settings.files_delete,
    };
    match method {
        DeleteMethod::Permanent => "clean.confirm.permanent",
        DeleteMethod::Trash => "clean.confirm.trash",
    }
}

/// Opens the macOS privacy pane that grants `reason`, when one exists.
pub(crate) fn open_privacy_pane(reason: FailReason, cx: &App) {
    if let Some(url) = privacy_url(reason) {
        cx.open_url(url);
    }
}

/// System Settings URL that grants `reason` (macOS only).
pub(crate) fn privacy_url(reason: FailReason) -> Option<&'static str> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    match reason {
        FailReason::FullDiskAccess => {
            Some("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
        }
        FailReason::AppManagement => {
            Some("x-apple.systempreferences:com.apple.preference.security?Privacy_AppBundles")
        }
        _ => None,
    }
}

/// The `sm` secondary button that opens the privacy pane for `reason` (a notice or report
/// can list several, and the view's one primary stays its main action); `None` where no
/// pane exists.
pub(crate) fn privacy_button(id: impl Into<ElementId>, reason: FailReason) -> Option<AnyElement> {
    privacy_url(reason)?;
    Some(
        ui::Button::new(
            id,
            tr(match reason {
                FailReason::AppManagement => "perm.open_app_management",
                _ => "perm.open_full_disk_access",
            }),
        )
        .small()
        .on_click(move |_, _, cx| open_privacy_pane(reason, cx))
        .into_any_element(),
    )
}

/// i18n key stem of a failure reason (`clean.reason.<stem>` title, `clean.hint.<stem>`).
pub(crate) const fn reason_stem(reason: FailReason) -> &'static str {
    match reason {
        FailReason::PermissionDenied => "permission_denied",
        FailReason::FullDiskAccess => "full_disk_access",
        FailReason::AppManagement => "app_management",
        FailReason::InUse => "in_use",
        FailReason::Protected => "protected",
        FailReason::ElevationCancelled => "elevation_cancelled",
        FailReason::TrashUnavailable => "trash_unavailable",
        FailReason::Other => "other",
    }
}

/// Every failure reason, in the order the report lists them.
const REASONS: [FailReason; 8] = [
    FailReason::FullDiskAccess,
    FailReason::AppManagement,
    FailReason::PermissionDenied,
    FailReason::ElevationCancelled,
    FailReason::InUse,
    FailReason::Protected,
    FailReason::TrashUnavailable,
    FailReason::Other,
];

/// Paths listed per failure group before "and N more".
pub(crate) const FAILURES_SHOWN: usize = 5;

/// Notice for places a scan could not read, with the fix when there is one.
pub(crate) fn denied_notice(denied: &[Denied], cx: &App) -> Option<AnyElement> {
    let first = denied.first()?;
    let reason = denied
        .iter()
        .map(|d| d.reason)
        .find(|r| privacy_url(*r).is_some())
        .unwrap_or(first.reason);
    let body = rust_i18n::t!(
        "perm.denied.body",
        count = format::count(u64::try_from(denied.len()).unwrap_or(u64::MAX)),
        path = first.path.clone()
    )
    .to_string();
    let hint = tr(&format!("clean.hint.{}", reason_stem(reason)));
    Some(notice(
        Tone::Warning,
        tr("perm.denied.title"),
        Some(format!("{body} {hint}").into()),
        privacy_button("denied-fix", reason).into_iter().collect(),
        cx,
    ))
}

/// The result of a clean: freed bytes, removed count, and the failures grouped by reason
/// with an explanation and, where one exists, a fix.
pub(crate) fn clean_report(
    report: &CleanReport,
    on_back: OnClick,
    on_rescan: OnClick,
    cx: &App,
) -> AnyElement {
    let groups = REASONS.into_iter().filter_map(|reason| {
        let failures: Vec<&Failure> = report
            .failures
            .iter()
            .filter(|f| f.reason == reason)
            .collect();
        failure_group(reason, &failures, cx)
    });
    let tone = if report.failures.is_empty() {
        Tone::Success
    } else {
        Tone::Warning
    };
    v_flex()
        .w_full()
        .gap(space::LG)
        .child(notice(
            tone,
            rust_i18n::t!("clean.done.title", bytes = format::bytes(report.freed))
                .to_string()
                .into(),
            Some(
                rust_i18n::t!(
                    "clean.done.body",
                    removed = format::count(report.removed),
                    failed =
                        format::count(u64::try_from(report.failures.len()).unwrap_or(u64::MAX))
                )
                .to_string()
                .into(),
            ),
            vec![
                ui::Button::new("clean-back", tr("clean.back"))
                    .small()
                    .on_click(on_back)
                    .into_any_element(),
                ui::Button::new("clean-rescan", tr("scan.rescan"))
                    .small()
                    .primary()
                    .icon(IconName::RefreshCw)
                    .on_click(on_rescan)
                    .into_any_element(),
            ],
            cx,
        ))
        .children(groups)
        .into_any_element()
}

/// Failures of one reason: explanation, fix, and the first few locations.
fn failure_group(reason: FailReason, failures: &[&Failure], cx: &App) -> Option<AnyElement> {
    if failures.is_empty() {
        return None;
    }
    let theme = cx.theme();
    let stem = reason_stem(reason);
    {
        let actions: Vec<AnyElement> = privacy_button(
            ElementId::from(SharedString::from(format!("fix-{stem}"))),
            reason,
        )
        .into_iter()
        .collect();
        let more = failures.len().saturating_sub(FAILURES_SHOWN);
        let paths = failures
            .iter()
            .take(FAILURES_SHOWN)
            .map(|f| {
                div()
                    .w_full()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis_middle()
                    .text_size(text::CAPTION)
                    .line_height(text::CAPTION_LINE_HEIGHT)
                    .text_color(theme.muted_foreground)
                    .child(f.location.display())
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let count = format::count(u64::try_from(failures.len()).unwrap_or(u64::MAX));
        Some(
            v_flex()
                .gap(space::SM)
                .child(notice(
                    Tone::Warning,
                    format!("{} · {count}", tr(&format!("clean.reason.{stem}"))).into(),
                    Some(tr(&format!("clean.hint.{stem}"))),
                    actions,
                    cx,
                ))
                .child(
                    v_flex()
                        .px(card::PADDING)
                        .gap(space::XXS)
                        .children(paths)
                        .when(more > 0, |this| {
                            this.child(
                                div()
                                    .text_size(text::CAPTION)
                                    .text_color(theme.muted_foreground)
                                    .child(
                                        rust_i18n::t!(
                                            "clean.more",
                                            n = format::count(u64::try_from(more).unwrap_or(0))
                                        )
                                        .to_string(),
                                    ),
                            )
                        }),
                )
                .into_any_element(),
        )
    }
}

/// The primary "Clean N items" button of a results page.
pub(crate) fn clean_button(
    id: &'static str,
    count: usize,
    enabled: bool,
    on_click: OnClick,
) -> AnyElement {
    ui::Button::new(
        id,
        rust_i18n::t!(
            "junk.clean",
            count = format::count(u64::try_from(count).unwrap_or(u64::MAX))
        )
        .to_string(),
    )
    .primary()
    .disabled(count == 0 || !enabled)
    .on_click(on_click)
    .into_any_element()
}

/// Gives a virtual list the remaining height of the page column.
pub(crate) fn list_frame(list: impl IntoElement) -> AnyElement {
    div()
        .flex_1()
        .min_h_0()
        .w_full()
        .child(list)
        .into_any_element()
}

// ---- Area page scaffolding (idle / empty / results cards) ----

/// The header toolbar action of a page showing results: a secondary "Scan again" (the
/// results card holds the page's one primary, Clean).
pub(crate) fn rescan_button(id: &'static str, enabled: bool, on_click: OnClick) -> AnyElement {
    ui::Button::new(id, tr("scan.rescan"))
        .icon(IconName::RefreshCw)
        .disabled(!enabled)
        .on_click(on_click)
        .into_any_element()
}

/// The idle content of an area page: a card with the area's icon, what the scan looks at,
/// and the page's one `lg` primary (`action`).
pub(crate) fn idle_card(category: Category, action: Option<AnyElement>) -> AnyElement {
    ui::Card::new()
        .child(
            ui::EmptyState::new(category.icon(), tr("junk.idle.title"))
                .when_some(category.covers(), ui::EmptyState::description)
                .when_some(action, ui::EmptyState::action),
        )
        .into_any_element()
}

/// The `lg` primary "Scan" of an idle page.
pub(crate) fn idle_scan_button(id: &'static str, enabled: bool, on_click: OnClick) -> AnyElement {
    ui::Button::new(id, tr("scan.scan"))
        .primary()
        .large()
        .icon(IconName::Search)
        .disabled(!enabled)
        .on_click(on_click)
        .into_any_element()
}

/// A card with a centred empty-state message (no action: the header offers "Scan again").
pub(crate) fn empty_card(
    icon: impl Into<Icon>,
    title: SharedString,
    body: SharedString,
) -> AnyElement {
    ui::Card::new()
        .child(ui::EmptyState::new(icon, title).description(body))
        .into_any_element()
}

/// The 28 px column header of a result table (11/500 muted labels over a hairline), laid
/// out like its rows: `grid` cells (the "select listed" checkbox in the checkbox column),
/// the name label where row titles start, then the fixed-width `columns` with the rows'
/// trailing gap, so every label sits over its column.
pub(crate) fn column_header(
    grid: ui::RowGrid,
    check: Option<AnyElement>,
    name: SharedString,
    columns: Vec<AnyElement>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    h_flex()
        .w_full()
        .flex_none()
        .h(control::HEIGHT_MD)
        .px(row::PAD_X)
        .border_b_1()
        .border_color(theme.border.alpha(theme.border.a * card::BORDER_ALPHA))
        .text_size(text::CAPTION)
        .line_height(text::CAPTION_LINE_HEIGHT)
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.muted_foreground)
        .whitespace_nowrap()
        .when(!grid.is_empty(), |this| {
            this.child(grid.cells(None, check, None).mr(row::SLOT_GAP))
        })
        .child(div().flex_1().min_w_0().child(name))
        .child(
            h_flex()
                .flex_none()
                .gap(row::GAP)
                .ml(row::GAP)
                .children(columns),
        )
        .into_any_element()
}

/// The inset body of a flush results card: fills the card, rows keep [`row::INSET`] off
/// its border (their hover and focus shapes are drawn on that inset box).
pub(crate) fn card_body() -> gpui_kit::Div {
    v_flex()
        .flex_1()
        .min_h_0()
        .w_full()
        .px(row::INSET)
        .pb(row::INSET)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractions_are_clamped_ratios() {
        assert!(fraction(1, 0).abs() < f32::EPSILON, "empty whole is zero");
        assert!((fraction(1, 4) - 0.25).abs() < 1e-6, "quarter");
        assert!(
            (fraction(9, 4) - 1.).abs() < f32::EPSILON,
            "overfull clamps to one"
        );
        assert!(
            (fraction(u64::MAX, u64::MAX) - 1.).abs() < f32::EPSILON,
            "huge values stay exact enough"
        );
    }

    #[test]
    fn privacy_panes_exist_only_for_macos_permissions() {
        assert_eq!(
            privacy_url(FailReason::FullDiskAccess).is_some(),
            cfg!(target_os = "macos"),
            "full disk access pane on macOS only"
        );
        assert!(
            privacy_url(FailReason::PermissionDenied).is_none(),
            "admin rights have no privacy pane"
        );
    }

    #[test]
    fn idents_show_unless_the_detail_already_ends_with_them() {
        assert_eq!(
            detail_ident(Some("com.tencent.xinWeChat"), "~/Library/Caches/Chromium"),
            Some("com.tencent.xinWeChat".into()),
            "an id the path does not carry is shown"
        );
        assert_eq!(
            detail_ident(
                Some("com.soda.music.helper"),
                "~/Library/Caches/com.soda.music.helper/"
            ),
            None,
            "the path's last component already names it"
        );
        assert_eq!(
            detail_ident(Some("ghostty"), r"C:\Users\me\AppData\Roaming\Ghostty"),
            None,
            "case-insensitive, Windows separators"
        );
        assert_eq!(
            detail_ident(
                Some("com.mitchellh.ghostty"),
                "~/Library/Preferences/com.mitchellh.ghostty.plist"
            ),
            None,
            "a file named after the id repeats it too"
        );
        assert_eq!(
            detail_ident(Some("com.x"), "~/Library/com.x/Cache"),
            Some("com.x".into()),
            "only the last component counts"
        );
        assert_eq!(
            detail_ident(Some("  "), "~/x"),
            None,
            "blank ids are dropped"
        );
        assert_eq!(detail_ident(None, "~/x"), None, "no id");
    }
}
