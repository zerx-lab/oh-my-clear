//! The tray's Open: brings an attached UI forward ([`Event::Activate`]) or launches the GUI
//! that belongs to this daemon ([`omc_ipc::layout`]). The GUI exits with its last window,
//! so a closed UI costs no memory; this is how it comes back.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use omc_engine::Engine;
use omc_ipc::RuntimeDir;
use omc_proto::Event;

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
        if let Some((since, child)) = &mut self.starting
            && since.elapsed() < LAUNCH_GRACE
            && matches!(child.try_wait(), Ok(None))
        {
            tracing::info!("the UI is still starting");
            return;
        }
        match self.launch() {
            Ok(child) => self.starting = Some((Instant::now(), child)),
            Err(err) => tracing::error!(exe = ?self.exe, "cannot launch the UI: {err}"),
        }
    }

    fn launch(&self) -> std::io::Result<tokio::process::Child> {
        let Some(exe) = self.exe.as_deref().filter(|exe| exe.is_file()) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "the oh-my-clear executable is not next to the daemon",
            ));
        };
        let log = std::fs::File::create(self.dir.ui_log_file())?;
        let child = omc_ipc::spawn_detached(|| {
            let mut cmd = tokio::process::Command::new(exe);
            cmd.current_dir(self.dir.path())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::from(log.try_clone()?))
                // Inherited when `cargo run` started the UI that spawned this daemon; a UI
                // launched from here must not rebuild the daemon under cargo.
                .env_remove("CARGO_PKG_NAME");
            Ok(cmd)
        })?;
        tracing::info!(exe = %exe.display(), pid = ?child.id(), "launched the UI");
        Ok(child)
    }
}
