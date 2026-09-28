//! Tracing setup shared by both binaries (`oh-my-clear`, `oh-my-clear-daemon`): env filter, stderr sink, panic hook.
//!
//! Filter comes from `RUST_LOG` (dev default in `.cargo/config.toml`), falling back to
//! `info`. Events go to **stderr**: stdout is a protocol channel in `oh-my-clear-daemon`
//! (`READY` handshake). `log` records from dependencies (gpui) are
//! bridged into tracing. Internal deps: none. Architecture: docs/memory/decisions/0008-process-split-daemon-ipc.md

use std::backtrace::Backtrace;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid RUST_LOG filter: {0}")]
    Filter(#[from] tracing_subscriber::filter::FromEnvError),
    #[error("failed to install the global tracing subscriber: {0}")]
    Init(#[from] tracing_subscriber::util::TryInitError),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Installs the global subscriber and the panic hook. Call once, first thing in `main`.
pub fn init() -> Result<()> {
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env()?;
    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_thread_names(true),
        )
        .try_init()?;
    install_panic_hook();
    Ok(())
}

/// Routes panics (ours are lint-banned; dependencies may still panic) through tracing
/// with a forced backtrace, so they land in the same sink as every other event.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let backtrace = Backtrace::force_capture();
        tracing::error!(target: "oh_my_clear::panic", %info, %backtrace, "panic");
    }));
}
