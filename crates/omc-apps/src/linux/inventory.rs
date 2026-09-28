//! `list_apps`: every source runs on its own thread (package list, desktop-file ownership,
//! Flatpak, Snap, processes, `AppImage` search), then the results are joined into records.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::thread::ScopedJoinHandle;

use omc_proto::apps::{AppInfo, AppSource};
use omc_proto::jobs::Phase;
use omc_proto::settings::CleanSettings;
use omc_scan::procs::{self, Process};
use omc_scan::{JobCtx, Walker, paths};

use super::AppDetail;
use super::common::{self, file_name};
use super::desktop::{self, DesktopEntry};
use super::pkg::{self, FlatpakApp, Manager, Package, SnapApp};
use super::wine;
use crate::{AppRecord, Error, Result};

/// Where a desktop entry was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DirKind {
    /// Distribution directories (`/usr/share/applications`…): package-owned.
    System,
    /// `~/.local/share/applications`.
    User,
    /// Flatpak exports (`user` = per-user installation).
    Flatpak {
        /// Per-user installation.
        user: bool,
    },
    /// `/var/lib/snapd/desktop/applications`.
    Snap,
}

/// Flatpak installation roots: (root, per user).
pub(super) fn flatpak_roots() -> Vec<(PathBuf, bool)> {
    let mut roots = vec![(PathBuf::from("/var/lib/flatpak"), false)];
    if let Some(data) = common::data_home() {
        roots.push((data.join("flatpak"), true));
    }
    roots
}

/// Application directories, most specific last (a user entry overrides a system one).
pub(super) fn application_dirs() -> Vec<(PathBuf, DirKind)> {
    let mut dirs: Vec<(PathBuf, DirKind)> = Vec::new();
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());
    for dir in data_dirs.split(':').filter(|d| d.starts_with('/')) {
        let dir = Path::new(dir);
        if dir.to_string_lossy().contains("flatpak/exports") || dir.starts_with("/var/lib/snapd") {
            continue;
        }
        dirs.push((dir.join("applications"), DirKind::System));
    }
    for fallback in ["/usr/share/applications", "/usr/local/share/applications"] {
        let fallback = PathBuf::from(fallback);
        if !dirs.iter().any(|(d, _)| *d == fallback) {
            dirs.push((fallback, DirKind::System));
        }
    }
    for (root, user) in flatpak_roots() {
        dirs.push((
            root.join("exports/share/applications"),
            DirKind::Flatpak { user },
        ));
    }
    dirs.push((
        PathBuf::from("/var/lib/snapd/desktop/applications"),
        DirKind::Snap,
    ));
    if let Some(data) = common::data_home() {
        dirs.push((data.join("applications"), DirKind::User));
    }
    dirs
}

/// Every desktop entry of [`application_dirs`] with the kind of its directory.
pub(super) fn load_entries(locales: &[String]) -> Vec<(DesktopEntry, DirKind)> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (dir, kind) in application_dirs() {
        for file in common::desktop_files(&dir) {
            // Symlinked data dirs (e.g. `/usr/local/share` → `/usr/share`) list files twice.
            let key = std::fs::canonicalize(&file).unwrap_or_else(|_| file.clone());
            if !seen.insert(key) {
                continue;
            }
            if let Some(entry) = DesktopEntry::load(&file, locales) {
                out.push((entry, kind));
            }
        }
    }
    out
}

/// Programs started by `entries` (resolved; launchers excluded).
pub(super) fn binaries_of<'a>(entries: impl IntoIterator<Item = &'a DesktopEntry>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let Some(program) = entry.program() else {
            continue;
        };
        if desktop::is_launcher(&program) {
            continue;
        }
        if let Some(path) = common::resolve_program(&program)
            && !out.contains(&path)
        {
            out.push(path);
        }
    }
    out
}

