//! Leftovers of uninstalled apps: Library entries named by a bundle id that no installed
//! app, running process or live launchd job claims; launchd plists whose program is gone;
//! privileged helpers without a launchd plist.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use omc_proto::jobs::{Denied, Location, Phase, SpecialAction};
use omc_proto::junk::{JunkGroup, JunkItem, JunkKind, JunkReport, Safety};
use omc_proto::settings::CleanSettings;
use omc_scan::{JobCtx, Scanned, Target, Walker};

use super::ident::{self, strip_suffixes};
use super::{access, attribution, files, inventory, par_map, plist_util, startup};
use crate::cmd;
use crate::{Error, Result};

/// `mdfind` bound.
const MDFIND_TIMEOUT: Duration = Duration::from_secs(20);

/// First components that make a name look like a reverse-DNS bundle id.
const TLDS: &[&str] = &[
    "ai", "app", "at", "au", "be", "biz", "br", "ca", "cc", "ch", "cn", "co", "com", "cz", "de",
    "dev", "dk", "edu", "es", "eu", "fi", "fm", "fr", "gg", "gov", "hk", "im", "in", "info", "io",
    "is", "it", "jp", "kr", "li", "ly", "me", "mx", "net", "nl", "no", "nz", "one", "org", "pl",
    "pro", "pt", "ru", "se", "sg", "sh", "so", "studio", "tech", "to", "tools", "tv", "tw", "uk",
    "us", "vc", "xyz",
];

/// Prefixes of ids that belong to macOS services outside Apple's namespace (Apple's own
/// names and shared frameworks are [`attribution::is_apple`] / [`attribution::is_shared`]).
const SYSTEM_PREFIXES: &[&str] = &["org.cups.", "com.openssh.", "org.openbsd.", "org.ntp."];

/// `name` looks like a bundle id: `tld.vendor.more…` (3+ components, letters in the
/// later ones); returned lowercase.
pub(super) fn bundle_id_like(name: &str) -> Option<String> {
    let parts: Vec<&str> = name.split('.').collect();
    if parts.len() < 3 {
        return None;
    }
    let first = parts.first()?;
    if !TLDS.contains(first) {
        return None;
    }
    let valid = parts.iter().skip(1).all(|p| {
        !p.is_empty()
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            && p.bytes().any(|b| b.is_ascii_alphabetic())
    });
    valid.then(|| name.to_lowercase())
}

/// The id an entry of a Library folder stands for (`TEAM.id`, `group.id`, `id.plist`…).
pub(super) fn entry_id(name: &str) -> Option<String> {
    let stem = strip_suffixes(name);
    let stem = ident::strip_team(stem).map_or(stem, |(_, rest)| rest);
    let stem = stem.strip_prefix("group.").unwrap_or(stem);
    bundle_id_like(stem)
}

/// What counts as installed.
#[derive(Debug, Default)]
pub(super) struct Known {
    ids: HashSet<String>,
    vendors: HashSet<String>,
}

impl Known {
    pub(super) fn add(&mut self, id: &str) {
        let id = id.trim().to_lowercase();
        if id.is_empty() {
            return;
        }
        if let Some(v) = ident::vendor(&id) {
            self.vendors.insert(v);
        }
        self.ids.insert(id);
    }

    /// `id` belongs to nothing installed.
    pub(super) fn is_orphan(&self, id: &str) -> bool {
        if attribution::is_apple(id)
            || attribution::is_shared(id)
            || SYSTEM_PREFIXES.iter().any(|w| id.starts_with(w))
        {
            return false;
        }
        if self.ids.contains(id) {
            return false;
        }
        // Any installed id that is a prefix component of this one (`com.x.app.helper`),
        // unless the rest names a sibling product (`com.x.app.beta` is another app).
        let mut prefix = String::new();
        let mut parts = id.split('.').peekable();
        while let Some(part) = parts.next() {
            if !prefix.is_empty() {
                prefix.push('.');
            }
            prefix.push_str(part);
            let sibling = parts
                .peek()
                .is_some_and(|next| attribution::is_sibling(next));
            if self.ids.contains(&prefix) && !sibling {
                return false;
            }
        }
        // Same vendor as something installed: too risky to call it orphaned. Generic
        // vendors (`com.github`) are never recorded and are judged by the id checks above.
        ident::vendor(id).is_none_or(|v| !self.vendors.contains(&v))
    }
}

