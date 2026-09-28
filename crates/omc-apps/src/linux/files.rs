//! `app_files`: user data that belongs to an app. Package-owned files are left to the
//! package manager (`run_uninstaller`); only `AppImage` files, Wine programs and unmanaged
//! programs are bundle items.
//!
//! XDG base children and home dot entries are judged by [`userconf`]: a name related to
//! the app plus evidence (the app's programs or Electron archive contain the path, the
//! running app has files open in it), weighed against other installed apps and same-named
//! commands. Elsewhere confidence follows names: package database, Flatpak id, snap name,
//! desktop id, window class and Electron `productName` are High; program and display names
//! are Medium; vendor-nested and loose tokens are Low. Names another installed app also
//! claims are dropped.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use omc_proto::apps::{AppFile, AppFileKind, AppFilesReport, AppSource, Confidence};
use omc_proto::jobs::{Location, Phase};
use omc_proto::settings::CleanSettings;
use omc_scan::target::push_target;
use omc_scan::{Guard, JobCtx, Target, Walker, paths};

use super::common::{self, file_name, norm};
use super::desktop::{self, DesktopEntry};
use super::electron::{self, ElectronNames};
use super::evidence::{self, Bases};
use super::pkg::Manager;
use super::wine::{self, WineDetail};
use super::{AppDetail, inventory, uninstall};
use crate::userconf::{self, Lead, NameMatch, Rival, Signals};
use crate::{AppFiles, AppRecord, Error, Result};

/// Names an app's folders may carry, by strength.
#[derive(Debug, Default)]
pub(super) struct Names {
    /// Package name, desktop ids, Flatpak id, snap name, window class, `productName`.
    pub(super) strong: BTreeSet<String>,
    /// Program names, id segments, display name, package name without channel suffix.
    pub(super) weak: BTreeSet<String>,
    /// Loose tokens (the first word of a multi-word display name).
    pub(super) low: BTreeSet<String>,
}

impl Names {
    fn add_strong(&mut self, name: &str) {
        let name = norm(name);
        if !common::is_generic_name(&name) {
            self.weak.remove(&name);
            self.low.remove(&name);
            self.strong.insert(name);
        }
    }

    fn add_weak(&mut self, name: &str) {
        let name = norm(name);
        if !common::is_generic_name(&name) && !self.strong.contains(&name) {
            self.low.remove(&name);
            self.weak.insert(name);
        }
    }

    fn add_low(&mut self, name: &str) {
        let name = norm(name);
        if !common::is_generic_name(&name)
            && !self.strong.contains(&name)
            && !self.weak.contains(&name)
        {
            self.low.insert(name);
        }
    }

    /// Confidence of a folder or file called `name` (case-insensitive).
    fn confidence(&self, name: &str) -> Option<Confidence> {
        let name = norm(name);
        if self.strong.contains(&name) {
            Some(Confidence::High)
        } else if self.weak.contains(&name) {
            Some(Confidence::Medium)
        } else if self.low.contains(&name) {
            Some(Confidence::Low)
        } else {
            None
        }
    }

    /// Best confidence of any [`name_candidates`] of a file name.
    fn file_confidence(&self, name: &str) -> Option<Confidence> {
        name_candidates(name)
            .into_iter()
            .filter_map(|c| self.confidence(c))
            .max()
    }

    /// Best confidence of a config file name without its extension or KDE `rc` suffix
    /// (whole names of base-folder children are judged by [`userconf`]).
    fn stem_confidence(&self, name: &str) -> Option<Confidence> {
        name_candidates(name)
            .into_iter()
            .skip(1)
            .filter_map(|c| self.confidence(c))
            .max()
    }

    fn remove(&mut self, name: &str) {
        self.strong.remove(name);
        self.weak.remove(name);
        self.low.remove(name);
    }

    /// Names of the Electron app's `package.json`: `userData` is `productName`, else
    /// `name`.
    fn add_electron(&mut self, names: &ElectronNames) {
        if let Some(product) = &names.product {
            self.add_strong(product);
        }
        if let Some(name) = &names.name {
            if names.product.is_some() {
                self.add_weak(name);
            } else {
                self.add_strong(name);
            }
        }
        if let Some(desktop) = &names.desktop {
            self.add_weak(desktop);
        }
    }
}

/// App names a config file or folder name may stand for: itself, without a config
/// extension, and KDE's `<app>rc` / `<app>staterc` / `<app>.notifyrc`.
pub(super) fn name_candidates(name: &str) -> Vec<&str> {
    const EXTENSIONS: &[&str] = &[
        ".cfg",
        ".conf",
        ".ini",
        ".json",
        ".notifyrc",
        ".toml",
        ".xml",
        ".yaml",
        ".yml",
    ];
    let mut out = vec![name];
    out.extend(EXTENSIONS.iter().filter_map(|e| name.strip_suffix(e)));
    out.extend(
        name.strip_suffix("staterc")
            .or_else(|| name.strip_suffix("rc"))
            .filter(|s| !s.is_empty()),
    );
    out
}

/// Package name without a release-channel suffix (`google-chrome-stable` →
/// `google-chrome`), which is how the app names its folders.
pub(super) fn without_channel(package: &str) -> Option<&str> {
    const CHANNELS: &[&str] = &[
        "-appimage",
        "-beta",
        "-bin",
        "-canary",
        "-dev",
        "-git",
        "-insiders",
        "-nightly",
        "-stable",
        "-unstable",
    ];
    CHANNELS
        .iter()
        .find_map(|c| package.strip_suffix(c))
        .filter(|s| !s.is_empty())
}

/// Names identifying one desktop entry (used for the app and to exclude other apps).
pub(super) fn entry_keys(entry: &DesktopEntry) -> Vec<String> {
    let mut keys = vec![norm(&entry.id)];
    keys.extend(common::last_segment(&entry.id));
    keys.extend(entry.wm_class.as_deref().map(norm));
    if let Some(program) = entry.program()
        && !desktop::is_launcher(&program)
        && let Some(name) = Path::new(&program).file_name()
    {
        keys.push(norm(&name.to_string_lossy()));
    }
    keys
}

