//! Long-running work: every scan, clean, app inventory, uninstall and startup change runs
//! as a daemon **job**. `start_job` answers at once with a [`JobId`]; the daemon then pushes
//! [`JobUpdate`]s (at most ~10 per second per job, lossy) and one final update whose
//! [`JobState`] is not `running`. The UI fetches the output with `job_result` and frees it
//! with `release_job`.
//!
//! Items of a finished job are addressed by [`ItemId`] (their index-like id inside that
//! job's output), never by path: a clean or uninstall can only touch what a scan of the same
//! daemon epoch reported.

use serde::{Deserialize, Serialize};

use crate::apps::{
    AppFilesReport, AppList, StartupChange, StartupItem, StartupList, UninstallReport,
};
use crate::files::{DupReport, FileReport, SpaceListing};
use crate::junk::JunkReport;

/// Daemon-assigned job id, unique per daemon epoch.
pub type JobId = u64;

/// Id of one item inside a finished job's output.
pub type ItemId = u32;

/// What a job does. Behaviour is tuned by the daemon's stored
/// [`crate::settings::CleanSettings`], read when the job starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "k", content = "p", rename_all = "snake_case")]
pub enum JobSpec {
    /// Scans one area. Output: the area's report (see [`ScanArea`]).
    Scan(ScanArea),
    /// Removes items of a finished scan job. Output: [`JobOutput::Clean`].
    Clean(CleanSpec),
    /// Lists installed applications. Output: [`JobOutput::Apps`].
    ListApps,
    /// Finds everything that belongs to one app of a finished `list_apps` job: bundle or
    /// install dir, support files, caches, preferences, launch items, registry keys…
    /// Output: [`JobOutput::AppFiles`].
    AppFiles {
        /// The `list_apps` job.
        apps_job: JobId,
        /// The app inside it.
        app: ItemId,
    },
    /// Uninstalls the app of a finished `app_files` job. Output: [`JobOutput::Uninstall`].
    Uninstall(UninstallSpec),
    /// Lists programs launched at login/boot. Output: [`JobOutput::Startup`].
    ListStartup,
    /// Enables, disables or removes one item of a finished `list_startup` job.
    /// Output: [`JobOutput::StartupChanged`].
    ChangeStartup {
        /// The `list_startup` job.
        list_job: JobId,
        /// The item inside it.
        item: ItemId,
        /// What to do.
        change: StartupChange,
    },
}

/// Scannable areas (one per sidebar entry that scans the file system).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "a", content = "p", rename_all = "snake_case")]
pub enum ScanArea {
    /// OS and app caches, logs, crash reports, temp files. Output: [`JobOutput::Junk`].
    SystemJunk,
    /// Browser caches (and, when enabled in settings, cookies/history/site data).
    /// Output: [`JobOutput::Junk`].
    BrowserData,
    /// Build output, package-manager and IDE caches. Output: [`JobOutput::Junk`].
    DeveloperJunk,
    /// Trash / Recycle Bin contents. Output: [`JobOutput::Junk`].
    Trash,
    /// Installer packages and disk images. Output: [`JobOutput::Junk`].
    Installers,
    /// Support files and registrations of apps that are no longer installed.
    /// Output: [`JobOutput::Junk`].
    Leftovers,
    /// Files above the size threshold or untouched for long. Output: [`JobOutput::Files`].
    LargeOldFiles,
    /// Files with identical content. Output: [`JobOutput::Duplicates`].
    Duplicates,
    /// Disk usage tree below `root`. Output: [`JobOutput::Space`]; drill down with the
    /// `space_children` request.
    SpaceLens {
        /// Absolute directory to measure.
        root: String,
    },
}

/// A clean request: which items of which scan job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanSpec {
    /// A finished scan job (any area, including `space_lens`, `large_old_files` and
    /// `duplicates`).
    pub scan_job: JobId,
    /// Items to remove.
    pub items: Vec<ItemId>,
}

/// An uninstall request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UninstallSpec {
    /// A finished `app_files` job.
    pub files_job: JobId,
    /// Related items to remove after the app itself (ids inside `files_job`). Items the
    /// vendor uninstaller already removed are skipped.
    pub items: Vec<ItemId>,
    /// Run the app's own uninstaller / package manager first (Windows, Linux packages).
    #[serde(default = "yes")]
    pub run_uninstaller: bool,
}

