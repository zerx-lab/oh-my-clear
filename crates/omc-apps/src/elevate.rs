//! The elevated helper: items that need administrator rights are written to a manifest
//! and removed by ONE run of `oh-my-clear-daemon elevated <manifest>` under administrator
//! rights, so the user sees a single password prompt per clean.
//!
//! Daemon side ([`run`]): the manifest goes into a fresh private folder (`0700` on Unix),
//! the helper is started through the OS prompt — macOS `osascript … with administrator
//! privileges`, Linux `pkexec`, Windows `Start-Process -Verb RunAs` — and awaited for up to
//! ten minutes (typing a password takes time) in phase `elevating`. The helper writes
//! `<manifest>.result.json`; the folder is removed afterwards.
//!
//! Helper side ([`serve`]): refuses a manifest that is a symlink, writable by others, or
//! in a folder that is not private; rebuilds the [`omc_scan::Guard`] from the manifest's
//! exclusions (plus the platform rules, with `HOME` passed through from the invoking user)
//! and runs [`remove::execute_local`]. Trash as root:
//! - macOS: renamed into the invoking user's `~/.Trash` (numbered on collision) and handed
//!   back to the user's uid;
//! - Linux: there is no per-user Trash for root to file into, so Trash items are deleted
//!   permanently (the user approved the prompt for exactly these items);
//! - Windows: the `trash` crate (the elevated process is the same user).

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use omc_proto::jobs::{DeleteMethod, FailReason, Location, Phase};
use omc_proto::settings::CleanSettings;
use omc_scan::delete::TrashBin;
use omc_scan::{JobCtx, Target, paths};
use serde::{Deserialize, Serialize};

use crate::remove::{self, TargetOutcome};
use crate::{Error, Result};

/// How long the helper (including the password prompt) may take.
const HELPER_TIMEOUT: Duration = Duration::from_secs(600);
/// How often the helper is polled.
const POLL: Duration = Duration::from_millis(100);
const MANIFEST: &str = "manifest.json";
const LOG: &str = "helper.log";

/// What the helper removes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Manifest {
    items: Vec<Item>,
    /// Absolute user exclusions.
    exclude: Vec<String>,
    backup_registry: bool,
    /// The invoking user (Unix).
    #[serde(default)]
    user: Option<InvokingUser>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Item {
    location: Location,
    contents_only: bool,
    min_age_secs: u64,
    method: DeleteMethod,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct InvokingUser {
    home: String,
    uid: u32,
}

/// Why the elevated run as a whole did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ElevationFailed {
    /// `elevation_cancelled`, `permission_denied` (no way to elevate, helper failed) or
    /// `other` (job cancelled).
    pub(crate) reason: FailReason,
    /// Detail.
    pub(crate) message: String,
}

impl ElevationFailed {
    fn new(reason: FailReason, message: impl Into<String>) -> Self {
        Self {
            reason,
            message: message.into(),
        }
    }

    fn denied(message: impl Into<String>) -> Self {
        Self::new(FailReason::PermissionDenied, message)
    }
}

impl Manifest {
    fn new(targets: &[Target], settings: &CleanSettings, user: Option<InvokingUser>) -> Self {
        Self {
            items: targets
                .iter()
                .map(|t| Item {
                    location: t.location.clone(),
                    contents_only: t.contents_only,
                    min_age_secs: t.min_age_secs,
                    method: t.method,
                })
                .collect(),
            exclude: settings
                .exclude
                .iter()
                .filter_map(|p| paths::normalize(&paths::expand(p)))
                .map(|p| p.display().to_string())
                .collect(),
            backup_registry: settings.backup_registry,
            user,
        }
    }

    /// Targets as the helper removes them, and where Trash items go.
    fn targets(&self) -> (Vec<Target>, TrashBin) {
        let permanent_trash = cfg!(not(any(target_os = "macos", windows)));
        let targets = self
            .items
            .iter()
            .map(|item| Target {
                location: item.location.clone(),
                bytes: 0,
                contents_only: item.contents_only,
                min_age_secs: item.min_age_secs,
                method: if permanent_trash {
                    DeleteMethod::Permanent
                } else {
                    item.method
                },
                needs_admin: false,
            })
            .collect();
        let bin = match &self.user {
            Some(user) if cfg!(target_os = "macos") => TrashBin::Folder {
                dir: Path::new(&user.home).join(".Trash"),
                owner: Some(user.uid),
            },
            _ => TrashBin::System,
        };
        (targets, bin)
    }

    fn settings(&self) -> CleanSettings {
        CleanSettings {
            exclude: self.exclude.clone(),
            backup_registry: self.backup_registry,
            elevate: false,
            ..CleanSettings::default()
        }
    }
}

