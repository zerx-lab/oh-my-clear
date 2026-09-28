//! Everything that belongs to one app: install folder or Store package, data folders,
//! shortcuts, registry keys (vendor keys, `Run` values, `App Paths`, COM/ProgID
//! registrations, file-type links), services, scheduled tasks and MSI registrations, plus
//! the deeper traces in [`super::related`] and per-user configuration outside `AppData`
//! ([`crate::userconf`]). Every item's confidence comes from the evidence-weighted
//! [`clues::confidence`] (per-user configuration: [`userconf::confidence`]).

use std::collections::HashSet;
use std::path::PathBuf;

use omc_proto::apps::{AppFile, AppFileKind, AppFilesReport, Confidence};
use omc_proto::jobs::{Location, Phase, SpecialAction};
use omc_proto::settings::CleanSettings;
use omc_scan::{Guard, JobCtx, Target, Walker};

use super::apps::{self, Peers};
use super::clues::{self, BinaryRole, Clue};
use super::parse::{self, Hive};
use super::{known, reg, related, tasks};
use crate::userconf::{self, AppSide, Bases, Limits, Names, Signals};
use crate::{AppFiles, AppRecord, Error, Result};

/// `…\CurrentVersion` below `SOFTWARE`.
const CURRENT_VERSION: &str = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion";

/// One thing found, before measuring.
#[derive(Debug, Clone)]
pub(super) struct Found {
    pub(super) location: Location,
    pub(super) kind: AppFileKind,
    pub(super) confidence: Confidence,
    pub(super) admin: bool,
}

impl Found {
    pub(super) fn path(path: String, kind: AppFileKind, confidence: Confidence) -> Self {
        let admin = known::needs_admin(&path);
        Self {
            location: Location::Path { path },
            kind,
            confidence,
            admin,
        }
    }

    pub(super) fn key(hive: Hive, path: &str, confidence: Confidence) -> Self {
        Self {
            location: Location::RegistryKey {
                key: hive.full(path),
            },
            kind: AppFileKind::Registry,
            confidence,
            admin: hive != Hive::CurrentUser,
        }
    }

    pub(super) fn value(hive: Hive, path: &str, name: &str, confidence: Confidence) -> Self {
        Self {
            location: Location::RegistryValue {
                key: hive.full(path),
                name: name.to_owned(),
            },
            kind: AppFileKind::Registry,
            confidence,
            admin: hive != Hive::CurrentUser,
        }
    }
}

/// What identifies the app's files.
pub(super) struct Matcher {
    /// Normalised product names not shared with another installed app.
    pub(super) products: Vec<String>,
    /// Normalised vendor.
    pub(super) publisher: Option<String>,
    /// Exact names (display name, install folder name) for [`Clue::ExactName`].
    pub(super) exact: Vec<String>,
    /// Lower-case `dir\` needles (install folder) and full program paths that a command
    /// or shortcut must mention.
    pub(super) needles: Vec<String>,
    /// Distinctive lower-case program file names (generic ones like `update.exe` left
    /// out), for traces keyed by program name only.
    pub(super) exes: Vec<String>,
    /// Folder names the app declared itself before it was uninstalled (Electron
    /// `productName`), kept for the post-uninstall rescan.
    pub(super) declared: Vec<String>,
    pub(super) peers: Peers,
}

impl Matcher {
    pub(super) fn product(&self, name: &str) -> bool {
        let normalized = parse::normalize_name(name);
        self.products.contains(&normalized)
    }

    pub(super) fn vendor(&self, name: &str) -> bool {
        let normalized = parse::normalize_publisher(name);
        self.publisher.as_deref() == Some(normalized.as_str())
    }

    /// The vendor folder/key itself may go (not shared, not an OS vendor).
    pub(super) fn vendor_removable(&self) -> bool {
        self.publisher
            .as_deref()
            .is_some_and(|p| !parse::is_os_vendor(p) && !self.peers.publishers.contains(p))
    }

    pub(super) fn exact(&self, name: &str) -> bool {
        self.exact
            .iter()
            .any(|e| e.eq_ignore_ascii_case(name.trim()))
    }

    /// Clues of a folder/key matched by product name.
    pub(super) fn name_clues(&self, name: &str) -> Vec<Clue> {
        let mut clues = vec![Clue::ProductName];
        if self.exact(name) {
            clues.push(Clue::ExactName);
        }
        clues.extend(clues::short_clue(name));
        clues
    }

