//! Preferences stored by the daemon (so scheduled or tray-started work sees the same
//! settings as the UI) and the system facts the UI shows.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::apps::Confidence;
use crate::jobs::DeleteMethod;

/// Everything the daemon persists (`settings.toml` in the config dir).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Scanning and cleaning behaviour.
    pub clean: CleanSettings,
    /// UI preferences (theme, language…), opaque to the daemon: string keys and values
    /// owned by omc-ui.
    pub ui: BTreeMap<String, String>,
}

/// Scanning and cleaning behaviour. Every field has a default, so older files load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "flat user preferences: one switch per setting"
)]
pub struct CleanSettings {
    // ---- scope
    /// Absolute paths (or `~/…`) that are never scanned, listed or removed.
    pub exclude: Vec<String>,
    /// Folders searched for large/old files, duplicates, installers and project build
    /// output. Empty = the home folder.
    pub file_roots: Vec<String>,
    /// Stay on the file system of each root (don't descend into other volumes, network
    /// shares or mounted disk images).
    pub one_file_system: bool,
    /// Skip hidden files and folders in large/old, duplicates and space lens.
    pub skip_hidden: bool,
    /// Scanner threads; 0 = one per logical CPU (fastest).
    pub scan_threads: u16,

    // ---- junk
    /// Temp files and logs newer than this many hours are left alone (apps may be using
    /// them). 0 = no age limit.
    pub junk_min_age_hours: u32,
    /// Skip caches of apps and browsers that are running (they would be recreated or
    /// break the running app).
    pub skip_running_apps: bool,
    /// Include system-wide locations that need administrator rights.
    pub include_system: bool,

    // ---- browsers
    /// Offer cookies for removal (signs you out of websites).
    pub browser_cookies: bool,
    /// Offer browsing/download history for removal.
    pub browser_history: bool,
    /// Offer local storage / `IndexedDB` / other site data for removal.
    pub browser_site_data: bool,
    /// Offer saved sessions (open tabs) for removal.
    pub browser_sessions: bool,

    // ---- developer
    /// Project build output (`node_modules`, `target`…) is offered only when the project
    /// was not modified for this many days. 0 = all projects.
    pub dev_project_min_age_days: u32,
    /// How deep below each root projects are searched.
    pub dev_project_max_depth: u16,

    // ---- large & old
    /// Files at least this large are "large".
    pub large_min_bytes: u64,
    /// Files not modified for this many days are "old". 0 = off.
    pub old_days: u32,
    /// Old files smaller than this are ignored.
    pub old_min_bytes: u64,

    // ---- duplicates
    /// Files smaller than this are not compared.
    pub dup_min_bytes: u64,

    // ---- removal
    /// How junk (caches, logs, temp, build output) is removed.
    pub junk_delete: DeleteMethod,
    /// How user files (large/old, duplicates, installers, space lens, apps, leftovers)
    /// are removed.
    pub files_delete: DeleteMethod,
    /// Ask for the administrator password when an item needs it (otherwise such items
    /// fail with `permission_denied`).
    pub elevate: bool,

    // ---- uninstaller
    /// List apps that ship with the OS (never uninstallable).
    pub show_system_apps: bool,
    /// Related items at or above this confidence are preselected.
    pub leftover_confidence: Confidence,
    /// Run the app's own uninstaller / package manager before removing leftovers.
    pub run_vendor_uninstaller: bool,
    /// Quit the app (gracefully, then forcibly) before uninstalling it.
    pub quit_running_apps: bool,
    /// Windows: create a System Restore point before uninstalling.
    pub restore_point: bool,
    /// Windows: export registry keys to a `.reg` backup before deleting them.
    pub backup_registry: bool,
}

/// 1 MiB.
const MIB: u64 = 1 << 20;

impl Default for CleanSettings {
    fn default() -> Self {
        Self {
            exclude: Vec::new(),
            file_roots: Vec::new(),
            one_file_system: true,
            skip_hidden: true,
            scan_threads: 0,
            junk_min_age_hours: 24,
            skip_running_apps: true,
            include_system: true,
            browser_cookies: false,
            browser_history: false,
            browser_site_data: false,
            browser_sessions: false,
            dev_project_min_age_days: 30,
            dev_project_max_depth: 8,
            large_min_bytes: 100 * MIB,
            old_days: 365,
            old_min_bytes: 10 * MIB,
            dup_min_bytes: MIB,
            junk_delete: DeleteMethod::Permanent,
            files_delete: DeleteMethod::Trash,
            elevate: true,
            show_system_apps: false,
            leftover_confidence: Confidence::High,
            run_vendor_uninstaller: true,
            quit_running_apps: true,
            restore_point: false,
            backup_registry: true,
        }
    }
}

/// Facts about the machine and oh-my-clear's permissions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemInfo {
    /// Operating system.
    pub os: Os,
    /// Version string (`15.1`, `10.0.26100`, `Ubuntu 24.04`).
    pub os_version: String,
    /// Home folder.
    pub home: String,
    /// The daemon runs as administrator/root.
    pub elevated: bool,
    /// macOS Full Disk Access of the daemon; `not_applicable` elsewhere.
    pub full_disk_access: Access,
    /// Mounted volumes with their capacity.
    pub volumes: Vec<Volume>,
}

/// Operating systems.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Os {
    /// macOS.
    Macos,
    /// Windows.
    Windows,
    /// Linux (and other Unix).
    Linux,
}

/// State of an OS permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    /// Granted.
    Granted,
    /// Not granted; the UI explains how to grant it.
    Denied,
    /// Could not be determined.
    Unknown,
    /// The OS has no such permission.
    NotApplicable,
}

/// A mounted volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Volume {
    /// Mount point / drive root.
    pub mount: String,
    /// Volume label.
    pub name: String,
    /// Capacity in bytes.
    pub total: u64,
    /// Available bytes.
    pub free: u64,
}
