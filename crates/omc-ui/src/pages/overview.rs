//! Start page: the machine (OS, volumes, permissions), a smart scan across the everyday
//! junk areas with per-area tiles that link to each area, a one-click clean of the safe
//! items found (listed per junk group: area, largest items, count, size), and the app's
//! key shortcuts. The scans are the shared store's ([`crate::scans::Scans`]): the area
//! pages show the same progress and results.

use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::base::Button as BaseButton;
use gpui_kit::component::{ActiveTheme as _, Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Action, AnyElement, AsyncApp, Context, ElementId, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, WeakEntity, Window, div, img,
};
use omc_ipc::client::{ClientEvent, ConnState};
use omc_proto::jobs::{CleanReport, FailReason, ItemId};
use omc_proto::settings::{Access, Os, SystemInfo, Volume};

use super::Navigate;
use super::junk::{kind_label, removal, report_card};
use super::widgets::flow::FlowView;
use super::widgets::{
    self, CleanConfirm, ConnChange, Connection, FlowPhase, OnClick, Removal, Tone, tr,
};
use crate::actions::{OpenSettings, Quit, ToggleAppearance, ToggleSidebar};
use crate::brand;
use crate::engine;
use crate::format;
use crate::jobs;
use crate::nav::Category;
use crate::scans::{self, Area, AreaSummary, SafeGroup, Scans};
use crate::tokens::{card, chrome, layout, page, radius, row, space, text};
use crate::ui;

/// Areas the smart scan covers, in tile order.
pub(crate) const SMART_AREAS: [Category; 5] = [
    Category::SystemJunk,
    Category::BrowserData,
    Category::DeveloperJunk,
    Category::Trash,
    Category::Installers,
];

/// Share of a volume in use from which its bar turns to the warning tone.
const VOLUME_WARN_SHARE: f32 = 0.9;

/// The store areas of [`SMART_AREAS`].
fn smart_areas() -> impl Iterator<Item = (Category, Area)> {
    SMART_AREAS
        .into_iter()
        .filter_map(|category| Area::of(category).map(|area| (category, area)))
}

/// The smart areas with a ready result that has safe items, in tile order.
fn safe_areas(scans: &Scans) -> impl Iterator<Item = (Category, Area, &AreaSummary)> {
    smart_areas().filter_map(move |(category, area)| {
        let summary = scans.summary(area)?;
        (scans.view(area).phase == FlowPhase::Ready && !summary.safe.is_empty())
            .then_some((category, area, summary))
    })
}

/// A breakdown row's detail: the group's largest items by name ("Chrome, Slack and
/// more"); `None` when its items have no names.
fn safe_names(group: &SafeGroup) -> Option<SharedString> {
    if group.names.is_empty() {
        return None;
    }
    let names = group.names.join(&tr("overview.safe_names_separator"));
    Some(if group.has_more() {
        rust_i18n::t!("overview.safe_names_more", names = names)
            .to_string()
            .into()
    } else {
        names.into()
    })
}

/// Reports of several cleans as one.
pub(crate) fn merge_reports<'a>(reports: impl IntoIterator<Item = &'a CleanReport>) -> CleanReport {
    let mut merged = CleanReport::default();
    for report in reports {
        merged.removed = merged.removed.saturating_add(report.removed);
        merged.freed = merged.freed.saturating_add(report.freed);
        merged.failures.extend(report.failures.iter().cloned());
    }
    merged
}

/// A tile's copy of its area in the store.
struct CardState {
    category: Category,
    flow: FlowView,
    summary: Option<AreaSummary>,
    /// Age of the shown result.
    age: Option<Duration>,
}

/// The start page.
pub(crate) struct OverviewPage {
    scans: Entity<Scans>,
    /// Only for the system facts (the store tracks job epochs).
    conn: Connection,
    info: Option<SystemInfo>,
    info_error: Option<SharedString>,
    info_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for OverviewPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverviewPage")
            .field("info", &self.info.is_some())
            .finish_non_exhaustive()
    }
}