/// Removes `targets` through one elevated helper run. One outcome per target, in order.
pub(crate) fn run(
    targets: &[Target],
    settings: &CleanSettings,
    ctx: &JobCtx,
) -> Result<Vec<TargetOutcome>, ElevationFailed> {
    ctx.set_phase(Phase::Elevating);
    let dir =
        PrivateDir::create().map_err(|e| ElevationFailed::denied(format!("temp folder: {e}")))?;
    let manifest_path = dir.0.join(MANIFEST);
    let manifest = Manifest::new(targets, settings, dir.user());
    write_private(&manifest_path, &manifest)
        .map_err(|e| ElevationFailed::denied(format!("writing the manifest: {e}")))?;
    let exe = std::env::current_exe()
        .map_err(|e| ElevationFailed::denied(format!("locating the daemon: {e}")))?;
    let mut cmd = helper_command(&exe, &manifest_path, manifest.user.as_ref())?;
    let log_path = dir.0.join(LOG);
    let status = spawn_and_wait(&mut cmd, &log_path, ctx);
    let log = fs::read_to_string(&log_path).unwrap_or_default();
    if let Ok(text) = fs::read_to_string(result_path(&manifest_path)) {
        return parse_result(&text, targets.len())
            .map_err(|e| ElevationFailed::denied(format!("helper result: {e}")));
    }
    let status = status?;
    if cancelled(status.code(), &log) {
        return Err(ElevationFailed::new(
            FailReason::ElevationCancelled,
            "administrator prompt was cancelled",
        ));
    }
    Err(ElevationFailed::denied(format!(
        "elevated helper failed ({status}): {}",
        log.trim()
    )))
}

/// Body of `oh-my-clear-daemon elevated <manifest>`.
pub(crate) fn serve(manifest_path: &Path) -> Result<()> {
    let owner = check_manifest_file(manifest_path)?;
    let manifest: Manifest = serde_json::from_slice(&fs::read(manifest_path)?)?;
    if let (Some(owner), Some(user)) = (owner, &manifest.user)
        && owner != user.uid
    {
        return Err(Error::Elevation(format!(
            "manifest owner {owner} is not the invoking user {}",
            user.uid
        )));
    }
    let (targets, bin) = manifest.targets();
    tracing::info!(items = targets.len(), "elevated removal");
    let outcomes = remove::execute_local(&targets, &manifest.settings(), &JobCtx::new(), &bin);
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(result_path(manifest_path))?;
    out.write_all(&serde_json::to_vec(&outcomes)?)?;
    Ok(())
}

fn result_path(manifest: &Path) -> PathBuf {
    let mut name = manifest.as_os_str().to_owned();
    name.push(".result.json");
    PathBuf::from(name)
}

fn parse_result(text: &str, expected: usize) -> Result<Vec<TargetOutcome>> {
    let outcomes: Vec<TargetOutcome> = serde_json::from_str(text)?;
    if outcomes.len() == expected {
        Ok(outcomes)
    } else {
        Err(Error::Parse {
            what: "elevated result".to_owned(),
            message: format!("{} outcomes for {expected} items", outcomes.len()),
        })
    }
}

/// The helper refuses manifests others could have written. Returns the owner uid (Unix).
fn check_manifest_file(path: &Path) -> Result<Option<u32>> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_file() {
        return Err(Error::Elevation(
            "manifest is not a regular file".to_owned(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let parent = path
            .parent()
            .ok_or_else(|| Error::Elevation("manifest has no folder".to_owned()))?;
        let dir = fs::symlink_metadata(parent)?;
        if meta.mode() & 0o022 != 0 {
            return Err(Error::Elevation(
                "manifest is writable by others".to_owned(),
            ));
        }
        if !dir.file_type().is_dir() || dir.mode() & 0o077 != 0 || dir.uid() != meta.uid() {
            return Err(Error::Elevation(
                "manifest folder is not private".to_owned(),
            ));
        }
        Ok(Some(meta.uid()))
    }
    #[cfg(not(unix))]
    {
        Ok(None)
    }
}

/// A fresh folder only the current user can enter; removed on drop.
#[derive(Debug)]
struct PrivateDir(PathBuf);

impl PrivateDir {
    fn create() -> std::io::Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!("omc-elevate-{}-{nanos}", std::process::id()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            fs::DirBuilder::new().mode(0o700).create(&dir)?;
        }
        #[cfg(not(unix))]
        fs::DirBuilder::new().create(&dir)?;
        Ok(Self(dir))
    }

    /// The invoking user (Unix).
    fn user(&self) -> Option<InvokingUser> {
        invoking_user(&self.0)
    }
}

/// The current user: owner of `dir`, which this process just created.
#[cfg(unix)]
fn invoking_user(dir: &Path) -> Option<InvokingUser> {
    use std::os::unix::fs::MetadataExt as _;
    let uid = fs::metadata(dir).ok()?.uid();
    let home = paths::home()?.display().to_string();
    Some(InvokingUser { home, uid })
}

#[cfg(not(unix))]
fn invoking_user(_dir: &Path) -> Option<InvokingUser> {
    None
}

impl Drop for PrivateDir {
    fn drop(&mut self) {
        if let Err(err) = fs::remove_dir_all(&self.0) {
            tracing::warn!(%err, dir = %self.0.display(), "removing the elevation folder");
        }
    }
}

fn write_private(path: &Path, manifest: &Manifest) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(&serde_json::to_vec(manifest)?)?;
    Ok(())
}