/// Every app bundle Spotlight knows (anywhere on disk).
fn spotlight_apps() -> Vec<PathBuf> {
    match cmd::run(
        "/usr/bin/mdfind",
        &["kMDItemContentType == 'com.apple.application-bundle'"],
        MDFIND_TIMEOUT,
    )
    .and_then(|o| o.ok("mdfind"))
    {
        Ok(text) => text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .collect(),
        Err(err) => {
            tracing::debug!(%err, "mdfind failed");
            Vec::new()
        }
    }
}

/// Ids of installed apps (folders + Spotlight) and their embedded bundles, running
/// processes and launchd jobs whose program exists.
fn known(threads: usize, ctx: &JobCtx) -> Known {
    let mut known = Known::default();
    let mut bundles: Vec<PathBuf> = inventory::discover(ctx)
        .into_iter()
        .map(|b| b.real)
        .collect();
    bundles.extend(spotlight_apps());
    bundles.sort();
    bundles.dedup();
    let ids = par_map(&bundles, threads, |b| {
        let mut ids = inventory::embedded_ids(b);
        ids.extend(plist_util::bundle_info(b).and_then(|i| i.id));
        ids
    });
    for id in ids.iter().flatten() {
        known.add(id);
    }
    for proc in omc_scan::procs::running() {
        if let Some(bundle) = proc.exe.as_deref().and_then(inventory::outer_app)
            && let Some(id) = plist_util::bundle_info(Path::new(&bundle)).and_then(|i| i.id)
        {
            known.add(&id);
        }
        if bundle_id_like(&proc.name).is_some() {
            known.add(&proc.name);
        }
    }
    for (dir, _) in startup::launch_dirs() {
        for plist in startup::plists(&dir) {
            if let Some(job) = plist_util::launch_job(&plist)
                && !job.target_missing()
            {
                known.add(&job.label);
                for id in &job.associated {
                    known.add(id);
                }
            }
        }
    }
    known
}

/// A Library folder searched for orphans.
struct Area {
    dir: PathBuf,
    /// Recreated by the OS/app (caches, saved state): preselected.
    safe: bool,
    system: bool,
}

fn areas(settings: &CleanSettings) -> Vec<Area> {
    let mut out = Vec::new();
    let user = |dir: PathBuf, safe: bool| Area {
        dir,
        safe,
        system: false,
    };
    if let Some(home) = omc_scan::paths::home() {
        let lib = home.join("Library");
        out.extend([
            user(lib.join("Application Support"), false),
            user(lib.join("Caches"), true),
            user(lib.join("Preferences"), false),
            user(lib.join("Preferences").join("ByHost"), false),
            user(lib.join("Containers"), false),
            user(lib.join("Group Containers"), false),
            user(lib.join("Saved Application State"), true),
            user(lib.join("Logs"), false),
            user(lib.join("HTTPStorages"), false),
            user(lib.join("WebKit"), false),
            user(lib.join("Cookies"), false),
            user(lib.join("Application Scripts"), false),
            user(
                lib.join("Application Support")
                    .join("com.apple.sharedfilelist")
                    .join("com.apple.LSSharedFileList.ApplicationRecentDocuments"),
                true,
            ),
        ]);
    }
    // Per-user caches and temp files (`C`, `T`); `0` holds daemons' state.
    for dir in files::darwin_dirs() {
        if dir.file_name().is_some_and(|n| n != "0") {
            out.push(user(dir, true));
        }
    }
    if settings.include_system {
        let system = |dir: &str, safe: bool| Area {
            dir: PathBuf::from(dir),
            safe,
            system: true,
        };
        out.extend([
            system("/Library/Application Support", false),
            system("/Library/Caches", true),
            system("/Library/Preferences", false),
            system("/Library/Logs", false),
        ]);
    }
    out
}

/// One found orphan before measuring.
#[derive(Debug)]
struct Orphan {
    kind: JunkKind,
    name: String,
    path: PathBuf,
    special: Option<SpecialAction>,
    safety: Safety,
    needs_admin: bool,
}