/// Candidate names of `app`.
pub(super) fn names_of(app: &AppRecord) -> Names {
    let mut names = Names::default();
    let detail = &app.detail;
    if let Some(package) = &detail.package {
        names.add_strong(package);
        if let Some(base) = without_channel(package) {
            names.add_weak(base);
        }
        if let Some(last) = common::last_segment(package)
            && app.info.source == AppSource::Flatpak
        {
            names.add_weak(&last);
        }
    }
    for entry in &detail.entries {
        let id = entry.id.split('_').next_back().unwrap_or(&entry.id);
        names.add_strong(&entry.id);
        names.add_strong(id);
        if let Some(last) = common::last_segment(&entry.id) {
            names.add_weak(&last);
        }
        if let Some(class) = &entry.wm_class {
            names.add_strong(class);
        }
    }
    for binary in &detail.binaries {
        if let Some(name) = file_name(binary) {
            names.add_weak(name);
            if let Some(stem) = name.strip_suffix(".exe") {
                names.add_weak(stem);
            }
        }
    }
    if let Some(image) = &detail.appimage {
        names.add_strong(&inventory::appimage_name(image));
    }
    let display = norm(&app.info.name);
    names.add_weak(&display);
    names.add_weak(&display.replace(' ', "-"));
    names.add_weak(&display.replace(' ', ""));
    if let Some((first, _)) = display.split_once(' ')
        && first.chars().count() >= 5
    {
        names.add_low(first);
    }
    names
}

/// One found item before measuring.
#[derive(Debug)]
struct Found {
    path: PathBuf,
    kind: AppFileKind,
    confidence: Confidence,
    needs_admin: bool,
}

#[derive(Debug, Default)]
struct Collector {
    items: Vec<Found>,
    seen: BTreeSet<PathBuf>,
}

impl Collector {
    fn add(&mut self, path: PathBuf, kind: AppFileKind, confidence: Confidence, needs_admin: bool) {
        // The same path found again (a name and evidence) keeps the stronger confidence.
        if let Some(same) = self.items.iter_mut().find(|f| f.path == path) {
            same.confidence = same.confidence.max(confidence);
            return;
        }
        if self.seen.iter().any(|p| paths::is_within(&path, p)) {
            return;
        }
        // A new directory swallows items already found inside it.
        self.items.retain(|f| !paths::is_within(&f.path, &path));
        self.seen.retain(|p| !paths::is_within(p, &path));
        self.seen.insert(path.clone());
        self.items.push(Found {
            path,
            kind,
            confidence,
            needs_admin,
        });
    }

    /// Adds `path` when it exists.
    fn add_existing(
        &mut self,
        path: PathBuf,
        kind: AppFileKind,
        confidence: Confidence,
        needs_admin: bool,
    ) {
        if path.symlink_metadata().is_ok() {
            self.add(path, kind, confidence, needs_admin);
        }
    }
}

/// Children of `dir` whose name (or config-file stem, see [`name_candidates`]) matches
/// `names`, with confidence.
fn matching_children(dir: &Path, names: &Names) -> Vec<(PathBuf, Confidence)> {
    common::children(dir)
        .into_iter()
        .filter_map(|p| {
            let confidence = names.file_confidence(file_name(&p)?)?;
            Some((p, confidence))
        })
        .collect()
}

/// Files of a base folder named after the app once a config extension or KDE `rc` suffix
/// is stripped (`dolphinrc`, `katestaterc`, `keepassxc.ini`).
fn config_files(dir: &Path, names: &Names) -> Vec<(PathBuf, Confidence)> {
    common::children(dir)
        .into_iter()
        .filter_map(|p| {
            let confidence = names.stem_confidence(file_name(&p)?)?;
            p.is_file().then_some((p, confidence))
        })
        .collect()
}

/// Folders of a base folder that group several apps of a vendor or tool; never searched
/// one level down.
fn is_container(name: &str) -> bool {
    const CONTAINERS: &[&str] = &["appimagekit", "desktop-directories", "menus", "session"];
    CONTAINERS.contains(&name) || evidence::is_shared(name)
}

/// `<base>/<vendor>/<name>` (JetBrains, `BraveSoftware`, Qt `<Org>/<App>.conf`): Low, since
/// the vendor folder is not the app's.
fn vendor_children(base: &Path, names: &Names) -> Vec<PathBuf> {
    common::children(base)
        .into_iter()
        .filter(|vendor| {
            file_name(vendor)
                .is_some_and(|n| names.file_confidence(n).is_none() && !is_container(n))
                && vendor.is_dir()
        })
        .flat_map(|vendor| common::children(&vendor))
        .filter(|p| {
            file_name(p).is_some_and(|n| {
                names
                    .file_confidence(n)
                    .is_some_and(|c| c > Confidence::Low)
            })
        })
        .collect()
}

/// A desktop entry or unit file references one of `binaries` or `names`.
fn references(entry_program: Option<&str>, binaries: &[PathBuf], names: &Names) -> bool {
    let Some(program) = entry_program else {
        return false;
    };
    let path = Path::new(program);
    if binaries.iter().any(|b| b == path) {
        return true;
    }
    !desktop::is_launcher(program)
        && path
            .file_name()
            .is_some_and(|n| names.strong.contains(&norm(&n.to_string_lossy())))
}

/// `ExecStart` program of a systemd unit file.
pub(super) fn unit_exec(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let value = line.trim().strip_prefix("ExecStart=")?;
        let value = value.trim_start_matches(['@', '-', ':', '+', '!']);
        desktop::exec_args(value).into_iter().next()
    })
}

/// Other installed apps, from their visible desktop entries.
#[derive(Debug, Default)]
struct Others {
    /// Their [`entry_keys`] (this app's package name excluded).
    keys: BTreeSet<String>,
    /// Their names, one set per entry: rivals for [`userconf`] candidates.
    rivals: Vec<userconf::Names>,
}

/// Other installed apps' desktop entries (entries of this app — its own files and ids, its
/// Flatpak/snap, one of its programs — skipped); their keys are dropped from `names`.
fn exclude_other_apps(names: &mut Names, app: &AppRecord, locales: &[String]) -> Others {
    let detail = &app.detail;
    let own: BTreeSet<&Path> = detail.entries.iter().map(|e| e.path.as_path()).collect();
    let own_ids: BTreeSet<&str> = detail.entries.iter().map(|e| e.id.as_str()).collect();
    let package = detail.package.as_deref();
    let package_key = package.map(norm);
    let mut others = Others::default();
    for (entry, _) in inventory::load_entries(locales) {
        let runs_own = || {
            entry
                .program()
                .and_then(|p| common::resolve_program(&p))
                .is_some_and(|p| detail.binaries.contains(&p))
        };
        if own.contains(entry.path.as_path())
            || own_ids.contains(entry.id.as_str())
            || (package.is_some() && entry.flatpak.as_deref() == package)
            || (package.is_some() && entry.snap.as_deref() == package)
            || !entry.is_visible_app()
            || runs_own()
        {
            continue;
        }
        let keys: Vec<String> = entry_keys(&entry)
            .into_iter()
            .filter(|k| package_key.as_deref() != Some(k.as_str()))
            .collect();
        let mut rival = userconf::Names::default();
        for key in keys.iter().chain(&entry.name) {
            rival.add(key);
        }
        if !rival.is_empty() {
            others.rivals.push(rival);
        }
        others.keys.extend(keys);
    }
    for key in &others.keys {
        names.remove(key);
    }
    others
}