fn utf8<'a>(path: &'a Path, what: &str) -> Result<&'a str, ElevationFailed> {
    path.to_str()
        .ok_or_else(|| ElevationFailed::denied(format!("{what} path is not UTF-8")))
}

/// The OS command that starts `exe elevated manifest` with administrator rights.
fn helper_command(
    exe: &Path,
    manifest: &Path,
    user: Option<&InvokingUser>,
) -> Result<Command, ElevationFailed> {
    #[cfg(target_os = "macos")]
    {
        let script = osascript_script(
            utf8(exe, "daemon")?,
            utf8(manifest, "manifest")?,
            user.map(|u| u.home.as_str()),
        );
        let mut cmd = crate::cmd::command("/usr/bin/osascript");
        cmd.arg("-e").arg(script);
        Ok(cmd)
    }
    #[cfg(windows)]
    {
        let _: Option<&InvokingUser> = user;
        let script = powershell_script(utf8(exe, "daemon")?, utf8(manifest, "manifest")?);
        let mut cmd = crate::cmd::command("powershell");
        cmd.args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            &script,
        ]);
        Ok(cmd)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _unused = utf8(manifest, "manifest")?;
        let mut cmd = crate::cmd::command("pkexec");
        // pkexec resets the environment; the guard needs the invoking user's home.
        if let Some(user) = user
            && Path::new("/usr/bin/env").exists()
        {
            cmd.arg("/usr/bin/env").arg(format!("HOME={}", user.home));
        }
        cmd.arg(exe).arg("elevated").arg(manifest);
        Ok(cmd)
    }
}

/// Starts the helper with its output in `log`, waits for it (bounded, cancellable).
fn spawn_and_wait(
    cmd: &mut Command,
    log: &Path,
    ctx: &JobCtx,
) -> Result<ExitStatus, ElevationFailed> {
    let out = File::create(log).map_err(|e| ElevationFailed::denied(format!("helper log: {e}")))?;
    let err = out
        .try_clone()
        .map_err(|e| ElevationFailed::denied(format!("helper log: {e}")))?;
    cmd.stdin(Stdio::null()).stdout(out).stderr(err);
    let mut child = cmd.spawn().map_err(|e| {
        ElevationFailed::denied(if e.kind() == std::io::ErrorKind::NotFound {
            format!("no way to obtain administrator rights: {e}")
        } else {
            format!("starting the elevation prompt: {e}")
        })
    })?;
    wait(&mut child, ctx)
}

fn wait(child: &mut Child, ctx: &JobCtx) -> Result<ExitStatus, ElevationFailed> {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(e) => {
                return Err(ElevationFailed::denied(format!(
                    "waiting for the helper: {e}"
                )));
            }
        }
        let stop = if ctx.is_cancelled() {
            Some(ElevationFailed::new(FailReason::Other, "cancelled"))
        } else if start.elapsed() >= HELPER_TIMEOUT {
            Some(ElevationFailed::denied("administrator prompt timed out"))
        } else {
            None
        };
        if let Some(stop) = stop {
            if let Err(err) = child.kill() {
                tracing::debug!(%err, "killing the elevation prompt");
            }
            if let Err(err) = child.wait() {
                tracing::debug!(%err, "reaping the elevation prompt");
            }
            return Err(stop);
        }
        std::thread::sleep(POLL);
    }
}

/// The user dismissed the prompt (or failed to authenticate).
fn cancelled(code: Option<i32>, log: &str) -> bool {
    if cfg!(target_os = "macos") {
        // osascript: "User canceled. (-128)".
        log.contains("(-128)") || log.to_ascii_lowercase().contains("user canceled")
    } else if cfg!(windows) {
        log.to_ascii_lowercase().contains("canceled by the user")
    } else {
        // pkexec: 126 = dialog dismissed, 127 = not authorized.
        matches!(code, Some(126 | 127))
    }
}

