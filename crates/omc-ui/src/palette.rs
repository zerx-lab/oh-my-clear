//! The command palette (⌘K / Ctrl+K, ADR 0024): a filterable list of every sidebar page and
//! every automation rule; choosing one jumps to it. Type to filter, ↑/↓ to move, Enter to
//! go, Esc to close.
//!
//! The arrow, Enter and Escape keys are bound to the palette's own actions in the context
//! `Palette > Input`: the text input would otherwise consume them (its bindings have the
//! same depth; the later one wins, see `actions::bindings`).

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Icon, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, ScrollHandle, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window, div,
};
use omc_proto::rules::{RuleId, RuleInfo};

use crate::actions::{PaletteCancel, PaletteConfirm, PaletteNext, PalettePrevious};
use crate::nav::{Category, NAV};
use crate::pages::widgets::{muted_cell, tr};
use crate::rules;
use crate::tokens::{page, space, text};
use crate::ui;

/// Where a palette entry leads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    /// A sidebar page.
    Page(Category),
    /// The Rules page with that rule opened.
    Rule(RuleId),
}

/// One row of the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) target: Target,
    /// Title of the page or name of the rule.
    pub(crate) label: SharedString,
    /// The sidebar group, or "Rule".
    pub(crate) group: SharedString,
    /// Lower-cased text the filter searches.
    haystack: String,
}

impl Entry {
    pub(crate) fn new(target: Target, label: SharedString, group: SharedString) -> Self {
        let haystack = format!("{label} {group}").to_lowercase();
        Self {
            target,
            label,
            group,
            haystack,
        }
    }
}

/// Every sidebar page in sidebar order, then every rule.
pub(crate) fn entries(rules: &[RuleInfo]) -> Vec<Entry> {
    let pages = NAV.iter().flat_map(|group| {
        let heading = group.label.map_or_else(|| tr("palette.group.general"), tr);
        group.items.iter().map(move |&category| {
            Entry::new(Target::Page(category), category.title(), heading.clone())
        })
    });
    let rules = rules.iter().map(|info| {
        Entry::new(
            Target::Rule(info.rule.id),
            info.rule.name.clone().into(),
            tr("palette.group.rule"),
        )
    });
    pages.chain(rules).collect()
}

/// Indices of the entries matching `query`, best first. Every word of the query must occur
/// in the label or group; a label that starts with the first word ranks before one that
/// merely contains the words, and those before matches only in the group. Equal ranks keep
/// their order.
pub(crate) fn matches(entries: &[Entry], query: &str) -> Vec<usize> {
    let query = query.to_lowercase();
    let words: Vec<&str> = query.split_whitespace().collect();
    let mut hits: Vec<(u8, usize)> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| words.iter().all(|word| entry.haystack.contains(word)))
        .map(|(ix, entry)| {
            let label = entry.label.to_lowercase();
            let rank = match words.first() {
                None => 0,
                Some(first) if label.starts_with(first) => 0,
                _ if words.iter().all(|word| label.contains(word)) => 1,
                _ => 2,
            };
            (rank, ix)
        })
        .collect();
    hits.sort_unstable();
    hits.into_iter().map(|(_, ix)| ix).collect()
}

/// The cursor after one step through `len` rows, wrapping at both ends.
pub(crate) fn step(cursor: usize, len: usize, forward: bool) -> usize {
    if len == 0 {
        return 0;
    }
    if forward {
        cursor.saturating_add(1).checked_rem(len).unwrap_or(0)
    } else if cursor == 0 {
        len.saturating_sub(1)
    } else {
        cursor.saturating_sub(1)
    }
}

/// What the palette asks its host to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaletteEvent {
    /// Go to the chosen entry (the host closes the palette).
    Choose(Target),
    /// Close without going anywhere.
    Dismiss,
}

