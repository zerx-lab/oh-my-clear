//! The deeper traces of one app, beyond folders, shortcuts and the obvious registry keys:
//! Windows Installer registration and component key paths only this product owns,
//! `SharedDLLs` counters, firewall rules, Event Log sources, `Installer\Folders`, COM type
//! libraries and interfaces, registered-application capabilities, `Uninstall` keys of the
//! same product in other views, Electron/Squirrel data folders, pinned and Quick Launch
//! shortcuts, `%TEMP%` leftovers, Windows Error Reporting and Prefetch files, Burn/MSI
//! package caches once the product is gone, and usage traces (`UserAssist`, `MUICache`,
//! `AppCompatFlags`, `FeatureUsage`, `OpenWithList`, `Tracing`). Scoring goes through
//! [`clues::confidence`]; usage traces are capped at Low (privacy, never needed).

use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};

use omc_proto::apps::{AppFileKind, Confidence};

use super::apps::{self, UninstallEntry};
use super::clues::{self, Clue, KeyPath, Ownership, SharedDll};
use super::files::{Found, Matcher, lnk_match};
use super::parse::{self, Hive};
use super::{known, leftovers, reg};
use crate::AppRecord;
use omc_scan::JobCtx;

/// `…\Installer\UserData` (one subkey per SID).
const USER_DATA: &str = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Installer\\UserData";

/// Files and folders: Electron data, pinned shortcuts, temp, crash reports, Prefetch,
/// package caches.
pub(super) fn files(app: &AppRecord, m: &Matcher, found: &mut Vec<Found>) {
    electron(app, m, found);
    pinned_shortcuts(m, found);
    temp(m, found);
    wer_reports(m, found);
    prefetch(m, found);
    package_cache(app, found);
}

/// Registry traces.
pub(super) fn registry(app: &AppRecord, m: &Matcher, ctx: &JobCtx, found: &mut Vec<Found>) {
    related_uninstall_keys(app, m, found);
    msi(app, m, ctx, found);
    if m.needles.is_empty() {
        return;
    }
    installer_folders(m, found);
    firewall_rules(m, found);
    event_log_sources(m, ctx, found);
    com_type_info(m, ctx, found);
    clients(m, found);
    usage_traces(m, found);
}

fn push_path(
    found: &mut Vec<Found>,
    path: String,
    kind: AppFileKind,
    evidence: &[Clue],
    cap: Confidence,
) {
    if let Some(confidence) = clues::confidence(evidence, cap) {
        found.push(Found::path(path, kind, confidence));
    }
}

fn push_key(found: &mut Vec<Found>, hive: Hive, path: &str, evidence: &[Clue], cap: Confidence) {
    if let Some(confidence) = clues::confidence(evidence, cap) {
        found.push(Found::key(hive, path, confidence));
    }
}

fn push_value(
    found: &mut Vec<Found>,
    hive: Hive,
    path: &str,
    name: &str,
    evidence: &[Clue],
    cap: Confidence,
) {
    if let Some(confidence) = clues::confidence(evidence, cap) {
        found.push(Found::value(hive, path, name, confidence));
    }
}

/// [`Clue::Reference`], plus [`Clue::Shared`] when another app's folder overlaps `path`.
fn reference(m: &Matcher, path: &str) -> Vec<Clue> {
    let mut evidence = vec![Clue::Reference];
    if m.peers.overlaps(path) {
        evidence.push(Clue::Shared);
    }
    evidence
}

// ---------------------------------------------------------------------------------------
// SharedDLLs

/// `SharedDLLs` keys (64-bit and 32-bit views).
const SHARED_DLLS: [&str; 2] = [
    "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\SharedDLLs",
    "SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\SharedDLLs",
];

/// Every `SharedDLLs` value: (key, file path, count).
pub(super) fn shared_dll_values() -> Vec<(&'static str, String, Option<u32>)> {
    let mut out = Vec::new();
    for key in SHARED_DLLS {
        if let Some(k) = reg::open(Hive::LocalMachine, key) {
            out.extend(
                reg::dword_values(&k)
                    .into_iter()
                    .map(|(name, count)| (key, name, count)),
            );
        }
    }
    out
}

