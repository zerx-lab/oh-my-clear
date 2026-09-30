//! The tray's Open and the prompt handoff of automation runs (ADR 0024): bring an attached
//! UI forward ([`Event::Activate`]) or show the prompt for a run ([`Event::Prompt`]), or
//! launch the GUI that belongs to this daemon ([`omc_ipc::layout`]; with `--prompt <run>` it
//! opens only that run's prompt window). The GUI exits with its last window, so a closed
//! UI costs no memory; this is how it comes back.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use omc_engine::Engine;
use omc_ipc::RuntimeDir;
use omc_proto::Event;
use omc_proto::rules::RunId;

/// A launched UI that has not attached yet counts as starting for this long: repeated
/// clicks meanwhile do not start a second one.
const LAUNCH_GRACE: Duration = Duration::from_secs(20);

#[derive(Debug)]
pub(crate) struct UiLauncher {
    exe: Option<PathBuf>,
    dir: RuntimeDir,
    starting: Option<(Instant, tokio::process::Child)>,
}

impl UiLauncher {
    /// The launcher for the GUI that belongs to the daemon at `daemon_exe`.
    pub(crate) fn new(daemon_exe: &Path, dir: RuntimeDir) -> Self {
        Self {
            exe: omc_ipc::layout::ui_exe(daemon_exe),
            dir,
            starting: None,
        }
    }

    /// Shows the UI: activates every attached one, else launches one unless a launch is
    /// still starting.
    pub(crate) fn open(&mut self, engine: &Engine) {
        if engine.notify_ui(Event::Activate) > 0 {
            self.starting = None;
            return;
        }
        self.launch_unless_starting(&[]);
    }

    /// Asks for the user's decision on `run`: every attached UI opens the prompt window;
    /// with none attached the GUI is launched with `--prompt <run>`. A UI that is already
    /// starting is not raced: on start every GUI asks the daemon for the runs waiting for
    /// an answer, so no prompt is lost.
    pub(crate) fn prompt(&mut self, engine: &Engine, run: RunId) {
        if engine.notify_ui(Event::Prompt { run }) > 0 {
            self.starting = None;
            return;
        }
        self.launch_unless_starting(&[OsString::from("--prompt"), OsString::from(run.to_string())]);
    }

    fn launch_unless_starting(&mut self, args: &[OsString]) {
        if let Some((since, child)) = &mut self.starting
            && since.elapsed() < LAUNCH_GRACE
            && matches!(child.try_wait(), Ok(None))
        {
            tracing::info!("the UI is still starting");
            return;
        }
        match self.launch(args) {
            Ok(child) => self.starting = Some((Instant::now(), child)),
            Err(err) => tracing::error!(exe = ?self.exe, "cannot launch the UI: {err}"),
        }
    }

    fn launch(&self, args: &[OsString]) -> std::io::Result<tokio::process::Child> {
        let Some(exe) = self.exe.as_deref().filter(|exe| exe.is_file()) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "the oh-my-clear executable is not next to the daemon",
            ));
        };
        let log = std::fs::File::create(self.dir.ui_log_file())?;
        let child = omc_ipc::spawn_detached(|| {
            let mut cmd = tokio::process::Command::new(exe);
            cmd.args(args)
                .current_dir(self.dir.path())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::from(log.try_clone()?))
                // Inherited when `cargo run` started the UI that spawned this daemon; a UI
                // launched from here must not rebuild the daemon under cargo.
                .env_remove("CARGO_PKG_NAME");
            Ok(cmd)
        })?;
        tracing::info!(exe = %exe.display(), pid = ?child.id(), ?args, "launched the UI");
        Ok(child)
    }
}