/// The palette overlay.
pub(crate) struct Palette {
    input: Entity<InputState>,
    entries: Vec<Entry>,
    /// Indices into `entries`, in display order.
    shown: Vec<usize>,
    /// Row of `shown` the Enter key chooses.
    cursor: usize,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for Palette {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Palette")
            .field("entries", &self.entries.len())
            .field("cursor", &self.cursor)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<PaletteEvent> for Palette {}

impl Palette {
    /// Opens with an empty query and the input focused.
    pub(crate) fn new(window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let rules = rules::entity(cx);
        let entries = entries(rules.read(cx).model().rules());
        let shown = matches(&entries, "");
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(tr("palette.placeholder")));
        input.update(cx, |input, cx| input.focus(window, cx));
        let subscriptions = vec![cx.subscribe(&input, |this, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let query = input.read(cx).value();
                this.shown = matches(&this.entries, &query);
                this.cursor = 0;
                this.scroll.scroll_to_item(0);
                cx.notify();
            }
        })];
        Self {
            input,
            entries,
            shown,
            cursor: 0,
            scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    fn move_cursor(&mut self, forward: bool, cx: &mut Context<'_, Self>) {
        self.cursor = step(self.cursor, self.shown.len(), forward);
        self.scroll.scroll_to_item(self.cursor);
        cx.notify();
    }

    fn choose(&mut self, row: usize, cx: &mut Context<'_, Self>) {
        let target = self
            .shown
            .get(row)
            .and_then(|&ix| self.entries.get(ix))
            .map(|entry| entry.target);
        if let Some(target) = target {
            cx.emit(PaletteEvent::Choose(target));
        }
    }

    fn icon(target: Target) -> Icon {
        match target {
            Target::Page(category) => Icon::new(category.icon()),
            Target::Rule(_) => Icon::new(Category::Rules.icon()),
        }
    }
}

impl Render for Palette {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let rows: Vec<_> = self
            .shown
            .iter()
            .enumerate()
            .filter_map(|(row, &ix)| Some((row, self.entries.get(ix)?)))
            .map(|(row, entry)| {
                ui::ListRow::new(
                    gpui_kit::ElementId::Name(SharedString::from(format!("palette-row-{row}"))),
                    entry.label.clone(),
                )
                .icon(Self::icon(entry.target))
                .trailing(muted_cell(entry.group.clone(), cx))
                .current(row == self.cursor)
                .on_click(cx.listener(move |this, _, _, cx| this.choose(row, cx)))
                .into_any_element()
            })
            .collect();
        let empty = rows.is_empty();
        let theme = cx.theme();
        let panel = v_flex()
            .id("palette-panel")
            .key_context("Palette")
            .on_action(cx.listener(|this, _: &PaletteNext, _, cx| this.move_cursor(true, cx)))
            .on_action(cx.listener(|this, _: &PalettePrevious, _, cx| this.move_cursor(false, cx)))
            .on_action(cx.listener(|this, _: &PaletteConfirm, _, cx| this.choose(this.cursor, cx)))
            .on_action(cx.listener(|_, _: &PaletteCancel, _, cx| cx.emit(PaletteEvent::Dismiss)))
            // Clicks inside the panel must not reach the scrim, which dismisses.
            .on_click(|_, _, cx| cx.stop_propagation())
            .w(page::PALETTE_WIDTH)
            .gap(space::MD)
            .p(space::MD)
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius_lg)
            .when(theme.shadow, gpui_kit::Styled::shadow_lg)
            .child(ui::TextInput::new(&self.input).search())
            .child(
                div()
                    .id("palette-list")
                    .max_h(page::PALETTE_LIST_MAX_HEIGHT)
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(v_flex().w_full().children(rows))
                    .when(empty, |this| {
                        this.child(
                            div()
                                .p(space::LG)
                                .text_size(text::BODY)
                                .line_height(text::BODY_LINE_HEIGHT)
                                .text_color(theme.muted_foreground)
                                .child(tr("palette.empty")),
                        )
                    }),
            );
        div()
            .id("palette-scrim")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .on_click(cx.listener(|_, _, _, cx| cx.emit(PaletteEvent::Dismiss)))
            .child(
                v_flex()
                    .size_full()
                    .items_center()
                    .pt(page::PALETTE_TOP)
                    .child(panel),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Entry> {
        let group = |g: &str| SharedString::from(g.to_owned());
        vec![
            Entry::new(
                Target::Page(Category::Overview),
                "Overview".into(),
                group("General"),
            ),
            Entry::new(
                Target::Page(Category::DeveloperJunk),
                "Developer Junk".into(),
                group("Cleanup"),
            ),
            Entry::new(
                Target::Page(Category::Rules),
                "Rules".into(),
                group("Automation"),
            ),
            Entry::new(
                Target::Page(Category::Activity),
                "Activity".into(),
                group("Automation"),
            ),
            Entry::new(
                Target::Rule(3),
                "Clean old build output".into(),
                group("Rule"),
            ),
            Entry::new(Target::Rule(4), "Weekly rules review".into(), group("Rule")),
        ]
    }

    fn targets(entries: &[Entry], hits: &[usize]) -> Vec<Target> {
        hits.iter()
            .filter_map(|&ix| entries.get(ix))
            .map(|entry| entry.target)
            .collect()
    }

    #[test]
    fn an_empty_query_lists_everything_in_order() {
        let entries = sample();
        assert_eq!(
            matches(&entries, "  "),
            [0, 1, 2, 3, 4, 5],
            "blank queries do not filter"
        );
    }

    #[test]
    fn every_word_must_match_and_case_does_not_matter() {
        let entries = sample();
        assert_eq!(
            targets(&entries, &matches(&entries, "BUILD out")),
            [Target::Rule(3)],
            "words may appear anywhere in the label"
        );
        assert!(
            matches(&entries, "build zebra").is_empty(),
            "one missing word rejects the entry"
        );
        assert_eq!(
            targets(&entries, &matches(&entries, "automation")),
            [
                Target::Page(Category::Rules),
                Target::Page(Category::Activity)
            ],
            "the group is searchable too"
        );
    }

    #[test]
    fn label_prefixes_rank_before_mentions_before_group_matches() {
        let entries = sample();
        assert_eq!(
            targets(&entries, &matches(&entries, "rule")),
            [
                Target::Page(Category::Rules),
                Target::Rule(4),
                Target::Rule(3)
            ],
            "'Rules' starts with it, 'Weekly rules review' contains it, and the rule that \
             only sits in the group 'Rule' comes last"
        );
    }

    #[test]
    fn the_cursor_wraps_both_ways_and_survives_an_empty_list() {
        assert_eq!(step(0, 3, true), 1, "down");
        assert_eq!(step(2, 3, true), 0, "down from the last wraps to the first");
        assert_eq!(step(0, 3, false), 2, "up from the first wraps to the last");
        assert_eq!(step(1, 3, false), 0, "up");
        assert_eq!(step(5, 0, true), 0, "no rows, no movement");
    }
}