/// Counters of files inside the app that only it references become items; returns
/// whether a file inside the app is still counted by other installers (then the install
/// folder is shared and must not be preselected).
pub(super) fn shared_dlls(m: &Matcher, found: &mut Vec<Found>) -> bool {
    if m.needles.is_empty() {
        return false;
    }
    let mut shared = false;
    for (key, file, count) in shared_dll_values() {
        if !m.owns_path(&reg::expand(&file)) {
            continue;
        }
        match clues::shared_dll(count) {
            SharedDll::Owned => push_value(
                found,
                Hive::LocalMachine,
                key,
                &file,
                &[Clue::Reference],
                Confidence::High,
            ),
            SharedDll::Shared => shared = true,
        }
    }
    shared
}

// ---------------------------------------------------------------------------------------
// Windows Installer

/// Product registration (removed by `msiexec /x` itself; offered for forced removal),
/// then the key paths of components only this product owns.
fn msi(app: &AppRecord, m: &Matcher, ctx: &JobCtx, found: &mut Vec<Found>) {
    let Some(code) = app.detail.msi_code.as_deref() else {
        return;
    };
    let Some(packed) = parse::packed_guid(code) else {
        return;
    };
    let sids = reg::open(Hive::LocalMachine, USER_DATA)
        .map(|k| reg::subkeys(&k))
        .unwrap_or_default();
    let mut registration = vec![
        (
            Hive::LocalMachine,
            format!("SOFTWARE\\Classes\\Installer\\Products\\{packed}"),
        ),
        (
            Hive::LocalMachine,
            format!("SOFTWARE\\Classes\\Installer\\Features\\{packed}"),
        ),
        (
            Hive::CurrentUser,
            format!("Software\\Microsoft\\Installer\\Products\\{packed}"),
        ),
        (
            Hive::CurrentUser,
            format!("Software\\Microsoft\\Installer\\Features\\{packed}"),
        ),
    ];
    registration.extend(sids.iter().map(|sid| {
        (
            Hive::LocalMachine,
            format!("{USER_DATA}\\{sid}\\Products\\{packed}"),
        )
    }));
    for (hive, path) in registration {
        if reg::exists(hive, &path) {
            push_key(found, hive, &path, &[Clue::MsiOwned], Confidence::Medium);
        }
    }
    for (hive, base) in [
        (
            Hive::LocalMachine,
            "SOFTWARE\\Classes\\Installer\\UpgradeCodes",
        ),
        (
            Hive::CurrentUser,
            "Software\\Microsoft\\Installer\\UpgradeCodes",
        ),
    ] {
        let Some(key) = reg::open(hive, base) else {
            continue;
        };
        for upgrade in reg::subkeys(&key) {
            let path = format!("{base}\\{upgrade}");
            let Some(sub) = reg::open(hive, &path) else {
                continue;
            };
            let names = reg::value_names(&sub);
            match clues::ownership(names.iter().map(String::as_str), &packed) {
                Ownership::Sole => {
                    push_key(found, hive, &path, &[Clue::MsiOwned], Confidence::Medium);
                }
                Ownership::Shared => {
                    if let Some(name) = names.iter().find(|n| n.eq_ignore_ascii_case(&packed)) {
                        push_value(
                            found,
                            hive,
                            &path,
                            name,
                            &[Clue::MsiOwned],
                            Confidence::Medium,
                        );
                    }
                }
                Ownership::Foreign => {}
            }
        }
    }
    component_key_paths(m, &sids, &packed, ctx, found);
}

