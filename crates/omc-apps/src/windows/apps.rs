//! Installed-app inventory: `Uninstall` registry keys (64-bit, 32-bit and per-user views)
//! and Store packages (`Get-AppxPackage`).

use std::collections::HashMap;
use std::time::Duration;

use omc_proto::apps::{AppInfo, AppSource};
use omc_proto::jobs::Phase;
use omc_proto::settings::CleanSettings;
use omc_scan::{JobCtx, Walker};

use super::parse::{self, AppxPackage, Hive};
use super::{AppDetail, Package, known, reg};
use crate::{AppRecord, Error, Result};

/// `…\CurrentVersion\Uninstall` sub path.
pub(crate) const UNINSTALL: &str = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall";
/// Its 32-bit view.
pub(crate) const UNINSTALL_WOW: &str =
    "SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall";

/// Every `Uninstall` root, 64-bit machine view first.
pub(crate) const UNINSTALL_ROOTS: [(Hive, &str); 4] = [
    (Hive::LocalMachine, UNINSTALL),
    (Hive::LocalMachine, UNINSTALL_WOW),
    (Hive::CurrentUser, UNINSTALL),
    (Hive::CurrentUser, UNINSTALL_WOW),
];

/// One `Uninstall` subkey as read.
#[derive(Debug, Clone, Default)]
pub(crate) struct UninstallEntry {
    pub(crate) hive: Option<Hive>,
    /// Sub path below the hive.
    pub(crate) path: String,
    /// Key name (product code or vendor key).
    pub(crate) key_name: String,
    pub(crate) name: Option<String>,
    pub(crate) version: Option<String>,
    pub(crate) publisher: Option<String>,
    pub(crate) install_location: Option<String>,
    pub(crate) uninstall: Option<String>,
    pub(crate) quiet_uninstall: Option<String>,
    pub(crate) estimated_kb: Option<u32>,
    pub(crate) display_icon: Option<String>,
    pub(crate) install_date: Option<String>,
    pub(crate) system_component: bool,
    /// `ParentKeyName`: the `Uninstall` key of the product this entry is a part of.
    pub(crate) parent_key: Option<String>,
    pub(crate) release_type: Option<String>,
    pub(crate) windows_installer: bool,
    pub(crate) no_remove: bool,
}

impl UninstallEntry {
    /// The hive (always set for read entries).
    pub(crate) fn hive(&self) -> Hive {
        self.hive.unwrap_or(Hive::LocalMachine)
    }

    /// `HKLM\…\Uninstall\<key>`.
    pub(crate) fn full_key(&self) -> String {
        self.hive().full(&self.path)
    }

    /// MSI product code: the key name of a Windows Installer entry, or the code in an
    /// `MsiExec.exe /X{…}` uninstall string.
    pub(crate) fn msi_code(&self) -> Option<String> {
        if parse::is_guid(&self.key_name)
            && (self.windows_installer
                || self
                    .uninstall
                    .as_deref()
                    .is_some_and(|u| u.to_ascii_lowercase().contains("msiexec")))
        {
            return Some(self.key_name.to_ascii_uppercase());
        }
        let uninstall = self.uninstall.as_deref()?;
        let line = parse::split_command(uninstall, |_| false)?;
        (line.exe_name().trim_end_matches(".exe") == "msiexec")
            .then(|| parse::find_guid(&line.args))
            .flatten()
    }

    /// Shown as an installed program (Programs and Features rules).
    pub(crate) fn is_listed(&self) -> bool {
        let Some(name) = self.name.as_deref() else {
            return false;
        };
        if self.system_component
            || self.parent_key.is_some()
            || parse::is_update_name(name)
            || self
                .release_type
                .as_deref()
                .is_some_and(parse::is_update_release)
        {
            return false;
        }
        self.uninstall.is_some() || self.windows_installer
    }

