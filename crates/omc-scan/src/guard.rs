//! [`Guard`]: the last check before anything is removed. Every removal path (normal,
//! trash, elevated helper) asks it; scanners never produce protected paths, so a refusal
//! means a bug or a hand-crafted request, and the item fails with `protected`.
//!
//! Refused:
//! - relative paths and paths with `..` (never resolved lexically);
//! - file-system roots, the home folder, and the standard folders that scanners only
//!   remove *children* of (`~/Library/Caches`, `%LOCALAPPDATA%`, `~/.cache`, `/tmp`…);
//! - anything inside OS trees (`/System`, `/usr` except `/usr/local`, `C:\Windows` except
//!   its temp/update/dump folders, `/etc`, `/boot`…);
//! - anything inside, or containing, a user exclusion.

use std::path::{Path, PathBuf};

use omc_proto::settings::CleanSettings;

use crate::paths;

/// Why the guard refused a path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// Relative, or contains `..`.
    #[error("not a normalized absolute path")]
    NotAbsolute,
    /// A root, the home folder, or a standard container folder.
    #[error("essential folder")]
    Essential,
    /// Inside an OS-owned tree.
    #[error("inside a system folder")]
    SystemTree,
    /// Inside or containing a user exclusion.
    #[error("excluded in settings")]
    Excluded,
}

/// The removal safety check.
#[derive(Debug, Clone)]
pub struct Guard {
    /// Paths that must never be removed themselves (their children may be).
    essential: Vec<PathBuf>,
    /// Trees nothing inside of may be removed…
    system_trees: Vec<PathBuf>,
    /// …except inside these.
    allowed_in_system: Vec<PathBuf>,
    /// User exclusions.
    exclude: Vec<PathBuf>,
}

impl Guard {
    /// The guard for the current user and `settings`.
    pub fn new(settings: &CleanSettings) -> Self {
        let exclude = settings
            .exclude
            .iter()
            .filter_map(|p| paths::normalize(&paths::expand(p)))
            .collect();
        let mut guard = Self::platform(paths::home().as_deref());
        guard.exclude = exclude;
        guard
    }

    /// Checks `path` for removal.
    pub fn check(&self, path: &Path) -> Result<(), Refusal> {
        let Some(path) = paths::normalize(path) else {
            return Err(Refusal::NotAbsolute);
        };
        if path.parent().is_none() || is_drive_root(&path) {
            return Err(Refusal::Essential);
        }
        if self.essential.iter().any(|e| same(&path, e)) {
            return Err(Refusal::Essential);
        }
        if self.system_trees.iter().any(|t| paths::is_within(&path, t))
            && !self
                .allowed_in_system
                .iter()
                .any(|a| paths::is_within(&path, a) && !same(&path, a))
        {
            return Err(Refusal::SystemTree);
        }
        if self
            .exclude
            .iter()
            .any(|ex| paths::is_within(&path, ex) || paths::is_within(ex, &path))
        {
            return Err(Refusal::Excluded);
        }
        Ok(())
    }

    /// `path` is excluded by the user (checked per entry when removing a folder's
    /// contents).
    pub fn is_excluded(&self, path: &Path) -> bool {
        self.exclude.iter().any(|ex| paths::is_within(path, ex))
    }