/// Icon theme roots searched for app icons.
fn icon_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(data) = common::data_home() {
        roots.push(data.join("icons"));
    }
    if let Some(home) = common::home() {
        roots.push(home.join(".icons"));
    }
    roots.push(PathBuf::from("/usr/local/share/icons"));
    roots.push(PathBuf::from("/usr/share/icons"));
    for (root, _) in flatpak_roots() {
        roots.push(root.join("exports/share/icons"));
    }
    roots
}

const ICON_SIZES: [&str; 5] = ["256x256", "128x128", "512x512", "64x64", "48x48"];

/// A PNG for `icon` (`Icon=` value): an absolute path, a hicolor theme icon, or a pixmap.
pub(super) fn resolve_icon(icon: &str, roots: &[PathBuf]) -> Option<PathBuf> {
    let icon = icon.trim();
    if icon.is_empty() {
        return None;
    }
    let path = Path::new(icon);
    if path.is_absolute() {
        return (path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("png"))
            && path.is_file())
        .then(|| path.to_path_buf());
    }
    let file = format!("{icon}.png");
    for root in roots {
        for size in ICON_SIZES {
            let candidate = root.join("hicolor").join(size).join("apps").join(&file);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let pixmap = Path::new("/usr/share/pixmaps").join(&file);
    pixmap.is_file().then_some(pixmap)
}

/// Package names shipped by desktop environments (settings, file managers, terminals):
/// shown as system apps.
fn is_desktop_core(package: &str) -> bool {
    const CORE: &[&str] = &[
        "gnome-control-center",
        "gnome-shell",
        "gnome-terminal",
        "gnome-console",
        "gnome-software",
        "gnome-system-monitor",
        "gnome-disk-utility",
        "gnome-session",
        "gnome-settings-daemon",
        "gnome-tweaks",
        "nautilus",
        "yelp",
        "ptyxis",
        "konsole",
        "dolphin",
        "systemsettings",
        "plasma-desktop",
        "plasma-workspace",
        "plasma-systemmonitor",
        "kinfocenter",
        "discover",
        "plasma-discover",
        "kwin",
        "xfce4-settings",
        "xfce4-terminal",
        "xfce4-panel",
        "xfce4-session",
        "thunar",
        "cinnamon",
        "cinnamon-control-center",
        "nemo",
        "mate-control-center",
        "mate-terminal",
        "caja",
        "lxqt-config",
        "pcmanfm",
        "pcmanfm-qt",
        "xdg-desktop-portal",
        "software-properties-gtk",
        "update-manager",
        "ubuntu-software",
        "yast2",
        "dconf-editor",
        "xterm",
    ];
    CORE.contains(&package)
}

/// `/proc/<pid>/comm` is truncated to 15 bytes.
const COMM_LEN: usize = 15;

/// Whether `process` belongs to the app described by `detail`.
pub(super) fn process_matches(
    source: AppSource,
    ident: Option<&str>,
    detail: &AppDetail,
    canonical: &[PathBuf],
    process: &Process,
) -> bool {
    if let Some(image) = &detail.appimage
        && process.exe.as_deref() == Some(image.as_path())
    {
        return true;
    }
    if source == AppSource::Snap
        && let Some(name) = ident
        && let Some(exe) = &process.exe
        && exe.starts_with(Path::new("/snap").join(name))
    {
        return true;
    }
    if source == AppSource::Flatpak {
        return false;
    }
    let exe_name = process.exe.as_deref().and_then(file_name);
    detail.binaries.iter().chain(canonical).any(|bin| {
        if process.exe.as_deref() == Some(bin.as_path()) {
            return true;
        }
        let Some(name) = file_name(bin) else {
            return false;
        };
        exe_name == Some(name)
            || process.name == name
            || (name.len() > COMM_LEN
                && process.name.len() == COMM_LEN
                && name.starts_with(&process.name))
    })
}

/// Canonical forms of `binaries` (symlinks such as `/usr/bin/firefox` → the real file).
pub(super) fn canonical_binaries(binaries: &[PathBuf]) -> Vec<PathBuf> {
    binaries
        .iter()
        .filter_map(|b| std::fs::canonicalize(b).ok())
        .filter(|c| !binaries.contains(c))
        .collect()
}

/// `AppImage` files in the usual places (`/opt` two levels deep).
pub(super) fn find_appimages() -> Vec<PathBuf> {
    let is_appimage = |p: &Path| {
        p.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("appimage"))
            && p.is_file()
    };
    let mut out = Vec::new();
    if let Some(home) = common::home() {
        for dir in ["Applications", ".local/bin", "Downloads", "Apps", "bin"] {
            out.extend(
                common::children(&home.join(dir))
                    .into_iter()
                    .filter(|p| is_appimage(p)),
            );
        }
    }
    for child in common::children(Path::new("/opt")) {
        if is_appimage(&child) {
            out.push(child);
        } else if child.is_dir() {
            out.extend(
                common::children(&child)
                    .into_iter()
                    .filter(|p| is_appimage(p)),
            );
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Display name of an `AppImage` file: its stem without version and architecture
/// (`Obsidian-1.5.3-x86_64.AppImage` → `Obsidian`).
pub(super) fn appimage_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut cut = stem.len();
    for (i, c) in stem.char_indices() {
        if (c == '-' || c == '_' || c == ' ' || c == '.')
            && stem.get(i.saturating_add(1)..).is_some_and(|rest| {
                rest.starts_with(|d: char| d.is_ascii_digit())
                    || rest.starts_with(['v', 'V'])
                        && rest.chars().nth(1).is_some_and(|d| d.is_ascii_digit())
                    || rest.starts_with("x86")
                    || rest.starts_with("aarch64")
                    || rest.starts_with("amd64")
                    || rest.starts_with("arm64")
            })
        {
            cut = i;
            break;
        }
    }
    let name = stem.get(..cut).unwrap_or(&stem).trim();
    if name.is_empty() {
        stem
    } else {
        name.to_owned()
    }
}

fn joined<T: Default>(handle: Option<ScopedJoinHandle<'_, T>>, what: &str) -> T {
    let Some(handle) = handle else {
        return T::default();
    };
    handle.join().unwrap_or_else(|_| {
        tracing::warn!(what, "inventory source thread panicked");
        T::default()
    })
}

/// Results of the parallel sources.
#[derive(Default)]
struct Sources {
    packages: Vec<(Manager, Option<Vec<Package>>)>,
    owners: BTreeMap<PathBuf, (Manager, String)>,
    flatpaks: Option<Vec<FlatpakApp>>,
    flatpak_running: HashSet<String>,
    snaps: Option<Vec<SnapApp>>,
    procs: Vec<Process>,
    appimages: Vec<PathBuf>,
}

fn gather(system_desktops: &[PathBuf], with_procs: bool) -> Sources {
    let managers: Vec<Manager> = Manager::ALL.into_iter().filter(|m| m.available()).collect();
    std::thread::scope(|s| {
        let lists: Vec<_> = managers
            .iter()
            .map(|m| (*m, s.spawn(move || m.packages())))
            .collect();
        let owners: Vec<_> = managers
            .iter()
            .map(|m| (*m, s.spawn(move || m.owners(system_desktops))))
            .collect();
        let flatpaks = s.spawn(pkg::flatpaks);
        let flatpak_running = with_procs.then(|| s.spawn(pkg::flatpak_running));
        let snaps = s.spawn(pkg::snaps);
        let procs = with_procs.then(|| s.spawn(procs::running));
        let appimages = s.spawn(find_appimages);

        let mut out = Sources::default();
        for (m, h) in lists {
            out.packages.push((m, joined(Some(h), "package list")));
        }
        for (m, h) in owners {
            for (path, name) in joined(Some(h), "desktop owners") {
                out.owners.entry(path).or_insert((m, name));
            }
        }
        out.flatpaks = joined(Some(flatpaks), "flatpak");
        out.flatpak_running = joined(flatpak_running, "flatpak ps");
        out.snaps = joined(Some(snaps), "snap");
        out.procs = joined(procs, "processes");
        out.appimages = joined(Some(appimages), "appimages");
        out
    })
}

fn source_of(m: Manager) -> AppSource {
    match m {
        Manager::Dpkg => AppSource::Deb,
        Manager::Rpm => AppSource::Rpm,
        Manager::Pacman => AppSource::Pacman,
    }
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

fn mtime(path: &Path) -> Option<i64> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .map(paths::unix_secs)
}

fn blank_info(name: String, source: AppSource) -> AppInfo {
    AppInfo {
        id: 0,
        name,
        version: None,
        publisher: None,
        ident: None,
        location: None,
        bytes: None,
        source,
        system: false,
        running: false,
        last_used: None,
        installed: None,
        icon: None,
        needs_admin: false,
    }
}

/// Prefers the entry named after the package, then the shortest id.
fn primary<'a>(entries: &[&'a DesktopEntry], ident: &str) -> Option<&'a DesktopEntry> {
    let ident = common::norm(ident);
    entries
        .iter()
        .find(|e| {
            let id = common::norm(&e.id);
            id == ident || common::last_segment(&id).is_some_and(|l| l == ident)
        })
        .or_else(|| entries.iter().min_by_key(|e| e.id.len()))
        .copied()
}