    /// Install folder: `InstallLocation`, else the folder of the icon or uninstaller
    /// program; never a broad/system folder.
    pub(crate) fn location(&self) -> Option<String> {
        let usable = |dir: &str| !known::is_broad_dir(dir) && std::path::Path::new(dir).is_dir();
        if let Some(dir) = self
            .install_location
            .as_deref()
            .map(reg::expand)
            .and_then(|d| parse::clean_path(&d))
            && usable(&dir)
        {
            return Some(dir);
        }
        let from_icon = self
            .icon_exe()
            .and_then(|exe| parse::parent_dir(&exe).map(str::to_owned));
        let from_uninstaller = self.uninstall.as_deref().and_then(|u| {
            let line = parse::split_command(&reg::expand(u), known::is_file)?;
            parse::is_absolute(&line.exe)
                .then(|| parse::parent_dir(&line.exe).map(str::to_owned))
                .flatten()
        });
        [from_icon, from_uninstaller]
            .into_iter()
            .flatten()
            .find(|dir| usable(dir) && !dir.to_ascii_lowercase().contains("\\installer"))
    }

    /// The program named by `DisplayIcon` (only `.exe`, outside `%WINDIR%`).
    pub(crate) fn icon_exe(&self) -> Option<String> {
        let icon = parse::icon_path(&reg::expand(self.display_icon.as_deref()?))?;
        (icon.to_ascii_lowercase().ends_with(".exe")
            && parse::is_absolute(&icon)
            && !parse::path_within(&icon, &known::windir()))
        .then_some(icon)
    }
}

/// Reads one `Uninstall` subkey.
fn read_entry(hive: Hive, root: &str, key_name: &str) -> Option<UninstallEntry> {
    let path = format!("{root}\\{key_name}");
    let key = reg::open(hive, &path)?;
    let flag = |name: &str| reg::dword(&key, name) == Some(1);
    Some(UninstallEntry {
        hive: Some(hive),
        key_name: key_name.to_owned(),
        name: reg::string(&key, "DisplayName"),
        version: reg::string(&key, "DisplayVersion"),
        publisher: reg::string(&key, "Publisher"),
        install_location: reg::string(&key, "InstallLocation"),
        uninstall: reg::string(&key, "UninstallString"),
        quiet_uninstall: reg::string(&key, "QuietUninstallString"),
        estimated_kb: reg::dword(&key, "EstimatedSize"),
        display_icon: reg::string(&key, "DisplayIcon"),
        install_date: reg::string(&key, "InstallDate"),
        system_component: flag("SystemComponent"),
        parent_key: reg::string(&key, "ParentKeyName"),
        release_type: reg::string(&key, "ReleaseType"),
        windows_installer: flag("WindowsInstaller"),
        no_remove: flag("NoRemove"),
        path,
    })
}

/// Every `Uninstall` subkey of every view (listed or not).
pub(crate) fn read_entries() -> Vec<UninstallEntry> {
    let mut out = Vec::new();
    for (hive, root) in UNINSTALL_ROOTS {
        let Some(key) = reg::open(hive, root) else {
            continue;
        };
        for name in reg::subkeys(&key) {
            if let Some(entry) = read_entry(hive, root, &name) {
                out.push(entry);
            }
        }
    }
    out
}

/// Listed entries merged across views: same name and version = one app.
pub(crate) fn merged_entries(entries: Vec<UninstallEntry>) -> Vec<Vec<UninstallEntry>> {
    let mut index: HashMap<(String, String), usize> = HashMap::new();
    let mut groups: Vec<Vec<UninstallEntry>> = Vec::new();
    for entry in entries.into_iter().filter(UninstallEntry::is_listed) {
        let key = (
            entry
                .name
                .as_deref()
                .unwrap_or_default()
                .trim()
                .to_lowercase(),
            entry
                .version
                .as_deref()
                .unwrap_or_default()
                .trim()
                .to_lowercase(),
        );
        if let Some(group) = index.get(&key).and_then(|i| groups.get_mut(*i)) {
            group.push(entry);
        } else {
            index.insert(key, groups.len());
            groups.push(vec![entry]);
        }
    }
    groups
}

/// First non-empty field across a merged group.
fn first<T: Clone>(
    group: &[UninstallEntry],
    field: impl Fn(&UninstallEntry) -> Option<T>,
) -> Option<T> {
    group.iter().find_map(field)
}

