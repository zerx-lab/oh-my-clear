//! Reports of the junk-style areas (system, browser, developer, trash, installers,
//! leftovers): groups of removable items with their aggregated size. Group kinds and item
//! tags are enums so the UI can localise them; names are raw (app, profile, file names).

use serde::{Deserialize, Serialize};

use crate::jobs::{Denied, ItemId};

/// Output of a junk-style scan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JunkReport {
    /// Non-empty groups, largest first.
    pub groups: Vec<JunkGroup>,
    /// Places the scan could not read (e.g. needs Full Disk Access).
    #[serde(default)]
    pub denied: Vec<Denied>,
}

/// Items of one kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JunkGroup {
    /// What the items are.
    pub kind: JunkKind,
    /// Items, largest first.
    pub items: Vec<JunkItem>,
}

impl JunkGroup {
    /// Sum of the items' sizes.
    pub fn bytes(&self) -> u64 {
        self.items
            .iter()
            .fold(0_u64, |sum, item| sum.saturating_add(item.bytes))
    }
}

/// One removable thing: a cache directory, a log folder, a trash location, an installer…
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JunkItem {
    /// Id for `clean`.
    pub id: ItemId,
    /// Display name: app, browser profile, project or file name.
    pub name: String,
    /// What is removed (a path, or a registry key for leftovers).
    pub location: crate::jobs::Location,
    /// Detail tag (cache, cookies, `node_modules`…); `None` when the group says it all.
    #[serde(default)]
    pub tag: Option<ItemTag>,
    /// Bytes freed by removing it.
    pub bytes: u64,
    /// Files inside.
    pub files: u64,
    /// Newest modification inside (Unix seconds).
    #[serde(default)]
    pub modified: Option<i64>,
    /// Whether the item is preselected.
    pub safety: Safety,
    /// Removing it needs administrator rights.
    #[serde(default)]
    pub needs_admin: bool,
    /// The owning app is running now (its cache is skipped or marked).
    #[serde(default)]
    pub app_running: bool,
    /// Technical identifier behind a friendly `name` (bundle id, folder or package name),
    /// shown as secondary text; `None` when `name` is already the identifier.
    #[serde(default)]
    pub ident: Option<String>,
    /// PNG of the owning app's icon (cached by the daemon), when the app is known.
    #[serde(default)]
    pub icon: Option<String>,
}

/// How confidently an item can be removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Safety {
    /// Regenerated automatically; preselected.
    Safe,
    /// Removable but the user may want it (cookies, history, old projects, installers of
    /// unknown use); shown, not preselected.
    Review,
}

/// Group kinds. The UI key is `junk.kind.<snake_case>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JunkKind {
    // ---- system
    /// Per-user app caches.
    UserCache,
    /// System-wide caches.
    SystemCache,
    /// Per-user logs.
    UserLog,
    /// System logs.
    SystemLog,
    /// Crash and diagnostic reports.
    CrashReport,
    /// Temporary files.
    TempFiles,
    /// Thumbnail and icon caches.
    Thumbnails,
    /// Downloaded OS updates already installed (Windows Update, Delivery Optimization,
    /// macOS updates).
    UpdateCache,
    /// OS package-manager caches (apt, dnf, pacman, zypper, Homebrew, winget, Chocolatey).
    PackageCache,
    /// Windows Error Reporting queues and memory dumps.
    ErrorReports,
    /// GPU shader caches (DirectX, NVIDIA/AMD, Mesa).
    ShaderCache,
    /// Mail attachments already saved in the mail store (macOS Mail Downloads).
    MailDownloads,
    /// iOS/iPadOS device backups and firmware downloads.
    DeviceBackups,
    /// Previous OS installation (`Windows.old`).
    OldOsInstall,
    // ---- browsers
    /// One browser's data; items are tagged with the data type and named by profile.
    Browser(Browser),
    // ---- developer
    /// Xcode build products, archives, device support, simulator caches.
    Xcode,
    /// Language package-manager caches (npm, yarn, pnpm, cargo, pip, Gradle, Maven, Go…).
    DevPackageCache,
    /// Build output and dependency folders inside projects (`node_modules`, `target`…).
    ProjectArtifacts,
    /// IDE and editor caches (JetBrains, VS Code…).
    IdeCache,
    /// Container/VM tool caches (Docker build cache folders, Vagrant boxes).
    ToolCache,
    // ---- trash
    /// Trash / Recycle Bin locations.
    Trash,
    // ---- installers
    /// Disk images (`.dmg`, `.iso`, `.img`).
    DiskImage,
    /// Installer packages (`.pkg`, `.msi`, `.msix`, `.deb`, `.rpm`).
    InstallerPackage,
    /// Setup executables and archives that look like installers.
    SetupProgram,
    // ---- leftovers
    /// Support files of apps that are no longer installed.
    OrphanFiles,
    /// Launch agents/daemons, autostart entries or Run values whose program is gone.
    OrphanLaunchItems,
    /// Uninstall registry entries whose program is gone (Windows).
    BrokenUninstallEntries,
    /// Shortcuts and desktop entries that point to nothing.
    BrokenShortcuts,
    /// Registry keys of software that is no longer installed (Windows).
    OrphanRegistry,
}

/// Browsers the scanner knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Browser {
    /// Google Chrome.
    Chrome,
    /// Chromium.
    Chromium,
    /// Microsoft Edge.
    Edge,
    /// Brave.
    Brave,
    /// Vivaldi.
    Vivaldi,
    /// Opera / Opera GX.
    Opera,
    /// Arc.
    Arc,
    /// Mozilla Firefox (and forks using its profile layout: `LibreWolf`, Waterfox, Zen).
    Firefox,
    /// Safari.
    Safari,
    /// Yandex Browser.
    Yandex,
}

/// Detail tags. The UI key is `junk.tag.<snake_case>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemTag {
    /// HTTP cache.
    Cache,
    /// JavaScript/WebAssembly code cache.
    CodeCache,
    /// GPU/shader cache.
    GpuCache,
    /// Service worker caches.
    ServiceWorker,
    /// Cookies (signs you out).
    Cookies,
    /// Browsing and download history.
    History,
    /// Local storage, `IndexedDB`, other site data (signs you out of some sites).
    SiteData,
    /// Saved sessions / tabs.
    Sessions,
    /// Logs.
    Logs,
    /// Crash reports.
    Crashes,
    /// Temporary files.
    Temp,
    /// `node_modules`.
    NodeModules,
    /// Build output (`target`, `build`, `dist`, `.next`, `DerivedData`…).
    BuildOutput,
    /// Python virtual environments / `__pycache__`.
    PythonEnv,
    /// Downloaded packages of a package manager.
    Packages,
    /// Simulator / emulator data.
    Simulator,
    /// Archived builds.
    Archives,
    /// Device support files.
    DeviceSupport,
    /// A file or folder in Downloads.
    Download,
}
