//! Leftovers of uninstalled programs: broken `Uninstall` entries, `Run` values and
//! Startup shortcuts whose program is gone, broken shortcuts, registry entries pointing
//! at deleted programs (firewall rules, `SharedDLLs` counters, `App Paths`, Event Log
//! sources, `Installer\Folders`, `MUICache`/`UserAssist`/`AppCompatFlags` traces), Burn and
//! MSI package caches of products no longer registered, stale vendor folders and
//! `HKCU\Software` vendor keys of software that is no longer installed. Conservative:
//! folders and keys need positive evidence or no trace of an installed owner, and only
//! traces Windows rebuilds on its own are preselected.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use omc_proto::jobs::{Location, Phase};
use omc_proto::junk::{JunkGroup, JunkItem, JunkKind, JunkReport, Safety};
use omc_proto::settings::CleanSettings;
use omc_scan::{JobCtx, Scanned, Target, Walker};

use super::apps::{self, UninstallEntry};
use super::clues;
use super::parse::{self, Hive};
use super::{files, known, reg, related};
use crate::{Error, Result};

/// Folders must be untouched this long to count as orphans.
const ORPHAN_AGE_SECS: i64 = 180 * 24 * 60 * 60;

/// An item before ids are assigned.
#[derive(Debug)]
struct Pending {
    kind: JunkKind,
    name: String,
    location: Location,
    bytes: u64,
    files: u64,
    modified: Option<i64>,
    safety: Safety,
    admin: bool,
}

impl Pending {
    fn registry(
        kind: JunkKind,
        name: String,
        location: Location,
        admin: bool,
        safety: Safety,
    ) -> Self {
        Self {
            kind,
            name,
            location,
            bytes: 0,
            files: 0,
            modified: None,
            safety,
            admin,
        }
    }

    fn file(kind: JunkKind, name: String, path: String, safety: Safety) -> Self {
        let bytes = std::fs::metadata(&path).map_or(0, |m| m.len());
        let admin = known::needs_admin(&path);
        Self {
            kind,
            name,
            location: Location::Path { path },
            bytes,
            files: 1,
            modified: None,
            safety,
            admin,
        }
    }
}

/// Names that prove software was once here and is gone.
#[derive(Debug, Default)]
struct Evidence(HashSet<String>);

impl Evidence {
    fn add_name(&mut self, name: &str) {
        for key in [
            parse::normalize_name(name),
            parse::normalize_publisher(name),
        ] {
            if key.chars().count() >= 3 && !parse::is_os_vendor(&key) {
                self.0.insert(key);
            }
        }
    }

    /// The folder names of a missing program path below well-known roots.
    fn add_path(&mut self, target: &str) {
        let roots = [
            known::env("ProgramFiles"),
            known::env("ProgramFiles(x86)"),
            known::env("ProgramData"),
            known::env("APPDATA"),
            known::env("LOCALAPPDATA"),
            known::env_join("LOCALAPPDATA", "Programs"),
        ];
        for root in roots.into_iter().flatten() {
            if !parse::path_within(target, &root) {
                continue;
            }
            let rest = target
                .get(root.len()..)
                .unwrap_or_default()
                .trim_start_matches('\\');
            let mut parts: Vec<&str> = rest.split('\\').filter(|p| !p.is_empty()).collect();
            parts.pop();
            for part in parts.into_iter().take(2) {
                self.add_name(part);
            }
        }
    }

    fn mentions(&self, name: &str) -> bool {
        self.0.contains(&parse::normalize_name(name))
            || self.0.contains(&parse::normalize_publisher(name))
    }
}