    /// Confidence of a folder/key matched by product name, below `parents` clues.
    pub(super) fn name_confidence(
        &self,
        name: &str,
        parents: &[Clue],
        cap: Confidence,
    ) -> Option<Confidence> {
        let mut all = parents.to_vec();
        all.extend(self.name_clues(name));
        clues::confidence(&all, cap)
    }

    pub(super) fn mentions(&self, text: &str) -> bool {
        let expanded = reg::expand(text);
        self.needles
            .iter()
            .any(|n| parse::mentions_dir(&expanded, n))
    }

    /// `path` (a plain path, already expanded) is the install folder, inside it, or one of
    /// the app's programs.
    pub(super) fn owns_path(&self, path: &str) -> bool {
        let lower = path
            .trim()
            .trim_matches('"')
            .to_lowercase()
            .replace('/', "\\");
        self.needles.iter().any(|n| {
            if n.ends_with('\\') {
                lower.starts_with(n.as_str()) || lower == n.trim_end_matches('\\')
            } else {
                lower == *n
            }
        })
    }
}

/// Builds the matcher of `app`; `declared` names come from an earlier scan.
fn matcher(app: &AppRecord, declared: Vec<String>) -> Matcher {
    let detail = &app.detail;
    let peers = apps::peers(&detail.keys, &app.info.name);
    let location = detail.location.as_deref();
    let mut products = parse::product_keys(&app.info.name, app.info.publisher.as_deref(), location);
    products.retain(|p| !peers.has_name(p) && !parse::is_os_vendor(p));
    let publisher = app
        .info
        .publisher
        .as_deref()
        .map(parse::normalize_publisher)
        .filter(|p| p.chars().count() >= 3);
    let mut exact = vec![app.info.name.trim().to_owned()];
    if let Some(dir) = location {
        exact.push(parse::file_name(dir).to_owned());
    }
    let mut needles = Vec::new();
    if let Some(dir) = location {
        needles.push(parse::dir_needle(dir));
    }
    if let Some(exe) = detail.icon_exe.as_deref() {
        needles.push(exe.to_lowercase());
    }
    let exes = detail
        .exes
        .iter()
        .filter(|e| !clues::is_generic_exe(e))
        .cloned()
        .collect();
    Matcher {
        products,
        publisher,
        exact,
        needles,
        exes,
        declared,
        peers,
    }
}

pub(crate) fn collect(
    app: &AppRecord,
    settings: &CleanSettings,
    walker: &Walker,
    ctx: &JobCtx,
) -> Result<AppFiles> {
    ctx.set_phase(Phase::Scanning);
    let m = matcher(app, Vec::new());
    let found = search(app, &m, settings, ctx)?;
    check_cancel(ctx)?;
    let found = dedupe(found, walker);
    measure(app, found, settings, walker, ctx)
}

/// Discovery again with the same identifiers after the vendor uninstaller ran: returns only
/// items that exist now and were not in `before` (ids renumbered from 0). Folders the app
/// declared itself in the first scan (Electron data folders) are searched again even
/// though the program that declared them is gone.
pub(crate) fn rescan(
    before: &AppFiles,
    settings: &CleanSettings,
    walker: &Walker,
    ctx: &JobCtx,
) -> Result<AppFiles> {
    ctx.set_phase(Phase::Scanning);
    let app = &before.app;
    let m = matcher(app, declared_folders(&before.report));
    let known: HashSet<String> = before
        .report
        .items
        .iter()
        .map(|i| i.location.display().to_lowercase())
        .collect();
    let found = search(app, &m, settings, ctx)?;
    check_cancel(ctx)?;
    let found: Vec<Found> = dedupe(found, walker)
        .into_iter()
        .filter(|f| !known.contains(&f.location.display().to_lowercase()))
        .filter(|f| registry_present(&f.location))
        .collect();
    measure(app, found, settings, walker, ctx)
}

/// Names of the High-confidence data folders of an earlier report.
fn declared_folders(report: &AppFilesReport) -> Vec<String> {
    report
        .items
        .iter()
        .filter(|i| i.kind == AppFileKind::Support && i.confidence == Confidence::High)
        .filter_map(|i| i.location.as_path())
        .map(|p| parse::file_name(p).to_owned())
        .collect()
}

/// A registry key/value still exists (other locations: `true`, paths are measured).
fn registry_present(location: &Location) -> bool {
    match location {
        Location::RegistryKey { key } => {
            parse::parse_reg_path(key).is_some_and(|(hive, path)| reg::exists(hive, path))
        }
        Location::RegistryValue { key, name } => parse::parse_reg_path(key)
            .is_some_and(|(hive, path)| reg::value_exists(hive, path, name)),
        Location::Path { .. } | Location::Special { .. } => true,
    }
}