/// A registry app record (no size or running state yet).
fn registry_record(group: &[UninstallEntry]) -> Option<AppRecord> {
    let head = group.first()?;
    let name = head.name.clone()?;
    let publisher = first(group, |e| e.publisher.clone());
    let location = first(group, UninstallEntry::location);
    let icon_exe = first(group, UninstallEntry::icon_exe);
    let mut exes = location
        .as_deref()
        .map(known::top_level_exes)
        .unwrap_or_default();
    if let Some(icon) = icon_exe.as_deref() {
        let exe = parse::file_name(icon).to_ascii_lowercase();
        if !exes.contains(&exe) {
            exes.push(exe);
        }
    }
    let system =
        parse::is_system_app(&name, publisher.as_deref()) || group.iter().any(|e| e.no_remove);
    let needs_admin = group.iter().any(|e| e.hive() == Hive::LocalMachine);
    let bytes = first(group, |e| e.estimated_kb)
        .filter(|kb| *kb > 0)
        .map(|kb| u64::from(kb).saturating_mul(1024));
    let info = AppInfo {
        id: 0,
        name,
        version: first(group, |e| e.version.clone()),
        publisher,
        ident: Some(head.key_name.clone()),
        location: location.clone(),
        bytes,
        source: AppSource::WinRegistry,
        system,
        running: false,
        last_used: None,
        installed: first(group, |e| {
            e.install_date
                .as_deref()
                .and_then(parse::parse_install_date)
        }),
        icon: None,
        needs_admin,
    };
    let detail = AppDetail {
        keys: group.iter().map(|e| (e.hive(), e.path.clone())).collect(),
        uninstall: first(group, |e| e.uninstall.clone()),
        quiet_uninstall: first(group, |e| e.quiet_uninstall.clone()),
        msi_code: first(group, UninstallEntry::msi_code),
        location,
        icon_exe,
        exes,
        package: None,
    };
    Some(AppRecord { info, detail })
}

/// `Get-AppxPackage` filtered to removable, non-framework, non-system packages.
const APPX_SCRIPT: &str = "Get-AppxPackage | Where-Object { -not $_.IsFramework -and $_.SignatureKind -ne 'System' -and -not $_.NonRemovable } | Select-Object Name,PackageFullName,PackageFamilyName,Version,Publisher,InstallLocation | ConvertTo-Json -Compress";

/// Store packages (empty with a warning when PowerShell fails).
pub(crate) fn store_packages() -> Vec<AppxPackage> {
    match known::powershell(APPX_SCRIPT, Duration::from_secs(60))
        .and_then(|text| parse::parse_json_list::<AppxPackage>(&text))
    {
        Ok(list) => list,
        Err(err) => {
            tracing::warn!(%err, "listing Store packages failed");
            Vec::new()
        }
    }
}

/// A Store app record.
fn store_record(pkg: AppxPackage) -> AppRecord {
    let manifest = pkg
        .install_location
        .as_deref()
        .and_then(|dir| std::fs::read_to_string(format!("{dir}\\AppxManifest.xml")).ok());
    let name = manifest
        .as_deref()
        .and_then(parse::manifest_display_name)
        .unwrap_or_else(|| pkg.name.clone());
    let publisher = manifest
        .as_deref()
        .and_then(parse::manifest_publisher)
        .or_else(|| pkg.publisher.as_deref().and_then(parse::dn_name));
    let location = pkg.install_location.as_deref().and_then(parse::clean_path);
    let exes = location
        .as_deref()
        .map(known::top_level_exes)
        .unwrap_or_default();
    let info = AppInfo {
        id: 0,
        name,
        version: pkg.version.clone(),
        publisher,
        ident: Some(
            pkg.package_family_name
                .clone()
                .unwrap_or_else(|| pkg.package_full_name.clone()),
        ),
        location: location.clone(),
        bytes: None,
        source: AppSource::WinStore,
        system: false,
        running: false,
        last_used: None,
        installed: None,
        icon: None,
        needs_admin: false,
    };
    let detail = AppDetail {
        keys: Vec::new(),
        uninstall: None,
        quiet_uninstall: None,
        msi_code: None,
        location: location.clone(),
        icon_exe: None,
        exes,
        package: Some(Package {
            full_name: pkg.package_full_name,
            family_name: pkg.package_family_name,
            location,
        }),
    };
    AppRecord { info, detail }
}