type Entries = [(DesktopEntry, DirKind)];

fn icon_of<'a>(
    entries: impl IntoIterator<Item = &'a DesktopEntry>,
    roots: &[PathBuf],
) -> Option<String> {
    entries
        .into_iter()
        .find_map(|e| e.icon.as_deref().and_then(|i| resolve_icon(i, roots)))
        .map(|p| display(&p))
}

/// Apps of distribution packages that own a visible system desktop entry.
fn package_apps(entries: &Entries, src: &Sources, roots: &[PathBuf], apps: &mut Vec<AppRecord>) {
    let mut by_package: BTreeMap<(String, Manager), Vec<&DesktopEntry>> = BTreeMap::new();
    for (entry, kind) in entries {
        if *kind != DirKind::System || !entry.is_visible_app() {
            continue;
        }
        if let Some((m, name)) = src.owners.get(&entry.path) {
            by_package
                .entry((name.clone(), *m))
                .or_default()
                .push(entry);
        }
    }
    let package_info: BTreeMap<(&str, Manager), &Package> = src
        .packages
        .iter()
        .flat_map(|(m, list)| {
            list.iter()
                .flatten()
                .map(move |p| ((p.name.as_str(), *m), p))
        })
        .collect();
    for ((name, manager), own) in &by_package {
        let Some(main) = primary(own, name) else {
            continue;
        };
        let package = package_info.get(&(name.as_str(), *manager));
        let mut info = blank_info(
            main.name.clone().unwrap_or_else(|| name.clone()),
            source_of(*manager),
        );
        info.ident = Some(name.clone());
        info.version = package.and_then(|p| p.version.clone());
        info.publisher = package.and_then(|p| p.publisher.clone());
        info.bytes = package.and_then(|p| p.bytes);
        info.installed = package
            .and_then(|p| p.installed)
            .or_else(|| mtime(&main.path));
        info.system = package.is_some_and(|p| p.essential) || is_desktop_core(name);
        info.needs_admin = true;
        info.icon = icon_of([main], roots);
        let binaries = binaries_of(own.iter().copied());
        info.location = Some(display(binaries.first().unwrap_or(&main.path)));
        apps.push(AppRecord {
            info,
            detail: AppDetail {
                package: Some(name.clone()),
                entries: own.iter().map(|e| (*e).clone()).collect(),
                binaries,
                ..AppDetail::default()
            },
        });
    }
}