impl OverviewPage {
    /// Creates the page and asks the daemon for the system facts.
    pub(crate) fn new(_window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let engine = engine::entity(cx);
        let scans = scans::entity(cx);
        let subscriptions = vec![
            cx.subscribe(&engine, |this, _, event: &ClientEvent, cx| {
                let change = this.conn.observe(event);
                if let ClientEvent::State(state) = event {
                    if matches!(state, ConnState::Connected { .. })
                        && (change == ConnChange::NewDaemon || this.info.is_none())
                    {
                        this.fetch_info(cx);
                    }
                    cx.notify();
                }
            }),
            // Tiles, progress and reports follow the store.
            cx.observe(&scans, |_, _, cx| cx.notify()),
        ];
        let mut this = Self {
            scans,
            conn: Connection::new(cx),
            info: None,
            info_error: None,
            info_task: None,
            _subscriptions: subscriptions,
        };
        if this.conn.is_connected() {
            this.fetch_info(cx);
        }
        this
    }

    fn fetch_info(&mut self, cx: &mut Context<'_, Self>) {
        let answer = jobs::system_info(cx);
        self.info_task = Some(
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let info = answer.await;
                if let Err(err) = this.update(cx, |this, cx| {
                    match info {
                        Ok(info) => {
                            this.info = Some(info);
                            this.info_error = None;
                        }
                        Err(message) => this.info_error = Some(message.into()),
                    }
                    cx.notify();
                }) {
                    tracing::debug!("overview closed before system info arrived: {err}");
                }
            }),
        );
    }

    /// Scans every smart area that has no fresh result (fresh ones are reused).
    fn smart_scan(&mut self, cx: &mut Context<'_, Self>) {
        self.scans.update(cx, |s, cx| {
            for (_, area) in smart_areas() {
                s.ensure(area, cx);
            }
        });
    }

    /// Rescans every smart area that is not running.
    fn rescan_all(&mut self, cx: &mut Context<'_, Self>) {
        self.scans.update(cx, |s, cx| {
            for (_, area) in smart_areas() {
                if !s.is_busy(area) {
                    s.rescan(area, cx);
                }
            }
        });
    }

    fn cancel_all(&mut self, cx: &mut Context<'_, Self>) {
        self.scans.update(cx, |s, cx| {
            for (_, area) in smart_areas() {
                s.cancel(area, cx);
            }
        });
    }

    /// Safe items per ready area: `(area, ids, bytes)`.
    fn safe_plan(scans: &Scans) -> Vec<(Area, Vec<ItemId>, u64)> {
        safe_areas(scans)
            .map(|(_, area, summary)| {
                (
                    area,
                    summary.safe.iter().map(|(id, _)| *id).collect(),
                    summary.safe_bytes(),
                )
            })
            .collect()
    }

    /// What "Clean safe items" removes, one row per junk group of each ready area: the
    /// group, its area, its largest items by name, count and size. A row opens its area,
    /// where every item can be reviewed.
    fn safe_rows(&self, cx: &mut Context<'_, Self>) -> Vec<AnyElement> {
        let groups: Vec<(Category, SafeGroup)> = safe_areas(self.scans.read(cx))
            .flat_map(|(category, _, summary)| {
                summary
                    .safe_groups
                    .iter()
                    .map(move |group| (category, group.clone()))
            })
            .collect();
        groups
            .into_iter()
            .enumerate()
            .map(|(ix, (category, group))| {
                ui::ListRow::new(("overview-safe", ix), kind_label(group.kind))
                    .icon(ui::row_icon(None, Icon::new(category.icon()), cx))
                    .detail_lead(category.title())
                    .when_some(safe_names(&group), ui::ListRow::detail)
                    .trailing(widgets::muted_cell(ui::item_count(group.count), cx))
                    .trailing(widgets::size_cell(format::bytes(group.bytes), cx))
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(Navigate(category))))
                    .into_any_element()
            })
            .collect()
    }

    fn ask_clean_safe(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let plan = Self::safe_plan(self.scans.read(cx));
        if plan.is_empty() {
            return;
        }
        let count = plan
            .iter()
            .fold(0_usize, |n, (_, ids, _)| n.saturating_add(ids.len()));
        let bytes = plan.iter().fold(0_u64, |n, (_, _, b)| n.saturating_add(*b));
        let user_files = plan.iter().any(|(area, _, _)| {
            smart_areas().any(|(category, a)| a == *area && removal(category) == Removal::UserFiles)
        });
        let confirm = CleanConfirm {
            count,
            bytes,
            removal: if user_files {
                Removal::UserFiles
            } else {
                Removal::Junk
            },
        };
        let scans = self.scans.downgrade();
        widgets::confirm_clean(
            confirm,
            move |_, cx| {
                let plan = plan.clone();
                if let Err(err) = scans.update(cx, |s, cx| {
                    for (area, ids, _) in plan {
                        s.clean(area, ids, cx);
                    }
                }) {
                    tracing::debug!("scan store gone before cleaning: {err}");
                }
            },
            window,
            cx,
        );
    }

    fn dismiss_reports(&mut self, cx: &mut Context<'_, Self>) {
        self.scans.update(cx, |s, cx| {
            for (_, area) in smart_areas() {
                s.dismiss_report(area, cx);
            }
        });
    }

    fn listener(
        cx: &mut Context<'_, Self>,
        f: impl Fn(&mut Self, &mut Window, &mut Context<'_, Self>) + 'static,
    ) -> OnClick {
        Box::new(cx.listener(move |this, _, window, cx| f(this, window, cx)))
    }

    /// The machine: OS on the card header, one capacity bar per volume.
    fn render_system(&self, cx: &Context<'_, Self>) -> Option<AnyElement> {
        let info = self.info.as_ref()?;
        let os = match info.os {
            Os::Macos => "macOS",
            Os::Windows => "Windows",
            Os::Linux => "Linux",
        };
        let volumes = info
            .volumes
            .iter()
            .enumerate()
            .map(|(ix, volume)| volume_row(ix, volume, cx))
            .collect::<Vec<_>>();
        Some(
            ui::Card::new()
                .header(
                    ui::CardHeader::new(format!("{os} {}", info.os_version))
                        .leading(
                            div()
                                .flex_none()
                                .text_color(cx.theme().muted_foreground)
                                .child(Icon::new(IconName::HardDrive).size(chrome::ICON)),
                        )
                        .when(info.elevated, |this| {
                            this.trailing(ui::Badge::new(tr("overview.elevated")))
                        }),
                )
                .when(!volumes.is_empty(), |this| {
                    this.child(v_flex().w_full().gap(space::XL).children(volumes))
                })
                .into_any_element(),
        )
    }

    fn render_permission(&self, cx: &Context<'_, Self>) -> Option<AnyElement> {
        let info = self.info.as_ref()?;
        (info.full_disk_access == Access::Denied).then(|| {
            widgets::notice(
                Tone::Warning,
                tr("perm.fda.title"),
                Some(tr("perm.fda.body")),
                widgets::privacy_button("overview-fda", FailReason::FullDiskAccess)
                    .into_iter()
                    .collect(),
                cx,
            )
        })
    }

    /// The smart areas as the store has them now.
    fn cards_state(&self, cx: &Context<'_, Self>) -> Vec<CardState> {
        let scans = self.scans.read(cx);
        let now = Instant::now();
        smart_areas()
            .map(|(category, area)| CardState {
                category,
                flow: scans.view(area),
                summary: scans.summary(area).cloned(),
                age: scans.age(area, now),
            })
            .collect()
    }

    /// One area: icon and title, the size found, and its state (progress, age, not
    /// scanned). The whole tile is one focusable button that opens the area.
    fn tile(
        key: usize,
        card: &CardState,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let area = card.category;
        let id = ElementId::from(("overview-tile", key));
        let focus = ui::focus_handle(&id, window, cx);
        let theme = cx.theme();
        let (muted, fg, danger) = (theme.muted_foreground, theme.foreground, theme.danger);
        let hairline = theme.border.alpha(theme.border.a * card::BORDER_ALPHA);
        let hover = theme.muted.alpha(theme.muted.a * row::HOVER_ALPHA);
        let press = theme.muted.alpha(theme.muted.a * row::PRESS_ALPHA);
        let (surface, radius) = (theme.group_box, theme.radius_lg);
        let state_text = |text: SharedString, color| {
            div()
                .w_full()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(text::SMALL)
                .line_height(text::SMALL_LINE_HEIGHT)
                .font_features(ui::tabular())
                .text_color(color)
                .child(text)
                .into_any_element()
        };
        let (value, state): (SharedString, AnyElement) = match (card.flow.phase, &card.summary) {
            (FlowPhase::Scanning | FlowPhase::Cleaning, _) => {
                let progress = &card.flow.progress;
                (
                    format::bytes(progress.bytes),
                    v_flex()
                        .w_full()
                        .gap(space::XS)
                        .child(
                            ui::ProgressBar::new(("overview-tile-bar", key)).value(
                                (progress.total > 0)
                                    .then(|| widgets::fraction(progress.done, progress.total)),
                            ),
                        )
                        .child(state_text(widgets::phase_label(progress.phase), muted))
                        .into_any_element(),
                )
            }
            (FlowPhase::Failed, _) => ("—".into(), state_text(tr("overview.card.failed"), danger)),
            (_, Some(summary)) => (
                format::bytes(summary.total),
                state_text(card.age.map(scans::scanned_ago).unwrap_or_default(), muted),
            ),
            _ => (
                "—".into(),
                state_text(tr("overview.card.not_scanned"), muted),
            ),
        };
        let tile = BaseButton::new(id)
            .track_focus(&focus)
            .accessibility_label(area.title())
            .flex_col()
            .items_start()
            .justify_start()
            .flex_1()
            .min_w(page::TILE_MIN_WIDTH)
            .gap(space::MD)
            .p(space::LG)
            .rounded(radius)
            .border_1()
            .border_color(hairline)
            .bg(surface)
            .line_height(text::BODY_LINE_HEIGHT)
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .active(move |style| style.bg(press))
            .child(
                h_flex()
                    .w_full()
                    .gap(space::SM)
                    .child(
                        div()
                            .flex_none()
                            .text_color(muted)
                            .child(Icon::new(area.icon()).size(chrome::ICON)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(text::BODY)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(fg)
                            .child(area.title()),
                    ),
            )
            .child(ui::Stat::new(tr("junk.found"), value))
            .child(state)
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(Navigate(area))));
        ui::focus_ring(tile, &focus, window, cx).into_any_element()
    }

    /// The Smart Scan card: its actions on the header, the area tiles, and — once results
    /// exist — the safe total with the page's one primary "Clean safe items" over the
    /// breakdown of what it removes.
    fn render_smart(&self, window: &mut Window, cx: &mut Context<'_, Self>) -> AnyElement {
        let cards = self.cards_state(cx);
        let busy = cards.iter().any(|c| c.flow.is_busy());
        let (plan, connected) = {
            let scans = self.scans.read(cx);
            (Self::safe_plan(scans), scans.is_connected())
        };
        let safe_bytes = plan.iter().fold(0_u64, |n, (_, _, b)| n.saturating_add(*b));
        let scanned = cards.iter().any(|c| c.summary.is_some());
        let smart_scan = ui::Button::new("overview-smart-scan", tr("overview.smart_scan"))
            .icon(IconName::Sparkles)
            .disabled(!connected)
            .on_click(Self::listener(cx, |this, _, cx| this.smart_scan(cx)));
        let actions: Vec<AnyElement> = if busy {
            vec![
                ui::Button::new("overview-cancel", tr("scan.cancel"))
                    .on_click(Self::listener(cx, |this, _, cx| this.cancel_all(cx)))
                    .into_any_element(),
            ]
        } else if scanned {
            vec![
                ui::Button::new("overview-rescan-all", tr("scans.rescan_all"))
                    .outline()
                    .icon(IconName::RefreshCw)
                    .disabled(!connected)
                    .on_click(Self::listener(cx, |this, _, cx| this.rescan_all(cx)))
                    .into_any_element(),
                smart_scan.into_any_element(),
            ]
        } else {
            vec![smart_scan.primary().large().into_any_element()]
        };
        let tiles = cards
            .iter()
            .enumerate()
            .map(|(key, card)| Self::tile(key, card, window, cx))
            .collect::<Vec<_>>();
        let offer = !busy && !plan.is_empty();
        let safe_rows = if offer {
            self.safe_rows(cx)
        } else {
            Vec::new()
        };
        let theme = cx.theme();
        let hairline = theme.border.alpha(theme.border.a * card::BORDER_ALPHA);
        let footer = offer.then(|| {
            v_flex()
                .w_full()
                .gap(space::MD)
                .pt(space::LG)
                .border_t_1()
                .border_color(hairline)
                .child(
                    h_flex()
                        .w_full()
                        .gap(space::XL)
                        .child(
                            ui::Stat::new(tr("overview.safe_total"), format::bytes(safe_bytes))
                                .tone(ui::Tone::Success),
                        )
                        .child(div().flex_1())
                        .child(
                            ui::Button::new("overview-clean-safe", tr("overview.clean_safe"))
                                .primary()
                                .disabled(!connected)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.ask_clean_safe(window, cx);
                                })),
                        ),
                )
                .child(v_flex().w_full().children(safe_rows))
        });
        ui::Card::new()
            .header(
                ui::CardHeader::new(tr("overview.smart_title"))
                    .description(tr("overview.smart_body"))
                    .trailing(ui::Toolbar::new().children(actions)),
            )
            .child(h_flex().w_full().flex_wrap().gap(space::LG).children(tiles))
            .children(footer)
            .into_any_element()
    }

    fn render_reports(&self, cx: &mut Context<'_, Self>) -> Option<AnyElement> {
        let cards = self.cards_state(cx);
        if cards.iter().any(|c| c.flow.phase == FlowPhase::Cleaning) {
            return None;
        }
        let reports: Vec<&CleanReport> = cards
            .iter()
            .filter(|c| c.flow.phase == FlowPhase::Cleaned)
            .filter_map(|c| c.flow.report.as_ref())
            .collect();
        if reports.is_empty() {
            return None;
        }
        let merged = merge_reports(reports);
        Some(report_card(
            &merged,
            Self::listener(cx, |this, _, cx| this.dismiss_reports(cx)),
            cx,
        ))
    }

    /// The app's key shortcuts as a compact wrapped list of label + key hints.
    fn render_shortcuts(cx: &Context<'_, Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let rows = [
            ("nav.settings", &OpenSettings as &dyn Action),
            ("nav.toggle_sidebar", &ToggleSidebar),
            ("appearance.toggle", &ToggleAppearance),
            ("main.quit", &Quit),
        ]
        .into_iter()
        .map(|(label, action)| {
            h_flex()
                .gap(space::SM)
                .child(
                    div()
                        .text_size(text::SMALL)
                        .line_height(text::SMALL_LINE_HEIGHT)
                        .text_color(muted)
                        .child(tr(label)),
                )
                .child(ui::Kbd::new(action))
        });
        v_flex()
            .w_full()
            .gap(space::SM)
            .child(
                div()
                    .text_size(text::CAPTION)
                    .line_height(text::CAPTION_LINE_HEIGHT)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(muted)
                    .child(tr("main.shortcuts")),
            )
            .child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .gap_x(space::XL)
                    .gap_y(space::SM)
                    .children(rows),
            )
            .into_any_element()
    }
}