/// Measures paths (dropping missing ones), assigns ids and builds targets.
fn measure(
    app: &AppRecord,
    found: Vec<Found>,
    settings: &CleanSettings,
    walker: &Walker,
    ctx: &JobCtx,
) -> Result<AppFiles> {
    ctx.set_phase(Phase::Measuring);
    let paths: Vec<PathBuf> = found
        .iter()
        .filter_map(|f| f.location.as_path().map(PathBuf::from))
        .collect();
    let mut measures = walker.measure_all(&paths, ctx).into_iter();
    let mut items = Vec::with_capacity(found.len());
    let mut targets = Vec::with_capacity(found.len());
    for f in found {
        let (bytes, target) = match &f.location {
            Location::Path { path } => {
                let Some(measure) = measures.next() else {
                    continue;
                };
                if measure.missing {
                    continue;
                }
                let target =
                    Target::path(path.clone(), measure.bytes, settings.files_delete).admin(f.admin);
                (measure.bytes, target)
            }
            other => (0, Target::location(other.clone(), 0).admin(f.admin)),
        };
        let id = omc_scan::target::push_target(&mut targets, target);
        ctx.add_items(1);
        ctx.add_bytes(bytes);
        items.push(AppFile {
            id,
            location: f.location,
            kind: f.kind,
            bytes,
            confidence: f.confidence,
            needs_admin: f.admin,
        });
    }
    check_cancel(ctx)?;
    let uninstaller = uninstaller_display(app);
    Ok(AppFiles {
        report: AppFilesReport {
            app: app.info.clone(),
            uninstaller,
            items,
        },
        targets,
        app: app.clone(),
    })
}

/// The install folder or package, then everything related to it.
fn search(
    app: &AppRecord,
    m: &Matcher,
    settings: &CleanSettings,
    ctx: &JobCtx,
) -> Result<Vec<Found>> {
    let mut found = Vec::new();
    if let Some(package) = app.detail.package.as_ref() {
        if let Some(dir) = package.location.as_deref() {
            found.push(Found {
                admin: true,
                ..Found::path(dir.to_owned(), AppFileKind::Bundle, Confidence::High)
            });
        }
        if let Some(family) = package.family_name.as_deref()
            && let Some(data) = known::env_join("LOCALAPPDATA", &format!("Packages\\{family}"))
        {
            found.push(Found::path(data, AppFileKind::Container, Confidence::High));
        }
    } else {
        let shared_dlls = related::shared_dlls(m, &mut found);
        if let Some(dir) = app.detail.location.as_deref() {
            let mut evidence = vec![Clue::Reference];
            if m.peers.overlaps(dir) || shared_dlls {
                evidence.push(Clue::Shared);
            }
            if let Some(confidence) = clues::confidence(&evidence, Confidence::High) {
                found.push(Found {
                    admin: app.info.needs_admin || known::needs_admin(dir),
                    ..Found::path(dir.to_owned(), AppFileKind::Bundle, confidence)
                });
            }
        }
        folders(m, &mut found);
        shortcuts(m, &mut found);
        related::files(app, m, &mut found);
        check_cancel(ctx)?;
        registry(app, m, ctx, &mut found);
        check_cancel(ctx)?;
        related::registry(app, m, ctx, &mut found);
        check_cancel(ctx)?;
        services_and_tasks(app, m, &mut found);
    }
    check_cancel(ctx)?;
    user_config(app, m, settings, &mut found);
    Ok(found)
}