/// A Wine desktop entry starts one of the app's shortcuts or programs.
fn wine_launches(entry: &DesktopEntry, wine: &WineDetail, binaries: &[PathBuf]) -> bool {
    let Some(exec) = entry.exec.as_deref().and_then(wine::parse_exec) else {
        return false;
    };
    let prefix = exec.prefix.clone().unwrap_or_else(|| wine.prefix.clone());
    exec.windows
        .iter()
        .filter_map(|w| wine::to_unix(&prefix, w))
        .chain(exec.unix)
        .any(|p| {
            wine.links.contains(&p)
                || binaries.contains(&p)
                || (p.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
                    && wine::link_program(&p, &prefix).is_some_and(|t| binaries.contains(&t)))
        })
}

/// The app itself when no package manager owns it, and its own user desktop entries.
fn bundle_items(app: &AppRecord, names: &Names, found: &mut Collector) {
    let detail = &app.detail;
    let source = app.info.source;
    let home = common::home();
    let in_home = |p: &Path| home.as_deref().is_some_and(|h| p.starts_with(h));
    if let Some(image) = &detail.appimage {
        found.add(
            image.clone(),
            AppFileKind::Bundle,
            Confidence::High,
            !in_home(image),
        );
        // AppImageUpdate keeps the previous version next to the image.
        let mut old = image.as_os_str().to_owned();
        old.push(".zs-old");
        found.add_existing(
            PathBuf::from(old),
            AppFileKind::Other,
            Confidence::High,
            !in_home(image),
        );
    }
    if let Some(dir) = detail.wine.as_ref().and_then(|w| w.dir.clone()) {
        found.add(dir, AppFileKind::Bundle, Confidence::Medium, false);
    }
    if source == AppSource::Desktop
        && let Some(binary) = detail.binaries.first()
        && in_home(binary)
    {
        let parent = binary.parent().filter(|p| {
            file_name(p).is_some_and(|n| names.confidence(n).is_some())
                && home.as_deref() != Some(*p)
        });
        found.add(
            parent.map_or_else(|| binary.clone(), Path::to_path_buf),
            AppFileKind::Bundle,
            Confidence::Medium,
            false,
        );
    }
    if matches!(source, AppSource::AppImage | AppSource::Desktop) {
        for entry in &detail.entries {
            if in_home(&entry.path) {
                found.add(
                    entry.path.clone(),
                    AppFileKind::Shortcut,
                    Confidence::High,
                    false,
                );
            }
        }
    }
}

/// `winemenubuilder`'s menu folder, `.menu`/`.directory` files and the Start Menu
/// shortcuts inside the prefix.
fn wine_items(app: &AppRecord, found: &mut Collector) {
    let Some(wine) = &app.detail.wine else {
        return;
    };
    let data = common::data_home();
    let config = common::config_home();
    let applications = data.as_ref().map(|d| d.join("applications"));
    if let Some(folder) = &wine.folder {
        found.add_existing(
            folder.clone(),
            AppFileKind::Shortcut,
            Confidence::High,
            false,
        );
        if let (Some(apps_dir), Some(data)) = (&applications, &data)
            && let Ok(rel) = folder.strip_prefix(apps_dir)
            && let Some(name) = wine::directory_file_name(rel)
        {
            found.add_existing(
                data.join("desktop-directories").join(name),
                AppFileKind::Shortcut,
                Confidence::High,
                false,
            );
        }
    }
    if let (Some(apps_dir), Some(config)) = (&applications, &config) {
        let merged = config.join("menus/applications-merged");
        for entry in &app.detail.entries {
            if let Ok(rel) = entry.path.strip_prefix(apps_dir)
                && let Some(name) = wine::menu_file_name(rel)
            {
                found.add_existing(
                    merged.join(name),
                    AppFileKind::Shortcut,
                    Confidence::High,
                    false,
                );
            }
        }
    }
    let folder_name = wine.folder.as_deref().and_then(file_name).map(norm);
    for link in &wine.links {
        let parent = link
            .parent()
            .filter(|p| folder_name.is_some() && file_name(p).map(norm) == folder_name);
        found.add_existing(
            parent.map_or_else(|| link.clone(), Path::to_path_buf),
            AppFileKind::Shortcut,
            Confidence::High,
            false,
        );
    }
}

/// KDE/Qt config files and vendor folders of the XDG bases (their other children and the
/// home dot entries: [`conf_items`]), Java/NSS per-app folders and Flatpak/Snap per-user
/// data.
fn data_items(app: &AppRecord, names: &Names, found: &mut Collector) {
    let detail = &app.detail;
    let source = app.info.source;
    let bases = [
        (common::config_home(), AppFileKind::Preferences),
        (common::data_home(), AppFileKind::Support),
        (common::state_home(), AppFileKind::Support),
        (common::cache_home(), AppFileKind::Cache),
    ];
    for (base, kind) in &bases {
        let Some(base) = base else { continue };
        for (path, confidence) in config_files(base, names) {
            found.add(path, *kind, confidence, false);
        }
        for path in vendor_children(base, names) {
            found.add(path, *kind, Confidence::Low, false);
        }
    }
    desktop_state_items(app, names, found);
    if let Some(home) = common::home() {
        for dir in [".mozilla", ".java", ".java/.userPrefs", ".pki"] {
            for (path, confidence) in matching_children(&home.join(dir), names) {
                found.add(
                    path,
                    AppFileKind::Support,
                    confidence.min(Confidence::Medium),
                    false,
                );
            }
        }
        if let Some(id) = &detail.package {
            match source {
                AppSource::Flatpak => found.add(
                    home.join(".var/app").join(id),
                    AppFileKind::Container,
                    Confidence::High,
                    false,
                ),
                AppSource::Snap => found.add(
                    home.join("snap").join(id),
                    AppFileKind::Container,
                    Confidence::High,
                    false,
                ),
                _ => {}
            }
        }
    }
}