/// Files, folders and registry values Windows Installer lists as key paths of components
/// only this product owns (a component another product or the system also claims is
/// kept, and so is a file other installers still count in `SharedDLLs`).
fn component_key_paths(
    m: &Matcher,
    sids: &[String],
    packed: &str,
    ctx: &JobCtx,
    found: &mut Vec<Found>,
) {
    let mut components: Vec<String> = Vec::new();
    for sid in sids {
        let base = format!("{USER_DATA}\\{sid}\\Components");
        if let Some(key) = reg::open(Hive::LocalMachine, &base) {
            components.extend(
                reg::subkeys(&key)
                    .into_iter()
                    .map(|c| format!("{base}\\{c}")),
            );
        }
    }
    let key_paths: Vec<KeyPath> = known::par_flat_map(&components, ctx, |path| {
        let Some(key) = reg::open(Hive::LocalMachine, path) else {
            return Vec::new();
        };
        let names = reg::value_names(&key);
        if clues::ownership(names.iter().map(String::as_str), packed) != Ownership::Sole {
            return Vec::new();
        }
        names
            .iter()
            .find(|n| n.eq_ignore_ascii_case(packed))
            .and_then(|n| reg::string(&key, n))
            .and_then(|raw| clues::parse_key_path(&raw))
            .into_iter()
            .collect()
    });
    if key_paths.is_empty() {
        return;
    }
    let shared_files: HashSet<String> = shared_dll_values()
        .into_iter()
        .filter(|(_, _, count)| clues::shared_dll(*count) == SharedDll::Shared)
        .map(|(_, file, _)| reg::expand(&file).to_lowercase())
        .collect();
    let windir = known::windir();
    let claimable = |path: &str| {
        parse::is_absolute(path)
            && !parse::path_within(path, &windir)
            && !m.peers.overlaps(path)
            && !known::is_broad_dir(path)
    };
    for key_path in key_paths {
        match key_path {
            KeyPath::File(path) => {
                if claimable(&path)
                    && !shared_files.contains(&path.to_lowercase())
                    && known::is_file(&path)
                {
                    push_path(
                        found,
                        path,
                        AppFileKind::Other,
                        &[Clue::MsiOwned],
                        Confidence::High,
                    );
                }
            }
            KeyPath::Folder(path) => {
                // Data written after installation may live there too: not preselected at
                // the default level.
                if claimable(&path) && std::path::Path::new(&path).is_dir() {
                    push_path(
                        found,
                        path,
                        AppFileKind::Support,
                        &[Clue::MsiOwned],
                        Confidence::Medium,
                    );
                }
            }
            KeyPath::Value {
                hive,
                key,
                name,
                wow32,
            } => {
                let redirected = (wow32 && hive == Hive::LocalMachine)
                    .then(|| clues::wow_key(&key))
                    .flatten()
                    .filter(|wow| reg::value_exists(hive, wow, &name));
                let key = redirected.unwrap_or(key);
                if reg::value_exists(hive, &key, &name) {
                    push_value(
                        found,
                        hive,
                        &key,
                        &name,
                        &[Clue::MsiOwned],
                        Confidence::High,
                    );
                }
            }
        }
    }
}

/// `Installer\Folders` values (folders Windows Installer created) inside the app.
fn installer_folders(m: &Matcher, found: &mut Vec<Found>) {
    let path = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Installer\\Folders";
    let Some(key) = reg::open(Hive::LocalMachine, path) else {
        return;
    };
    for name in reg::value_names(&key) {
        if m.owns_path(&name) {
            push_value(
                found,
                Hive::LocalMachine,
                path,
                &name,
                &reference(m, &name),
                Confidence::High,
            );
        }
    }
}

/// Unlisted `Uninstall` entries of the same product: parts naming it as `ParentKeyName`,
/// registrations of its MSI code in another view, and hidden entries whose files live in
/// the install folder.
fn related_uninstall_keys(app: &AppRecord, m: &Matcher, found: &mut Vec<Found>) {
    let own: Vec<&str> = app
        .detail
        .keys
        .iter()
        .map(|(_, path)| parse::split_key(path).1)
        .collect();
    let msi = app.detail.msi_code.as_deref();
    let dir = app.detail.location.as_deref();
    for entry in apps::read_entries() {
        let hive = entry.hive();
        if app
            .detail
            .keys
            .iter()
            .any(|(h, p)| *h == hive && p.eq_ignore_ascii_case(&entry.path))
        {
            continue;
        }
        if related_entry(&entry, &own, msi, dir, m) {
            found.push(Found::key(hive, &entry.path, Confidence::High));
        }
    }
}

fn related_entry(
    entry: &UninstallEntry,
    own: &[&str],
    msi: Option<&str>,
    dir: Option<&str>,
    m: &Matcher,
) -> bool {
    if entry
        .parent_key
        .as_deref()
        .is_some_and(|p| own.iter().any(|o| o.eq_ignore_ascii_case(p.trim())))
    {
        return true;
    }
    if let Some(code) = msi
        && entry
            .msi_code()
            .is_some_and(|c| c.eq_ignore_ascii_case(code))
    {
        return true;
    }
    if entry.is_listed() || dir.is_none() {
        return false;
    }
    let in_dir = entry
        .install_location
        .as_deref()
        .and_then(parse::clean_path)
        .is_some_and(|l| dir.is_some_and(|d| parse::path_within(&reg::expand(&l), d)));
    in_dir || entry.uninstall.as_deref().is_some_and(|u| m.mentions(u))
}

