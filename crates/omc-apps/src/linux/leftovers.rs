//! Leftovers: XDG folders of software that is no longer installed (untouched for 90 days),
//! data of removed Flatpaks and snaps, autostart entries and units whose program is gone,
//! and desktop entries that launch nothing.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use omc_proto::jobs::{Denied, Location, Phase};
use omc_proto::junk::{JunkGroup, JunkItem, JunkKind, JunkReport, Safety};
use omc_proto::settings::CleanSettings;
use omc_scan::target::push_target;
use omc_scan::{JobCtx, Scanned, Target, Walker, errors, paths};

use super::common::{self, DAY_SECS, file_name, norm};
use super::desktop::{self, DesktopEntry};
use super::pkg::{self, Manager};
use super::{files, inventory};
use crate::{Error, Result};

/// XDG folders untouched for less than this are never orphans (the app may reappear or
/// be a portable tool).
const ORPHAN_MIN_AGE_DAYS: i64 = 90;

/// Folders of the desktop environment, toolkits and the OS itself (normalized names).
const DESKTOP_FOLDERS: &[&str] = &[
    "applications",
    "autostart",
    "akonadi",
    "baloo",
    "backgrounds",
    "color",
    "dbus-1",
    "dconf",
    "desktop-directories",
    "environment.d",
    "evolution-data-server",
    "fcitx",
    "fcitx5",
    "flatpak",
    "fontconfig",
    "fonts",
    "gstreamer-1.0",
    "gtk-2.0",
    "gtk-3.0",
    "gtk-4.0",
    "gvfs-metadata",
    "ibus",
    "icc",
    "icons",
    "kactivitymanagerd",
    "keyrings",
    "kscreen",
    "kwin",
    "menus",
    "mesa_shader_cache",
    "mesa_shader_cache_db",
    "mime",
    "mozc",
    "nvidia",
    "pipewire",
    "pki",
    "pulse",
    "qt5ct",
    "qt6ct",
    "qtproject",
    "kvantum",
    "radv_builtin_shaders",
    "session",
    "sessions",
    "sounds",
    "systemd",
    "themes",
    "thumbnails",
    "tracker",
    "tracker3",
    "trash",
    "user-dirs",
    "vulkan",
    "wireplumber",
    "xdg-desktop-portal",
    "xorg",
    "xsettingsd",
    "ubuntu-report",
    "update-notifier",
    "ibus-table",
    "imsettings",
    "pki",
    "abrt",
    "containers",
    "recently-used",
    "recently-used.xbel",
    "event-sound-cache",
    "motd.legal-displayed",
    "oh-my-clear",
    "omc",
    "nautilus",
    "totem",
    "at-spi",
    "at-spi2",
    "enchant",
    "gegl-0.4",
    "babl-0.1",
    "webkitgtk",
    "webkit",
    "geoclue",
    "speech-dispatcher",
    "orca",
    "yelp",
    "folks",
    "telepathy",
    "zeitgeist",
    "gsconnect",
    "cosmic",
    "hypr",
    "sway",
    "i3",
    "waybar",
    "kitty",
    "labwc",
    "niri",
    "river",
    "wayfire",
    "xfce4",
    "xfconf",
    "lxqt",
    "lxsession",
    "lxpanel",
    "openbox",
    "mate",
    "cinnamon",
    "budgie-desktop",
    "deepin",
    // Vendor folders shared by several apps: too coarse to call orphans.
    "jetbrains",
    "google",
    "microsoft",
    "mozilla",
    "bravesoftware",
    "opera",
    "vivaldi",
    "electron",
];

/// Name prefixes of desktop-environment folders.
const DESKTOP_FOLDER_PREFIXES: &[&str] = &[
    "gnome",
    "kde",
    "plasma",
    "xfce",
    "org.gnome.",
    "org.kde.",
    "org.freedesktop.",
    "gtk",
    "mate-",
    "cinnamon",
    "lxqt",
    "gvfs",
    "gsettings",
    "ubuntu",
    "fedora",
    "kded",
    "ksycoca",
    "kwallet",
    "kglobal",
    "kio",
    "kxmlgui",
    "kconf",
    "khotkeys",
    "kactivity",
];