/// A volume: name and mount, used / free (tabular), and a 4 px capacity bar that turns to
/// the warning tone when the volume is nearly full.
fn volume_row(ix: usize, volume: &Volume, cx: &Context<'_, OverviewPage>) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let used = volume.total.saturating_sub(volume.free);
    let share = widgets::fraction(used, volume.total);
    let (name, mount) = if volume.name.is_empty() {
        (volume.mount.clone(), None)
    } else {
        (volume.name.clone(), Some(volume.mount.clone()))
    };
    v_flex()
        .w_full()
        .gap(space::SM)
        .child(
            h_flex()
                .w_full()
                .gap(space::MD)
                .child(
                    div()
                        .flex_none()
                        .max_w_1_2()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_size(text::BODY)
                        .line_height(text::BODY_LINE_HEIGHT)
                        .font_weight(FontWeight::MEDIUM)
                        .child(name),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis_middle()
                        .text_size(text::SMALL)
                        .text_color(muted)
                        .children(mount),
                )
                .child(
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .text_size(text::SMALL)
                        .font_features(ui::tabular())
                        .text_color(muted)
                        .child(
                            rust_i18n::t!(
                                "overview.volume_usage",
                                used = format::bytes(used),
                                free = format::bytes(volume.free),
                                total = format::bytes(volume.total)
                            )
                            .to_string(),
                        ),
                ),
        )
        .child(
            ui::ProgressBar::new(("overview-volume", ix))
                .value(Some(share))
                .tone(if share >= VOLUME_WARN_SHARE {
                    ui::Tone::Warning
                } else {
                    ui::Tone::Accent
                }),
        )
        .into_any_element()
}