/// Desktop-integration state: KDE UI layouts and session files, Flatpak overrides,
/// `AppImage` integration flags.
fn desktop_state_items(app: &AppRecord, names: &Names, found: &mut Collector) {
    let detail = &app.detail;
    let source = app.info.source;
    if let Some(data) = common::data_home() {
        // KDE toolbar/menu layouts.
        for (path, confidence) in matching_children(&data.join("kxmlgui5"), names) {
            found.add(path, AppFileKind::Preferences, confidence, false);
        }
        if let Some(id) = &detail.package
            && source == AppSource::Flatpak
        {
            found.add_existing(
                data.join("flatpak/overrides").join(id),
                AppFileKind::Preferences,
                Confidence::High,
                false,
            );
        }
        // `<name>_no_desktopintegration` flags of the AppImage desktop integration.
        for flag in common::children(&data.join("appimagekit")) {
            if file_name(&flag).is_some_and(|n| {
                n.split_once('_')
                    .is_some_and(|(app_name, _)| names.strong.contains(&norm(app_name)))
            }) {
                found.add(flag, AppFileKind::Other, Confidence::Medium, false);
            }
        }
    }
    if let Some(config) = common::config_home() {
        // KDE session-restore files: `<app>_<session id>…`.
        for file in common::children(&config.join("session")) {
            let confidence = file_name(&file)
                .and_then(|n| n.split_once('_'))
                .and_then(|(app_name, _)| names.confidence(app_name));
            if let Some(confidence) = confidence {
                found.add(
                    file,
                    AppFileKind::SavedState,
                    confidence.min(Confidence::Medium),
                    false,
                );
            }
        }
    }
}

/// System-wide configuration a plain removal keeps: Flatpak system overrides, `/etc`
/// folders and package conffiles (purge semantics; Low, never preselected).
fn system_items(app: &AppRecord, names: &Names, settings: &CleanSettings, found: &mut Collector) {
    let detail = &app.detail;
    let source = app.info.source;
    if let Some(id) = &detail.package
        && source == AppSource::Flatpak
        && settings.include_system
    {
        found.add_existing(
            Path::new("/var/lib/flatpak/overrides").join(id),
            AppFileKind::Preferences,
            Confidence::Low,
            true,
        );
    }
    if settings.include_system
        && let Some(manager) = Manager::of(source)
    {
        for name in &names.strong {
            found.add_existing(
                Path::new("/etc").join(name),
                AppFileKind::Preferences,
                Confidence::Low,
                true,
            );
        }
        if let Some(package) = &detail.package {
            for conf in manager.conffiles(package) {
                if conf.starts_with("/etc") {
                    found.add_existing(conf, AppFileKind::Preferences, Confidence::Low, true);
                }
            }
        }
    }
}

/// Icon theme names and `appimagekit_<md5>` prefixes of `entries`.
fn icon_keys<'a>(
    entries: impl IntoIterator<Item = &'a DesktopEntry>,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut icons = BTreeSet::new();
    let mut prefixes = BTreeSet::new();
    for entry in entries {
        if let Some(icon) = entry
            .icon
            .as_deref()
            .filter(|i| !i.contains('/') && !i.is_empty())
        {
            icons.insert(icon.to_owned());
        }
        prefixes.extend(appimagekit_prefix(&entry.id).map(str::to_owned));
    }
    (icons, prefixes)
}

/// `appimagekit_<md5>` of an `AppImage` integration file name
/// (`appimagekit_0f…9e-Obsidian.desktop`, `appimagekit_0f…9e_obsidian.png`).
pub(super) fn appimagekit_prefix(name: &str) -> Option<&str> {
    let hash = name.strip_prefix("appimagekit_")?;
    let hex = hash.get(..32)?;
    (hex.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| name.get(..44))
        .flatten()
}

/// Autostart entries, systemd user units (and their `.wants` links), user and desktop
/// launchers, icons and MIME packages.
fn launch_items(app: &AppRecord, names: &Names, locales: &[String], found: &mut Collector) {
    let detail = &app.detail;
    let binaries = &detail.binaries;
    let wine = detail.wine.as_ref();
    let launches = |e: &DesktopEntry| {
        references(e.program().as_deref(), binaries, names)
            || detail
                .appimage
                .as_deref()
                .is_some_and(|img| inventory::launches(e, img))
            || wine.is_some_and(|w| wine_launches(e, w, binaries))
    };
    if let Some(config) = common::config_home() {
        for file in common::desktop_files(&config.join("autostart")) {
            let Some(entry) = DesktopEntry::load(&file, locales) else {
                continue;
            };
            if names.strong.contains(&norm(&entry.id)) || launches(&entry) {
                found.add(file, AppFileKind::LaunchItem, Confidence::High, false);
            }
        }
        let units_dir = config.join("systemd/user");
        let mut units: BTreeSet<String> = BTreeSet::new();
        for file in common::children(&units_dir) {
            if file.extension().is_none_or(|e| e != "service") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            if references(unit_exec(&text).as_deref(), binaries, names) {
                units.extend(file_name(&file).map(str::to_owned));
                found.add(file, AppFileKind::LaunchItem, Confidence::High, false);
            }
        }
        for wants in common::children(&units_dir) {
            if wants.extension().is_some_and(|e| e == "wants") {
                for unit in &units {
                    found.add_existing(
                        wants.join(unit),
                        AppFileKind::LaunchItem,
                        Confidence::High,
                        false,
                    );
                }
            }
        }
    }
    let mut shortcuts: Vec<DesktopEntry> = Vec::new();
    let launcher_dirs = [
        common::data_home().map(|d| d.join("applications")),
        common::desktop_dir(),
    ];
    for dir in launcher_dirs.iter().flatten() {
        for file in common::desktop_files(dir) {
            let stem = file_name(&file).map(norm).unwrap_or_default();
            let by_name = names
                .strong
                .iter()
                .any(|n| n.chars().count() >= 4 && stem.contains(n.as_str()));
            let entry = DesktopEntry::load(&file, locales);
            if by_name || entry.as_ref().is_some_and(launches) {
                found.add(file, AppFileKind::Shortcut, Confidence::High, false);
                shortcuts.extend(entry);
            }
        }
    }
    icon_items(detail, names, &shortcuts, found);
}