fn flatpak_apps(entries: &Entries, src: &Sources, roots: &[PathBuf], apps: &mut Vec<AppRecord>) {
    for app in src.flatpaks.iter().flatten() {
        let prefix = format!("{}.", app.id);
        let own: Vec<&DesktopEntry> = entries
            .iter()
            .filter(|(e, k)| {
                matches!(k, DirKind::Flatpak { .. })
                    && (e.flatpak.as_deref() == Some(app.id.as_str())
                        || e.id == app.id
                        || e.id.starts_with(&prefix))
            })
            .map(|(e, _)| e)
            .collect();
        let mut info = blank_info(app.name.clone(), AppSource::Flatpak);
        info.ident = Some(app.id.clone());
        info.version.clone_from(&app.version);
        info.publisher.clone_from(&app.origin);
        info.bytes = app.bytes;
        info.needs_admin = !app.user;
        info.running = src.flatpak_running.contains(&app.id);
        let root = flatpak_roots()
            .into_iter()
            .find(|(_, user)| *user == app.user)
            .map(|(r, _)| r.join("app").join(&app.id));
        info.installed = root.as_deref().and_then(mtime);
        info.location = root.as_deref().map(display);
        info.icon = resolve_icon(&app.id, roots)
            .map(|p| display(&p))
            .or_else(|| icon_of(own.iter().copied(), roots));
        apps.push(AppRecord {
            info,
            detail: AppDetail {
                package: Some(app.id.clone()),
                flatpak_user: app.user,
                entries: own.into_iter().cloned().collect(),
                ..AppDetail::default()
            },
        });
    }
}

