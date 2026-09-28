//! Running helper programs (`plutil`, `launchctl`, `reg`, `powershell`, `dpkg-query`…):
//! no console window on Windows, a deadline, and captured output.

use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use crate::{Error, Result};

/// How often a running child is polled.
const POLL: Duration = Duration::from_millis(20);

/// A command that never opens a console window on Windows.
pub(crate) fn command(program: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut cmd = Command::new(program);
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd
    }
    #[cfg(not(windows))]
    {
        Command::new(program)
    }
}

/// Captured result of [`run`].
#[derive(Debug)]
pub(crate) struct Output {
    /// Exit status.
    pub(crate) status: ExitStatus,
    /// Standard output (lossy UTF-8).
    pub(crate) stdout: String,
    /// Standard error (lossy UTF-8).
    pub(crate) stderr: String,
}

impl Output {
    /// `stdout` when the command succeeded, else an [`Error::Command`].
    pub(crate) fn ok(self, program: &str) -> Result<String> {
        if self.status.success() {
            Ok(self.stdout)
        } else {
            Err(Error::Command {
                program: program.to_owned(),
                message: format!("{}: {}", self.status, self.stderr.trim()),
            })
        }
    }
}

/// Runs `program args…` with stdin closed, capturing output, killing it after `timeout`.
pub(crate) fn run(program: &str, args: &[&str], timeout: Duration) -> Result<Output> {
    let mut cmd = command(program);
    cmd.args(args);
    run_command(program, &mut cmd, timeout)
}

/// [`run`] for a prepared command.
pub(crate) fn run_command(program: &str, cmd: &mut Command, timeout: Duration) -> Result<Output> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| Error::Command {
        program: program.to_owned(),
        message: e.to_string(),
    })?;
    // Drain both pipes on threads so a chatty child never blocks on a full pipe.
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let status = wait_deadline(program, &mut child, timeout)?;
    Ok(Output {
        status,
        stdout: join(stdout),
        stderr: join(stderr),
    })
}

/// Waits for `child` until `timeout`, then kills it and fails.
pub(crate) fn wait_deadline(
    program: &str,
    child: &mut Child,
    timeout: Duration,
) -> Result<ExitStatus> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if start.elapsed() >= timeout {
            if let Err(err) = child.kill() {
                tracing::debug!(%err, program, "kill after timeout");
            }
            if let Err(err) = child.wait() {
                tracing::debug!(%err, program, "reap after timeout");
            }
            return Err(Error::Command {
                program: program.to_owned(),
                message: format!("timed out after {}s", timeout.as_secs()),
            });
        }
        std::thread::sleep(POLL);
    }
}

type Drain = Option<std::thread::JoinHandle<String>>;

fn drain(pipe: Option<impl std::io::Read + Send + 'static>) -> Drain {
    let mut pipe = pipe?;
    std::thread::Builder::new()
        .name("omc-cmd-pipe".to_owned())
        .spawn(move || {
            let mut buf = Vec::new();
            if let Err(err) = pipe.read_to_end(&mut buf) {
                tracing::debug!(%err, "reading child output");
            }
            String::from_utf8_lossy(&buf).into_owned()
        })
        .ok()
}

fn join(handle: Drain) -> String {
    handle.and_then(|h| h.join().ok()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn captures_output_and_times_out() {
        use super::*;

        let out = run(
            "sh",
            &["-c", "echo hi; echo err >&2"],
            Duration::from_secs(5),
        );
        assert!(
            out.is_ok_and(|o| o.status.success() && o.stdout == "hi\n" && o.stderr == "err\n"),
            "stdout and stderr captured"
        );
        let slow = run("sh", &["-c", "sleep 5"], Duration::from_millis(100));
        assert!(
            matches!(slow, Err(Error::Command { .. })),
            "a hung helper is killed"
        );
    }
}