fn yes() -> bool {
    true
}

/// A job's state and progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobStatus {
    /// Lifecycle state.
    pub state: JobState,
    /// Counters so far (final values once the job ended).
    pub progress: Progress,
}

/// Lifecycle of a job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "s", rename_all = "snake_case")]
pub enum JobState {
    /// Working.
    Running,
    /// Finished; `job_result` returns the output.
    Done,
    /// Stopped by an error.
    Failed {
        /// What went wrong.
        message: String,
    },
    /// Stopped by `cancel_job`; `job_result` may still return a partial output.
    Cancelled,
}

impl JobState {
    /// `true` once the job will not change any more.
    pub const fn is_finished(&self) -> bool {
        !matches!(self, Self::Running)
    }
}

/// Progress counters. `total`/`done` are set only when the work is countable up front
/// (cleaning N items, hashing N candidates); otherwise progress is indeterminate.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    /// What the job is doing now.
    pub phase: Phase,
    /// Entries visited (files and directories).
    pub items: u64,
    /// Bytes found (scans) or freed (cleaning).
    pub bytes: u64,
    /// Bytes `bytes` is expected to reach (cleaning: the size of the selected items, as
    /// scanned); 0 = unknown.
    #[serde(default)]
    pub bytes_total: u64,
    /// Units finished out of `total`.
    #[serde(default)]
    pub done: u64,
    /// Units of countable work; 0 = indeterminate.
    #[serde(default)]
    pub total: u64,
    /// Path or item being processed (display only, may be shortened).
    #[serde(default)]
    pub current: Option<String>,
}

/// Coarse step of a job.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Queued / preparing.
    #[default]
    Starting,
    /// Walking the file system or enumerating sources.
    Scanning,
    /// Computing sizes of found items.
    Measuring,
    /// Comparing file contents (duplicates).
    Hashing,
    /// Waiting for the app to quit.
    Quitting,
    /// The vendor uninstaller or package manager is running.
    Uninstalling,
    /// Waiting for the user to approve administrator rights.
    Elevating,
    /// Deleting items.
    Removing,
    /// Wrapping up.
    Finishing,
}

/// An update pushed to every attached UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobUpdate {
    /// The job.
    pub job: JobId,
    /// What kind of job it is (lets a UI that restarted route updates to the right page).
    pub kind: JobKind,
    /// Its state.
    pub status: JobStatus,
}

/// Kind of a job, mirrored from its [`JobSpec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// [`JobSpec::Scan`].
    Scan,
    /// [`JobSpec::Clean`].
    Clean,
    /// [`JobSpec::ListApps`].
    ListApps,
    /// [`JobSpec::AppFiles`].
    AppFiles,
    /// [`JobSpec::Uninstall`].
    Uninstall,
    /// [`JobSpec::ListStartup`].
    ListStartup,
    /// [`JobSpec::ChangeStartup`].
    ChangeStartup,
}

impl JobSpec {
    /// The spec's kind.
    pub const fn kind(&self) -> JobKind {
        match self {
            Self::Scan(_) => JobKind::Scan,
            Self::Clean(_) => JobKind::Clean,
            Self::ListApps => JobKind::ListApps,
            Self::AppFiles { .. } => JobKind::AppFiles,
            Self::Uninstall(_) => JobKind::Uninstall,
            Self::ListStartup => JobKind::ListStartup,
            Self::ChangeStartup { .. } => JobKind::ChangeStartup,
        }
    }
}

/// Output of a finished job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "o", content = "p", rename_all = "snake_case")]
pub enum JobOutput {
    /// Junk-style areas: system, browser, developer, trash, installers, leftovers.
    Junk(JunkReport),
    /// Large and old files.
    Files(FileReport),
    /// Duplicate groups.
    Duplicates(DupReport),
    /// Top level of a space-lens tree.
    Space(SpaceListing),
    /// Result of a clean.
    Clean(CleanReport),
    /// Installed apps.
    Apps(AppList),
    /// Everything that belongs to one app.
    AppFiles(AppFilesReport),
    /// Result of an uninstall.
    Uninstall(UninstallReport),
    /// Startup items.
    Startup(StartupList),
    /// The changed startup item; `None` once removed.
    StartupChanged(Option<StartupItem>),
}