/// Theme icons and MIME packages of the app's own and found launchers (`AppImage`
/// integration files share an `appimagekit_<md5>` prefix).
fn icon_items(
    detail: &AppDetail,
    names: &Names,
    shortcuts: &[DesktopEntry],
    found: &mut Collector,
) {
    let Some(data) = common::data_home() else {
        return;
    };
    let (icons, prefixes) = icon_keys(detail.entries.iter().chain(shortcuts));
    let matches = |path: &Path| {
        let Some(stem) = path.file_stem().map(|s| s.to_string_lossy()) else {
            return false;
        };
        icons.contains(stem.as_ref()) || prefixes.iter().any(|p| stem.starts_with(p.as_str()))
    };
    if !icons.is_empty() || !prefixes.is_empty() {
        for size in common::children(&data.join("icons/hicolor")) {
            for context in ["apps", "mimetypes"] {
                for icon in common::children(&size.join(context)) {
                    if matches(&icon) {
                        found.add(icon, AppFileKind::Other, Confidence::Medium, false);
                    }
                }
            }
        }
        for icon in common::children(&data.join("pixmaps")) {
            if matches(&icon) {
                found.add(icon, AppFileKind::Other, Confidence::Medium, false);
            }
        }
    }
    for package in common::children(&data.join("mime/packages")) {
        let hit = file_name(&package).is_some_and(|n| {
            prefixes.iter().any(|p| n.starts_with(p.as_str()))
                || n.strip_suffix(".xml")
                    .is_some_and(|s| names.strong.contains(&norm(s)))
        });
        if hit {
            found.add(package, AppFileKind::Other, Confidence::Medium, false);
        }
    }
}

/// Folders (with roles) the running app has open; none for Flatpak apps (their processes
/// see sandbox paths).
fn open_roots(app: &AppRecord) -> BTreeSet<(PathBuf, AppFileKind)> {
    if app.info.source == AppSource::Flatpak {
        return BTreeSet::new();
    }
    let Some(bases) = Bases::current() else {
        return BTreeSet::new();
    };
    let mut pids = uninstall::pids_of(app);
    if let Some(image) = &app.detail.appimage {
        pids.extend(evidence::appimage_pids(image));
    }
    pids.sort_unstable();
    pids.dedup();
    if pids.is_empty() {
        return BTreeSet::new();
    }
    evidence::open_roots(&pids, &bases)
}

/// Open folders that are not [`userconf`] candidates (`leads` are judged by [`conf_items`]):
/// strong evidence when the folder name also relates to the app; Low otherwise, since a
/// process also holds files of its host — Steam, a Wine prefix, a runtime. Folders of
/// other apps and Wine prefixes are dropped.
fn evidence_items(
    app: &AppRecord,
    names: &Names,
    others: &BTreeSet<String>,
    open: &BTreeSet<(PathBuf, AppFileKind)>,
    leads: &BTreeSet<PathBuf>,
    found: &mut Collector,
) {
    let wine_prefix = app.detail.wine.as_ref().map(|w| w.prefix.as_path());
    for (root, kind) in open {
        if leads.contains(root) {
            continue;
        }
        let Some(name) = file_name(root) else {
            continue;
        };
        let bare = name.trim_start_matches('.');
        let claimed = name_candidates(bare)
            .into_iter()
            .any(|c| others.contains(&norm(c)));
        let is_prefix = wine_prefix.is_some_and(|p| p.starts_with(root))
            || root.join("system.reg").exists()
            || root.join("drive_c").exists();
        if claimed || is_prefix {
            continue;
        }
        let confidence = if names.file_confidence(bare).is_some() {
            Confidence::High
        } else {
            Confidence::Low
        };
        found.add(root.clone(), *kind, confidence, false);
    }
}

/// Names the app's per-user configuration may carry: display name, package, desktop ids,
/// window classes, program names, `AppImage` name and Electron names.
fn conf_names(app: &AppRecord, electron: Option<&ElectronNames>) -> userconf::Names {
    let detail = &app.detail;
    let mut names = userconf::Names::default();
    names.add_display(&app.info.name);
    if let Some(package) = &detail.package {
        names.add(package);
        if let Some(base) = without_channel(package) {
            names.add(base);
        }
        if app.info.source == AppSource::Flatpak
            && let Some(last) = common::last_segment(package)
        {
            names.add_hint(&last);
        }
    }
    for entry in &detail.entries {
        names.add(&entry.id);
        if let Some(last) = common::last_segment(&entry.id) {
            names.add_hint(&last);
        }
        if let Some(class) = &entry.wm_class {
            names.add(class);
        }
    }
    for binary in &detail.binaries {
        if let Some(name) = file_name(binary) {
            names.add(name.strip_suffix(".exe").unwrap_or(name));
        }
    }
    if let Some(image) = &detail.appimage {
        names.add(&inventory::appimage_name(image));
    }
    if let Some(electron) = electron {
        for name in [&electron.product, &electron.name, &electron.desktop]
            .into_iter()
            .flatten()
        {
            names.add(name);
        }
    }
    names
}

/// Folders many programs live in: a program directly inside one is its own install root.
fn shared_dirs(home: &Path, command_dirs: &[PathBuf]) -> Vec<PathBuf> {
    const SYSTEM: &[&str] = &[
        "/",
        "/bin",
        "/opt",
        "/sbin",
        "/snap/bin",
        "/usr",
        "/usr/bin",
        "/usr/games",
        "/usr/lib",
        "/usr/lib32",
        "/usr/lib64",
        "/usr/libexec",
        "/usr/local",
        "/usr/local/bin",
        "/usr/local/lib",
        "/usr/local/libexec",
        "/usr/local/sbin",
        "/usr/local/share",
        "/usr/sbin",
        "/usr/share",
    ];
    let mut out: Vec<PathBuf> = SYSTEM.iter().map(PathBuf::from).collect();
    out.push(home.to_path_buf());
    out.extend(
        [
            "bin",
            ".local",
            ".local/bin",
            ".local/lib",
            ".local/share",
            "Applications",
            "Desktop",
            "Downloads",
        ]
        .iter()
        .map(|rel| home.join(rel)),
    );
    out.extend(command_dirs.iter().cloned());
    out
}

/// The install folder of `binary` (resolved) when it is the app's own: its folder, or the
/// folder above a `bin`/`sbin`/`libexec` folder (`/opt/<app>`, `/usr/lib/<app>`). A program
/// directly inside a `shared` folder (`/usr/bin`, `~/Applications`) is its own root.
fn install_root(binary: &Path, shared: &[PathBuf]) -> PathBuf {
    let is_shared = |p: &Path| shared.iter().any(|s| s == p);
    let Some(dir) = binary.parent().filter(|d| !is_shared(d)) else {
        return binary.to_path_buf();
    };
    let bin_dir = file_name(dir).is_some_and(|n| matches!(n, "bin" | "sbin" | "libexec"));
    match dir.parent() {
        Some(top) if bin_dir && !is_shared(top) => top.to_path_buf(),
        _ => dir.to_path_buf(),
    }
}