// ---------------------------------------------------------------------------------------
// Firewall, Event Log, COM, clients

/// Windows Defender Firewall rules (`v2.x|Action=…|App=…|` strings).
pub(super) const FIREWALL_RULES: &str =
    "SYSTEM\\CurrentControlSet\\Services\\SharedAccess\\Parameters\\FirewallPolicy\\FirewallRules";

/// Firewall rules whose program lives in the app.
fn firewall_rules(m: &Matcher, found: &mut Vec<Found>) {
    let path = FIREWALL_RULES;
    let Some(key) = reg::open(Hive::LocalMachine, path) else {
        return;
    };
    for (name, rule) in reg::string_values(&key) {
        if clues::firewall_field(&rule, "App").is_some_and(|app| m.owns_path(&reg::expand(app))) {
            push_value(
                found,
                Hive::LocalMachine,
                path,
                &name,
                &[Clue::Reference],
                Confidence::High,
            );
        }
    }
}

/// Event Log sources whose message files live in the app.
fn event_log_sources(m: &Matcher, ctx: &JobCtx, found: &mut Vec<Found>) {
    let base = "SYSTEM\\CurrentControlSet\\Services\\EventLog";
    let Some(logs) = reg::open(Hive::LocalMachine, base) else {
        return;
    };
    let mut sources: Vec<String> = Vec::new();
    for log in reg::subkeys(&logs) {
        let path = format!("{base}\\{log}");
        if let Some(key) = reg::open(Hive::LocalMachine, &path) {
            sources.extend(
                reg::subkeys(&key)
                    .into_iter()
                    .map(|s| format!("{path}\\{s}")),
            );
        }
    }
    let hits: Vec<String> = known::par_flat_map(&sources, ctx, |path| {
        let Some(key) = reg::open(Hive::LocalMachine, path) else {
            return Vec::new();
        };
        let hit = [
            "EventMessageFile",
            "CategoryMessageFile",
            "ParameterMessageFile",
        ]
        .iter()
        .filter_map(|v| reg::string(&key, v))
        .any(|files| files.split(';').any(|f| m.owns_path(&reg::expand(f))));
        if hit { vec![path.clone()] } else { Vec::new() }
    });
    for path in hits {
        push_key(
            found,
            Hive::LocalMachine,
            &path,
            &[Clue::Reference],
            Confidence::High,
        );
    }
}

/// `…\CLSID\{GUID}` keys already found → their upper-case GUIDs.
fn found_clsids(found: &[Found]) -> HashSet<String> {
    found
        .iter()
        .filter_map(|f| match &f.location {
            omc_proto::jobs::Location::RegistryKey { key } => {
                let (parent, guid) = parse::split_key(key);
                (parse::file_name(parent).eq_ignore_ascii_case("CLSID") && parse::is_guid(guid))
                    .then(|| guid.to_ascii_uppercase())
            }
            _ => None,
        })
        .collect()
}

/// Type libraries whose files live in the app, and interfaces proxied by its COM classes.
fn com_type_info(m: &Matcher, ctx: &JobCtx, found: &mut Vec<Found>) {
    let clsids = found_clsids(found);
    for (hive, base) in [
        (Hive::CurrentUser, "Software\\Classes"),
        (Hive::LocalMachine, "SOFTWARE\\Classes"),
        (Hive::LocalMachine, "SOFTWARE\\Classes\\WOW6432Node"),
    ] {
        let typelib = format!("{base}\\TypeLib");
        let libs = reg::open(hive, &typelib)
            .map(|k| reg::subkeys(&k))
            .unwrap_or_default();
        let lib_hits: Vec<String> = known::par_flat_map(&libs, ctx, |guid| {
            let path = format!("{typelib}\\{guid}");
            if typelib_mentions(m, hive, &path) {
                vec![path]
            } else {
                Vec::new()
            }
        });
        for path in lib_hits {
            push_key(found, hive, &path, &[Clue::Reference], Confidence::High);
        }
        if clsids.is_empty() {
            continue;
        }
        let interface = format!("{base}\\Interface");
        let iids = reg::open(hive, &interface)
            .map(|k| reg::subkeys(&k))
            .unwrap_or_default();
        let iid_hits: Vec<String> = known::par_flat_map(&iids, ctx, |iid| {
            let path = format!("{interface}\\{iid}");
            let proxied = reg::open(hive, &format!("{path}\\ProxyStubClsid32"))
                .and_then(|k| reg::string(&k, ""))
                .is_some_and(|clsid| clsids.contains(&clsid.trim().to_ascii_uppercase()));
            if proxied { vec![path] } else { Vec::new() }
        });
        for path in iid_hits {
            push_key(found, hive, &path, &[Clue::Reference], Confidence::High);
        }
    }
}

