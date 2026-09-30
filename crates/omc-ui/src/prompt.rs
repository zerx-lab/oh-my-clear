//! The prompt window (ADR 0024): a compact window that asks the user what to do with an
//! automation run waiting for a decision — Clean now, Snooze or Skip this time.
//!
//! Two ways in, one window:
//! - the daemon launches `oh-my-clear --prompt <run>` when no UI is attached; that process
//!   opens only this window ([`Launch::Prompt`], [`open_prompt_window`]);
//! - an attached UI receives `Event::Prompt` and opens the same window ([`open`]).
//!
//! There is at most one window per run (a second request focuses it). On every connect the
//! UI also prompts for runs asking right now that it has not prompted for yet, so a prompt
//! that raced a starting UI is not lost — once per run per process.

use std::collections::{HashMap, HashSet};

use gpui_kit::BorrowAppContext as _;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AnyWindowHandle, App, AppContext as _, Context, Div, Entity, FocusHandle,
    FontWeight, Global, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Window, WindowKind, div,
};
use omc_proto::rules::{Decision, RuleRun, RunId, RunState};

use crate::nav::Category;
use crate::pages::widgets::runs::{item_rows, run_summary, snooze_select, state_label, state_tone};
use crate::pages::widgets::{self, tr};
use crate::rules::{self, Rules, RulesEvent};
use crate::title_bar::AppTitleBar;
use crate::tokens::{chrome, space, text};
use crate::ui;
use crate::window::window_options;
use crate::{Error, Result};

/// Items listed in the window (the largest first; the summary counts all of them).
const SHOWN_ITEMS: usize = 5;

/// What the process was asked to do at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launch {
    /// The normal start: the main window.
    Main,
    /// `--prompt <run>`: only the prompt window of that run.
    Prompt(u64),
}

/// The result of [`parse_args`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    /// What to start.
    pub launch: Launch,
    /// Arguments that were not understood and are ignored (the caller warns about them).
    pub ignored: Vec<String>,
}

/// Reads the command line (without the program name). Only `--prompt <run>` and
/// `--prompt=<run>` mean something; anything else, including a missing or non-numeric run
/// id, is collected in [`Parsed::ignored`] and the normal start applies.
pub fn parse_args<I, S>(args: I) -> Parsed
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut launch = Launch::Main;
    let mut ignored = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let arg = arg.as_ref();
        let (value, spelled) = if arg == "--prompt" {
            let value = args.next().map(|value| value.as_ref().to_owned());
            let spelled = match &value {
                Some(value) => format!("{arg} {value}"),
                None => arg.to_owned(),
            };
            (value, spelled)
        } else if let Some(value) = arg.strip_prefix("--prompt=") {
            (Some(value.to_owned()), arg.to_owned())
        } else {
            ignored.push(arg.to_owned());
            continue;
        };
        match value.as_deref().map(str::parse::<u64>) {
            Some(Ok(run)) => launch = Launch::Prompt(run),
            _ => ignored.push(spelled),
        }
    }
    Parsed { launch, ignored }
}

/// Which prompt windows exist and which runs this process already prompted for.
#[derive(Debug)]
struct Book<H> {
    open: HashMap<RunId, H>,
    seen: HashSet<RunId>,
}

impl<H> Default for Book<H> {
    fn default() -> Self {
        Self {
            open: HashMap::new(),
            seen: HashSet::new(),
        }
    }
}

impl<H: Copy> Book<H> {
    /// Marks `run` as prompted and returns its window, if one is open.
    fn claim(&mut self, run: RunId) -> Option<H> {
        self.seen.insert(run);
        self.open.get(&run).copied()
    }

    /// `true` the first time `run` is offered; later offers (a reconnect listing the same
    /// run) are refused, even after its window was closed.
    fn first_sight(&mut self, run: RunId) -> bool {
        self.seen.insert(run)
    }

    fn opened(&mut self, run: RunId, handle: H) {
        self.open.insert(run, handle);
    }

    fn closed(&mut self, run: RunId) {
        self.open.remove(&run);
    }
}

struct Prompts(Book<AnyWindowHandle>);

impl Global for Prompts {}

fn with_book<R>(cx: &mut App, f: impl FnOnce(&mut Book<AnyWindowHandle>) -> R) -> R {
    if !cx.has_global::<Prompts>() {
        cx.set_global(Prompts(Book::default()));
    }
    cx.update_global(|prompts: &mut Prompts, _| f(&mut prompts.0))
}