/// The home screen's page header: the app logo in the page-header tile, the product name
/// as the 20/600 title, and the tagline below (the layout of [`ui::PageHeader`], which
/// takes Lucide icons only).
fn render_intro(cx: &Context<'_, OverviewPage>) -> AnyElement {
    let theme = cx.theme();
    v_flex()
        .w_full()
        .gap(space::XS)
        .child(
            h_flex()
                .w_full()
                .gap(space::LG)
                .child(
                    img(brand::mark())
                        .flex_none()
                        .size(layout::PAGE_ICON_TILE)
                        .rounded(radius::outer(theme.radius)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(text::PAGE_TITLE)
                        .line_height(text::PAGE_TITLE_LINE_HEIGHT)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child("oh-my-clear"),
                ),
        )
        .child(
            div()
                .text_size(text::BODY)
                .line_height(text::BODY_LINE_HEIGHT)
                .text_color(theme.muted_foreground)
                .child(tr("app.tagline")),
        )
        .into_any_element()
}

impl Render for OverviewPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let connected = self.conn.is_connected();
        let mut column = widgets::page_column("page-overview")
            .overflow_y_scroll()
            .child(render_intro(cx))
            .children(widgets::connection_notice(connected, cx))
            .children(self.render_permission(cx))
            .children(self.render_system(cx));
        if let Some(error) = self.info_error.clone() {
            column = column.child(widgets::notice(
                Tone::Warning,
                tr("overview.info_failed"),
                Some(error),
                Vec::new(),
                cx,
            ));
        }
        let errors: Vec<AnyElement> = self
            .cards_state(cx)
            .into_iter()
            .filter_map(|card| {
                let error = card.flow.error?;
                Some(widgets::notice(
                    Tone::Danger,
                    card.category.title(),
                    Some(error),
                    Vec::new(),
                    cx,
                ))
            })
            .collect();
        column = column
            .children(self.render_reports(cx))
            .child(self.render_smart(window, cx))
            .children(errors)
            .child(Self::render_shortcuts(cx));
        v_flex().size_full().child(column)
    }
}

