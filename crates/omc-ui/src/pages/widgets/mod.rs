//! Shared building blocks of the area pages.
//!
//! - [`flow`]: the scan → review → clean job lifecycle ([`Flow`] + [`FlowHost`]), stream
//!   coalescing ([`Throttle`]) and daemon-epoch tracking ([`Connection`]).
//! - [`parts`]: stateless elements — header, notices, job progress, size bars, row cells,
//!   the clean confirmation and the clean report, and the idle/empty/results card scaffolding
//!   every area page shares.
//! - [`slots`]: single-job tracking ([`slots::JobSlot`] + [`slots::SlotHost`]) for pages
//!   that follow independent jobs (app list, app files, uninstall, startup changes).

pub(crate) mod flow;
pub(crate) mod parts;
pub(crate) mod runs;
pub(crate) mod slots;

pub(crate) use flow::{ConnChange, Connection, Flow, FlowHost, FlowPhase, Throttle, release};
pub(crate) use parts::{
    CleanConfirm, Counters, OnClick, Removal, Tone, area_header, card_body, check, chip,
    clean_button, clean_report, column_header, confirm_clean, connection_notice, denied_notice,
    empty_card, error_notice, fraction, idle_card, idle_scan_button, job_progress, list_frame,
    muted_cell, notice, page_column, phase_label, privacy_button, rescan_button, size_bar,
    size_cell, tr,
};

/// Opens pages headless (no daemon: the engine reports it unavailable).
#[cfg(test)]
pub(crate) mod test_support {
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Context, Entity, Render, TestAppContext, Window,
    };

    use crate::tokens::chrome;

    /// Initialises the UI layer and opens a window showing the page `build` creates.
    pub(crate) fn open_page<V: Render + 'static>(
        cx: &mut TestAppContext,
        build: impl FnOnce(&mut Window, &mut Context<'_, V>) -> V + 'static,
    ) -> Option<(AnyWindowHandle, Entity<V>)> {
        let init = cx.update(crate::init);
        assert!(init.is_ok(), "UI initialises headless: {init:?}");
        let opened = cx.update(|cx| {
            let options = crate::window_options(chrome::MAIN_WINDOW, chrome::MAIN_WINDOW_MIN, cx);
            gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| build(window, cx)))
        });
        assert!(
            opened.is_ok(),
            "the page opens: {:?}",
            opened.as_ref().err()
        );
        cx.run_until_parked();
        opened.ok()
    }
}
