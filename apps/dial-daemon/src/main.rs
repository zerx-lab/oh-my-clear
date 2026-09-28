//! dial-daemon: headless execution layer (ADR 0008). Owns sessions, agents, PTYs, worktrees
//! and the journal on one tokio runtime; UI processes attach over dial-ipc and may come and
//! go. Planned subcommands: `run` (default), `status`, `stop`, `logs`, `mcp` (stdio MCP
//! proxy for injected agents), `install-login-item`. Never links gpui (checked by
//! `cargo xtask layers`).

use std::process::ExitCode;

fn main() -> ExitCode {
    if let Err(err) = dial_telemetry::init() {
        report_startup_error(&err);
        return ExitCode::FAILURE;
    }
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "dial-daemon starting");
    ExitCode::SUCCESS
}

#[expect(
    clippy::print_stderr,
    reason = "tracing failed to initialise, stderr is the only channel left"
)]
fn report_startup_error(err: &dial_telemetry::Error) {
    eprintln!("dial-daemon: {err}");
}