fn record_denied(dir: &Path, err: &std::io::Error, denied: &mut Vec<Denied>) {
    if err.kind() != std::io::ErrorKind::NotFound {
        denied.push(Denied {
            path: dir.display().to_string(),
            reason: omc_scan::errors::classify(err, dir),
        });
    }
}

fn scan_areas(
    known: &Known,
    areas: &[Area],
    walker: &Walker,
    ctx: &JobCtx,
    out: &mut Vec<Orphan>,
    denied: &mut Vec<Denied>,
) {
    for area in areas {
        if ctx.is_cancelled() {
            return;
        }
        let entries = match std::fs::read_dir(&area.dir) {
            Ok(e) => e,
            Err(err) => {
                record_denied(&area.dir, &err, denied);
                continue;
            }
        };
        for entry in entries.flatten() {
            ctx.add_items(1);
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = entry_id(&name) else {
                continue;
            };
            let path = entry.path();
            if !known.is_orphan(&id) || walker.options().is_excluded(&path) {
                continue;
            }
            out.push(Orphan {
                kind: JunkKind::OrphanFiles,
                name: strip_suffixes(&name).to_owned(),
                needs_admin: area.system || access::needs_admin(&path),
                path,
                special: None,
                safety: if area.safe {
                    Safety::Safe
                } else {
                    Safety::Review
                },
            });
        }
    }
}

/// Launchd plists whose program is gone, and privileged helpers no plist starts.
fn scan_launchd(settings: &CleanSettings, out: &mut Vec<Orphan>) {
    let mut helper_refs: HashSet<PathBuf> = HashSet::new();
    let mut labels: HashSet<String> = HashSet::new();
    for (dir, domain) in startup::launch_dirs() {
        let system = domain != startup::Domain::UserAgent;
        for plist in startup::plists(&dir) {
            let Some(job) = plist_util::launch_job(&plist) else {
                continue;
            };
            labels.insert(job.label.clone());
            if let Some(p) = &job.program {
                helper_refs.insert(PathBuf::from(p));
            }
            if job.label.to_lowercase().starts_with("com.apple.") || !job.target_missing() {
                continue;
            }
            if system && !settings.include_system {
                continue;
            }
            out.push(Orphan {
                kind: JunkKind::OrphanLaunchItems,
                name: job.label.clone(),
                special: Some(SpecialAction::UnloadLaunchJob {
                    label: job.label,
                    plist: plist.display().to_string(),
                }),
                path: plist,
                safety: Safety::Review,
                needs_admin: system && !access::is_root(),
            });
        }
    }
    if !settings.include_system {
        return;
    }
    let helpers = Path::new("/Library/PrivilegedHelperTools");
    let Ok(entries) = std::fs::read_dir(helpers) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let referenced = labels.contains(&name)
            || helper_refs
                .iter()
                .any(|r| omc_scan::paths::is_within(r, &path));
        if referenced || name.to_lowercase().starts_with("com.apple.") {
            continue;
        }
        out.push(Orphan {
            kind: JunkKind::OrphanFiles,
            name,
            path,
            special: None,
            safety: Safety::Review,
            needs_admin: !access::is_root(),
        });
    }
}