/// Per-user configuration outside the `AppData` folders (`%USERPROFILE%\.<name>`,
/// `%USERPROFILE%\.config\<name>`), weighed by [`userconf`]: the app's programs or
/// Electron archive naming it, its names, same-named commands and other installed apps.
fn user_config(app: &AppRecord, m: &Matcher, settings: &CleanSettings, found: &mut Vec<Found>) {
    let Some(home) = known::env("USERPROFILE").map(PathBuf::from) else {
        return;
    };
    let names = config_names(app, m);
    if names.is_empty() {
        return;
    }
    let files = config_files(app, m);
    let own: Vec<PathBuf> = app
        .detail
        .location
        .iter()
        .chain(
            app.detail
                .package
                .as_ref()
                .and_then(|p| p.location.as_ref()),
        )
        .map(PathBuf::from)
        .collect();
    let side = AppSide {
        names: &names,
        files: &files,
        own: &own,
        wide: true,
    };
    let dirs = userconf::command_dirs(std::env::var_os("PATH").as_deref(), Some(&home));
    let (leads, stats) = userconf::leads(
        &Bases::windows(&home),
        side,
        &dirs,
        Limits::default(),
        usize::from(settings.scan_threads),
    );
    tracing::debug!(app = %app.info.name, leads = leads.len(), ?stats, "per-user config search");
    let mut peers = Names::default();
    for name in &m.peers.names {
        peers.add(name);
    }
    let guard = Guard::new(settings);
    for lead in leads {
        let signals = Signals {
            name: Some(lead.name),
            binary: lead.binary,
            open: false,
            command: lead.command(),
            rival: clues::config_rival(&peers, &lead.candidate.stem, lead.name),
        };
        let Some(confidence) = userconf::confidence(signals) else {
            continue;
        };
        if guard.check(&lead.candidate.path).is_err() {
            continue;
        }
        let Some(path) = lead.candidate.path.to_str() else {
            continue;
        };
        found.push(Found::path(
            path.to_owned(),
            lead.candidate.kind(),
            confidence,
        ));
    }
}

/// The app's names for the per-user configuration search: display name, product names,
/// distinctive program names and Electron `productName`/`name`.
fn config_names(app: &AppRecord, m: &Matcher) -> Names {
    let mut names = Names::default();
    names.add_display(&app.info.name);
    for product in &m.products {
        names.add(product);
    }
    for exe in &m.exes {
        names.add(exe.strip_suffix(".exe").unwrap_or(exe));
    }
    let manifest = app
        .detail
        .location
        .as_deref()
        .map(related::electron_manifest_names)
        .unwrap_or_default();
    for name in m.declared.iter().chain(&manifest) {
        names.add(name);
    }
    names
}

/// Files searched for configuration paths, most telling first: the `DisplayIcon` program,
/// Electron archives, other distinctive programs, then libraries named after the app, all
/// from the top level of the install folder and its newest Squirrel `app-<version>` folder.
fn config_files(app: &AppRecord, m: &Matcher) -> Vec<PathBuf> {
    let Some(dir) = app.detail.location.as_deref() else {
        return Vec::new();
    };
    let main = app.detail.icon_exe.as_deref().map(parse::file_name);
    let mut stems: Vec<String> = m.products.clone();
    stems.extend(
        m.exes
            .iter()
            .map(|e| parse::normalize_name(e.strip_suffix(".exe").unwrap_or(e))),
    );
    let mut ranked: Vec<(BinaryRole, String)> = Vec::new();
    for root in related::app_roots(dir).into_iter().take(2) {
        let asar = format!("{root}\\resources\\app.asar");
        if known::is_file(&asar) {
            ranked.push((BinaryRole::Archive, asar));
        }
        for (name, path, is_dir) in known::entries(&root) {
            if is_dir {
                continue;
            }
            if let Some(role) = clues::binary_role(&name, main, &stems) {
                ranked.push((role, path));
            }
        }
    }
    ranked.sort_by_key(|(role, _)| *role);
    ranked.into_iter().map(|(_, p)| PathBuf::from(p)).collect()
}