/// A folder of the desktop environment, a toolkit or the OS itself (never an orphan).
pub(super) fn is_desktop_folder(name: &str) -> bool {
    DESKTOP_FOLDERS.contains(&name) || DESKTOP_FOLDER_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// `name` (normalized folder name) belongs to something installed.
pub(super) fn is_installed(name: &str, installed: &BTreeSet<String>) -> bool {
    let name = name.trim_start_matches('.');
    if installed.contains(name) {
        return true;
    }
    for variant in [
        name.replace(' ', "-"),
        name.replace(' ', ""),
        name.replace('_', "-"),
    ] {
        if installed.contains(&variant) {
            return true;
        }
    }
    // `libreoffice` ↔ `libreoffice-core`, `code` ↔ `code.desktop`-style suffixes.
    if installed
        .range(name.to_owned()..)
        .take_while(|n| n.starts_with(name))
        .any(|n| {
            n.strip_prefix(name)
                .is_some_and(|r| r.starts_with(['-', '.', '_']))
        })
    {
        return true;
    }
    // `org.example.App` → `app`; `discord-canary` → `discord`.
    if common::last_segment(name).is_some_and(|l| installed.contains(&l)) {
        return true;
    }
    name.char_indices()
        .filter(|(_, c)| matches!(c, '-' | '_' | '.'))
        .filter_map(|(i, _)| name.get(..i))
        .any(|prefix| prefix.chars().count() >= 4 && installed.contains(prefix))
}

/// What is installed, as far as leftovers are concerned.
#[derive(Debug, Default)]
struct Installed {
    /// Normalized names of packages, Flatpaks, snaps, desktop entries, `AppImage` files
    /// and programs on `PATH`.
    names: BTreeSet<String>,
    /// At least one distribution package database was read.
    packages_ok: bool,
    /// Flatpak app ids, when flatpak answered.
    flatpak_ids: Option<BTreeSet<String>>,
    /// Snap names, when snap answered.
    snap_names: Option<BTreeSet<String>>,
}

fn program_names() -> Vec<String> {
    common::path_dirs()
        .iter()
        .flat_map(|d| common::children(d))
        .filter_map(|p| file_name(&p).map(norm))
        .collect()
}

/// Names of everything installed (all packages, not just apps), sources in parallel.
fn installed(locales: &[String]) -> Installed {
    let managers: Vec<Manager> = Manager::ALL.into_iter().filter(|m| m.available()).collect();
    std::thread::scope(|s| {
        let lists: Vec<_> = managers
            .iter()
            .map(|m| s.spawn(move || m.packages()))
            .collect();
        let flatpaks = s.spawn(pkg::flatpaks);
        let snaps = s.spawn(pkg::snaps);
        let entries = s.spawn(|| inventory::load_entries(locales));
        let appimages = s.spawn(inventory::find_appimages);
        let programs = s.spawn(program_names);

        let mut out = Installed::default();
        for list in lists.into_iter().filter_map(|h| h.join().ok().flatten()) {
            out.packages_ok = true;
            out.names.extend(list.into_iter().map(|p| norm(&p.name)));
        }
        out.flatpak_ids = flatpaks
            .join()
            .ok()
            .flatten()
            .map(|apps| apps.into_iter().map(|a| a.id).collect());
        for id in out.flatpak_ids.iter().flatten() {
            out.names.insert(norm(id));
            out.names.extend(common::last_segment(id));
        }
        out.snap_names = snaps
            .join()
            .ok()
            .flatten()
            .map(|list| list.into_iter().map(|s| s.name).collect());
        for name in out.snap_names.iter().flatten() {
            out.names.insert(norm(name));
        }
        for (entry, _) in entries.join().unwrap_or_default() {
            out.names.extend(files::entry_keys(&entry));
            if let Some(name) = &entry.name {
                let name = norm(name);
                out.names.insert(name.replace(' ', ""));
                out.names.insert(name.replace(' ', "-"));
                out.names.insert(name);
            }
        }
        for image in appimages.join().unwrap_or_default() {
            out.names.insert(norm(&inventory::appimage_name(&image)));
        }
        out.names.extend(programs.join().unwrap_or_default());
        out
    })
}

/// One leftover before measuring.
#[derive(Debug)]
struct Candidate {
    kind: JunkKind,
    name: String,
    path: PathBuf,
    safety: Safety,
    /// Only counts when untouched for [`ORPHAN_MIN_AGE_DAYS`].
    aged: bool,
}

impl Candidate {
    fn new(kind: JunkKind, name: impl Into<String>, path: PathBuf, safety: Safety) -> Self {
        Self {
            kind,
            name: name.into(),
            path,
            safety,
            aged: false,
        }
    }
}

/// A measured leftover.
#[derive(Debug)]
struct Row {
    cand: Candidate,
    bytes: u64,
    files: u64,
    modified: Option<i64>,
}

fn list_dir(dir: &Path, denied: &mut Vec<Denied>) -> Vec<PathBuf> {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries.flatten().map(|e| e.path()).collect(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => {
            denied.push(Denied {
                path: dir.display().to_string(),
                reason: errors::classify(&err, dir),
            });
            Vec::new()
        }
    }
}

/// A real directory (not a symlink).
fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

fn entry_name(entry: &DesktopEntry) -> String {
    entry.name.clone().unwrap_or_else(|| entry.id.clone())
}

/// XDG folders of uninstalled software (only when a package database was read, else
/// everything would look orphaned), and data of removed Flatpaks and snaps.
fn orphan_folders(installed: &Installed, found: &mut Vec<Candidate>, denied: &mut Vec<Denied>) {
    if installed.packages_ok {
        let bases = [
            (common::config_home(), Safety::Review),
            (common::data_home(), Safety::Review),
            (common::state_home(), Safety::Review),
            (common::cache_home(), Safety::Safe),
        ];
        for (base, safety) in bases {
            let Some(base) = base else { continue };
            for child in list_dir(&base, denied) {
                let Some(name) = file_name(&child) else {
                    continue;
                };
                let key = norm(name.trim_start_matches('.'));
                if !is_real_dir(&child)
                    || common::is_generic_name(&key)
                    || is_desktop_folder(&key)
                    || is_installed(&key, &installed.names)
                {
                    continue;
                }
                let mut cand = Candidate::new(JunkKind::OrphanFiles, name, child.clone(), safety);
                cand.aged = true;
                found.push(cand);
            }
        }
    }
    let Some(home) = common::home() else {
        return;
    };
    for (dir, known) in [
        (home.join(".var/app"), installed.flatpak_ids.as_ref()),
        (home.join("snap"), installed.snap_names.as_ref()),
    ] {
        let Some(known) = known else { continue };
        for child in list_dir(&dir, denied) {
            if let Some(name) = file_name(&child)
                && !known.contains(name)
                && is_real_dir(&child)
            {
                found.push(Candidate::new(
                    JunkKind::OrphanFiles,
                    name,
                    child.clone(),
                    Safety::Review,
                ));
            }
        }
    }
}

/// Autostart entries and systemd user units whose program is gone; desktop entries that
/// launch nothing.
fn broken_entries(locales: &[String], found: &mut Vec<Candidate>) {
    if let Some(config) = common::config_home() {
        for file in common::desktop_files(&config.join("autostart")) {
            if let Some(entry) = DesktopEntry::load(&file, locales)
                && entry.target_missing()
            {
                found.push(Candidate::new(
                    JunkKind::OrphanLaunchItems,
                    entry_name(&entry),
                    file,
                    Safety::Review,
                ));
            }
        }
        for file in common::children(&config.join("systemd/user")) {
            if file.extension().is_none_or(|e| e != "service") || !file.is_file() {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            if let Some(program) = files::unit_exec(&text)
                && Path::new(&program).is_absolute()
                && !Path::new(&program).exists()
            {
                let name = file_name(&file).unwrap_or_default().to_owned();
                found.push(Candidate::new(
                    JunkKind::OrphanLaunchItems,
                    name,
                    file,
                    Safety::Review,
                ));
            }
        }
    }
    let mut shortcut_dirs: Vec<PathBuf> = Vec::new();
    shortcut_dirs.extend(common::data_home().map(|d| d.join("applications")));
    shortcut_dirs.extend(
        paths::env_dir("XDG_DESKTOP_DIR").or_else(|| common::home().map(|h| h.join("Desktop"))),
    );
    for dir in shortcut_dirs {
        for file in common::desktop_files(&dir) {
            if let Some(entry) = DesktopEntry::load(&file, locales)
                && entry.kind.as_deref() == Some("Application")
                && entry.target_missing()
            {
                found.push(Candidate::new(
                    JunkKind::BrokenShortcuts,
                    entry_name(&entry),
                    file,
                    Safety::Review,
                ));
            }
        }
    }
}

/// Groups rows by kind (largest first) and assigns ids in final order.
fn build_report(
    rows: Vec<Row>,
    denied: Vec<Denied>,
    settings: &CleanSettings,
    ctx: &JobCtx,
) -> Scanned<JunkReport> {
    let mut groups: Vec<(JunkKind, Vec<Row>)> = Vec::new();
    for row in rows {
        let kind = row.cand.kind;
        match groups.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, list)) => list.push(row),
            None => groups.push((kind, vec![row])),
        }
    }
    for (_, list) in &mut groups {
        list.sort_by(|a, b| {
            b.bytes
                .cmp(&a.bytes)
                .then_with(|| a.cand.path.cmp(&b.cand.path))
        });
    }
    groups.sort_by_cached_key(|(kind, list)| {
        let total = list.iter().fold(0_u64, |s, r| s.saturating_add(r.bytes));
        (std::cmp::Reverse(total), *kind)
    });
    let mut targets = Vec::new();
    let mut report = JunkReport {
        groups: Vec::with_capacity(groups.len()),
        denied,
    };
    for (kind, list) in groups {
        let mut items = Vec::with_capacity(list.len());
        for row in list {
            let path = row.cand.path.display().to_string();
            let id = push_target(
                &mut targets,
                Target::path(path.clone(), row.bytes, settings.files_delete),
            );
            ctx.add_bytes(row.bytes);
            items.push(JunkItem {
                id,
                name: row.cand.name,
                location: Location::Path { path },
                tag: None,
                bytes: row.bytes,
                files: row.files,
                modified: row.modified,
                safety: row.cand.safety,
                needs_admin: false,
                app_running: false,
                ident: None,
                icon: None,
            });
        }
        ctx.add_items(u64::try_from(items.len()).unwrap_or(u64::MAX));
        report.groups.push(JunkGroup { kind, items });
    }
    Scanned { report, targets }
}