/// Files searched for candidate paths, most telling first (Electron `app.asar` archives,
/// the resolved programs, the `AppImage`), and the app's install roots.
fn search_files(detail: &AppDetail, shared: &[PathBuf]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut programs: Vec<PathBuf> = Vec::new();
    let mut own: Vec<PathBuf> = Vec::new();
    for binary in &detail.binaries {
        let real = std::fs::canonicalize(binary).unwrap_or_else(|_| binary.clone());
        if let Some(asar) = real
            .parent()
            .map(|d| d.join("resources/app.asar"))
            .filter(|a| a.is_file() && !files.contains(a))
        {
            files.push(asar);
        }
        let root = install_root(&real, shared);
        if !own.contains(&root) {
            own.push(root);
        }
        if !programs.contains(&real) {
            programs.push(real);
        }
    }
    if let Some(image) = &detail.appimage {
        if !programs.contains(image) {
            programs.push(image.clone());
        }
        if !own.contains(image) {
            own.push(image.clone());
        }
    }
    files.extend(programs);
    (files, own)
}

/// How strongly another installed app claims `stem`.
fn rival(stem: &str, rivals: &[userconf::Names]) -> Rival {
    match rivals.iter().filter_map(|r| r.relate(stem)).max() {
        Some(NameMatch::Exact) => Rival::Equal,
        Some(NameMatch::Partial) => Rival::Weaker,
        None => Rival::None,
    }
}

/// What decides a [`Lead`] besides its own name and binary evidence.
#[derive(Debug, Default)]
struct Judge<'a> {
    /// Roots the running app has files open in.
    open: Vec<PathBuf>,
    /// The Electron `userData` folder name (`productName`, else `name`).
    user_data: Option<&'a str>,
    /// Other installed apps' names.
    rivals: &'a [userconf::Names],
}

impl Judge<'_> {
    /// Confidence of `lead`: the running app has files open below it when an `open` root
    /// lies inside it; Electron's `$XDG_CONFIG_HOME/<userData>` counts as direct evidence.
    fn confidence(&self, lead: &Lead) -> Option<Confidence> {
        let c = &lead.candidate;
        let user_data = c.base == userconf::Base::Config && self.user_data == Some(c.name.as_str());
        userconf::confidence(Signals {
            name: Some(lead.name),
            binary: lead.binary || user_data,
            open: self.open.iter().any(|root| paths::is_within(root, &c.path)),
            command: lead.command(),
            rival: rival(&c.stem, self.rivals),
        })
    }
}

/// XDG base children and home dot entries backed by [`userconf`] evidence; returns every
/// candidate judged (kept or not), so open-file evidence does not add them again.
fn conf_items(
    app: &AppRecord,
    names: &userconf::Names,
    judge: &Judge<'_>,
    settings: &CleanSettings,
    threads: usize,
    found: &mut Collector,
) -> BTreeSet<PathBuf> {
    let Some(home) = common::home() else {
        return BTreeSet::new();
    };
    let bases = userconf::Bases::xdg(&home);
    let dirs = userconf::command_dirs(std::env::var_os("PATH").as_deref(), Some(&home));
    let (files, own) = search_files(&app.detail, &shared_dirs(&home, &dirs));
    let side = userconf::AppSide {
        names,
        files: &files,
        own: &own,
        wide: false,
    };
    let (leads, stats) = userconf::leads(&bases, side, &dirs, userconf::Limits::default(), threads);
    tracing::debug!(
        app = %app.info.name,
        leads = leads.len(),
        files = stats.files,
        skipped = stats.skipped,
        bytes = stats.bytes,
        elapsed_ms = stats.elapsed.as_millis(),
        "per-user config search"
    );
    let guard = Guard::new(settings);
    let mut judged = BTreeSet::new();
    for lead in leads {
        if let Some(confidence) = judge.confidence(&lead)
            && guard.check(&lead.candidate.path).is_ok()
        {
            found.add(
                lead.candidate.path.clone(),
                lead.candidate.kind(),
                confidence,
                false,
            );
        }
        judged.insert(lead.candidate.path);
    }
    judged
}

pub(super) fn app_files(
    app: &AppRecord,
    settings: &CleanSettings,
    walker: &Walker,
    ctx: &JobCtx,
) -> Result<AppFiles> {
    ctx.set_phase(Phase::Scanning);
    let locales = desktop::locales();
    let mut names = names_of(app);
    let package_files = || match (Manager::of(app.info.source), &app.detail.package) {
        (Some(manager), Some(package)) => manager.files(package),
        _ => Vec::new(),
    };
    let electron = electron::electron_names(&app.detail.binaries, package_files);
    if let Some(electron) = &electron {
        names.add_electron(electron);
    }
    let conf = conf_names(app, electron.as_ref());
    let others = exclude_other_apps(&mut names, app, &locales);
    let open = open_roots(app);
    let judge = Judge {
        open: open.iter().map(|(root, _)| root.clone()).collect(),
        user_data: electron
            .as_ref()
            .and_then(|e| e.product.as_deref().or(e.name.as_deref())),
        rivals: &others.rivals,
    };

    let mut found = Collector::default();
    bundle_items(app, &names, &mut found);
    wine_items(app, &mut found);
    let threads = walker.options().threads;
    let leads = conf_items(app, &conf, &judge, settings, threads, &mut found);
    evidence_items(app, &names, &others.keys, &open, &leads, &mut found);
    data_items(app, &names, &mut found);
    system_items(app, &names, settings, &mut found);
    launch_items(app, &names, &locales, &mut found);
    if ctx.is_cancelled() {
        return Err(Error::Cancelled);
    }
    found
        .items
        .retain(|f| !walker.options().is_excluded(&f.path));
    found
        .items
        .sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.path.cmp(&b.path)));

    ctx.set_phase(Phase::Measuring);
    let paths: Vec<PathBuf> = found.items.iter().map(|f| f.path.clone()).collect();
    let measures = walker.measure_all(&paths, ctx);
    if ctx.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let mut items = Vec::with_capacity(found.items.len());
    let mut targets = Vec::with_capacity(found.items.len());
    for (f, m) in found.items.into_iter().zip(measures) {
        if m.missing {
            continue;
        }
        let path = f.path.display().to_string();
        let target =
            Target::path(path.clone(), m.bytes, settings.files_delete).admin(f.needs_admin);
        let id = push_target(&mut targets, target);
        ctx.add_bytes(m.bytes);
        items.push(AppFile {
            id,
            location: Location::Path { path },
            kind: f.kind,
            bytes: m.bytes,
            confidence: f.confidence,
            needs_admin: f.needs_admin,
        });
    }
    ctx.add_items(u64::try_from(items.len()).unwrap_or(u64::MAX));
    Ok(AppFiles {
        report: AppFilesReport {
            app: app.info.clone(),
            uninstaller: uninstall::command_line(app),
            items,
        },
        targets,
        app: app.clone(),
    })
}

