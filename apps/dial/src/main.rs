//! dial GUI process: gpui-kit viewport over the background `dial-daemon` (ADR 0008).
//! Wires dial-ui to a dial-ipc client once those crates have code.

use std::process::ExitCode;

fn main() -> ExitCode {
    if let Err(err) = dial_telemetry::init() {
        report_startup_error(&err);
        return ExitCode::FAILURE;
    }
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "dial starting");
    ExitCode::SUCCESS
}

#[expect(
    clippy::print_stderr,
    reason = "tracing failed to initialise, stderr is the only channel left"
)]
fn report_startup_error(err: &dial_telemetry::Error) {
    eprintln!("dial: {err}");
}