pub(crate) fn scan(
    settings: &CleanSettings,
    walker: &Walker,
    ctx: &JobCtx,
) -> Result<Scanned<JunkReport>> {
    ctx.set_phase(Phase::Scanning);
    let (packages, entries) = std::thread::scope(|scope| {
        let store = scope.spawn(apps::store_packages);
        let entries = apps::read_entries();
        let packages = store.join().unwrap_or_else(|_| {
            tracing::warn!("Store package listing thread panicked");
            Vec::new()
        });
        (packages, entries)
    });
    let mut pending = Vec::new();
    let mut evidence = Evidence::default();
    broken_entries(&entries, &mut evidence, &mut pending);
    launch_items(&mut evidence, &mut pending);
    broken_shortcuts(&mut evidence, &mut pending);
    dangling_registry(&mut evidence, &mut pending);
    if ctx.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let installed = installed_names(&entries, &packages);
    orphan_folders(&installed, &evidence, walker, ctx, &mut pending);
    orphan_keys(&installed, &mut pending);
    orphan_package_caches(&entries, walker, ctx, &mut pending);
    if ctx.is_cancelled() {
        return Err(Error::Cancelled);
    }
    Ok(assemble(pending, settings, walker, ctx))
}

fn location_missing(entry: &UninstallEntry) -> bool {
    let Some(raw) = entry.install_location.as_deref() else {
        return true;
    };
    match parse::clean_path(&reg::expand(raw)) {
        Some(dir) if parse::is_absolute(&dir) => known::target_missing(&dir),
        Some(_) | None => true,
    }
}

/// Windows Installer still knows the product (machine-wide, current user, or any user
/// profile's `UserData`).
pub(super) fn msi_registered(code: &str) -> bool {
    let Some(packed) = parse::packed_guid(code) else {
        return true;
    };
    let user_data = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Installer\\UserData";
    let sids = reg::open(Hive::LocalMachine, user_data)
        .map(|k| reg::subkeys(&k))
        .unwrap_or_default();
    [
        (
            Hive::LocalMachine,
            format!("SOFTWARE\\Classes\\Installer\\Products\\{packed}"),
        ),
        (
            Hive::CurrentUser,
            format!("Software\\Microsoft\\Installer\\Products\\{packed}"),
        ),
    ]
    .into_iter()
    .chain(sids.iter().map(|sid| {
        (
            Hive::LocalMachine,
            format!("{user_data}\\{sid}\\Products\\{packed}"),
        )
    }))
    .any(|(hive, path)| reg::exists(hive, &path))
}

fn broken_entries(entries: &[UninstallEntry], evidence: &mut Evidence, out: &mut Vec<Pending>) {
    for entry in entries {
        let Some(name) = entry.name.as_deref() else {
            continue;
        };
        if entry.system_component
            || entry.parent_key.is_some()
            || parse::is_update_name(name)
            || entry
                .release_type
                .as_deref()
                .is_some_and(parse::is_update_release)
        {
            continue;
        }
        let broken = if let Some(code) = entry.msi_code() {
            !msi_registered(&code) && location_missing(entry)
        } else if let Some(command) = entry.uninstall.as_deref() {
            known::command_missing(command) && location_missing(entry)
        } else {
            false
        };
        if !broken {
            continue;
        }
        evidence.add_name(name);
        if let Some(publisher) = entry.publisher.as_deref() {
            evidence.add_name(publisher);
        }
        if let Some(dir) = entry
            .install_location
            .as_deref()
            .and_then(parse::clean_path)
        {
            evidence.add_name(parse::file_name(&dir));
        }
        out.push(Pending::registry(
            JunkKind::BrokenUninstallEntries,
            name.to_owned(),
            Location::RegistryKey {
                key: entry.full_key(),
            },
            entry.hive() != Hive::CurrentUser,
            Safety::Review,
        ));
    }
}

