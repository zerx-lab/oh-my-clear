//! Per-user runtime directory (socket, `daemon.lock`, `endpoint.json`, `daemon.log`), the
//! single-instance lock, the endpoint file and ids (ADR 0008).

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// `sun_path` holds 104 bytes on macOS/BSD (108 on Linux); stay under the smaller one.
#[cfg(unix)]
const MAX_SOCKET_PATH: usize = 103;

/// How long [`DaemonLock::acquire`] retries a held lock: a previous holder's fd can
/// outlive it for a few milliseconds in a forked child (fork → exec window).
const LOCK_RETRY: Duration = Duration::from_millis(200);
const LOCK_RETRY_STEP: Duration = Duration::from_millis(20);

/// The private per-user directory the daemon and its clients meet in.
#[derive(Debug, Clone)]
pub struct RuntimeDir {
    path: PathBuf,
    #[cfg(unix)]
    uid: u32,
}

impl RuntimeDir {
    /// Resolves, creates (`0700`) and verifies the runtime directory:
    /// - macOS: `~/Library/Application Support/dev.zerx.oh-my-clear/run`, else `$TMPDIR/oh-my-clear`;
    /// - other unix: `$XDG_RUNTIME_DIR/oh-my-clear`, else `/tmp/oh-my-clear-<user>`;
    /// - Windows: `%LOCALAPPDATA%\oh-my-clear\run`.
    ///
    /// On unix the first candidate whose socket path fits `sun_path` wins, and the
    /// directory must be a real directory owned by this user without group/other access.
    pub fn resolve() -> Result<Self> {
        #[cfg(unix)]
        {
            let candidates = unix_candidates();
            let path = candidates
                .into_iter()
                .find(|dir| dir.join(SOCKET_NAME).as_os_str().len() <= MAX_SOCKET_PATH)
                .ok_or(Error::NoRuntimeDir(
                    "no candidate fits the socket path limit",
                ))?;
            Self::at(path)
        }
        #[cfg(windows)]
        {
            let base = std::env::var_os("LOCALAPPDATA")
                .ok_or(Error::NoRuntimeDir("LOCALAPPDATA is not set"))?;
            Self::at(PathBuf::from(base).join("oh-my-clear").join("run"))
        }
    }

    /// Uses `path` as the runtime directory, creating and verifying it like [`resolve`](Self::resolve).
    pub fn at(path: PathBuf) -> Result<Self> {
        #[cfg(unix)]
        {
            let uid = ensure_private_dir(&path)?;
            Ok(Self { path, uid })
        }
        #[cfg(windows)]
        {
            fs::create_dir_all(&path)?;
            Ok(Self { path })
        }
    }

    /// The directory.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Where a spawned daemon's stderr (its log) goes.
    pub fn log_file(&self) -> PathBuf {
        self.path.join("daemon.log")
    }

    pub(crate) fn endpoint_file(&self) -> PathBuf {
        self.path.join("endpoint.json")
    }

    #[cfg(unix)]
    pub(crate) fn socket_path(&self) -> PathBuf {
        self.path.join(SOCKET_NAME)
    }

    /// Owner uid; peers must match it.
    #[cfg(unix)]
    pub(crate) fn uid(&self) -> u32 {
        self.uid
    }
}

#[cfg(unix)]
const SOCKET_NAME: &str = "d.sock";

#[cfg(unix)]
fn unix_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if cfg!(target_os = "macos") {
        if let Some(home) = std::env::var_os("HOME") {
            out.push(
                PathBuf::from(home)
                    .join("Library/Application Support/dev.zerx.oh-my-clear")
                    .join("run"),
            );
        }
        if let Some(tmp) = std::env::var_os("TMPDIR") {
            out.push(PathBuf::from(tmp).join("oh-my-clear"));
        }
    } else if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR") {
        out.push(PathBuf::from(xdg).join("oh-my-clear"));
    }
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "user".to_owned());
    out.push(std::env::temp_dir().join(format!("oh-my-clear-{user}")));
    out
}

/// Creates `dir` with `0700` if missing and checks it the way tmux does: a real directory,
/// owned by the current user, no group/other permissions. Returns the owner uid.
#[cfg(unix)]
fn ensure_private_dir(dir: &Path) -> Result<u32> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};

    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(err.into()),
    }
    let unsafe_dir = |reason| Error::UnsafeRuntimeDir {
        path: dir.to_owned(),
        reason,
    };
    let meta = fs::symlink_metadata(dir)?;
    if !meta.file_type().is_dir() {
        return Err(unsafe_dir("not a directory"));
    }
    // A file we create carries our effective uid; std has no safe `geteuid`. Creating it
    // also fails outright in a 0700 directory owned by someone else.
    let probe = dir.join(format!(
        ".owner-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let own_uid = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .and_then(|file| file.metadata())
        .map(|meta| meta.uid());
    if let Err(err) = fs::remove_file(&probe) {
        tracing::debug!(path = %probe.display(), "could not remove ownership probe: {err}");
    }
    let own_uid = own_uid?;
    if meta.uid() != own_uid {
        return Err(unsafe_dir("owned by another user"));
    }
    if meta.mode() & 0o077 != 0 {
        // Ours but too open (e.g. created by an older build): tighten it.
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(own_uid)
}

/// Exclusive single-daemon lock on `daemon.lock`, held for the daemon's lifetime. Only
/// its holder may bind, publish or remove the endpoint.
#[derive(Debug)]
pub struct DaemonLock {
    _file: File,
}

impl DaemonLock {
    /// Takes the lock, retrying briefly. `Ok(None)` when another daemon holds it.
    pub fn acquire(dir: &RuntimeDir) -> Result<Option<Self>> {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(dir.path().join("daemon.lock"))?;
        let mut waited = Duration::ZERO;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Some(Self { _file: file })),
                Err(TryLockError::WouldBlock) if waited < LOCK_RETRY => {
                    std::thread::sleep(LOCK_RETRY_STEP);
                    waited = waited.saturating_add(LOCK_RETRY_STEP);
                }
                Err(TryLockError::WouldBlock) => return Ok(None),
                Err(TryLockError::Error(err)) => return Err(err.into()),
            }
        }
    }
}