#[cfg(test)]
mod tests {
    use omc_proto::apps::AppInfo;

    use super::super::AppDetail;
    use super::*;

    fn record(source: AppSource, name: &str, detail: AppDetail) -> AppRecord {
        AppRecord {
            info: AppInfo {
                id: 0,
                name: name.to_owned(),
                version: None,
                publisher: None,
                ident: detail.package.clone(),
                location: None,
                bytes: None,
                source,
                system: false,
                running: false,
                last_used: None,
                installed: None,
                icon: None,
                needs_admin: false,
            },
            detail,
        }
    }

    #[test]
    fn names_rank_ids_above_programs() {
        let entry = DesktopEntry {
            id: "org.gnome.Rhythmbox3".to_owned(),
            wm_class: Some("rhythmbox".to_owned()),
            ..DesktopEntry::default()
        };
        let app = record(
            AppSource::Deb,
            "Rhythmbox",
            AppDetail {
                package: Some("rhythmbox".to_owned()),
                entries: vec![entry],
                binaries: vec![PathBuf::from("/usr/bin/rhythmbox")],
                ..AppDetail::default()
            },
        );
        let names = names_of(&app);
        assert_eq!(
            names.confidence("Rhythmbox"),
            Some(Confidence::High),
            "package name"
        );
        assert_eq!(
            names.confidence("org.gnome.rhythmbox3"),
            Some(Confidence::High),
            "desktop id"
        );
        assert_eq!(
            names.confidence("rhythmbox3"),
            Some(Confidence::Medium),
            "id segment is weak"
        );
        assert_eq!(names.confidence("gnome"), None, "generic names dropped");
    }

    #[test]
    fn unit_exec_strips_prefixes() {
        assert_eq!(
            unit_exec("[Service]\nExecStart=-/usr/bin/syncthing serve\n").as_deref(),
            Some("/usr/bin/syncthing"),
            "prefix stripped"
        );
    }

    #[test]
    fn collector_keeps_outermost_paths() {
        let mut c = Collector::default();
        c.add(
            PathBuf::from("/h/.config/app/sub"),
            AppFileKind::Other,
            Confidence::High,
            false,
        );
        c.add(
            PathBuf::from("/h/.config/app"),
            AppFileKind::Preferences,
            Confidence::High,
            false,
        );
        c.add(
            PathBuf::from("/h/.config/app/x"),
            AppFileKind::Other,
            Confidence::High,
            false,
        );
        assert_eq!(c.items.len(), 1, "nested paths collapse: {:?}", c.items);
    }

    #[test]
    fn config_file_names_map_to_apps() {
        assert_eq!(
            name_candidates("dolphinrc"),
            vec!["dolphinrc", "dolphin"],
            "KDE rc"
        );
        assert!(
            name_candidates("katestaterc").contains(&"kate"),
            "KDE state rc"
        );
        assert!(
            name_candidates("kate.notifyrc").contains(&"kate"),
            "notifyrc"
        );
        assert!(
            name_candidates("keepassxc.ini").contains(&"keepassxc"),
            "Qt settings"
        );
        assert_eq!(name_candidates("rc"), vec!["rc"], "bare suffix kept whole");
    }

    #[test]
    fn channel_suffixes_strip() {
        assert_eq!(
            without_channel("google-chrome-stable"),
            Some("google-chrome"),
            "stable"
        );
        assert_eq!(
            without_channel("visual-studio-code-bin"),
            Some("visual-studio-code"),
            "AUR bin"
        );
        assert_eq!(without_channel("firefox"), None, "no suffix");
        assert_eq!(without_channel("-beta"), None, "nothing left");
    }

    #[test]
    fn appimagekit_prefixes() {
        let hash = "0123456789abcdef0123456789abcdef";
        assert_eq!(
            appimagekit_prefix(&format!("appimagekit_{hash}-Joplin")),
            Some(format!("appimagekit_{hash}").as_str()),
            "desktop file"
        );
        assert_eq!(
            appimagekit_prefix("appimagekit_nothex0123456789abcdef0123456789"),
            None,
            "not a hash"
        );
        assert_eq!(appimagekit_prefix("appimagekit_0123"), None, "too short");
    }

    #[test]
    fn evidence_tiers_rank_names() {
        let entry = DesktopEntry {
            id: "com.visualstudio.code".to_owned(),
            wm_class: Some("Code".to_owned()),
            ..DesktopEntry::default()
        };
        let app = record(
            AppSource::Deb,
            "Visual Studio Code",
            AppDetail {
                package: Some("code-insiders".to_owned()),
                entries: vec![entry],
                binaries: vec![PathBuf::from("/usr/share/code/code-bin")],
                ..AppDetail::default()
            },
        );
        let mut names = names_of(&app);
        assert_eq!(
            names.confidence("code"),
            Some(Confidence::High),
            "window class"
        );
        assert_eq!(
            names.confidence("code-bin"),
            Some(Confidence::Medium),
            "program name"
        );
        assert_eq!(
            names.confidence("visual"),
            Some(Confidence::Low),
            "first display word"
        );
        assert_eq!(
            names.file_confidence("coderc"),
            Some(Confidence::High),
            "rc file of a strong name"
        );
        names.add_electron(&ElectronNames {
            product: Some("Code - OSS".to_owned()),
            name: Some("code-oss-dev".to_owned()),
            desktop: None,
        });
        assert_eq!(
            names.confidence("Code - OSS"),
            Some(Confidence::High),
            "productName"
        );
        assert_eq!(
            names.confidence("code-oss-dev"),
            Some(Confidence::Medium),
            "name behind productName"
        );
        names.add_electron(&ElectronNames {
            name: Some("vscodium".to_owned()),
            ..ElectronNames::default()
        });
        assert_eq!(
            names.confidence("vscodium"),
            Some(Confidence::High),
            "name without productName is the userData folder"
        );
    }

