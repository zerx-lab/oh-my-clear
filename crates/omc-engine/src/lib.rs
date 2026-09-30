//! Daemon core: the connection router behind `oh-my-clear-daemon`.
//!
//! Linked only by apps/oh-my-clear-daemon; never by the UI. Allowed internal deps: `LAYERS` in xtask/src/layers.rs. Architecture: docs/memory/decisions/0008-process-split-daemon-ipc.md
//!
//! [`Engine::serve`] serves authenticated [`omc_ipc::server::Connection`]s. Every outgoing
//! frame goes through one bounded per-connection queue drained by a writer task. Requests:
//!
//! - meta: `ping`, `shutdown` (and a stray `hello` → `bad_request`);
//! - `system_info` (omc-apps, on a blocking thread);
//! - `get_settings` / `put_settings`: the settings store (`settings.rs`, persisted as
//!   `settings.toml` in the config dir, see [`default_settings_path`]); a put is validated,
//!   written atomically on a blocking thread and announced to every UI with
//!   `settings_changed`;
//! - `start_job`, `job_status`, `job_result`, `cancel_job`, `release_job`, `space_children`:
//!   the job manager (`jobs.rs`). Every job reads a settings snapshot when it starts and
//!   runs on a blocking thread (omc-scan scanners, omc-apps inventory/removal); scans run
//!   concurrently, mutating jobs (`clean`, `uninstall`, `change_startup`) one at a time.
//!   Progress is pushed to UIs as `job` events (at most 10 per second per job, only on
//!   change) plus exactly one final update; outputs are retained until released (at most
//!   64 finished jobs, oldest evicted first).
//!
//! - `list_rules`, `put_rule`, `delete_rule`, `run_rule`, `list_runs`, `get_run`,
//!   `decide_run`: automation rules (`rules/`, ADR 0024). Rules, per-rule last runs and a
//!   bounded activity history persist as `rules.toml` next to `settings.toml` (see
//!   [`default_rules_path`]). [`Engine::start_automation`] starts the wall-clock
//!   scheduler; a due run scans through the ordinary scan job, filters, then cleans
//!   through the ordinary clean job (`Confirm::Auto`) or waits for the user's decision
//!   (`Confirm::Ask`). The daemon shell follows runs through [`Engine::subscribe_prompts`]
//!   (a run needs a decision now) and [`Engine::automation_status`] (pending runs for the
//!   tray, whether the daemon must stay resident).
//!
//! Nothing that touches the disk runs on a connection task.

mod engine;
mod jobs;
mod rules;
mod settings;

pub use engine::{Engine, EngineConfig};
pub use rules::{AutomationStatus, PendingRun, default_rules_path};
pub use settings::default_settings_path;

/// Engine failures.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Reading a frame from the client failed (I/O, oversized or malformed frame).
    #[error("ipc: {0}")]
    Ipc(#[from] omc_ipc::Error),
    /// A persisted file (settings, rules) could not be written.
    #[error("file {}: {source}", path.display())]
    FileIo {
        /// The file (or its temp sibling's target).
        path: std::path::PathBuf,
        /// The I/O error.
        source: std::io::Error,
    },
    /// The settings could not be encoded as TOML.
    #[error("encode settings: {0}")]
    SettingsEncode(#[from] toml::ser::Error),
    /// The rules could not be encoded as TOML.
    #[error("encode rules: {0}")]
    RulesEncode(toml::ser::Error),
}

/// Result alias of this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;