/// How files are removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteMethod {
    /// Unlink immediately.
    #[default]
    Permanent,
    /// Move to Trash / Recycle Bin (restorable). Falls back to failing the item, never to
    /// a silent permanent delete.
    Trash,
}

/// Something the daemon can remove.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "l", rename_all = "snake_case")]
pub enum Location {
    /// A file or directory.
    Path {
        /// Absolute path.
        path: String,
    },
    /// A registry key and its whole subtree (Windows). `key` starts with the hive:
    /// `HKCU\…`, `HKLM\…`, `HKCR\…`, `HKU\…`.
    RegistryKey {
        /// Full key path.
        key: String,
    },
    /// One named value of a registry key (Windows).
    RegistryValue {
        /// Full key path.
        key: String,
        /// Value name (`""` = the default value).
        name: String,
    },
    /// An OS operation that has no single path (emptying the Trash through Finder when
    /// oh-my-clear lacks Full Disk Access, deleting a service or scheduled task…).
    Special {
        /// What to run.
        action: SpecialAction,
    },
}

impl Location {
    /// The path, for [`Location::Path`].
    pub fn as_path(&self) -> Option<&str> {
        match self {
            Self::Path { path } => Some(path),
            _ => None,
        }
    }

    /// Human-readable form (path, registry path, or action name).
    pub fn display(&self) -> String {
        match self {
            Self::Path { path } => path.clone(),
            Self::RegistryKey { key } => key.clone(),
            Self::RegistryValue { key, name } => format!("{key}\\{name}"),
            Self::Special { action } => format!("{action:?}"),
        }
    }
}

/// Operations behind [`Location::Special`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "a", rename_all = "snake_case")]
pub enum SpecialAction {
    /// Empty the user's Trash / Recycle Bin through the OS (Finder, `Clear-RecycleBin`).
    EmptyTrash,
    /// Stop and delete a Windows service.
    DeleteService {
        /// Service name.
        name: String,
    },
    /// Delete a Windows scheduled task.
    DeleteScheduledTask {
        /// Task path, e.g. `\Vendor\Updater`.
        path: String,
    },
    /// `launchctl bootout` + delete a launchd job's plist (macOS).
    UnloadLaunchJob {
        /// launchd label.
        label: String,
        /// The plist.
        plist: String,
    },
    /// Remove a macOS login item by name (System Events).
    RemoveLoginItem {
        /// Login item name.
        name: String,
    },
    /// `pkgutil --forget` a macOS installer receipt.
    ForgetPackage {
        /// Package id.
        id: String,
    },
}

/// Result of a clean (also the removal part of an uninstall).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanReport {
    /// Items fully removed.
    pub removed: u64,
    /// Bytes freed (moved to Trash counts too).
    pub freed: u64,
    /// Items (or parts of items) that could not be removed.
    pub failures: Vec<Failure>,
}

/// One thing that could not be removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    /// What failed.
    pub location: Location,
    /// Why.
    pub reason: FailReason,
    /// OS error text.
    pub message: String,
}

/// Classes of removal failures (the UI explains each and offers a fix where one exists).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailReason {
    /// Needs administrator rights (and elevation was declined or disabled).
    PermissionDenied,
    /// macOS: needs Full Disk Access.
    FullDiskAccess,
    /// macOS: needs the App Management permission to modify another app.
    AppManagement,
    /// Locked / in use by a running process.
    InUse,
    /// Refused by the safety guard (system path or user exclusion).
    Protected,
    /// The user cancelled the administrator prompt.
    ElevationCancelled,
    /// Trash is unavailable on that volume and deletion method was Trash.
    TrashUnavailable,
    /// Anything else.
    Other,
}

/// Why a scan could not look somewhere; the UI offers the matching fix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Denied {
    /// The directory that could not be read.
    pub path: String,
    /// What would let the scan in.
    pub reason: FailReason,
}