fn check_cancel(ctx: &JobCtx) -> Result<()> {
    if ctx.is_cancelled() {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

/// The command shown as "uninstaller".
pub(crate) fn uninstaller_display(app: &AppRecord) -> Option<String> {
    let detail = &app.detail;
    if let Some(package) = detail.package.as_ref() {
        return Some(format!("Remove-AppxPackage -Package {}", package.full_name));
    }
    if let Some(code) = detail.msi_code.as_deref() {
        return Some(format!("msiexec.exe /x {code} /qb-! /norestart"));
    }
    detail
        .quiet_uninstall
        .clone()
        .or_else(|| detail.uninstall.clone())
}

/// Removes duplicates and items inside other items (the install folder stays its own
/// item), drops excluded paths, and orders install folder first, then by kind.
fn dedupe(found: Vec<Found>, walker: &Walker) -> Vec<Found> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut unique: Vec<Found> = Vec::with_capacity(found.len());
    for f in found {
        if let Some(path) = f.location.as_path()
            && walker.options().is_excluded(std::path::Path::new(path))
        {
            continue;
        }
        let key = f.location.display().to_lowercase();
        if seen.insert(key) {
            unique.push(f);
        } else if let Some(existing) = unique.iter_mut().find(|u| {
            u.location
                .display()
                .eq_ignore_ascii_case(&f.location.display())
        }) && f.confidence > existing.confidence
        {
            existing.confidence = f.confidence;
        }
    }
    let containers: Vec<(String, bool)> = unique
        .iter()
        .filter_map(|f| match &f.location {
            Location::Path { path } => Some((path.clone(), true)),
            Location::RegistryKey { key } => Some((key.clone(), false)),
            _ => None,
        })
        .collect();
    let inside = |item: &str, is_path: bool| {
        containers.iter().any(|(outer, outer_is_path)| {
            *outer_is_path == is_path
                && !outer.eq_ignore_ascii_case(item)
                && parse::path_within(item, outer)
        })
    };
    unique.retain(|f| match &f.location {
        Location::Path { path } => f.kind == AppFileKind::Bundle || !inside(path, true),
        Location::RegistryKey { key } => !inside(key, false),
        Location::RegistryValue { key, .. } => !containers
            .iter()
            .any(|(outer, is_path)| !*is_path && parse::path_within(key, outer)),
        Location::Special { .. } => true,
    });
    unique.sort_by(|a, b| {
        (
            a.kind != AppFileKind::Bundle,
            a.kind,
            std::cmp::Reverse(a.confidence),
            a.location.display().to_lowercase(),
        )
            .cmp(&(
                b.kind != AppFileKind::Bundle,
                b.kind,
                std::cmp::Reverse(b.confidence),
                b.location.display().to_lowercase(),
            ))
    });
    unique
}

/// Data folders named after the product or `<vendor>\<product>`.
fn folders(m: &Matcher, found: &mut Vec<Found>) {
    let roots: [(Option<String>, Confidence); 9] = [
        (known::env("APPDATA"), Confidence::Medium),
        (known::env("LOCALAPPDATA"), Confidence::Medium),
        (
            known::env_join("LOCALAPPDATA", "Programs"),
            Confidence::Medium,
        ),
        (known::local_low(), Confidence::Medium),
        (known::env("ProgramData"), Confidence::Medium),
        (known::env("ProgramFiles"), Confidence::Medium),
        (known::env("ProgramFiles(x86)"), Confidence::Medium),
        (known::env("PUBLIC"), Confidence::Low),
        (known::env_join("USERPROFILE", "Documents"), Confidence::Low),
    ];
    let mut seen_roots: Vec<String> = Vec::new();
    for (root, cap) in roots {
        let Some(root) = root else { continue };
        if seen_roots.iter().any(|r| r.eq_ignore_ascii_case(&root)) {
            continue;
        }
        seen_roots.push(root.clone());
        for (name, path, is_dir) in known::entries(&root) {
            if !is_dir || m.peers.overlaps(&path) || known::is_broad_dir(&path) {
                continue;
            }
            if m.product(&name) {
                if let Some(confidence) = m.name_confidence(&name, &[], cap) {
                    found.push(Found::path(path, AppFileKind::Support, confidence));
                }
            } else if m.vendor(&name) {
                vendor_folder(m, &path, cap, found);
            }
        }
    }
}

/// `<root>\<Vendor>`: its product subfolders, or the vendor folder itself when nothing
/// else lives in it.
fn vendor_folder(m: &Matcher, vendor: &str, cap: Confidence, found: &mut Vec<Found>) {
    let children = known::entries(vendor);
    let matched: Vec<&(String, String, bool)> = children
        .iter()
        .filter(|(name, path, is_dir)| *is_dir && m.product(name) && !m.peers.overlaps(path))
        .collect();
    if matched.is_empty() {
        return;
    }
    if m.vendor_removable() && matched.len() == children.len() && !m.peers.overlaps(vendor) {
        if let Some(confidence) = clues::confidence(&[Clue::VendorName, Clue::ProductName], cap) {
            found.push(Found::path(
                vendor.to_owned(),
                AppFileKind::Support,
                confidence,
            ));
        }
        return;
    }
    for (name, path, _) in matched {
        if let Some(confidence) = m.name_confidence(name, &[Clue::VendorParent], cap) {
            found.push(Found::path(path.clone(), AppFileKind::Support, confidence));
        }
    }
}

/// Start menu and desktop shortcuts that point into the install folder or are named
/// after the product.
fn shortcuts(m: &Matcher, found: &mut Vec<Found>) {
    let start_menus = [
        known::env_join("APPDATA", "Microsoft\\Windows\\Start Menu\\Programs"),
        known::env_join("ProgramData", "Microsoft\\Windows\\Start Menu\\Programs"),
    ];
    for root in start_menus.into_iter().flatten() {
        start_menu_dir(m, &root, true, 0, found);
    }
    let desktops = [
        known::env_join("USERPROFILE", "Desktop"),
        known::env_join("PUBLIC", "Desktop"),
    ];
    for root in desktops.into_iter().flatten() {
        for (name, path, is_dir) in known::entries(&root) {
            if !is_dir && let Some(confidence) = lnk_match(m, &name, &path) {
                found.push(Found::path(path, AppFileKind::Shortcut, confidence));
            }
        }
    }
}

/// Returns whether every entry of `dir` belongs to the app.
fn start_menu_dir(
    m: &Matcher,
    dir: &str,
    is_root: bool,
    depth: u32,
    found: &mut Vec<Found>,
) -> bool {
    let entries = known::entries(dir);
    let mut all_ours = !entries.is_empty();
    let mut ours = Vec::new();
    for (name, path, is_dir) in entries {
        if is_dir {
            if m.product(&name) && !is_root_program_group(&name) {
                if let Some(confidence) = m.name_confidence(&name, &[], Confidence::High) {
                    ours.push(Found::path(path, AppFileKind::Shortcut, confidence));
                } else {
                    all_ours = false;
                }
            } else if depth < 3 && start_menu_dir(m, &path, false, depth.saturating_add(1), found) {
                // Every entry is ours: a program group of the vendor or product.
                if let Some(confidence) =
                    clues::confidence(&[Clue::VendorName, Clue::ProductName], Confidence::High)
                {
                    ours.push(Found::path(path, AppFileKind::Shortcut, confidence));
                }
            } else {
                all_ours = false;
            }
        } else if let Some(confidence) = lnk_match(m, &name, &path) {
            ours.push(Found::path(path, AppFileKind::Shortcut, confidence));
        } else if !name.eq_ignore_ascii_case("desktop.ini") {
            all_ours = false;
        }
    }
    let name = parse::file_name(dir);
    let folder_is_ours =
        !is_root && all_ours && (m.product(name) || (m.vendor(name) && m.vendor_removable()));
    if folder_is_ours {
        // The caller adds the folder itself; its entries are covered.
        return true;
    }
    found.extend(ours);
    false
}

/// Start menu groups shared by many programs.
fn is_root_program_group(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "startup"
            | "accessories"
            | "administrative tools"
            | "maintenance"
            | "system tools"
            | "windows powershell"
    )
}