/// Discovery record the daemon publishes after binding (`endpoint.json`, `0600`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Endpoint {
    pub(crate) protocol: u32,
    pub(crate) build: String,
    pub(crate) pid: u32,
    pub(crate) epoch: String,
    /// Socket path (unix) or pipe name (Windows).
    pub(crate) address: String,
    /// Hex auth token; the first frame of every connection must carry it.
    pub(crate) token: String,
}

impl Endpoint {
    /// Reads the endpoint file; `Ok(None)` when there is none.
    pub(crate) fn load(dir: &RuntimeDir) -> Result<Option<Self>> {
        match fs::read(dir.endpoint_file()) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    /// Writes the endpoint file atomically (temp file + rename), readable only by the user.
    pub(crate) fn store(&self, dir: &RuntimeDir) -> Result<()> {
        let target = dir.endpoint_file();
        let tmp = dir
            .path()
            .join(format!("endpoint.json.{}.tmp", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(&serde_json::to_vec(self)?)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, &target)?;
        Ok(())
    }

    /// Removes the endpoint file if it still describes the daemon owning `token`.
    pub(crate) fn remove_if_owned(dir: &RuntimeDir, token: &str) {
        match Self::load(dir) {
            Ok(Some(endpoint)) if endpoint.token == token => {
                if let Err(err) = fs::remove_file(dir.endpoint_file()) {
                    tracing::warn!("could not remove endpoint file: {err}");
                }
            }
            Ok(_) => {}
            Err(err) => tracing::warn!("could not read endpoint file: {err}"),
        }
    }
}

/// A fresh daemon epoch (random per daemon start).
pub fn new_epoch() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// A fresh 64-hex-digit auth token from two v4 UUIDs (244 random bits).
pub(crate) fn new_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Build id of the daemon executable at `exe`: the package version plus the file's
/// modification time. The daemon reports it for its own executable; a client compares it
/// with the executable it would spawn and restarts a daemon from another build.
pub fn build_id(exe: &Path) -> String {
    let stamp = fs::metadata(exe)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map_or_else(|| "unknown".to_owned(), |age| age.as_nanos().to_string());
    format!("{}+{stamp}", env!("CARGO_PKG_VERSION"))
}

/// Daemon side: writes the single `READY <protocol> <pid> <epoch>` line to stdout, the
/// spawning client's readiness signal. Nothing else is ever written to stdout.
pub fn announce_ready(epoch: &str) -> std::io::Result<()> {
    let line = format!(
        "{READY_PREFIX}{} {} {epoch}\n",
        omc_proto::PROTOCOL,
        std::process::id()
    );
    let mut out = std::io::stdout().lock();
    out.write_all(line.as_bytes())?;
    out.flush()
}

pub(crate) const READY_PREFIX: &str = "READY ";

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!("omc-ipc-test-{}", uuid::Uuid::new_v4().simple()))
    }

    #[test]
    fn lock_is_exclusive_and_released_on_drop() {
        let path = temp_dir();
        let dir = RuntimeDir::at(path.clone());
        assert!(dir.is_ok(), "runtime dir created: {dir:?}");
        let Ok(dir) = dir else { return };
        let first = DaemonLock::acquire(&dir);
        assert!(matches!(first, Ok(Some(_))), "first holder gets the lock");
        assert!(
            matches!(DaemonLock::acquire(&dir), Ok(None)),
            "second holder is refused while the first lives"
        );
        drop(first);
        assert!(
            matches!(DaemonLock::acquire(&dir), Ok(Some(_))),
            "lock is free again after drop"
        );
        assert!(fs::remove_dir_all(path).is_ok(), "cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn open_runtime_dir_is_tightened_to_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let path = temp_dir();
        assert!(fs::create_dir_all(&path).is_ok(), "dir created");
        assert!(
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).is_ok(),
            "loosened"
        );
        assert!(RuntimeDir::at(path.clone()).is_ok(), "own dir accepted");
        let mode = fs::metadata(&path).map(|m| m.permissions().mode() & 0o777);
        assert!(
            matches!(mode, Ok(0o700)),
            "mode tightened to 0700: {mode:?}"
        );
        assert!(fs::remove_dir_all(path).is_ok(), "cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_runtime_dir_is_rejected() {
        let target = temp_dir();
        let link = temp_dir();
        assert!(fs::create_dir_all(&target).is_ok(), "target created");
        assert!(
            std::os::unix::fs::symlink(&target, &link).is_ok(),
            "link created"
        );
        let got = RuntimeDir::at(link.clone());
        assert!(
            matches!(got, Err(Error::UnsafeRuntimeDir { .. })),
            "a symlink is not a private directory: {got:?}"
        );
        assert!(
            fs::remove_file(link).is_ok() && fs::remove_dir_all(target).is_ok(),
            "cleanup"
        );
    }
}