fn snap_apps(entries: &Entries, src: &Sources, roots: &[PathBuf], apps: &mut Vec<AppRecord>) {
    for snap in src.snaps.iter().flatten() {
        let prefix = format!("{}_", snap.name);
        let own: Vec<&DesktopEntry> = entries
            .iter()
            .filter(|(e, k)| {
                *k == DirKind::Snap
                    && (e.snap.as_deref() == Some(snap.name.as_str())
                        || e.id == snap.name
                        || e.id.starts_with(&prefix))
            })
            .map(|(e, _)| e)
            .collect();
        let name = primary(&own, &snap.name)
            .and_then(|e| e.name.clone())
            .unwrap_or_else(|| snap.name.clone());
        let mut info = blank_info(name, AppSource::Snap);
        info.ident = Some(snap.name.clone());
        info.version.clone_from(&snap.version);
        info.publisher.clone_from(&snap.publisher);
        info.system = snap.system;
        info.needs_admin = true;
        let file =
            Path::new("/var/lib/snapd/snaps").join(format!("{}_{}.snap", snap.name, snap.rev));
        if let Ok(meta) = std::fs::metadata(&file) {
            info.bytes = Some(meta.len());
            info.installed = meta.modified().ok().map(paths::unix_secs);
        }
        let root = Path::new("/snap").join(&snap.name);
        info.location = Some(display(&root));
        let gui_icon = root.join("current/meta/gui/icon.png");
        info.icon = if gui_icon.is_file() {
            Some(display(&gui_icon))
        } else {
            icon_of(own.iter().copied(), roots)
        };
        apps.push(AppRecord {
            info,
            detail: AppDetail {
                package: Some(snap.name.clone()),
                entries: own.into_iter().cloned().collect(),
                ..AppDetail::default()
            },
        });
    }
}