fn startup_folders() -> Vec<String> {
    [
        known::env_join(
            "APPDATA",
            "Microsoft\\Windows\\Start Menu\\Programs\\Startup",
        ),
        known::env_join(
            "ProgramData",
            "Microsoft\\Windows\\Start Menu\\Programs\\StartUp",
        ),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// A missing `.lnk` target (`None` when the link has no local target or it exists).
fn missing_link_target(path: &str) -> Option<String> {
    let target = parse::lnk_target(&known::read_small(path)?)?;
    known::target_missing(&target).then_some(target)
}

fn launch_items(evidence: &mut Evidence, out: &mut Vec<Pending>) {
    for (hive, base, _) in files::run_keys() {
        for suffix in ["Run", "RunOnce"] {
            let path = format!("{base}\\{suffix}");
            let Some(key) = reg::open(hive, &path) else {
                continue;
            };
            for (name, data) in reg::string_values(&key) {
                if !known::command_missing(&data) {
                    continue;
                }
                if let Some(target) = parse::command_target(&reg::expand(&data), known::is_file) {
                    evidence.add_path(&target);
                }
                out.push(Pending::registry(
                    JunkKind::OrphanLaunchItems,
                    name.clone(),
                    Location::RegistryValue {
                        key: hive.full(&path),
                        name,
                    },
                    hive != Hive::CurrentUser,
                    Safety::Safe,
                ));
            }
        }
    }
    for dir in startup_folders() {
        for (name, path, is_dir) in known::entries(&dir) {
            if is_dir || !name.to_ascii_lowercase().ends_with(".lnk") {
                continue;
            }
            if let Some(target) = missing_link_target(&path) {
                evidence.add_path(&target);
                out.push(Pending::file(
                    JunkKind::OrphanLaunchItems,
                    name,
                    path,
                    Safety::Safe,
                ));
            }
        }
    }
}

fn broken_shortcuts(evidence: &mut Evidence, out: &mut Vec<Pending>) {
    let startup = startup_folders();
    let roots = [
        (
            known::env_join("APPDATA", "Microsoft\\Windows\\Start Menu\\Programs"),
            4,
            Safety::Safe,
        ),
        (
            known::env_join("ProgramData", "Microsoft\\Windows\\Start Menu\\Programs"),
            4,
            Safety::Safe,
        ),
        (known::env_join("USERPROFILE", "Desktop"), 0, Safety::Review),
        (known::env_join("PUBLIC", "Desktop"), 0, Safety::Review),
    ];
    for (root, depth, safety) in roots {
        let Some(root) = root else { continue };
        for path in known::shortcuts(&root, depth) {
            if startup.iter().any(|s| parse::path_within(&path, s)) {
                continue;
            }
            if let Some(target) = missing_link_target(&path) {
                evidence.add_path(&target);
                let name = parse::file_name(&path).to_owned();
                out.push(Pending::file(JunkKind::BrokenShortcuts, name, path, safety));
            }
        }
    }
}

/// A registry key (`value` `None`) or value left by removed software.
fn push_orphan(
    out: &mut Vec<Pending>,
    (hive, path, value): (Hive, &str, Option<&str>),
    name: String,
    safety: Safety,
) {
    let location = match value {
        Some(value) => Location::RegistryValue {
            key: hive.full(path),
            name: value.to_owned(),
        },
        None => Location::RegistryKey {
            key: hive.full(path),
        },
    };
    out.push(Pending::registry(
        JunkKind::OrphanRegistry,
        name,
        location,
        hive != Hive::CurrentUser,
        safety,
    ));
}

/// The (expanded) program, file or folder is gone for sure.
fn gone(path: &str) -> bool {
    known::target_missing(&reg::expand(path))
}

/// Registry entries whose program, file or folder is gone. Traces Windows rebuilds by
/// itself (`MUICache`, `UserAssist`, Compatibility Assistant) are `Safe`; entries that
/// configure something (firewall rules, compatibility layers, counters) need review.
fn dangling_registry(evidence: &mut Evidence, out: &mut Vec<Pending>) {
    dangling_settings(evidence, out);
    dangling_traces(out);
}

/// Firewall rules, `SharedDLLs` counters, `App Paths`, Event Log sources and
/// `Installer\Folders` entries of removed programs.
fn dangling_settings(evidence: &mut Evidence, out: &mut Vec<Pending>) {
    if let Some(key) = reg::open(Hive::LocalMachine, related::FIREWALL_RULES) {
        for (value, rule) in reg::string_values(&key) {
            let Some(program) = clues::firewall_field(&rule, "App") else {
                continue;
            };
            if !gone(program) {
                continue;
            }
            evidence.add_path(&reg::expand(program));
            let name = clues::firewall_field(&rule, "Name")
                .filter(|n| !n.starts_with('@'))
                .map_or_else(|| parse::file_name(program).to_owned(), str::to_owned);
            push_orphan(
                out,
                (Hive::LocalMachine, related::FIREWALL_RULES, Some(&value)),
                name,
                Safety::Review,
            );
        }
    }
    for (key, file, _) in related::shared_dll_values() {
        if gone(&file) {
            let name = parse::file_name(&file).to_owned();
            push_orphan(
                out,
                (Hive::LocalMachine, key, Some(&file)),
                name,
                Safety::Review,
            );
        }
    }
    let app_paths = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths";
    for hive in [Hive::CurrentUser, Hive::LocalMachine] {
        let Some(key) = reg::open(hive, app_paths) else {
            continue;
        };
        for exe in reg::subkeys(&key) {
            let path = format!("{app_paths}\\{exe}");
            let program = reg::open(hive, &path)
                .and_then(|k| reg::string(&k, ""))
                .and_then(|p| parse::clean_path(&p));
            if let Some(program) = program
                && gone(&program)
            {
                evidence.add_path(&reg::expand(&program));
                push_orphan(out, (hive, &path, None), exe, Safety::Review);
            }
        }
    }
    let event_log = "SYSTEM\\CurrentControlSet\\Services\\EventLog";
    if let Some(logs) = reg::open(Hive::LocalMachine, event_log) {
        for log in reg::subkeys(&logs) {
            let log_path = format!("{event_log}\\{log}");
            let Some(log_key) = reg::open(Hive::LocalMachine, &log_path) else {
                continue;
            };
            for source in reg::subkeys(&log_key) {
                let path = format!("{log_path}\\{source}");
                let files = reg::open(Hive::LocalMachine, &path)
                    .and_then(|k| reg::string(&k, "EventMessageFile"))
                    .unwrap_or_default();
                let mut files = files
                    .split(';')
                    .map(str::trim)
                    .filter(|f| !f.is_empty())
                    .peekable();
                if files.peek().is_some() && files.all(gone) {
                    push_orphan(
                        out,
                        (Hive::LocalMachine, &path, None),
                        source,
                        Safety::Review,
                    );
                }
            }
        }
    }
    let folders = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Installer\\Folders";
    if let Some(key) = reg::open(Hive::LocalMachine, folders) {
        for folder in reg::value_names(&key) {
            if gone(&folder) {
                let name = parse::file_name(folder.trim_end_matches('\\')).to_owned();
                push_orphan(
                    out,
                    (Hive::LocalMachine, folders, Some(&folder)),
                    name,
                    Safety::Review,
                );
            }
        }
    }
}

/// `MUICache`, `UserAssist` and compatibility entries of removed programs.
fn dangling_traces(out: &mut Vec<Pending>) {
    let mui = "Software\\Classes\\Local Settings\\Software\\Microsoft\\Windows\\Shell\\MuiCache";
    if let Some(key) = reg::open(Hive::CurrentUser, mui) {
        for value in reg::value_names(&key) {
            if let Some(program) = clues::mui_cache_program(&value)
                && gone(program)
            {
                let name = parse::file_name(program).to_owned();
                push_orphan(
                    out,
                    (Hive::CurrentUser, mui, Some(&value)),
                    name,
                    Safety::Safe,
                );
            }
        }
    }
    let user_assist = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\UserAssist";
    if let Some(key) = reg::open(Hive::CurrentUser, user_assist) {
        for guid in reg::subkeys(&key) {
            let path = format!("{user_assist}\\{guid}\\Count");
            let Some(count) = reg::open(Hive::CurrentUser, &path) else {
                continue;
            };
            for value in reg::value_names(&count) {
                if let Some(program) =
                    clues::user_assist_path(&value, |var| std::env::var(var).ok())
                    && gone(&program)
                {
                    let name = parse::file_name(&program).to_owned();
                    push_orphan(
                        out,
                        (Hive::CurrentUser, &path, Some(&value)),
                        name,
                        Safety::Safe,
                    );
                }
            }
        }
    }
    let compat = "Software\\Microsoft\\Windows NT\\CurrentVersion\\AppCompatFlags";
    for (hive, path, safety) in [
        (
            Hive::CurrentUser,
            format!("{compat}\\Compatibility Assistant\\Store"),
            Safety::Safe,
        ),
        (
            Hive::CurrentUser,
            format!("{compat}\\Compatibility Assistant\\Persisted"),
            Safety::Safe,
        ),
        (
            Hive::CurrentUser,
            format!("{compat}\\Layers"),
            Safety::Review,
        ),
        (
            Hive::LocalMachine,
            format!("{compat}\\Layers"),
            Safety::Review,
        ),
    ] {
        let Some(key) = reg::open(hive, &path) else {
            continue;
        };
        for value in reg::value_names(&key) {
            if gone(&value) {
                let name = parse::file_name(&value).to_owned();
                push_orphan(out, (hive, &path, Some(&value)), name, safety);
            }
        }
    }
}

/// `%ProgramData%\Package Cache\{code}[v<version>]` folders of Burn bundles and MSI
/// packages Windows no longer knows (no `Uninstall` key, MSI registration or dependency
/// provider), untouched for a week (not an install in progress). Hash-named folders
/// (Visual Studio) are never touched.
fn orphan_package_caches(
    entries: &[UninstallEntry],
    walker: &Walker,
    ctx: &JobCtx,
    out: &mut Vec<Pending>,
) {
    const MIN_AGE_SECS: i64 = 7 * 24 * 60 * 60;
    let Some(cache) = known::env_join("ProgramData", "Package Cache") else {
        return;
    };
    let keys: HashSet<String> = entries
        .iter()
        .map(|e| e.key_name.to_ascii_uppercase())
        .collect();
    let mut candidates: Vec<(String, String)> = Vec::new();
    for (name, path, is_dir) in known::entries(&cache) {
        let Some(code) = clues::package_cache_code(&name) else {
            continue;
        };
        if !is_dir
            || keys.contains(&code)
            || entries
                .iter()
                .any(|e| e.msi_code().is_some_and(|c| c.eq_ignore_ascii_case(&code)))
            || msi_registered(&code)
            || reg::exists(
                Hive::LocalMachine,
                &format!("SOFTWARE\\Classes\\Installer\\Dependencies\\{code}"),
            )
            || walker.options().is_excluded(Path::new(&path))
        {
            continue;
        }
        let label = known::entries(&path)
            .into_iter()
            .find(|(file, _, is_dir)| {
                let lower = file.to_ascii_lowercase();
                !is_dir
                    && Path::new(&lower)
                        .extension()
                        .is_some_and(|e| e == "exe" || e == "msi")
            })
            .map_or(name, |(file, _, _)| file);
        candidates.push((label, path));
    }
    if candidates.is_empty() {
        return;
    }
    ctx.set_phase(Phase::Measuring);
    let paths: Vec<PathBuf> = candidates.iter().map(|(_, p)| PathBuf::from(p)).collect();
    let measures = walker.measure_all(&paths, ctx);
    let cutoff = omc_scan::paths::now_secs().saturating_sub(MIN_AGE_SECS);
    for ((name, path), measure) in candidates.into_iter().zip(measures) {
        if measure.missing || measure.incomplete || measure.newest.is_some_and(|t| t > cutoff) {
            continue;
        }
        out.push(Pending {
            kind: JunkKind::OrphanFiles,
            name,
            location: Location::Path { path },
            bytes: measure.bytes,
            files: measure.files,
            modified: measure.newest,
            safety: Safety::Review,
            admin: true,
        });
    }
}

/// Normalised names and vendors of everything installed or running.
fn installed_names(entries: &[UninstallEntry], packages: &[parse::AppxPackage]) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut add = |text: &str| {
        for key in [
            parse::normalize_name(text),
            parse::normalize_publisher(text),
        ] {
            if !key.is_empty() {
                names.insert(key);
            }
        }
    };
    for entry in entries {
        let Some(name) = entry.name.as_deref() else {
            continue;
        };
        let location = entry
            .install_location
            .as_deref()
            .and_then(parse::clean_path);
        for key in parse::product_keys(name, entry.publisher.as_deref(), location.as_deref()) {
            add(&key);
        }
        add(name);
        if let Some(publisher) = entry.publisher.as_deref() {
            add(publisher);
        }
        if let Some(dir) = location.as_deref() {
            add(parse::file_name(dir));
        }
    }
    for package in packages {
        add(&package.name);
        for part in package.name.split('.') {
            add(part);
        }
        if let Some(publisher) = package.publisher.as_deref().and_then(parse::dn_name) {
            add(&publisher);
        }
    }
    for process in omc_scan::procs::running() {
        let name = process.name.to_ascii_lowercase();
        add(name.strip_suffix(".exe").unwrap_or(&name));
    }
    if let Some(services) = reg::open(Hive::LocalMachine, "SYSTEM\\CurrentControlSet\\Services") {
        for name in reg::subkeys(&services) {
            add(&name);
        }
    }
    names
}

fn is_installed(installed: &HashSet<String>, name: &str) -> bool {
    installed.contains(&parse::normalize_name(name))
        || installed.contains(&parse::normalize_publisher(name))
}

fn is_candidate_name(name: &str) -> bool {
    let normalized = parse::normalize_name(name);
    normalized.chars().count() >= 3
        && !parse::is_os_vendor(&normalized)
        && !parse::is_os_vendor(&parse::normalize_publisher(name))
        && !name.starts_with('.')
}

fn orphan_folders(
    installed: &HashSet<String>,
    evidence: &Evidence,
    walker: &Walker,
    ctx: &JobCtx,
    out: &mut Vec<Pending>,
) {
    if evidence.0.is_empty() {
        return;
    }
    let mut candidates: Vec<(String, String)> = Vec::new();
    for root in [
        known::env("APPDATA"),
        known::env("LOCALAPPDATA"),
        known::env("ProgramData"),
    ]
    .into_iter()
    .flatten()
    {
        for (name, path, is_dir) in known::entries(&root) {
            if is_dir
                && is_candidate_name(&name)
                && evidence.mentions(&name)
                && !is_installed(installed, &name)
                && !known::is_broad_dir(&path)
                && !walker.options().is_excluded(Path::new(&path))
            {
                candidates.push((name, path));
            }
        }
    }
    if candidates.is_empty() {
        return;
    }
    ctx.set_phase(Phase::Measuring);
    let paths: Vec<PathBuf> = candidates.iter().map(|(_, p)| PathBuf::from(p)).collect();
    let measures = walker.measure_all(&paths, ctx);
    let cutoff = omc_scan::paths::now_secs().saturating_sub(ORPHAN_AGE_SECS);
    for ((name, path), measure) in candidates.into_iter().zip(measures) {
        if measure.missing || measure.incomplete || measure.newest.is_some_and(|t| t > cutoff) {
            continue;
        }
        let admin = known::needs_admin(&path);
        out.push(Pending {
            kind: JunkKind::OrphanFiles,
            name,
            location: Location::Path { path },
            bytes: measure.bytes,
            files: measure.files,
            modified: measure.newest,
            safety: Safety::Review,
            admin,
        });
    }
}

/// Normalised names of top-level folders where programs keep files.
fn folder_names() -> HashSet<String> {
    let roots = [
        known::env("APPDATA"),
        known::env("LOCALAPPDATA"),
        known::env_join("LOCALAPPDATA", "Programs"),
        known::local_low(),
        known::env("ProgramData"),
        known::env("ProgramFiles"),
        known::env("ProgramFiles(x86)"),
    ];
    let mut names = HashSet::new();
    for root in roots.into_iter().flatten() {
        for (name, _, is_dir) in known::entries(&root) {
            if is_dir {
                names.insert(parse::normalize_name(&name));
                names.insert(parse::normalize_publisher(&name));
            }
        }
    }
    names
}

fn orphan_keys(installed: &HashSet<String>, out: &mut Vec<Pending>) {
    let Some(software) = reg::open(Hive::CurrentUser, "Software") else {
        return;
    };
    let folders = folder_names();
    for vendor in reg::subkeys(&software) {
        if !is_candidate_name(&vendor)
            || is_installed(installed, &vendor)
            || folders.contains(&parse::normalize_name(&vendor))
            || folders.contains(&parse::normalize_publisher(&vendor))
            || reg::exists(Hive::LocalMachine, &format!("SOFTWARE\\{vendor}"))
            || reg::exists(
                Hive::LocalMachine,
                &format!("SOFTWARE\\WOW6432Node\\{vendor}"),
            )
        {
            continue;
        }
        let path = format!("Software\\{vendor}");
        let children = reg::open(Hive::CurrentUser, &path)
            .map(|k| reg::subkeys(&k))
            .unwrap_or_default();
        if children
            .iter()
            .any(|c| is_installed(installed, c) || folders.contains(&parse::normalize_name(c)))
        {
            continue;
        }
        out.push(Pending::registry(
            JunkKind::OrphanRegistry,
            vendor,
            Location::RegistryKey {
                key: Hive::CurrentUser.full(&path),
            },
            false,
            Safety::Review,
        ));
    }
}

/// Groups (largest first), items (largest first, then by name), ids and targets.
fn assemble(
    pending: Vec<Pending>,
    settings: &CleanSettings,
    walker: &Walker,
    ctx: &JobCtx,
) -> Scanned<JunkReport> {
    let mut seen = HashSet::new();
    let mut by_kind: BTreeMap<JunkKind, Vec<Pending>> = BTreeMap::new();
    for item in pending {
        if let Some(path) = item.location.as_path()
            && walker.options().is_excluded(Path::new(path))
        {
            continue;
        }
        if seen.insert(item.location.display().to_lowercase()) {
            by_kind.entry(item.kind).or_default().push(item);
        }
    }
    let mut groups: Vec<(JunkKind, Vec<Pending>)> = by_kind.into_iter().collect();
    for (_, items) in &mut groups {
        items.sort_by(|a, b| {
            b.bytes
                .cmp(&a.bytes)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
    }
    let total = |items: &[Pending]| {
        items
            .iter()
            .fold(0_u64, |sum, i| sum.saturating_add(i.bytes))
    };
    groups.sort_by(|(ka, a), (kb, b)| total(b).cmp(&total(a)).then(ka.cmp(kb)));

    let mut targets = Vec::new();
    let mut report = JunkReport::default();
    for (kind, items) in groups {
        let mut group = JunkGroup {
            kind,
            items: Vec::with_capacity(items.len()),
        };
        for item in items {
            let target = match &item.location {
                Location::Path { path } => {
                    Target::path(path.clone(), item.bytes, settings.files_delete)
                }
                other => Target::location(other.clone(), 0),
            }
            .admin(item.admin);
            let id = omc_scan::target::push_target(&mut targets, target);
            ctx.add_items(1);
            ctx.add_bytes(item.bytes);
            group.items.push(JunkItem {
                id,
                name: item.name,
                location: item.location,
                tag: None,
                bytes: item.bytes,
                files: item.files,
                modified: item.modified,
                safety: item.safety,
                needs_admin: item.admin,
                app_running: false,
                ident: None,
                icon: None,
            });
        }
        report.groups.push(group);
    }
    Scanned { report, targets }
}
