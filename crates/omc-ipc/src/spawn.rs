//! Detached process launch, shared by the UI (spawning the daemon) and the daemon
//! (launching the UI from the tray). No `unsafe`, no `daemonize` (ADR 0008).

/// Spawns the command `build` returns so that it outlives this process and its signals:
/// its own process group on Unix; on Windows no console, a new process group, and out of
/// the caller's job object when the job allows it (`build` runs again for the retry).
/// Dropping the returned child leaves the process running; tokio reaps it if it exits
/// while this process lives.
pub fn spawn_detached(
    build: impl Fn() -> std::io::Result<tokio::process::Command>,
) -> std::io::Result<tokio::process::Child> {
    #[cfg(windows)]
    {
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
        let mut cmd = build()?;
        cmd.kill_on_drop(false)
            .creation_flags(base | CREATE_BREAKAWAY_FROM_JOB);
        match cmd.spawn() {
            Ok(child) => return Ok(child),
            // The job we run in may forbid breakaway.
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                tracing::debug!("breakaway from job denied; spawning inside it");
            }
            Err(err) => return Err(err),
        }
        let mut cmd = build()?;
        cmd.kill_on_drop(false).creation_flags(base);
        cmd.spawn()
    }
    #[cfg(not(windows))]
    {
        let mut cmd = build()?;
        cmd.kill_on_drop(false);
        #[cfg(unix)]
        cmd.process_group(0);
        cmd.spawn()
    }
}
