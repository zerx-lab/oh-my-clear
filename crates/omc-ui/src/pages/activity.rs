//! Automation → Activity (ADR 0024): runs that wait for the user are pinned on top with
//! their decision — Clean now, Snooze (5 min / 15 min / 1 h) or Skip this time — followed by
//! the timeline of every other run: rule, when it started, how it ended, expandable to the
//! items it acted on.

use std::collections::HashSet;

use gpui_kit::component::{Icon, IconName, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, ElementId, Entity, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window,
};
use omc_proto::rules::{Decision, RuleRun, RunId, RunState};

use super::widgets::runs::{
    isolated, item_rows, run_summary, snooze_select, state_label, state_tone,
};
use super::widgets::{self, size_cell, tr};
use crate::format;
use crate::nav::Category;
use crate::rules::{self, Rules};
use crate::tokens::{row, space};
use crate::ui;

/// Items listed under a pending run.
const PENDING_ITEMS: usize = 3;
/// Items listed under an expanded history row.
const EXPANDED_ITEMS: usize = 25;

/// The leading icon column of history rows (the disclosure chevron).
const GRID: ui::RowGrid = ui::RowGrid::new().icon();

/// The Activity page.
pub(crate) struct ActivityPage {
    rules: Entity<Rules>,
    expanded: HashSet<RunId>,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for ActivityPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActivityPage")
            .field("expanded", &self.expanded.len())
            .finish_non_exhaustive()
    }
}

impl ActivityPage {
    /// Creates the page over the shared store.
    pub(crate) fn new(_window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let rules = rules::entity(cx);
        let subscriptions = vec![cx.observe(&rules, |_, _, cx| cx.notify())];
        Self {
            rules,
            expanded: HashSet::new(),
            _subscriptions: subscriptions,
        }
    }

    fn decide(&mut self, run: RunId, decision: Decision, cx: &mut Context<'_, Self>) {
        self.rules
            .update(cx, |_, cx| Rules::decide(run, decision, cx));
    }

    fn toggle(&mut self, run: RunId, cx: &mut Context<'_, Self>) {
        if !self.expanded.remove(&run) {
            self.expanded.insert(run);
        }
        cx.notify();
    }

    fn pending_block(
        run: &RuleRun,
        now: i64,
        connected: bool,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let id = run.id;
        let detail = format!("{} · {}", run_summary(run), state_label(&run.state, now));
        let name = |prefix: &str| ElementId::Name(SharedString::from(format!("{prefix}-{id}")));
        let actions = h_flex()
            .gap(space::MD)
            .child(
                ui::Button::new(name("run-clean"), tr("runs.clean_now"))
                    .primary()
                    .small()
                    .disabled(!connected)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.decide(id, Decision::CleanNow, cx);
                    })),
            )
            .child(snooze_select(
                name("run-snooze"),
                true,
                !connected,
                cx.listener(move |this, decision: &Decision, _, cx| {
                    this.decide(id, *decision, cx);
                }),
            ))
            .child(
                ui::Button::new(name("run-skip"), tr("runs.skip"))
                    .ghost()
                    .small()
                    .disabled(!connected)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.decide(id, Decision::Skip, cx);
                    })),
            );
        v_flex()
            .w_full()
            .child(
                ui::ListRow::new(name("run-pending"), run.rule_name.clone())
                    .detail(detail)
                    .trailing(isolated(name("run-actions"), actions)),
            )
            .children(item_rows("pending-item", run, PENDING_ITEMS, true, cx))
            .into_any_element()
    }

    fn history_block(&self, run: &RuleRun, now: i64, cx: &mut Context<'_, Self>) -> AnyElement {
        let id = run.id;
        let open = self.expanded.contains(&id);
        let detail = format!(
            "{} · {}",
            format::relative(run.started, now),
            run_summary(run)
        );
        let chevron = Icon::new(if open {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        })
        .size(row::CHEVRON);
        let mut block = v_flex().w_full().child(
            ui::ListRow::new(
                ElementId::Name(SharedString::from(format!("run-row-{id}"))),
                run.rule_name.clone(),
            )
            .grid(GRID)
            .icon(chevron)
            .detail(detail)
            .trailing(ui::Badge::new(state_label(&run.state, now)).tone(state_tone(&run.state)))
            .trailing(size_cell(
                match run.state {
                    RunState::Done { freed, .. } => format::bytes(freed),
                    _ => SharedString::default(),
                },
                cx,
            ))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle(id, cx))),
        );
        if open {
            if let RunState::Failed { message } = &run.state {
                block = block.child(
                    ui::ListRow::new(
                        ElementId::Name(SharedString::from(format!("run-error-{id}"))),
                        message.clone(),
                    )
                    .grid(GRID),
                );
            }
            block = block.children(item_rows("history-item", run, EXPANDED_ITEMS, true, cx));
        }
        block.into_any_element()
    }
}

impl Render for ActivityPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let now = format::now();
        let (connected, error, pending, history) = {
            let store = self.rules.read(cx);
            let model = store.model();
            (
                store.is_connected(),
                store.error().cloned(),
                model.pending().into_iter().cloned().collect::<Vec<_>>(),
                model
                    .runs()
                    .iter()
                    .filter(|run| !run.state.awaits_user())
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        };
        let mut column = widgets::page_column("activity-page")
            .child(widgets::area_header(Category::Activity, Vec::new(), cx))
            .children(widgets::connection_notice(connected, cx));
        if let Some(error) = error {
            column = column.child(widgets::error_notice(
                error,
                Box::new(cx.listener(|this, _, _, cx| {
                    this.rules.update(cx, Rules::dismiss_error);
                })),
                cx,
            ));
        }
        if !pending.is_empty() {
            let blocks: Vec<AnyElement> = pending
                .iter()
                .map(|run| Self::pending_block(run, now, connected, cx))
                .collect();
            column = column.child(
                ui::Card::new()
                    .flush()
                    .header(
                        ui::CardHeader::new(tr("activity.pending.title"))
                            .description(tr("activity.pending.body")),
                    )
                    .child(widgets::card_body().children(blocks)),
            );
        }
        if history.is_empty() && pending.is_empty() {
            column = column.child(
                ui::Card::new().child(
                    ui::EmptyState::new(Category::Activity.icon(), tr("activity.empty.title"))
                        .description(tr("activity.empty.body")),
                ),
            );
        } else if !history.is_empty() {
            let blocks: Vec<AnyElement> = history
                .iter()
                .map(|run| self.history_block(run, now, cx))
                .collect();
            column = column.child(
                ui::Card::new()
                    .flush()
                    .header(ui::CardHeader::new(tr("activity.history.title")))
                    .child(widgets::card_body().children(blocks)),
            );
        }
        v_flex().size_full().child(column.overflow_y_scroll())
    }
}