/// The subscription that prompts for pending runs after a connect.
struct Watch {
    _subscription: Subscription,
}

impl Global for Watch {}

/// Follows the rules store: after every (re)load, opens a prompt for each run that asks
/// right now and was not prompted for yet. Idempotent.
pub(crate) fn attach(cx: &mut App) {
    if cx.has_global::<Watch>() {
        return;
    }
    let rules = rules::entity(cx);
    let subscription = cx.subscribe(&rules, |rules, event: &RulesEvent, cx| {
        if *event != RulesEvent::Loaded {
            return;
        }
        let asking: Vec<RunId> = rules
            .read(cx)
            .model()
            .pending()
            .into_iter()
            .filter(|run| matches!(run.state, RunState::Pending { until: None }))
            .map(|run| run.id)
            .collect();
        for run in asking {
            if with_book(cx, |book| book.first_sight(run))
                && let Err(err) = open_window(run, cx)
            {
                tracing::error!(run, "failed to open the prompt window: {err}");
            }
        }
    });
    cx.set_global(Watch {
        _subscription: subscription,
    });
}

/// Opens the prompt window of `run`, or brings its open one forward.
pub(crate) fn open(run: RunId, cx: &mut App) {
    if let Err(err) = open_or_focus(run, cx) {
        tracing::error!(run, "failed to open the prompt window: {err}");
    }
}

/// Starts the prompt-only process mode: the prompt window of `run` and nothing else.
pub fn open_prompt_window(run: u64, cx: &mut App) -> Result<()> {
    open_or_focus(run, cx)
}

fn open_or_focus(run: RunId, cx: &mut App) -> Result<()> {
    attach(cx);
    if let Some(handle) = with_book(cx, |book| book.claim(run))
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        cx.activate(true);
        return Ok(());
    }
    open_window(run, cx)
}

fn open_window(run: RunId, cx: &mut App) -> Result<()> {
    // Above the other windows (a pop-up) and not one to resize or park in the Dock; a
    // platform without pop-up windows gets a normal one.
    let mut opened = None;
    for kind in [WindowKind::PopUp, WindowKind::Normal] {
        let mut options = window_options(chrome::PROMPT_WINDOW, chrome::PROMPT_WINDOW_MIN, cx);
        options.kind = kind.clone();
        options.is_resizable = false;
        options.is_minimizable = false;
        match gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| PromptView::new(run, window, cx))
        }) {
            Ok((handle, _)) => {
                opened = Some(handle);
                break;
            }
            Err(err) => tracing::warn!(run, ?kind, "prompt window not opened: {err:#}"),
        }
    }
    let handle = opened.ok_or_else(|| Error::Window {
        window: "prompt",
        message: "no window kind could be opened".to_owned(),
    })?;
    with_book(cx, |book| book.opened(run, handle));
    cx.activate(true);
    Ok(())
}

/// The prompt window's root view.
struct PromptView {
    run: RunId,
    rules: Entity<Rules>,
    focus_handle: FocusHandle,
    /// The answer was sent; the window closes when the daemon accepts it.
    deciding: bool,
    /// The daemon has no such run.
    missing: bool,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for PromptView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PromptView")
            .field("run", &self.run)
            .finish_non_exhaustive()
    }
}

impl PromptView {
    fn new(run: RunId, window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let rules = rules::entity(cx);
        let subscriptions = vec![
            cx.observe_window_appearance(window, |_, window, cx| {
                crate::theme::system_appearance_changed(window.appearance(), cx);
            }),
            cx.observe(&rules, |this, rules, cx| {
                if rules.read(cx).error().is_some() {
                    this.deciding = false;
                }
                cx.notify();
            }),
            cx.subscribe_in(
                &rules,
                window,
                move |this, _, event: &RulesEvent, window, cx| match *event {
                    RulesEvent::Decided(id) if id == this.run => window.remove_window(),
                    RulesEvent::RunMissing(id) if id == this.run => {
                        this.missing = true;
                        cx.notify();
                    }
                    _ => {}
                },
            ),
            cx.on_release(move |_, cx| with_book(cx, |book| book.closed(run))),
        ];
        rules.update(cx, |_, cx| Rules::load_run(run, cx));
        Self {
            run,
            rules,
            focus_handle,
            deciding: false,
            missing: false,
            _subscriptions: subscriptions,
        }
    }

    fn decide(&mut self, decision: Decision, cx: &mut Context<'_, Self>) {
        let run = self.run;
        self.deciding = true;
        self.rules
            .update(cx, |_, cx| Rules::decide(run, decision, cx));
        cx.notify();
    }