/// A `.lnk` that points into the install folder ([`Clue::Reference`]) or is named after
/// the product.
pub(super) fn lnk_match(m: &Matcher, name: &str, path: &str) -> Option<Confidence> {
    let stem = name.to_ascii_lowercase();
    let stem = stem.strip_suffix(".lnk")?;
    if !m.needles.is_empty()
        && let Some(bytes) = known::read_small(path)
        && m.needles.iter().any(|n| parse::bytes_mention(&bytes, n))
    {
        return clues::confidence(&[Clue::Reference], Confidence::High);
    }
    if m.product(stem) {
        m.name_confidence(stem, &[], Confidence::High)
    } else {
        None
    }
}

/// Uninstall keys, vendor keys, `Run` values, `App Paths`, classes.
fn registry(app: &AppRecord, m: &Matcher, ctx: &JobCtx, found: &mut Vec<Found>) {
    for (hive, path) in &app.detail.keys {
        found.push(Found::key(*hive, path, Confidence::High));
    }
    vendor_keys(m, found);
    if !m.needles.is_empty() {
        run_values(m, found);
        app_paths(m, found);
        classes(m, ctx, found);
    }
}

/// `HKCU|HKLM\SOFTWARE[\WOW6432Node]\<Vendor>\<Product>` and `…\<Product>`.
fn vendor_keys(m: &Matcher, found: &mut Vec<Found>) {
    for (hive, base) in [
        (Hive::CurrentUser, "Software"),
        (Hive::CurrentUser, "Software\\WOW6432Node"),
        (Hive::LocalMachine, "SOFTWARE"),
        (Hive::LocalMachine, "SOFTWARE\\WOW6432Node"),
    ] {
        let Some(key) = reg::open(hive, base) else {
            continue;
        };
        for name in reg::subkeys(&key) {
            let path = format!("{base}\\{name}");
            if parse::is_os_vendor(&parse::normalize_name(&name)) && !m.vendor(&name) {
                continue;
            }
            if m.product(&name) && !parse::is_os_vendor(&parse::normalize_name(&name)) {
                if let Some(confidence) = m.name_confidence(&name, &[], Confidence::High) {
                    found.push(Found::key(hive, &path, confidence));
                }
            } else if m.vendor(&name) {
                let Some(vendor) = reg::open(hive, &path) else {
                    continue;
                };
                let children = reg::subkeys(&vendor);
                let matched: Vec<&String> = children.iter().filter(|c| m.product(c)).collect();
                if matched.is_empty() {
                    continue;
                }
                let has_values = vendor.values().is_ok_and(|mut v| v.next().is_some());
                if m.vendor_removable() && matched.len() == children.len() && !has_values {
                    if let Some(confidence) =
                        clues::confidence(&[Clue::VendorName, Clue::ProductName], Confidence::High)
                    {
                        found.push(Found::key(hive, &path, confidence));
                    }
                } else {
                    for child in matched {
                        if let Some(confidence) =
                            m.name_confidence(child, &[Clue::VendorParent], Confidence::High)
                        {
                            found.push(Found::key(hive, &format!("{path}\\{child}"), confidence));
                        }
                    }
                }
            }
        }
    }
}