    #[cfg(target_os = "macos")]
    fn platform(home: Option<&Path>) -> Self {
        let mut essential: Vec<PathBuf> = [
            "/Applications",
            "/Library",
            "/Library/Caches",
            "/Library/Logs",
            "/Library/Application Support",
            "/Library/Preferences",
            "/Library/LaunchAgents",
            "/Library/LaunchDaemons",
            "/Library/PrivilegedHelperTools",
            "/Users",
            "/Volumes",
            "/private",
            "/private/var",
            "/private/var/folders",
            "/private/var/log",
            "/private/tmp",
            "/tmp",
            "/var",
            "/opt",
            "/opt/homebrew",
            "/usr/local",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
        if let Some(home) = home {
            essential.push(home.to_path_buf());
            for sub in [
                "Applications",
                "Desktop",
                "Documents",
                "Downloads",
                "Movies",
                "Music",
                "Pictures",
                "Public",
                "Library",
                "Library/Application Support",
                "Library/Caches",
                "Library/Containers",
                "Library/Group Containers",
                "Library/Logs",
                "Library/Preferences",
                "Library/LaunchAgents",
                "Library/Mobile Documents",
                "Library/CloudStorage",
                "Library/Mail",
                "Library/Messages",
                "Library/Keychains",
                "Library/Saved Application State",
                ".Trash",
                ".cache",
                ".config",
                ".local",
                ".ssh",
            ] {
                essential.push(home.join(sub));
            }
        }
        let system_trees = [
            "/System",
            "/bin",
            "/sbin",
            "/usr",
            "/etc",
            "/private/etc",
            "/private/var/db",
            "/Library/Apple",
            "/cores",
            "/dev",
        ]
        .into_iter()
        .map(PathBuf::from)
        .chain(home.iter().map(|h| h.join("Library/Keychains")))
        .collect();
        let allowed_in_system = ["/usr/local", "/private/var/db/receipts"]
            .into_iter()
            .map(PathBuf::from)
            .collect();
        Self {
            essential,
            system_trees,
            allowed_in_system,
            exclude: Vec::new(),
        }
    }

    #[cfg(windows)]
    fn platform(home: Option<&Path>) -> Self {
        let windir = paths::env_dir("SystemRoot").unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let mut essential: Vec<PathBuf> = Vec::new();
        for var in [
            "ProgramFiles",
            "ProgramFiles(x86)",
            "ProgramW6432",
            "ProgramData",
            "LOCALAPPDATA",
            "APPDATA",
            "TEMP",
            "TMP",
            "PUBLIC",
            "ALLUSERSPROFILE",
            "CommonProgramFiles",
            "CommonProgramFiles(x86)",
        ] {
            if let Some(dir) = paths::env_dir(var) {
                essential.push(dir);
            }
        }
        if let Some(system_drive) = paths::env_dir("SystemDrive") {
            essential.push(system_drive.join("Users"));
            essential.push(system_drive.join("$Recycle.Bin"));
        }
        if let Some(home) = home {
            essential.push(home.to_path_buf());
            for sub in [
                "AppData",
                "AppData\\Local",
                "AppData\\LocalLow",
                "AppData\\Roaming",
                "AppData\\Local\\Temp",
                "AppData\\Local\\Microsoft",
                "AppData\\Local\\Microsoft\\Windows",
                "AppData\\Local\\Packages",
                "AppData\\Roaming\\Microsoft",
                "AppData\\Roaming\\Microsoft\\Windows\\Start Menu",
                "AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs",
                "Desktop",
                "Documents",
                "Downloads",
                "Music",
                "Pictures",
                "Videos",
                "OneDrive",
                ".cache",
                ".config",
            ] {
                essential.push(home.join(sub));
            }
        }
        let allowed_in_system = [
            "Temp",
            "SoftwareDistribution\\Download",
            "SoftwareDistribution\\DeliveryOptimization",
            "ServiceProfiles\\NetworkService\\AppData\\Local\\Microsoft\\Windows\\DeliveryOptimization",
            "Minidump",
            "LiveKernelReports",
            "Logs",
            "Prefetch",
            "Downloaded Program Files",
            "System32\\Tasks",
        ]
        .into_iter()
        .map(|sub| windir.join(sub))
        .collect();
        // MEMORY.DMP is a file directly in the Windows folder.
        let mut allowed: Vec<PathBuf> = allowed_in_system;
        allowed.push(windir.join("MEMORY.DMP"));
        essential.push(windir.clone());
        Self {
            essential,
            system_trees: vec![windir],
            allowed_in_system: allowed,
            exclude: Vec::new(),
        }
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    fn platform(home: Option<&Path>) -> Self {
        let mut essential: Vec<PathBuf> = [
            "/home",
            "/tmp",
            "/var",
            "/var/tmp",
            "/var/cache",
            "/var/log",
            "/var/lib",
            "/opt",
            "/usr/local",
            "/usr/local/bin",
            "/usr/local/share",
            "/media",
            "/mnt",
            "/run",
            "/srv",
            "/root",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
        if let Some(home) = home {
            essential.push(home.to_path_buf());
            for sub in [
                ".cache",
                ".config",
                ".local",
                ".local/share",
                ".local/share/Trash",
                ".local/share/applications",
                ".local/state",
                ".config/autostart",
                ".var",
                ".var/app",
                ".ssh",
                ".gnupg",
                "snap",
                "Desktop",
                "Documents",
                "Downloads",
                "Music",
                "Pictures",
                "Videos",
                "Public",
                "Templates",
            ] {
                essential.push(home.join(sub));
            }
        }
        let system_trees = [
            "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32", "/usr", "/etc", "/boot",
            "/proc", "/sys", "/dev", "/run", "/snap", "/var/lib",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
        let allowed_in_system = [
            "/usr/local",
            "/var/lib/snapd/cache",
            "/var/lib/systemd/coredump",
            "/var/lib/apt/lists",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
        Self {
            essential,
            system_trees,
            allowed_in_system,
            exclude: Vec::new(),
        }
    }
}

fn same(a: &Path, b: &Path) -> bool {
    if cfg!(any(windows, target_os = "macos")) {
        paths::eq_ignore_case(a, b)
    } else {
        a == b
    }
}

/// `C:\` (a prefix plus the root separator and nothing else).
fn is_drive_root(path: &Path) -> bool {
    use std::path::Component;
    path.components()
        .all(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard_with(exclude: &[&str]) -> Guard {
        Guard::new(&CleanSettings {
            exclude: exclude.iter().map(|s| (*s).to_owned()).collect(),
            ..CleanSettings::default()
        })
    }

    #[test]
    fn roots_home_and_relative_paths_are_refused() {
        let guard = guard_with(&[]);
        let root = if cfg!(windows) { r"C:\" } else { "/" };
        assert_eq!(
            guard.check(Path::new(root)),
            Err(Refusal::Essential),
            "root"
        );
        assert_eq!(
            guard.check(Path::new("relative/x")),
            Err(Refusal::NotAbsolute),
            "relative"
        );
        if let Some(home) = paths::home() {
            assert_eq!(guard.check(&home), Err(Refusal::Essential), "home itself");
            assert_eq!(
                guard.check(&home.join("x").join("..").join("y")),
                Err(Refusal::NotAbsolute),
                "`..` never resolves"
            );
            assert!(
                guard
                    .check(&home.join("Downloads").join("setup.dmg"))
                    .is_ok(),
                "a file inside Downloads is removable"
            );
        }
    }

    #[test]
    fn exclusions_protect_their_tree_and_ancestors() {
        let Some(home) = paths::home() else { return };
        let excluded = home.join("Projects").join("keep");
        let guard = guard_with(&[&excluded.display().to_string()]);
        assert_eq!(
            guard.check(&excluded.join("a.txt")),
            Err(Refusal::Excluded),
            "inside"
        );
        assert_eq!(
            guard.check(&home.join("Projects")),
            Err(Refusal::Excluded),
            "an ancestor would delete the exclusion"
        );
        assert!(
            guard.check(&home.join("Projects").join("other")).is_ok(),
            "a sibling is fine"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_system_trees_are_refused() {
        let guard = guard_with(&[]);
        assert_eq!(
            guard.check(Path::new("/System/Library/Caches/x")),
            Err(Refusal::SystemTree),
            "/System"
        );
        assert_eq!(
            guard.check(Path::new("/usr/lib/x")),
            Err(Refusal::SystemTree),
            "/usr"
        );
        assert!(
            guard.check(Path::new("/usr/local/Caskroom/x")).is_ok(),
            "/usr/local is allowed"
        );
        assert!(
            guard
                .check(Path::new("/private/var/db/receipts/com.x.bom"))
                .is_ok(),
            "receipts are allowed"
        );
        assert_eq!(
            guard.check(Path::new("/Library/Caches")),
            Err(Refusal::Essential),
            "container itself"
        );
        assert!(
            guard.check(Path::new("/Library/Caches/com.x")).is_ok(),
            "its children are fine"
        );
    }
}
