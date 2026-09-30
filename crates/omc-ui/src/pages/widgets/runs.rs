//! Elements the automation views share (activity page, prompt window, overview dashboard):
//! run state wording and tone, the "Snooze" dropdown, item rows and a click isolator.

use gpui_kit::{
    AnyElement, App, ElementId, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};
use omc_proto::rules::{Decision, RuleRun, RunState};

use super::parts::{size_cell, tr};
use crate::format;
use crate::ui;

/// The snooze choices, in menu order.
pub(crate) const SNOOZE_MINUTES: [u32; 3] = [5, 15, 60];

/// One-line wording of where a run is.
pub(crate) fn state_label(state: &RunState, now: i64) -> SharedString {
    let text = match state {
        RunState::Scanning => rust_i18n::t!("runs.state.scanning").to_string(),
        RunState::Pending { until: None } => rust_i18n::t!("runs.state.pending").to_string(),
        RunState::Pending { until: Some(at) } => {
            rust_i18n::t!("runs.state.snoozed", when = format::relative(*at, now)).to_string()
        }
        RunState::Deferred { until } => {
            rust_i18n::t!("runs.state.deferred", when = format::relative(*until, now)).to_string()
        }
        RunState::Cleaning => rust_i18n::t!("runs.state.cleaning").to_string(),
        RunState::Done { freed, failed: 0 } => {
            rust_i18n::t!("runs.state.done", bytes = format::bytes(*freed)).to_string()
        }
        RunState::Done { freed, failed } => rust_i18n::t!(
            "runs.state.done_failed",
            bytes = format::bytes(*freed),
            failed = format::count(*failed)
        )
        .to_string(),
        RunState::Nothing => rust_i18n::t!("runs.state.nothing").to_string(),
        RunState::Skipped => rust_i18n::t!("runs.state.skipped").to_string(),
        RunState::Failed { .. } => rust_i18n::t!("runs.state.failed").to_string(),
    };
    text.into()
}

/// The tone a state shows in.
pub(crate) const fn state_tone(state: &RunState) -> ui::Tone {
    match state {
        RunState::Pending { .. } | RunState::Deferred { .. } => ui::Tone::Warning,
        RunState::Done { failed: 0, .. } => ui::Tone::Success,
        RunState::Done { .. } | RunState::Failed { .. } => ui::Tone::Danger,
        RunState::Scanning | RunState::Cleaning => ui::Tone::Accent,
        RunState::Nothing | RunState::Skipped => ui::Tone::Neutral,
    }
}

/// "12 items · 3.4 GB".
pub(crate) fn run_summary(run: &RuleRun) -> SharedString {
    rust_i18n::t!(
        "runs.summary",
        count = format::count(run.item_count),
        bytes = format::bytes(run.bytes)
    )
    .to_string()
    .into()
}

/// Menu text of a snooze choice: "5 min", "1 h".
fn snooze_label(minutes: u32) -> SharedString {
    let text = if minutes >= 60 && minutes.is_multiple_of(60) {
        rust_i18n::t!("runs.snooze_hours", n = minutes / 60).to_string()
    } else {
        rust_i18n::t!("runs.snooze_minutes", n = minutes).to_string()
    };
    text.into()
}

/// The "Snooze ▾" dropdown: choosing a delay calls `on_pick` with the matching
/// [`Decision::Snooze`].
pub(crate) fn snooze_select(
    id: impl Into<ElementId>,
    small: bool,
    disabled: bool,
    on_pick: impl Fn(&Decision, &mut Window, &mut App) + 'static,
) -> ui::Select {
    let options = SNOOZE_MINUTES.iter().map(|&minutes| {
        (
            SharedString::from(minutes.to_string()),
            snooze_label(minutes),
        )
    });
    let select = ui::Select::new(id, options)
        .label(tr("runs.snooze"))
        .disabled(disabled)
        .on_change(move |value, window, cx| {
            let minutes = value
                .parse::<u32>()
                .ok()
                .filter(|minutes| SNOOZE_MINUTES.contains(minutes));
            if let Some(minutes) = minutes {
                on_pick(&Decision::Snooze { minutes }, window, cx);
            } else {
                tracing::warn!(%value, "unknown snooze choice");
            }
        });
    if small { select.small() } else { select }
}

/// Wraps row controls so a click on them does not also activate the clickable row around.
pub(crate) fn isolated(id: impl Into<ElementId>, child: impl IntoElement) -> AnyElement {
    div()
        .id(id)
        .flex_none()
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(child)
        .into_any_element()
}

/// Rows for the first `limit` items of `run` (name, path, size) and a closing "and N more"
/// line when the run holds more than shown. `indent` lines the rows up under a row that has
/// a leading icon.
pub(crate) fn item_rows(
    prefix: &'static str,
    run: &RuleRun,
    limit: usize,
    indent: bool,
    cx: &App,
) -> Vec<AnyElement> {
    let grid = if indent {
        ui::RowGrid::new().icon()
    } else {
        ui::RowGrid::new()
    };
    let mut rows: Vec<AnyElement> = run
        .items
        .iter()
        .take(limit)
        .enumerate()
        .map(|(ix, item)| {
            ui::ListRow::new(
                ElementId::Name(SharedString::from(format!("{prefix}-{}-{ix}", run.id))),
                item.name.clone(),
            )
            .grid(grid)
            .detail(format::tilde(&item.location))
            .trailing(size_cell(format::bytes(item.bytes), cx))
            .into_any_element()
        })
        .collect();
    let shown = u64::try_from(rows.len()).unwrap_or(u64::MAX);
    let hidden = run.item_count.saturating_sub(shown);
    if hidden > 0 {
        rows.push(
            ui::ListRow::new(
                ElementId::Name(SharedString::from(format!("{prefix}-{}-more", run.id))),
                rust_i18n::t!("runs.more_items", count = format::count(hidden)).to_string(),
            )
            .grid(grid)
            .into_any_element(),
        );
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_finished_run_with_failures_is_not_shown_as_a_success() {
        assert_eq!(
            state_tone(&RunState::Done {
                freed: 1,
                failed: 0
            }),
            ui::Tone::Success,
            "a clean run"
        );
        assert_eq!(
            state_tone(&RunState::Done {
                freed: 1,
                failed: 2
            }),
            ui::Tone::Danger,
            "some items could not be removed"
        );
        assert_eq!(
            state_tone(&RunState::Pending { until: Some(5) }),
            ui::Tone::Warning,
            "a snoozed run still needs the user"
        );
    }
}