pub(super) fn leftovers(
    settings: &CleanSettings,
    walker: &Walker,
    ctx: &JobCtx,
) -> Result<Scanned<JunkReport>> {
    ctx.set_phase(Phase::Scanning);
    let locales = desktop::locales();
    let installed = installed(&locales);
    if ctx.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let mut denied = Vec::new();
    let mut found: Vec<Candidate> = Vec::new();
    orphan_folders(&installed, &mut found, &mut denied);
    broken_entries(&locales, &mut found);

    let mut seen = BTreeSet::new();
    found.retain(|c| seen.insert(c.path.clone()) && !walker.options().is_excluded(&c.path));
    if ctx.is_cancelled() {
        return Err(Error::Cancelled);
    }
    ctx.set_phase(Phase::Measuring);
    let paths: Vec<PathBuf> = found.iter().map(|c| c.path.clone()).collect();
    let measures = walker.measure_all(&paths, ctx);
    if ctx.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let cutoff = paths::now_secs().saturating_sub(ORPHAN_MIN_AGE_DAYS.saturating_mul(DAY_SECS));
    let rows: Vec<Row> = found
        .into_iter()
        .zip(measures)
        .filter(|(_, m)| !m.missing)
        .filter_map(|(cand, m)| {
            let modified = m.newest.or_else(|| {
                std::fs::symlink_metadata(&cand.path)
                    .and_then(|meta| meta.modified())
                    .ok()
                    .map(paths::unix_secs)
            });
            if cand.aged && modified.is_none_or(|t| t > cutoff) {
                return None;
            }
            Some(Row {
                cand,
                bytes: m.bytes,
                files: m.files,
                modified,
            })
        })
        .collect();
    Ok(build_report(rows, denied, settings, ctx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_matching_variants() {
        let installed: BTreeSet<String> = ["libreoffice-core", "code", "discord", "google-chrome"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        assert!(is_installed("libreoffice", &installed), "package prefix");
        assert!(is_installed("code", &installed), "exact");
        assert!(is_installed("discord-canary", &installed), "folder prefix");
        assert!(
            is_installed("com.discordapp.discord", &installed),
            "reverse DNS"
        );
        assert!(!is_installed("slack", &installed), "unknown");
        assert!(
            !is_installed("cod", &installed),
            "short prefix is not a match"
        );
    }

    #[test]
    fn desktop_folders_are_never_orphans() {
        for name in [
            "gtk-3.0",
            "dconf",
            "gnome-shell",
            "kdeconnect",
            "plasma-workspace",
            "trash",
        ] {
            assert!(is_desktop_folder(name), "{name} is a desktop folder");
        }
        assert!(!is_desktop_folder("slack"), "apps are not");
    }
}