/// `TypeLib\{GUID}\<version>\<lcid>\win32|win64` names a file in the app.
fn typelib_mentions(m: &Matcher, hive: Hive, path: &str) -> bool {
    let Some(lib) = reg::open(hive, path) else {
        return false;
    };
    reg::subkeys(&lib).iter().any(|version| {
        let version_path = format!("{path}\\{version}");
        let Some(version_key) = reg::open(hive, &version_path) else {
            return false;
        };
        reg::subkeys(&version_key)
            .iter()
            .filter(|lcid| lcid.chars().all(|c| c.is_ascii_hexdigit()))
            .any(|lcid| {
                ["win32", "win64"].iter().any(|platform| {
                    reg::open(hive, &format!("{version_path}\\{lcid}\\{platform}"))
                        .and_then(|k| reg::string(&k, ""))
                        .is_some_and(|file| m.mentions(&file))
                })
            })
    })
}

/// `Software\Clients\<type>\<name>` (default browser, mail client…) and the
/// `RegisteredApplications` values pointing at the app's capabilities.
fn clients(m: &Matcher, found: &mut Vec<Found>) {
    for (hive, software) in [
        (Hive::CurrentUser, "Software"),
        (Hive::LocalMachine, "SOFTWARE"),
    ] {
        let base = format!("{software}\\Clients");
        if let Some(types) = reg::open(hive, &base) {
            for kind in reg::subkeys(&types) {
                let kind_path = format!("{base}\\{kind}");
                let Some(kind_key) = reg::open(hive, &kind_path) else {
                    continue;
                };
                for client in reg::subkeys(&kind_key) {
                    let path = format!("{kind_path}\\{client}");
                    if client_mentions(m, hive, &path) {
                        push_key(found, hive, &path, &[Clue::Reference], Confidence::High);
                    }
                }
            }
        }
        let registered = format!("{software}\\RegisteredApplications");
        let Some(key) = reg::open(hive, &registered) else {
            continue;
        };
        for (name, target) in reg::string_values(&key) {
            let target = target.trim().trim_matches('\\');
            let owner = target
                .strip_suffix("\\Capabilities")
                .or_else(|| target.strip_suffix("\\capabilities"))
                .unwrap_or(target);
            if client_mentions(m, hive, target) || client_mentions(m, hive, owner) {
                push_value(
                    found,
                    hive,
                    &registered,
                    &name,
                    &[Clue::Reference],
                    Confidence::High,
                );
                if owner.to_ascii_lowercase().contains("\\clients\\") {
                    push_key(found, hive, owner, &[Clue::Reference], Confidence::High);
                } else {
                    push_key(found, hive, target, &[Clue::Reference], Confidence::High);
                }
            }
        }
    }
}

/// A client or capabilities key whose command, icon or application icon is in the app.
fn client_mentions(m: &Matcher, hive: Hive, path: &str) -> bool {
    let direct = reg::open(hive, path).is_some_and(|k| {
        ["ApplicationIcon", "ApplicationName"]
            .iter()
            .filter_map(|v| reg::string(&k, v))
            .any(|v| m.mentions(&v))
    });
    direct
        || ["shell\\open\\command", "DefaultIcon", "Capabilities"]
            .iter()
            .filter_map(|sub| reg::open(hive, &format!("{path}\\{sub}")))
            .any(|k| {
                ["", "ApplicationIcon"]
                    .iter()
                    .filter_map(|v| reg::string(&k, v))
                    .any(|v| m.mentions(&v))
            })
}

// ---------------------------------------------------------------------------------------
// Usage traces (Low: privacy only)