/// `Run`/`RunOnce` values launching the app, plus their `StartupApproved` state.
fn run_values(m: &Matcher, found: &mut Vec<Found>) {
    for (hive, base, approved) in run_keys() {
        for suffix in ["Run", "RunOnce"] {
            let path = format!("{base}\\{suffix}");
            let Some(key) = reg::open(hive, &path) else {
                continue;
            };
            for (name, data) in reg::string_values(&key) {
                if !m.mentions(&data) {
                    continue;
                }
                found.push(Found::value(hive, &path, &name, Confidence::High));
                if suffix == "Run"
                    && let Some(state) = reg::open(hive, approved)
                    && reg::bytes(&state, &name).is_some()
                {
                    found.push(Found::value(hive, approved, &name, Confidence::High));
                }
            }
        }
    }
}

/// `(hive, …\CurrentVersion, StartupApproved key)` of each `Run` view.
pub(crate) fn run_keys() -> [(Hive, &'static str, &'static str); 3] {
    [
        (
            Hive::CurrentUser,
            CURRENT_VERSION,
            "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run",
        ),
        (
            Hive::LocalMachine,
            CURRENT_VERSION,
            "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run",
        ),
        (
            Hive::LocalMachine,
            "SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion",
            "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run32",
        ),
    ]
}

/// `App Paths\<exe>` pointing into the install folder.
fn app_paths(m: &Matcher, found: &mut Vec<Found>) {
    let base = format!("{CURRENT_VERSION}\\App Paths");
    for hive in [Hive::CurrentUser, Hive::LocalMachine] {
        let Some(key) = reg::open(hive, &base) else {
            continue;
        };
        for name in reg::subkeys(&key) {
            let path = format!("{base}\\{name}");
            let Some(sub) = reg::open(hive, &path) else {
                continue;
            };
            let hit = [reg::string(&sub, ""), reg::string(&sub, "Path")]
                .into_iter()
                .flatten()
                .any(|v| m.mentions(&v));
            if hit {
                found.push(Found::key(hive, &path, Confidence::High));
            }
        }
    }
}

/// Subkeys whose default value names the handler of a `ProgID` or COM class.
const PROGID_PROBES: [&str; 3] = [
    "shell\\open\\command",
    "DefaultIcon",
    "shell\\edit\\command",
];
const CLSID_PROBES: [&str; 3] = ["InprocServer32", "LocalServer32", "DefaultIcon"];

/// Class keys that are containers, not `ProgIDs`.
fn is_class_container(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "clsid"
            | "wow6432node"
            | "installer"
            | "interface"
            | "typelib"
            | "appid"
            | "applications"
            | "local settings"
            | "mime"
            | "directory"
            | "folder"
            | "drive"
            | "*"
            | "allfilesystemobjects"
            | "systemfileassociations"
            | "record"
            | "component categories"
    )
}