    #[test]
    fn collector_keeps_strongest_confidence_of_a_path() {
        let mut c = Collector::default();
        let path = PathBuf::from("/h/.config/app");
        c.add(
            path.clone(),
            AppFileKind::Preferences,
            Confidence::Low,
            false,
        );
        c.add(path.clone(), AppFileKind::Support, Confidence::High, false);
        c.add(path, AppFileKind::Cache, Confidence::Medium, false);
        assert_eq!(c.items.len(), 1, "one item per path: {:?}", c.items);
        assert_eq!(
            c.items.first().map(|f| (f.kind, f.confidence)),
            Some((AppFileKind::Preferences, Confidence::High)),
            "first kind, strongest confidence"
        );
    }

    #[test]
    fn install_roots_stop_at_shared_folders() {
        let shared = shared_dirs(Path::new("/home/u"), &[]);
        let root = |b: &str| install_root(Path::new(b), &shared);
        assert_eq!(
            root("/usr/bin/obsidian"),
            PathBuf::from("/usr/bin/obsidian"),
            "a program in /usr/bin is its own root"
        );
        assert_eq!(
            root("/opt/Obsidian/obsidian"),
            PathBuf::from("/opt/Obsidian"),
            "/opt/<app>"
        );
        assert_eq!(
            root("/usr/lib/zed/bin/zed"),
            PathBuf::from("/usr/lib/zed"),
            "folder above bin/"
        );
        assert_eq!(
            root("/usr/local/bin/tool"),
            PathBuf::from("/usr/local/bin/tool"),
            "shared bin folder"
        );
        assert_eq!(
            root("/home/u/.local/bin/tool"),
            PathBuf::from("/home/u/.local/bin/tool"),
            "per-user bin folder"
        );
        assert_eq!(
            root("/home/u/Applications/Joplin.AppImage"),
            PathBuf::from("/home/u/Applications/Joplin.AppImage"),
            "~/Applications is shared"
        );
        assert_eq!(
            root("/home/u/.local/share/JetBrains/Toolbox/bin/jetbrains-toolbox"),
            PathBuf::from("/home/u/.local/share/JetBrains/Toolbox"),
            "per-user install folder"
        );
    }

    #[test]
    fn rivals_rank_by_name_match() {
        let mut other = userconf::Names::default();
        other.add("Zettlr");
        let rivals = [other];
        assert_eq!(rival("zettlr", &rivals), Rival::Equal, "same name");
        assert_eq!(rival("zettlr-backup", &rivals), Rival::Weaker, "prefix");
        assert_eq!(rival("joplin", &rivals), Rival::None, "unrelated");
        assert_eq!(rival("zettlr", &[]), Rival::None, "no other apps");
    }

    fn lead(path: &str, stem: &str, name: NameMatch) -> Lead {
        Lead {
            candidate: userconf::Candidate {
                path: PathBuf::from(path),
                name: stem.to_owned(),
                stem: stem.to_owned(),
                base: userconf::Base::Config,
                is_dir: true,
            },
            name,
            binary: false,
            foreign_command: false,
            own_command: false,
        }
    }

    #[test]
    fn open_files_below_a_lead_are_evidence() {
        let partial = lead("/h/.config/zettlr", "zettlr", NameMatch::Partial);
        let judge = |open: &str, rivals| Judge {
            open: vec![PathBuf::from(open)],
            user_data: None,
            rivals,
        };
        let mut other = userconf::Names::default();
        other.add("zettlr");
        let rivals = [other];
        assert_eq!(
            Judge::default().confidence(&partial),
            Some(Confidence::Low),
            "partial name alone"
        );
        assert_eq!(
            judge("/h/.config/zettlr", &[]).confidence(&partial),
            Some(Confidence::High),
            "open files inside"
        );
        assert_eq!(
            judge("/h/.config/zettlr-old", &[]).confidence(&partial),
            Some(Confidence::Low),
            "a longer-named sibling is not inside"
        );
        assert_eq!(
            judge("/elsewhere", &rivals).confidence(&partial),
            None,
            "an exact rival drops a name-only match"
        );
        assert_eq!(
            judge("/h/.config/zettlr", &rivals).confidence(&partial),
            Some(Confidence::Medium),
            "open files survive an exact rival"
        );
    }

    #[test]
    fn electron_user_data_folder_is_evidence() {
        let judge = Judge {
            user_data: Some("Code"),
            ..Judge::default()
        };
        let config = lead("/h/.config/Code", "Code", NameMatch::Exact);
        assert_eq!(
            judge.confidence(&config),
            Some(Confidence::High),
            "$XDG_CONFIG_HOME/<productName>"
        );
        let lower = lead("/h/.config/code", "code", NameMatch::Exact);
        assert_eq!(
            judge.confidence(&lower),
            Some(Confidence::Medium),
            "the userData name is case-sensitive"
        );
        let mut cache = lead("/h/.cache/Code", "Code", NameMatch::Exact);
        cache.candidate.base = userconf::Base::Cache;
        assert_eq!(
            judge.confidence(&cache),
            Some(Confidence::Medium),
            "only the config base holds userData"
        );
    }

    #[test]
    fn conf_names_cover_ids_programs_and_electron() {
        let entry = DesktopEntry {
            id: "org.signal.Signal".to_owned(),
            wm_class: Some("Signal".to_owned()),
            ..DesktopEntry::default()
        };
        let app = record(
            AppSource::Deb,
            "Signal Messenger",
            AppDetail {
                package: Some("signal-desktop-beta".to_owned()),
                entries: vec![entry],
                binaries: vec![PathBuf::from("/opt/Signal/signal-desktop-bin")],
                ..AppDetail::default()
            },
        );
        let electron = ElectronNames {
            product: Some("Signal Beta".to_owned()),
            ..ElectronNames::default()
        };
        let names = conf_names(&app, Some(&electron));
        let exact = Some(NameMatch::Exact);
        assert_eq!(names.relate("Signal"), exact, "window class");
        assert_eq!(
            names.relate("signal-desktop"),
            exact,
            "package without channel"
        );
        assert_eq!(names.relate("signal-desktop-bin"), exact, "program name");
        assert_eq!(names.relate("Signal Beta"), exact, "productName");
        assert_eq!(
            names.relate("messenger"),
            Some(NameMatch::Partial),
            "display-name word"
        );
        assert_eq!(names.relate("telegram"), None, "unrelated");
    }
}