/// Traces Explorer and the compatibility engine keep about the app's programs.
fn usage_traces(m: &Matcher, found: &mut Vec<Found>) {
    let low = |found: &mut Vec<Found>, hive: Hive, path: &str, name: &str| {
        push_value(found, hive, path, name, &[Clue::Reference], Confidence::Low);
    };
    // UserAssist: ROT13 value names, known-folder GUID prefixes.
    let user_assist = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\UserAssist";
    if let Some(key) = reg::open(Hive::CurrentUser, user_assist) {
        for guid in reg::subkeys(&key) {
            let path = format!("{user_assist}\\{guid}\\Count");
            let Some(count) = reg::open(Hive::CurrentUser, &path) else {
                continue;
            };
            for name in reg::value_names(&count) {
                let decoded = clues::user_assist_path(&name, |var| std::env::var(var).ok());
                if decoded.is_some_and(|p| m.owns_path(&p)) {
                    low(found, Hive::CurrentUser, &path, &name);
                }
            }
        }
    }
    let mui = "Software\\Classes\\Local Settings\\Software\\Microsoft\\Windows\\Shell\\MuiCache";
    if let Some(key) = reg::open(Hive::CurrentUser, mui) {
        for name in reg::value_names(&key) {
            if clues::mui_cache_program(&name).is_some_and(|p| m.owns_path(p)) {
                low(found, Hive::CurrentUser, mui, &name);
            }
        }
    }
    let compat = "Software\\Microsoft\\Windows NT\\CurrentVersion\\AppCompatFlags";
    let feature = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\FeatureUsage";
    let path_named = [
        (Hive::CurrentUser, format!("{compat}\\Layers")),
        (Hive::LocalMachine, format!("{compat}\\Layers")),
        (
            Hive::CurrentUser,
            format!("{compat}\\Compatibility Assistant\\Store"),
        ),
        (
            Hive::CurrentUser,
            format!("{compat}\\Compatibility Assistant\\Persisted"),
        ),
        (Hive::CurrentUser, format!("{feature}\\AppSwitched")),
        (Hive::CurrentUser, format!("{feature}\\ShowJumpView")),
        (Hive::CurrentUser, format!("{feature}\\AppLaunch")),
        (Hive::CurrentUser, format!("{feature}\\AppBadgeUpdated")),
    ];
    for (hive, path) in path_named {
        let Some(key) = reg::open(hive, &path) else {
            continue;
        };
        for name in reg::value_names(&key) {
            if m.owns_path(&name) {
                low(found, hive, &path, &name);
            }
        }
    }
    program_traces(m, found);
}