pub(super) fn leftovers(
    settings: &CleanSettings,
    walker: &Walker,
    ctx: &JobCtx,
) -> Result<Scanned<JunkReport>> {
    ctx.set_phase(Phase::Scanning);
    let known = known(walker.options().threads, ctx);
    if ctx.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let mut orphans = Vec::new();
    let mut denied = Vec::new();
    scan_areas(
        &known,
        &areas(settings),
        walker,
        ctx,
        &mut orphans,
        &mut denied,
    );
    scan_launchd(settings, &mut orphans);
    if ctx.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let mut seen = HashSet::new();
    orphans.retain(|o| seen.insert(o.path.clone()));

    ctx.set_phase(Phase::Measuring);
    let paths: Vec<PathBuf> = orphans.iter().map(|o| o.path.clone()).collect();
    let sizes = walker.measure_all(&paths, ctx);
    let mut groups: Vec<(JunkKind, Vec<(JunkItem, Target)>)> = Vec::new();
    for (orphan, size) in orphans.into_iter().zip(sizes) {
        if size.missing {
            continue;
        }
        ctx.add_bytes(size.bytes);
        let (location, target) = if let Some(action) = orphan.special {
            let location = Location::Special { action };
            (location.clone(), Target::location(location, size.bytes))
        } else {
            let path = orphan.path.display().to_string();
            let target = Target::path(path.clone(), size.bytes, settings.files_delete);
            (Location::Path { path }, target)
        };
        let item = JunkItem {
            id: 0,
            name: orphan.name,
            location,
            tag: None,
            bytes: size.bytes,
            files: size.files,
            modified: size.newest,
            safety: orphan.safety,
            needs_admin: orphan.needs_admin,
            app_running: false,
            ident: None,
            icon: None,
        };
        let target = target.admin(orphan.needs_admin);
        match groups.iter_mut().find(|(k, _)| *k == orphan.kind) {
            Some((_, items)) => items.push((item, target)),
            None => groups.push((orphan.kind, vec![(item, target)])),
        }
    }
    let mut report = JunkReport {
        groups: Vec::with_capacity(groups.len()),
        denied,
    };
    let mut targets = Vec::new();
    for (_, items) in &mut groups {
        items.sort_by(|a, b| {
            b.0.bytes
                .cmp(&a.0.bytes)
                .then_with(|| a.0.name.cmp(&b.0.name))
        });
    }
    groups.sort_by_key(|(_, items)| {
        std::cmp::Reverse(
            items
                .iter()
                .fold(0_u64, |s, (i, _)| s.saturating_add(i.bytes)),
        )
    });
    for (kind, items) in groups {
        let mut group = JunkGroup {
            kind,
            items: Vec::with_capacity(items.len()),
        };
        for (mut item, target) in items {
            item.id = omc_scan::target::push_target(&mut targets, target);
            group.items.push(item);
        }
        report.groups.push(group);
    }
    Ok(Scanned { report, targets })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_id_shapes() {
        assert_eq!(
            bundle_id_like("com.foo.Bar").as_deref(),
            Some("com.foo.bar"),
            "id"
        );
        assert_eq!(bundle_id_like("com.foo"), None, "two components");
        assert_eq!(bundle_id_like("v1.2.3"), None, "version string");
        assert_eq!(bundle_id_like("update.log.old"), None, "not a tld");
        assert_eq!(bundle_id_like("com.foo.1"), None, "numeric component");
        assert_eq!(bundle_id_like("com..x"), None, "empty component");
        assert_eq!(
            entry_id("ABCDE12345.group.com.foo.bar").as_deref(),
            Some("com.foo.bar"),
            "team + group prefixes"
        );
        assert_eq!(
            entry_id("com.foo.bar.savedState").as_deref(),
            Some("com.foo.bar"),
            "saved state suffix"
        );
    }

    #[test]
    fn orphans_exclude_installed_vendors_and_whitelist() {
        let mut known = Known::default();
        known.add("com.google.Chrome");
        known.add("org.example.tool");
        assert!(!known.is_orphan("com.google.chrome"), "installed");
        assert!(
            !known.is_orphan("com.google.chrome.helper"),
            "sub-id of installed"
        );
        assert!(!known.is_orphan("com.google.keystone"), "same vendor");
        assert!(!known.is_orphan("com.apple.safari"), "apple");
        assert!(
            !known.is_orphan("org.sparkle-project.sparkle"),
            "shared framework"
        );
        assert!(!known.is_orphan("is.workflow.shortcuts"), "Apple Shortcuts");
        assert!(known.is_orphan("com.gone.app"), "unknown vendor");
        let mut solo = Known::default();
        solo.add("com.github.solo.tool");
        assert!(
            !solo.is_orphan("com.github.solo.tool.helper"),
            "part of the app"
        );
        assert!(
            solo.is_orphan("com.github.solo.tool.beta"),
            "a sibling product is judged on its own"
        );
        assert!(
            known.is_orphan("com.github.someone.app"),
            "generic vendors are judged by id only"
        );
    }
}