/// Every installed app (system apps flagged, not filtered).
pub(crate) fn list(
    _settings: &CleanSettings,
    walker: &Walker,
    ctx: &JobCtx,
) -> Result<Vec<AppRecord>> {
    ctx.set_phase(Phase::Scanning);
    let (packages, entries) = std::thread::scope(|scope| {
        let store = scope.spawn(store_packages);
        let entries = read_entries();
        let packages = store.join().unwrap_or_else(|_| {
            tracing::warn!("Store package listing thread panicked");
            Vec::new()
        });
        (packages, entries)
    });
    if ctx.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let mut apps: Vec<AppRecord> = merged_entries(entries)
        .iter()
        .filter_map(|group| registry_record(group))
        .collect();
    apps.extend(packages.into_iter().map(store_record));
    ctx.add_items(u64::try_from(apps.len()).unwrap_or(u64::MAX));

    let running: Vec<String> = omc_scan::procs::running()
        .into_iter()
        .map(|p| p.name.to_ascii_lowercase())
        .collect();
    for app in &mut apps {
        app.info.running = app.detail.exes.iter().any(|exe| running.contains(exe));
    }

    ctx.set_phase(Phase::Measuring);
    let unmeasured: Vec<usize> = apps
        .iter()
        .enumerate()
        .filter(|(_, a)| a.info.bytes.is_none() && a.info.location.is_some())
        .map(|(i, _)| i)
        .collect();
    let paths: Vec<std::path::PathBuf> = unmeasured
        .iter()
        .filter_map(|i| {
            apps.get(*i)?
                .info
                .location
                .as_deref()
                .map(std::path::PathBuf::from)
        })
        .collect();
    let measures = walker.measure_all(&paths, ctx);
    for (i, measure) in unmeasured.iter().zip(measures) {
        if let Some(app) = apps.get_mut(*i)
            && !measure.missing
        {
            app.info.bytes = Some(measure.bytes);
        }
    }
    if ctx.is_cancelled() {
        return Err(Error::Cancelled);
    }
    Ok(apps)
}

/// What other installed apps claim: normalised names, vendors and install folders.
#[derive(Debug, Default)]
pub(crate) struct Peers {
    pub(crate) names: std::collections::HashSet<String>,
    pub(crate) publishers: std::collections::HashSet<String>,
    pub(crate) locations: Vec<String>,
}

impl Peers {
    /// A folder or key named `normalized` belongs to another app too.
    pub(crate) fn has_name(&self, normalized: &str) -> bool {
        self.names.contains(normalized)
    }

    /// `path` is inside (or contains) another app's install folder.
    pub(crate) fn overlaps(&self, path: &str) -> bool {
        self.locations
            .iter()
            .any(|loc| parse::path_within(path, loc) || parse::path_within(loc, path))
    }
}

/// Listed registry apps other than the one owning `own_keys`.
pub(crate) fn peers(own_keys: &[(Hive, String)], own_name: &str) -> Peers {
    let mut peers = Peers::default();
    let own_name = own_name.trim().to_lowercase();
    for entry in read_entries().into_iter().filter(UninstallEntry::is_listed) {
        let hive = entry.hive();
        if own_keys
            .iter()
            .any(|(h, p)| *h == hive && p.eq_ignore_ascii_case(&entry.path))
            || entry
                .name
                .as_deref()
                .is_some_and(|n| n.trim().to_lowercase() == own_name)
        {
            continue;
        }
        let location = entry.location();
        let name = entry.name.clone().unwrap_or_default();
        for key in parse::product_keys(&name, entry.publisher.as_deref(), location.as_deref()) {
            peers.names.insert(key);
        }
        if let Some(publisher) = entry.publisher.as_deref() {
            peers
                .publishers
                .insert(parse::normalize_publisher(publisher));
        }
        if let Some(location) = location {
            peers.locations.push(location);
        }
    }
    peers
}