/// `AppImage` files, with the user desktop entries that launch them.
fn appimage_apps(
    user_entries: &[&DesktopEntry],
    src: &Sources,
    roots: &[PathBuf],
    apps: &mut Vec<AppRecord>,
) -> BTreeSet<PathBuf> {
    let mut claimed: BTreeSet<PathBuf> = BTreeSet::new();
    let home = common::home();
    for image in &src.appimages {
        let own: Vec<&DesktopEntry> = user_entries
            .iter()
            .filter(|e| launches(e, image))
            .copied()
            .collect();
        claimed.extend(own.iter().map(|e| e.path.clone()));
        let name = own
            .iter()
            .find_map(|e| e.name.clone())
            .unwrap_or_else(|| appimage_name(image));
        let mut info = blank_info(name, AppSource::AppImage);
        info.ident = file_name(image).map(str::to_owned);
        info.location = Some(display(image));
        if let Ok(meta) = std::fs::metadata(image) {
            info.bytes = Some(meta.len());
            info.installed = meta.modified().ok().map(paths::unix_secs);
        }
        info.needs_admin = !home.as_deref().is_some_and(|h| image.starts_with(h));
        info.icon = icon_of(own.iter().copied(), roots);
        apps.push(AppRecord {
            info,
            detail: AppDetail {
                entries: own.into_iter().cloned().collect(),
                binaries: vec![image.clone()],
                appimage: Some(image.clone()),
                ..AppDetail::default()
            },
        });
    }
    claimed
}

/// `entry`'s `Exec` runs `file`.
pub(super) fn launches(entry: &DesktopEntry, file: &Path) -> bool {
    entry
        .exec
        .as_deref()
        .is_some_and(|x| desktop::exec_args(x).iter().any(|a| Path::new(a) == file))
}

/// User desktop entries no package manager owns (and that are not overrides of one).
fn desktop_apps(
    entries: &Entries,
    user_entries: &[&DesktopEntry],
    claimed: &BTreeSet<PathBuf>,
    roots: &[PathBuf],
    apps: &mut Vec<AppRecord>,
) {
    let system_ids: HashSet<&str> = entries
        .iter()
        .filter(|(_, k)| *k != DirKind::User)
        .map(|(e, _)| e.id.as_str())
        .collect();
    let home = common::home();
    for entry in user_entries {
        if claimed.contains(&entry.path)
            || !entry.is_visible_app()
            || system_ids.contains(entry.id.as_str())
            || entry.flatpak.is_some()
            || entry.snap.is_some()
            || entry.target_missing()
        {
            continue;
        }
        let Some(program) = entry.program() else {
            continue;
        };
        if program.starts_with("/snap/bin/")
            || Path::new(&program)
                .file_name()
                .is_some_and(|n| n == "flatpak")
        {
            continue;
        }
        let mut info = blank_info(
            entry.name.clone().unwrap_or_else(|| entry.id.clone()),
            AppSource::Desktop,
        );
        info.ident = Some(entry.id.clone());
        let binaries = binaries_of([*entry]);
        info.location = Some(display(binaries.first().unwrap_or(&entry.path)));
        info.installed = mtime(&entry.path);
        info.needs_admin = binaries
            .first()
            .is_some_and(|b| !home.as_deref().is_some_and(|h| b.starts_with(h)));
        info.icon = icon_of([*entry], roots);
        apps.push(AppRecord {
            info,
            detail: AppDetail {
                entries: vec![(*entry).clone()],
                binaries,
                ..AppDetail::default()
            },
        });
    }
}

/// Windows programs of this user's Wine menu (one app per Start Menu folder).
fn wine_apps(locales: &[String], roots: &[PathBuf], apps: &mut Vec<AppRecord>) {
    for app in wine::apps(locales) {
        let Some(main) = app.entries.first() else {
            continue;
        };
        let mut info = blank_info(
            main.name.clone().unwrap_or_else(|| main.id.clone()),
            AppSource::Desktop,
        );
        let menu = app.detail.folder.as_deref().unwrap_or(&main.path);
        info.ident = Some(display(menu));
        info.location = app
            .detail
            .dir
            .as_deref()
            .or_else(|| app.programs.first().map(PathBuf::as_path))
            .map(display);
        info.installed = mtime(menu);
        info.icon = icon_of(&app.entries, roots);
        apps.push(AppRecord {
            info,
            detail: AppDetail {
                binaries: app.programs.into_iter().filter(|p| p.exists()).collect(),
                entries: app.entries,
                wine: Some(app.detail),
                ..AppDetail::default()
            },
        });
    }
}