    fn open_main_window(cx: &mut App) {
        cx.defer(|cx| crate::main_view::show_in_main_window(Category::Activity, cx));
    }

    fn heading(run: &RuleRun, now: i64, cx: &App) -> AnyElement {
        let theme = cx.theme();
        v_flex()
            .w_full()
            .gap(space::XS)
            .child(
                h_flex()
                    .w_full()
                    .gap(space::MD)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(text::SECTION)
                            .line_height(text::SECTION_LINE_HEIGHT)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(run.rule_name.clone()),
                    )
                    .child(
                        ui::Badge::new(state_label(&run.state, now)).tone(state_tone(&run.state)),
                    ),
            )
            .child(
                div()
                    .text_size(text::BODY)
                    .line_height(text::BODY_LINE_HEIGHT)
                    .text_color(theme.muted_foreground)
                    .child(if run.state.awaits_user() {
                        tr("prompt.body")
                    } else {
                        tr("prompt.settled")
                    }),
            )
            .into_any_element()
    }

    fn summary(run: &RuleRun) -> AnyElement {
        h_flex()
            .w_full()
            .gap(space::XXL)
            .child(ui::Stat::new(tr("prompt.size"), crate::format::bytes(run.bytes)).hero())
            .child(ui::Stat::new(
                tr("prompt.items"),
                crate::format::count(run.item_count),
            ))
            .into_any_element()
    }

    fn open_button(cx: &mut Context<'_, Self>) -> ui::Button {
        ui::Button::new("prompt-open", tr("prompt.open"))
            .ghost()
            .on_click(cx.listener(|_, _, _, cx| Self::open_main_window(cx)))
    }

    fn render_ask(&self, run: &RuleRun, connected: bool, cx: &mut Context<'_, Self>) -> AnyElement {
        let blocked = self.deciding || !connected;
        h_flex()
            .w_full()
            .gap(space::MD)
            .child(Self::open_button(cx))
            .child(div().flex_1())
            .child(
                ui::Button::new("prompt-skip", tr("runs.skip"))
                    .ghost()
                    .disabled(blocked)
                    .on_click(cx.listener(|this, _, _, cx| this.decide(Decision::Skip, cx))),
            )
            .child(snooze_select(
                "prompt-snooze",
                false,
                blocked,
                cx.listener(|this, decision: &Decision, _, cx| this.decide(*decision, cx)),
            ))
            .child(
                ui::Button::new("prompt-clean", tr("runs.clean_now"))
                    .primary()
                    .loading(self.deciding)
                    .disabled(blocked)
                    .on_click(cx.listener(|this, _, _, cx| this.decide(Decision::CleanNow, cx)))
                    .tooltip(run_summary(run)),
            )
            .into_any_element()
    }

    fn render_close(cx: &mut Context<'_, Self>) -> AnyElement {
        h_flex()
            .w_full()
            .gap(space::MD)
            .child(Self::open_button(cx))
            .child(div().flex_1())
            .child(
                ui::Button::new("prompt-close", tr("prompt.close"))
                    .primary()
                    .on_click(|_, window, _| window.remove_window()),
            )
            .into_any_element()
    }

    /// The window body below the titlebar for the run's current state.
    fn render_content(
        &self,
        run: Option<RuleRun>,
        connected: bool,
        error: Option<SharedString>,
        cx: &mut Context<'_, Self>,
    ) -> Div {
        let content = v_flex().size_full().gap(space::XL).p(space::XXL);
        let Some(run) = run else {
            let message = if self.missing {
                tr("prompt.missing")
            } else {
                tr("prompt.loading")
            };
            return content
                .child(
                    div()
                        .text_size(text::BODY)
                        .line_height(text::BODY_LINE_HEIGHT)
                        .text_color(cx.theme().muted_foreground)
                        .child(message),
                )
                .when(self.missing, |this| {
                    this.child(div().flex_1()).child(Self::render_close(cx))
                });
        };
        let footer = if run.state.awaits_user() {
            self.render_ask(&run, connected, cx)
        } else {
            Self::render_close(cx)
        };
        content
            .child(Self::heading(&run, crate::format::now(), cx))
            .child(Self::summary(&run))
            .when(!run.items.is_empty(), |this| {
                this.child(
                    ui::Card::new()
                        .flush()
                        .child(widgets::card_body().children(item_rows(
                            "prompt-item",
                            &run,
                            SHOWN_ITEMS,
                            false,
                            cx,
                        ))),
                )
            })
            .children(error.map(|error| {
                widgets::notice(
                    widgets::Tone::Danger,
                    tr("scan.error"),
                    Some(error),
                    Vec::new(),
                    cx,
                )
            }))
            .child(div().flex_1())
            .child(footer)
    }
}