/// `ProgIDs`, COM classes, `Applications\<exe>` and `OpenWithProgids` links whose handler
/// lives in the install folder. Top-level class keys number in the tens of thousands:
/// only the few probe subkeys of each are opened, on several threads.
fn classes(m: &Matcher, ctx: &JobCtx, found: &mut Vec<Found>) {
    for (hive, base) in [
        (Hive::CurrentUser, "Software\\Classes"),
        (Hive::LocalMachine, "SOFTWARE\\Classes"),
    ] {
        let Some(root) = reg::open(hive, base) else {
            continue;
        };
        let names = reg::subkeys(&root);
        drop(root);
        let probe = |path: &str, probes: &[&str]| {
            probes.iter().any(|p| {
                reg::open(hive, &format!("{path}\\{p}"))
                    .and_then(|k| reg::string(&k, ""))
                    .is_some_and(|v| m.mentions(&v))
            })
        };
        let progids: Vec<String> = known::par_flat_map(&names, ctx, |name| {
            if name.starts_with('.') || is_class_container(name) {
                return Vec::new();
            }
            let path = format!("{base}\\{name}");
            if probe(&path, &PROGID_PROBES) {
                vec![name.clone()]
            } else {
                Vec::new()
            }
        });
        for progid in &progids {
            found.push(Found::key(
                hive,
                &format!("{base}\\{progid}"),
                Confidence::High,
            ));
        }
        let mut containers = vec![format!("{base}\\CLSID")];
        if hive == Hive::LocalMachine {
            containers.push(format!("{base}\\WOW6432Node\\CLSID"));
        }
        for container in containers {
            let Some(key) = reg::open(hive, &container) else {
                continue;
            };
            let clsids = reg::subkeys(&key);
            drop(key);
            let hits: Vec<String> = known::par_flat_map(&clsids, ctx, |clsid| {
                let path = format!("{container}\\{clsid}");
                if probe(&path, &CLSID_PROBES) {
                    vec![path]
                } else {
                    Vec::new()
                }
            });
            for path in hits {
                found.push(Found::key(hive, &path, Confidence::High));
            }
        }
        let apps_base = format!("{base}\\Applications");
        if let Some(key) = reg::open(hive, &apps_base) {
            for exe in reg::subkeys(&key) {
                let path = format!("{apps_base}\\{exe}");
                if probe(&path, &PROGID_PROBES) {
                    found.push(Found::key(hive, &path, Confidence::High));
                }
            }
        }
        if progids.is_empty() {
            continue;
        }
        let links: Vec<(String, String)> = known::par_flat_map(&names, ctx, |name| {
            if !name.starts_with('.') {
                return Vec::new();
            }
            let path = format!("{base}\\{name}\\OpenWithProgids");
            let Some(key) = reg::open(hive, &path) else {
                return Vec::new();
            };
            let Ok(values) = key.values() else {
                return Vec::new();
            };
            values
                .map(|(value, _)| value)
                .filter(|value| progids.iter().any(|p| p.eq_ignore_ascii_case(value)))
                .map(|value| (path.clone(), value))
                .collect()
        });
        for (path, value) in links {
            found.push(Found::value(hive, &path, &value, Confidence::High));
        }
    }
}

/// Services and scheduled tasks whose program lives in the install folder.
fn services_and_tasks(app: &AppRecord, m: &Matcher, found: &mut Vec<Found>) {
    let Some(dir) = app.detail.location.as_deref() else {
        return;
    };
    let windir = known::windir();
    let base = "SYSTEM\\CurrentControlSet\\Services";
    if let Some(key) = reg::open(Hive::LocalMachine, base) {
        for name in reg::subkeys(&key) {
            let Some(service) = reg::open(Hive::LocalMachine, &format!("{base}\\{name}")) else {
                continue;
            };
            let Some(image) = reg::string(&service, "ImagePath") else {
                continue;
            };
            let image = reg::expand(&image);
            if parse::service_image(&image, &windir, known::is_file)
                .is_some_and(|exe| parse::path_within(&exe, dir))
            {
                found.push(Found {
                    location: Location::Special {
                        action: SpecialAction::DeleteService { name },
                    },
                    kind: AppFileKind::Service,
                    confidence: Confidence::High,
                    admin: true,
                });
            }
        }
    }
    match tasks::list() {
        Ok(list) => {
            for task in list.iter().filter(|t| !t.is_microsoft()) {
                if task.exec.iter().any(|e| m.mentions(e)) {
                    found.push(Found {
                        location: Location::Special {
                            action: SpecialAction::DeleteScheduledTask {
                                path: task.path.clone(),
                            },
                        },
                        kind: AppFileKind::ScheduledTask,
                        confidence: Confidence::High,
                        admin: !tasks::runs_as_current_user(task),
                    });
                }
            }
        }
        Err(err) => tracing::warn!(%err, "listing scheduled tasks failed"),
    }
}