fn mark_running(apps: &mut [AppRecord], procs: &[Process]) {
    let own_pid = std::process::id();
    for app in apps {
        if app.info.source == AppSource::Flatpak {
            continue;
        }
        let canonical = canonical_binaries(&app.detail.binaries);
        app.info.running = procs.iter().any(|p| {
            p.pid != own_pid
                && process_matches(
                    app.info.source,
                    app.info.ident.as_deref(),
                    &app.detail,
                    &canonical,
                    p,
                )
        });
    }
}

pub(super) fn list_apps(
    _settings: &CleanSettings,
    _walker: &Walker,
    ctx: &JobCtx,
) -> Result<Vec<AppRecord>> {
    ctx.set_phase(Phase::Scanning);
    let locales = desktop::locales();
    let entries = load_entries(&locales);
    ctx.add_items(u64::try_from(entries.len()).unwrap_or(u64::MAX));
    let system_desktops: Vec<PathBuf> = entries
        .iter()
        .filter(|(e, k)| *k == DirKind::System && e.is_visible_app())
        .map(|(e, _)| e.path.clone())
        .collect();
    let src = gather(&system_desktops, true);
    if ctx.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let roots = icon_roots();
    let mut apps: Vec<AppRecord> = Vec::new();
    package_apps(&entries, &src, &roots, &mut apps);
    flatpak_apps(&entries, &src, &roots, &mut apps);
    snap_apps(&entries, &src, &roots, &mut apps);
    let user_entries: Vec<&DesktopEntry> = entries
        .iter()
        .filter(|(e, k)| *k == DirKind::User && e.kind.as_deref() == Some("Application"))
        .map(|(e, _)| e)
        .collect();
    let claimed = appimage_apps(&user_entries, &src, &roots, &mut apps);
    desktop_apps(&entries, &user_entries, &claimed, &roots, &mut apps);
    wine_apps(&locales, &roots, &mut apps);
    mark_running(&mut apps, &src.procs);
    ctx.add_items(u64::try_from(apps.len()).unwrap_or(u64::MAX));
    Ok(apps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appimage_names_drop_version_and_arch() {
        assert_eq!(
            appimage_name(Path::new("/x/Obsidian-1.5.3-x86_64.AppImage")),
            "Obsidian",
            "version"
        );
        assert_eq!(
            appimage_name(Path::new("/x/nvim.appimage")),
            "nvim",
            "plain"
        );
        assert_eq!(
            appimage_name(Path::new("/x/Joplin_v2.14.AppImage")),
            "Joplin",
            "v-prefixed"
        );
        assert_eq!(
            appimage_name(Path::new("/x/my-tool-x86_64.AppImage")),
            "my-tool",
            "arch"
        );
    }

    #[test]
    fn processes_match_by_path_name_and_truncated_comm() {
        let detail = AppDetail {
            binaries: vec![PathBuf::from("/usr/bin/org.example.longname")],
            ..AppDetail::default()
        };
        let proc_named = |name: &str, exe: Option<&str>| Process {
            pid: 1,
            name: name.to_owned(),
            exe: exe.map(PathBuf::from),
        };
        let m = |p: &Process| process_matches(AppSource::Deb, None, &detail, &[], p);
        assert!(
            m(&proc_named("org.example.lon", None)),
            "truncated comm matches"
        );
        assert!(
            m(&proc_named("x", Some("/usr/bin/org.example.longname"))),
            "exe path matches"
        );
        assert!(
            !m(&proc_named("bash", Some("/usr/bin/bash"))),
            "others do not"
        );
        let snap = AppDetail::default();
        assert!(
            process_matches(
                AppSource::Snap,
                Some("firefox"),
                &snap,
                &[],
                &proc_named(
                    "firefox",
                    Some("/snap/firefox/4848/usr/lib/firefox/firefox")
                )
            ),
            "snap processes run from /snap/<name>"
        );
    }
}
