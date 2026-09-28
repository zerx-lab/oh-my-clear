//! Installed applications, their related files, uninstall results and startup items.

use serde::{Deserialize, Serialize};

use crate::jobs::{CleanReport, ItemId, Location};

/// Output of `list_apps`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppList {
    /// Apps sorted by name (case-insensitive).
    pub apps: Vec<AppInfo>,
}

/// One installed application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppInfo {
    /// Id inside the `list_apps` job.
    pub id: ItemId,
    /// Display name.
    pub name: String,
    /// Version string.
    #[serde(default)]
    pub version: Option<String>,
    /// Vendor.
    #[serde(default)]
    pub publisher: Option<String>,
    /// Stable identifier: macOS bundle id, Windows uninstall key name / package family,
    /// Linux package, Flatpak or Snap name.
    #[serde(default)]
    pub ident: Option<String>,
    /// Bundle path or install directory.
    #[serde(default)]
    pub location: Option<String>,
    /// Size of the app itself (not its data), when known.
    #[serde(default)]
    pub bytes: Option<u64>,
    /// Where the app came from.
    pub source: AppSource,
    /// Ships with the OS or is required by it; hidden unless settings show system apps,
    /// never uninstallable.
    #[serde(default)]
    pub system: bool,
    /// A process of the app is running.
    #[serde(default)]
    pub running: bool,
    /// Last launch (Unix seconds), when the OS records it.
    #[serde(default)]
    pub last_used: Option<i64>,
    /// Install date (Unix seconds), when known.
    #[serde(default)]
    pub installed: Option<i64>,
    /// PNG file with the app icon (cached by the daemon), when one could be extracted.
    #[serde(default)]
    pub icon: Option<String>,
    /// Removing the app needs administrator rights.
    #[serde(default)]
    pub needs_admin: bool,
}

/// Origins of installed apps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppSource {
    /// macOS `.app` bundle installed by drag-and-drop or an installer.
    MacBundle,
    /// macOS App Store app.
    MacAppStore,
    /// macOS app managed by Homebrew Cask.
    Homebrew,
    /// Windows program registered under an `Uninstall` registry key.
    WinRegistry,
    /// Windows Store / MSIX package.
    WinStore,
    /// Debian package.
    Deb,
    /// RPM package.
    Rpm,
    /// Arch Linux package.
    Pacman,
    /// Flatpak app.
    Flatpak,
    /// Snap package.
    Snap,
    /// `AppImage` file.
    AppImage,
    /// Desktop entry not owned by any package manager.
    Desktop,
}

/// Output of `app_files`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppFilesReport {
    /// The app.
    pub app: AppInfo,
    /// The app's own uninstaller or package-manager command, for display.
    #[serde(default)]
    pub uninstaller: Option<String>,
    /// Everything found, bundle/install dir first, then by kind.
    pub items: Vec<AppFile>,
}

/// One thing that belongs to an app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppFile {
    /// Id for `uninstall`.
    pub id: ItemId,
    /// What it is.
    pub location: Location,
    /// Role.
    pub kind: AppFileKind,
    /// Size (0 for registry entries and actions).
    pub bytes: u64,
    /// How sure the match is; the UI preselects items at or above the confidence set in
    /// settings.
    pub confidence: Confidence,
    /// Needs administrator rights.
    #[serde(default)]
    pub needs_admin: bool,
}

/// Roles of app files (UI key `apps.kind.<snake_case>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppFileKind {
    /// The `.app` bundle, install directory, `AppImage` or package itself.
    Bundle,
    /// Application support / program data.
    Support,
    /// Caches.
    Cache,
    /// Preferences / configuration.
    Preferences,
    /// Logs and crash reports.
    Logs,
    /// Sandbox container.
    Container,
    /// App group container shared by an app family.
    GroupContainer,
    /// Saved window state.
    SavedState,
    /// Launch agent / daemon / privileged helper / autostart entry.
    LaunchItem,
    /// Login item.
    LoginItem,
    /// Installer receipt.
    Receipt,
    /// Plug-ins and extensions (Internet plug-ins, kernel/system extensions, services).
    Extension,
    /// Start menu, desktop or dock shortcuts and desktop entries.
    Shortcut,
    /// Registry key or value.
    Registry,
    /// Windows service.
    Service,
    /// Windows scheduled task.
    ScheduledTask,
    /// Web data (cookies, HTTP storage, `WebKit` data).
    WebData,
    /// Anything else.
    Other,
}

/// Match confidence.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Matched by name only; could belong to something else.
    Low,
    /// Matched by vendor/app folder name.
    Medium,
    /// Matched by bundle id, package database, install location or registry reference.
    #[default]
    High,
}

/// Output of `uninstall`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UninstallReport {
    /// What the vendor uninstaller did.
    pub uninstaller: UninstallerOutcome,
    /// Removal of the bundle and related items.
    pub clean: CleanReport,
}

/// Result of running the app's own uninstaller.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "u", rename_all = "snake_case")]
pub enum UninstallerOutcome {
    /// No uninstaller exists or it was not requested.
    #[default]
    NotRun,
    /// It exited successfully.
    Succeeded,
    /// It failed; related items were still removed if requested.
    Failed {
        /// Exit code.
        code: Option<i32>,
        /// Detail.
        message: String,
    },
    /// The user cancelled it (or the elevation prompt).
    Cancelled,
}

/// Output of `list_startup`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupList {
    /// Items sorted by name.
    pub items: Vec<StartupItem>,
}

/// One program started at login or boot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupItem {
    /// Id inside the `list_startup` job.
    pub id: ItemId,
    /// Display name (label, value name, entry name).
    pub name: String,
    /// Program and arguments, when known.
    #[serde(default)]
    pub command: Option<String>,
    /// Where it is registered (plist, registry value, file).
    pub location: Location,
    /// Mechanism.
    pub kind: StartupKind,
    /// Starts for this user only, or for everyone.
    pub scope: Scope,
    /// Currently enabled.
    pub enabled: bool,
    /// Changing it needs administrator rights.
    #[serde(default)]
    pub needs_admin: bool,
    /// Vendor, when known.
    #[serde(default)]
    pub publisher: Option<String>,
    /// The program it starts no longer exists.
    #[serde(default)]
    pub missing_target: bool,
    /// Technical identifier behind a friendly `name` (launchd label, value name, file
    /// name), shown as secondary text; `None` when `name` is already the identifier.
    #[serde(default)]
    pub ident: Option<String>,
    /// PNG of the owning app's icon (cached by the daemon), when the app is known.
    #[serde(default)]
    pub icon: Option<String>,
}

/// Startup mechanisms (UI key `startup.kind.<snake_case>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupKind {
    /// macOS login item.
    LoginItem,
    /// macOS launch agent.
    LaunchAgent,
    /// macOS launch daemon.
    LaunchDaemon,
    /// Windows `Run` registry value.
    RunKey,
    /// Windows Startup folder shortcut.
    StartupFolder,
    /// Windows scheduled task triggered at logon/boot.
    ScheduledTask,
    /// Windows auto-start service (non-Microsoft).
    Service,
    /// XDG autostart desktop entry.
    XdgAutostart,
    /// systemd user unit.
    SystemdUnit,
}

/// Who an item applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// The current user.
    User,
    /// Every user.
    System,
}

/// Changes to a startup item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupChange {
    /// Start it again.
    Enable,
    /// Keep it registered but don't start it (reversible).
    Disable,
    /// Delete the registration.
    Remove,
}