/// Traces keyed by program file name only: `OpenWithList`, `Tracing`, `HeapLeakDetection`.
fn program_traces(m: &Matcher, found: &mut Vec<Found>) {
    if m.exes.is_empty() {
        return;
    }
    let is_ours = |exe: &str| m.exes.iter().any(|e| e.eq_ignore_ascii_case(exe));
    let file_exts = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\FileExts";
    if let Some(key) = reg::open(Hive::CurrentUser, file_exts) {
        for ext in reg::subkeys(&key) {
            let path = format!("{file_exts}\\{ext}\\OpenWithList");
            let Some(list) = reg::open(Hive::CurrentUser, &path) else {
                continue;
            };
            for (name, exe) in reg::string_values(&list) {
                if !name.eq_ignore_ascii_case("MRUList") && is_ours(&exe) {
                    let evidence = [Clue::Reference];
                    push_value(
                        found,
                        Hive::CurrentUser,
                        &path,
                        &name,
                        &evidence,
                        Confidence::Low,
                    );
                }
            }
        }
    }
    for base in [
        "SOFTWARE\\Microsoft\\Tracing",
        "SOFTWARE\\WOW6432Node\\Microsoft\\Tracing",
    ] {
        let Some(key) = reg::open(Hive::LocalMachine, base) else {
            continue;
        };
        for name in reg::subkeys(&key) {
            if clues::tracing_program(&name).is_some_and(|stem| is_ours(&format!("{stem}.exe"))) {
                push_key(
                    found,
                    Hive::LocalMachine,
                    &format!("{base}\\{name}"),
                    &[Clue::Reference],
                    Confidence::Low,
                );
            }
        }
    }
    let radar = "SOFTWARE\\Microsoft\\RADAR\\HeapLeakDetection\\DiagnosedApplications";
    if let Some(key) = reg::open(Hive::LocalMachine, radar) {
        for name in reg::subkeys(&key) {
            if is_ours(&name) {
                push_key(
                    found,
                    Hive::LocalMachine,
                    &format!("{radar}\\{name}"),
                    &[Clue::Reference],
                    Confidence::Low,
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Files

/// Electron/Squirrel data folders named in the app's own `package.json`
/// (`%APPDATA%\<productName>`, `%LOCALAPPDATA%\<name>`, `%LOCALAPPDATA%\<name>-updater`).
fn electron(app: &AppRecord, m: &Matcher, found: &mut Vec<Found>) {
    let mut names = m.declared.clone();
    if let Some(dir) = app.detail.location.as_deref() {
        for name in electron_manifest_names(dir) {
            if !names.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
                names.push(name);
            }
        }
    }
    let dir = app.detail.location.as_deref();
    for name in names {
        let candidates = [
            known::env_join("APPDATA", &name),
            known::env_join("LOCALAPPDATA", &name),
            known::env_join("LOCALAPPDATA", &format!("{name}-updater")),
        ];
        for path in candidates.into_iter().flatten() {
            if known::is_broad_dir(&path)
                || !std::path::Path::new(&path).is_dir()
                || dir
                    .is_some_and(|d| parse::path_within(d, &path) && !parse::path_within(&path, d))
            {
                continue;
            }
            let mut evidence = vec![Clue::Declared];
            evidence.extend(clues::short_clue(&name));
            if m.peers.has_name(&parse::normalize_name(&name)) || m.peers.overlaps(&path) {
                evidence.push(Clue::Shared);
            }
            push_path(
                found,
                path,
                AppFileKind::Support,
                &evidence,
                Confidence::High,
            );
        }
    }
}

/// The install folder, then its Squirrel `app-<version>` folders (newest first).
pub(super) fn app_roots(dir: &str) -> Vec<String> {
    let mut roots = vec![dir.to_owned()];
    let mut versions: Vec<String> = known::entries(dir)
        .into_iter()
        .filter(|(name, _, is_dir)| *is_dir && name.to_ascii_lowercase().starts_with("app-"))
        .map(|(_, path, _)| path)
        .collect();
    versions.sort();
    roots.extend(versions.into_iter().rev());
    roots
}

/// `productName`/`name` of an Electron app: `resources\app.asar` or `resources\app` in the
/// install folder, or in its Squirrel `app-<version>` folders.
pub(super) fn electron_manifest_names(dir: &str) -> Vec<String> {
    for root in app_roots(dir) {
        let asar = format!("{root}\\resources\\app.asar");
        let json = asar_package_json(&asar).or_else(|| {
            known::read_small(&format!("{root}\\resources\\app\\package.json"))
                .and_then(|b| String::from_utf8(b).ok())
        });
        if let Some(json) = json {
            let names = clues::electron_names(&json);
            if !names.is_empty() {
                return names;
            }
        }
    }
    Vec::new()
}

/// `package.json` inside an `app.asar` (header and one entry read, never the archive).
fn asar_package_json(path: &str) -> Option<String> {
    const MAX_PACKAGE_JSON: u64 = 1 << 20;
    let mut file = std::fs::File::open(path).ok()?;
    let mut head = [0_u8; 16];
    file.read_exact(&mut head).ok()?;
    let layout = clues::asar_layout(&head)?;
    let mut json = vec![0_u8; layout.json_len];
    file.read_exact(&mut json).ok()?;
    let json = String::from_utf8(json).ok()?;
    let entry = clues::asar_entry(&json, "package.json")?;
    if entry.size > MAX_PACKAGE_JSON {
        return None;
    }
    if entry.unpacked {
        return known::read_small(&format!("{path}.unpacked\\package.json"))
            .and_then(|b| String::from_utf8(b).ok());
    }
    file.seek(SeekFrom::Start(
        layout.data_start.checked_add(entry.offset)?,
    ))
    .ok()?;
    let mut data = vec![0_u8; usize::try_from(entry.size).ok()?];
    file.read_exact(&mut data).ok()?;
    String::from_utf8(data).ok()
}

/// Quick Launch, taskbar and Start pins.
fn pinned_shortcuts(m: &Matcher, found: &mut Vec<Found>) {
    let Some(quick) = known::env_join("APPDATA", "Microsoft\\Internet Explorer\\Quick Launch")
    else {
        return;
    };
    let roots = [
        (quick.clone(), 0),
        (format!("{quick}\\User Pinned\\TaskBar"), 0),
        (format!("{quick}\\User Pinned\\StartMenu"), 0),
        (format!("{quick}\\User Pinned\\ImplicitAppShortcuts"), 1),
    ];
    for (root, depth) in roots {
        for path in known::shortcuts(&root, depth) {
            if let Some(confidence) = lnk_match(m, parse::file_name(&path), &path) {
                found.push(Found::path(path, AppFileKind::Shortcut, confidence));
            }
        }
    }
}

/// `%TEMP%` entries named after the product (or starting with a long product name).
fn temp(m: &Matcher, found: &mut Vec<Found>) {
    let Some(root) = known::env("TEMP") else {
        return;
    };
    for (name, path, _) in known::entries(&root) {
        if known::is_broad_dir(&path) || m.peers.overlaps(&path) {
            continue;
        }
        let normalized = parse::normalize_name(&name);
        if m.product(&name) {
            if let Some(confidence) = m.name_confidence(&name, &[], Confidence::High) {
                found.push(Found::path(path, AppFileKind::Cache, confidence));
            }
        } else if m
            .products
            .iter()
            .any(|p| p.chars().count() >= 5 && normalized.starts_with(p.as_str()))
        {
            push_path(
                found,
                path,
                AppFileKind::Cache,
                &[Clue::NamePrefix],
                Confidence::High,
            );
        }
    }
}

/// Windows Error Reporting folders of the app's programs.
fn wer_reports(m: &Matcher, found: &mut Vec<Found>) {
    if m.exes.is_empty() {
        return;
    }
    for var in ["LOCALAPPDATA", "ProgramData"] {
        for sub in ["ReportArchive", "ReportQueue"] {
            let Some(root) = known::env_join(var, &format!("Microsoft\\Windows\\WER\\{sub}"))
            else {
                continue;
            };
            for (name, path, is_dir) in known::entries(&root) {
                let ours = is_dir
                    && clues::wer_program(&name)
                        .is_some_and(|p| m.exes.iter().any(|e| clues::wer_matches(p, e)));
                if ours {
                    push_path(
                        found,
                        path,
                        AppFileKind::Logs,
                        &[Clue::Reference],
                        Confidence::High,
                    );
                }
            }
        }
    }
}

/// `%WINDIR%\Prefetch\<PROGRAM>-<hash>.pf` of the app's programs (admin, Low).
fn prefetch(m: &Matcher, found: &mut Vec<Found>) {
    if m.exes.is_empty() {
        return;
    }
    let root = format!("{}\\Prefetch", known::windir());
    for (name, path, is_dir) in known::entries(&root) {
        if !is_dir && clues::prefetch_program(&name).is_some_and(|p| m.exes.contains(&p)) {
            push_path(
                found,
                path,
                AppFileKind::Cache,
                &[Clue::Reference],
                Confidence::Low,
            );
        }
    }
}

/// Burn bundle caches and cached MSI packages (`%ProgramData%\Package Cache\{code}…`)
/// once the product is gone: before that they are the uninstaller's own files.
/// (`%WINDIR%\Installer` is left alone: the removal guard protects the Windows folder.)
fn package_cache(app: &AppRecord, found: &mut Vec<Found>) {
    let Some(cache) = known::env_join("ProgramData", "Package Cache") else {
        return;
    };
    let registered = app
        .detail
        .keys
        .iter()
        .any(|(hive, path)| reg::exists(*hive, path));
    if !registered
        && let Some(program) = app
            .detail
            .uninstall
            .as_deref()
            .and_then(|u| parse::split_command(&reg::expand(u), known::is_file))
            .map(|line| line.exe)
        && parse::path_within(&program, &cache)
        && let Some(bundle) = parse::parent_dir(&program)
        && parse::parent_dir(bundle).is_some_and(|p| p.eq_ignore_ascii_case(&cache))
        && clues::package_cache_code(parse::file_name(bundle)).is_some()
    {
        push_path(
            found,
            bundle.to_owned(),
            AppFileKind::Receipt,
            &[Clue::Reference],
            Confidence::High,
        );
    }
    let Some(code) = app.detail.msi_code.as_deref() else {
        return;
    };
    if leftovers::msi_registered(code) {
        return;
    }
    for (name, path, is_dir) in known::entries(&cache) {
        if is_dir && clues::package_cache_code(&name).is_some_and(|c| c.eq_ignore_ascii_case(code))
        {
            push_path(
                found,
                path,
                AppFileKind::Receipt,
                &[Clue::Reference],
                Confidence::High,
            );
        }
    }
}