/// `s` as one POSIX shell word.
#[cfg_attr(
    not(any(target_os = "macos", test)),
    expect(dead_code, reason = "macOS only")
)]
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `s` inside an `AppleScript` string literal.
#[cfg_attr(
    not(any(target_os = "macos", test)),
    expect(dead_code, reason = "macOS only")
)]
fn applescript_escape(s: &str) -> String {
    s.replace('\\', r"\\").replace('"', "\\\"")
}

/// `do shell script "HOME=… exe elevated manifest" with administrator privileges`.
#[cfg_attr(
    not(any(target_os = "macos", test)),
    expect(dead_code, reason = "macOS only")
)]
fn osascript_script(exe: &str, manifest: &str, home: Option<&str>) -> String {
    let env = home
        .map(|h| format!("HOME={} ", sh_quote(h)))
        .unwrap_or_default();
    let shell = format!("{env}{} elevated {}", sh_quote(exe), sh_quote(manifest));
    format!(
        "do shell script \"{}\" with prompt \"oh-my-clear needs administrator rights to remove the selected items.\" with administrator privileges",
        applescript_escape(&shell)
    )
}

/// `s` as a single-quoted PowerShell string (typographic quotes are quotes too).
#[cfg_attr(not(any(windows, test)), expect(dead_code, reason = "Windows only"))]
fn ps_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len().saturating_add(2));
    out.push('\'');
    for c in s.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// Starts `exe elevated "<manifest>"` through UAC and exits with its code.
#[cfg_attr(not(any(windows, test)), expect(dead_code, reason = "Windows only"))]
fn powershell_script(exe: &str, manifest: &str) -> String {
    format!(
        "$p = Start-Process -FilePath {} -ArgumentList 'elevated',{} -Verb RunAs -WindowStyle Hidden -Wait -PassThru; exit $p.ExitCode",
        ps_quote(exe),
        ps_quote(&format!("\"{manifest}\""))
    )
}

#[cfg(test)]
mod tests {
    use omc_proto::jobs::{Failure, SpecialAction};

    use super::*;

    fn manifest() -> Manifest {
        let targets = [
            Target::contents("/Library/Caches/com.x", 5, DeleteMethod::Permanent).older_than(60),
            Target::path("/Applications/X.app", 9, DeleteMethod::Trash).admin(true),
            Target::location(
                Location::Special {
                    action: SpecialAction::ForgetPackage {
                        id: "com.x.pkg".to_owned(),
                    },
                },
                0,
            ),
        ];
        let settings = CleanSettings {
            exclude: vec!["/Users/me/keep".to_owned()],
            ..CleanSettings::default()
        };
        Manifest::new(
            &targets,
            &settings,
            Some(InvokingUser {
                home: "/Users/me".to_owned(),
                uid: 501,
            }),
        )
    }

    #[test]
    fn manifest_round_trips_and_rebuilds_targets() {
        let m = manifest();
        let json = serde_json::to_string(&m);
        assert!(json.is_ok(), "serializes");
        let Ok(json) = json else { return };
        let back: Result<Manifest, _> = serde_json::from_str(&json);
        assert!(back.as_ref().is_ok_and(|b| *b == m), "round trip: {back:?}");
        let (targets, bin) = m.targets();
        assert_eq!(targets.len(), 3, "one target per item");
        let first = targets.first();
        assert!(
            first.is_some_and(|t| t.contents_only && t.min_age_secs == 60),
            "contents and age kept: {first:?}"
        );
        let trash = targets.get(1).map(|t| t.method);
        if cfg!(target_os = "macos") {
            assert_eq!(
                trash,
                Some(DeleteMethod::Trash),
                "macOS files into the user's Trash"
            );
            assert_eq!(
                bin,
                TrashBin::Folder {
                    dir: PathBuf::from("/Users/me/.Trash"),
                    owner: Some(501)
                },
                "the invoking user's Trash"
            );
        } else if cfg!(windows) {
            assert_eq!(trash, Some(DeleteMethod::Trash), "same-user Recycle Bin");
        } else {
            assert_eq!(
                trash,
                Some(DeleteMethod::Permanent),
                "root has no Trash on Linux"
            );
        }
        assert_eq!(
            m.settings().exclude,
            vec!["/Users/me/keep".to_owned()],
            "exclusions"
        );
        assert!(!m.settings().elevate, "the helper never elevates again");
    }