#[cfg(test)]
mod tests {
    use omc_proto::jobs::{CleanReport, FailReason, Failure, Location};
    use omc_proto::junk::{JunkGroup, JunkItem, JunkKind, JunkReport, Safety};

    use super::merge_reports;

    fn item(id: u32, bytes: u64, safety: Safety, running: bool) -> JunkItem {
        JunkItem {
            id,
            name: String::new(),
            location: Location::Path {
                path: format!("/x/{id}"),
            },
            tag: None,
            bytes,
            files: 1,
            modified: None,
            safety,
            needs_admin: false,
            app_running: running,
            ident: None,
            icon: None,
        }
    }

    #[test]
    fn reports_merge_counts_and_failures() {
        let failure = Failure {
            location: Location::Path {
                path: "/a".to_owned(),
            },
            reason: FailReason::InUse,
            message: String::new(),
        };
        let a = CleanReport {
            removed: 2,
            freed: 5,
            failures: vec![failure.clone()],
        };
        let b = CleanReport {
            removed: 1,
            freed: u64::MAX,
            failures: vec![failure],
        };
        let merged = merge_reports([&a, &b]);
        assert_eq!(merged.removed, 3, "removed adds up");
        assert_eq!(merged.freed, u64::MAX, "freed saturates");
        assert_eq!(merged.failures.len(), 2, "failures concatenate");
    }