impl Render for PromptView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let (run, connected, error) = {
            let store = self.rules.read(cx);
            (
                store.model().run(self.run).cloned(),
                store.is_connected(),
                store.error().cloned(),
            )
        };
        let content = self.render_content(run, connected, error, cx);
        div()
            .id("prompt-window")
            .key_context("Prompt")
            .track_focus(&self.focus_handle)
            .relative()
            .size_full()
            .bg(cx.theme().background)
            .child(
                v_flex()
                    .size_full()
                    // Under the titlebar.
                    .child(div().flex_none().h(chrome::TITLE_BAR_HEIGHT))
                    .child(content),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_flag_takes_a_run_id_in_either_spelling() {
        assert_eq!(
            parse_args(["--prompt", "42"]),
            Parsed {
                launch: Launch::Prompt(42),
                ignored: Vec::new()
            },
            "separate value"
        );
        assert_eq!(
            parse_args(["--prompt=7"]).launch,
            Launch::Prompt(7),
            "inline value"
        );
        assert_eq!(
            parse_args(Vec::<String>::new()).launch,
            Launch::Main,
            "no arguments start the main window"
        );
    }

    #[test]
    fn unknown_or_invalid_arguments_are_ignored_and_reported() {
        let parsed = parse_args(["-psn_0_1234", "--prompt", "soon", "--verbose"]);
        assert_eq!(
            parsed.launch,
            Launch::Main,
            "an invalid run id does not start a prompt"
        );
        assert_eq!(
            parsed.ignored,
            ["-psn_0_1234", "--prompt soon", "--verbose"],
            "each ignored argument is reported"
        );
        assert_eq!(
            parse_args(["--prompt"]).ignored,
            ["--prompt"],
            "a flag without a value"
        );
        assert_eq!(
            parse_args(["--prompt=-1"]).launch,
            Launch::Main,
            "a negative id is invalid"
        );
        let mixed = parse_args(["--bogus", "--prompt", "9"]);
        assert_eq!(
            (mixed.launch, mixed.ignored),
            (Launch::Prompt(9), vec!["--bogus".to_owned()]),
            "a valid prompt survives unknown neighbours"
        );
    }

    #[test]
    fn a_run_is_prompted_once_per_process_but_its_open_window_is_found() {
        let mut book: Book<u32> = Book::default();
        assert!(book.first_sight(5), "the first listing prompts");
        assert!(
            !book.first_sight(5),
            "a reconnect listing it again does not"
        );
        assert_eq!(book.claim(5), None, "no window yet");
        book.opened(5, 77);
        assert_eq!(
            book.claim(5),
            Some(77),
            "an open window is focused, not duplicated"
        );
        book.closed(5);
        assert_eq!(book.claim(5), None, "a closed window is gone");
        assert!(
            !book.first_sight(5),
            "and closing it does not make the connect-time prompt fire again"
        );
        assert!(book.first_sight(6), "another run still prompts");
    }

    #[gpui_kit::test]
    fn a_run_gets_one_window_that_closes_once_the_daemon_accepts_the_answer(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let init = cx.update(crate::init);
        assert!(init.is_ok(), "UI initialises headless: {init:?}");
        for run in [7, 7, 8] {
            let opened = cx.update(|cx| open_prompt_window(run, cx));
            assert!(opened.is_ok(), "the prompt window opens: {opened:?}");
            cx.run_until_parked();
        }
        assert_eq!(
            cx.update(|cx| cx.windows().len()),
            2,
            "asking twice for run 7 focuses its window; run 8 gets its own"
        );

        let store = cx.update(rules::entity);
        cx.update(|cx| {
            store.update(cx, |_, cx| cx.emit(RulesEvent::Decided(7)));
        });
        cx.run_until_parked();
        assert_eq!(
            cx.update(|cx| cx.windows().len()),
            1,
            "the answered run's window closes, the other stays"
        );

        let reopened = cx.update(|cx| open_prompt_window(7, cx));
        assert!(
            reopened.is_ok(),
            "the run can be prompted again: {reopened:?}"
        );
        cx.run_until_parked();
        assert_eq!(
            cx.update(|cx| cx.windows().len()),
            2,
            "a closed prompt is not remembered as open"
        );
    }
}