    #[test]
    fn shell_and_applescript_quoting() {
        assert_eq!(sh_quote("a b"), "'a b'", "spaces");
        assert_eq!(
            sh_quote("it's $HOME"),
            r"'it'\''s $HOME'",
            "quote and dollar"
        );
        let script = osascript_script(
            "/Apps/My \"Clear\".app/d",
            "/tmp/it's $x\\y/manifest.json",
            Some("/Users/me"),
        );
        let expected_shell = r#"HOME='/Users/me' '/Apps/My \"Clear\".app/d' elevated '/tmp/it'\\''s $x\\y/manifest.json'"#;
        assert!(
            script.starts_with(&format!("do shell script \"{expected_shell}\"")),
            "AppleScript-escaped shell command: {script}"
        );
        assert!(
            script.ends_with("with administrator privileges"),
            "asks for admin: {script}"
        );
    }

    #[test]
    fn powershell_quoting() {
        assert_eq!(ps_quote("C:\\a b"), "'C:\\a b'", "plain");
        assert_eq!(
            ps_quote("O'Neil\u{2019}s"),
            "'O''Neil\u{2019}\u{2019}s'",
            "quotes doubled"
        );
        let script = powershell_script("C:\\Program Files\\omc\\d.exe", "C:\\Users\\O'N\\m.json");
        assert_eq!(
            script,
            "$p = Start-Process -FilePath 'C:\\Program Files\\omc\\d.exe' -ArgumentList 'elevated','\"C:\\Users\\O''N\\m.json\"' -Verb RunAs -WindowStyle Hidden -Wait -PassThru; exit $p.ExitCode",
            "script"
        );
    }

    #[test]
    fn result_parsing_checks_the_count() {
        let outcomes = vec![
            TargetOutcome {
                freed: 7,
                failures: Vec::new(),
            },
            TargetOutcome {
                freed: 0,
                failures: vec![Failure {
                    location: Location::Path {
                        path: "/x".to_owned(),
                    },
                    reason: FailReason::InUse,
                    message: "busy".to_owned(),
                }],
            },
        ];
        let text = serde_json::to_string(&outcomes).unwrap_or_default();
        assert!(
            parse_result(&text, 2).is_ok_and(|p| p == outcomes),
            "matching count parses"
        );
        assert!(
            parse_result(&text, 3).is_err(),
            "count mismatch is an error"
        );
        assert!(parse_result("nope", 0).is_err(), "garbage is an error");
    }

    #[test]
    fn cancellation_detection() {
        if cfg!(target_os = "macos") {
            assert!(
                cancelled(Some(1), "execution error: User canceled. (-128)"),
                "osascript cancel"
            );
            assert!(
                !cancelled(Some(1), "execution error: boom (-2700)"),
                "other error"
            );
        } else if cfg!(windows) {
            assert!(
                cancelled(
                    Some(1),
                    "This command cannot be run due to the error: The operation was canceled by the user."
                ),
                "UAC cancel"
            );
        } else {
            assert!(cancelled(Some(126), ""), "dismissed");
            assert!(!cancelled(Some(1), ""), "helper error");
        }
    }

    #[test]
    fn serve_removes_listed_items_and_writes_the_result() {
        let dir = PrivateDir::create();
        assert!(dir.is_ok(), "private dir");
        let Ok(dir) = dir else { return };
        let victim = dir.0.join("victim.bin");
        assert!(fs::write(&victim, vec![0u8; 4096]).is_ok(), "write victim");
        let targets = [Target::path(
            victim.display().to_string(),
            0,
            DeleteMethod::Permanent,
        )];
        let m = Manifest::new(&targets, &CleanSettings::default(), dir.user());
        let path = dir.0.join(MANIFEST);
        assert!(write_private(&path, &m).is_ok(), "manifest written");
        let served = serve(&path);
        assert!(served.is_ok(), "served: {served:?}");
        assert!(!victim.exists(), "item removed");
        let text = fs::read_to_string(result_path(&path)).unwrap_or_default();
        assert!(
            parse_result(&text, 1).is_ok_and(|r| r
                .first()
                .is_some_and(|o| o.failures.is_empty() && o.freed > 0)),
            "result written: {text}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn serve_refuses_a_world_writable_manifest() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = PrivateDir::create();
        let Ok(dir) = dir else { return };
        let path = dir.0.join(MANIFEST);
        assert!(
            write_private(&path, &manifest()).is_ok(),
            "manifest written"
        );
        assert!(
            fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).is_ok(),
            "chmod"
        );
        assert!(
            matches!(serve(&path), Err(Error::Elevation(_))),
            "tampered manifest refused"
        );
    }
}
