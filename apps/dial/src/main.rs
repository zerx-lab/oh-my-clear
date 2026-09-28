//! dial GUI process: gpui-kit viewport over the background `dial-daemon` (ADR 0008).
//! Wires dial-ui to a dial-ipc client once those crates have code.

use std::process::ExitCode;

use gpui_kit::App;

fn main() -> ExitCode {
    if let Err(err) = dial_telemetry::init() {
        report_startup_error(&err);
        return ExitCode::FAILURE;
    }
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "dial starting");

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            if let Err(err) = start(cx) {
                tracing::error!("dial failed to start: {err}");
                cx.quit();
            }
        });
    ExitCode::SUCCESS
}

fn start(cx: &mut App) -> dial_ui::Result<()> {
    dial_ui::init(cx)?;
    // The UI is a viewport: closing its last window ends the process; the daemon (and
    // every agent) keeps running and a new UI reattaches.
    cx.on_window_closed(|cx, _| {
        if cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();
    dial_ui::open_main_window(cx)?;
    cx.activate(true);
    Ok(())
}

#[expect(
    clippy::print_stderr,
    reason = "tracing failed to initialise, stderr is the only channel left"
)]
fn report_startup_error(err: &dial_telemetry::Error) {
    eprintln!("dial: {err}");
}