    use gpui_kit::TestAppContext;
    use omc_proto::jobs::JobOutput;

    use super::OverviewPage;
    use gpui_kit::component::WindowExt as _;

    use crate::pages::widgets::test_support::open_page;
    use crate::scans::{self, Area};

    #[gpui_kit::test]
    fn overview_renders_disconnected_then_offers_safe_items(cx: &mut TestAppContext) {
        let Some((window, page)) = open_page(cx, OverviewPage::new) else {
            return;
        };
        let connected = cx.update(|cx| page.read(cx).conn.is_connected());
        assert!(
            !connected,
            "without a daemon the overview shows the disconnected state"
        );
        let report = JunkReport {
            groups: vec![JunkGroup {
                kind: JunkKind::UserCache,
                items: vec![
                    item(0, 10, Safety::Safe, false),
                    item(1, 5, Safety::Review, false),
                ],
            }],
            denied: Vec::new(),
        };
        let store = cx.update(scans::entity);
        store.update(cx, |s, cx| {
            s.force_scanned(Area::SystemJunk, JobOutput::Junk(report), cx);
        });
        cx.run_until_parked();
        let plan = cx.update(|cx| OverviewPage::safe_plan(store.read(cx)));
        assert_eq!(
            plan,
            vec![(Area::SystemJunk, vec![0], 10)],
            "only the safe item is offered"
        );
        let opened = window.update(cx, |_, window, cx| {
            page.update(cx, |page, cx| page.ask_clean_safe(window, cx));
        });
        assert!(opened.is_ok(), "the window is alive");
        cx.run_until_parked();
        let dialog = window.update(cx, |_, window, cx| window.has_active_dialog(cx));
        assert_eq!(
            dialog.ok(),
            Some(true),
            "cleaning asks for confirmation first"
        );
    }
}
