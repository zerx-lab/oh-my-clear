//! Installed `.app` bundles: discovery in the application folders, `Info.plist` facts,
//! App Store and Homebrew origin, size, running state, last use, icon.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use omc_proto::apps::{AppInfo, AppSource};
use omc_proto::jobs::Phase;
use omc_proto::settings::CleanSettings;
use omc_scan::{JobCtx, Walker};

use super::plist_util::{self, BundleInfo};
use super::{AppDetail, access, icons, par_map};
use crate::cmd;
use crate::{AppRecord, Error, Result};

/// Apps in `/Applications` that ship with macOS (the rest of the OS apps live in
/// `/System/Applications`).
const OS_APPS: &[&str] = &["com.apple.Safari"];

/// Homebrew prefixes (Apple silicon, Intel).
const BREW_PREFIXES: &[&str] = &["/opt/homebrew", "/usr/local"];

/// `mdls` over many paths.
const MDLS_TIMEOUT: Duration = Duration::from_secs(20);

/// Paths per `mdls` call (argument list limit).
const MDLS_CHUNK: usize = 200;

/// A discovered bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Bundle {
    /// Where it is listed (may be a symlink, e.g. `/Applications/Safari.app`).
    pub(super) path: PathBuf,
    /// The bundle directory itself (symlinks resolved).
    pub(super) real: PathBuf,
    /// Found in an OS app folder.
    pub(super) system: bool,
}

/// Application folders: (folder, OS apps).
fn roots() -> Vec<(PathBuf, bool)> {
    let mut out = vec![(PathBuf::from("/Applications"), false)];
    if let Some(home) = omc_scan::paths::home() {
        out.push((home.join("Applications"), false));
    }
    out.push((PathBuf::from("/System/Applications"), true));
    out
}

fn is_app_name(name: &std::ffi::OsStr) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("app"))
}

/// Every `.app` directly in the application folders or one folder below
/// (`Utilities`, `Setapp`, vendor folders, `Chrome Apps`).
pub(super) fn discover(ctx: &JobCtx) -> Vec<Bundle> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (root, system) in roots() {
        collect(&root, system, 2, ctx, &mut seen, &mut out);
    }
    out
}

fn collect(
    dir: &Path,
    system: bool,
    depth: u8,
    ctx: &JobCtx,
    seen: &mut HashSet<PathBuf>,
    out: &mut Vec<Bundle>,
) {
    if ctx.is_cancelled() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        ctx.add_items(1);
        if is_app_name(&entry.file_name()) {
            let real = if kind.is_symlink() {
                match std::fs::canonicalize(&path) {
                    Ok(r) if r.is_dir() => r,
                    _ => continue,
                }
            } else if kind.is_dir() {
                path.clone()
            } else {
                continue;
            };
            if seen.insert(real.clone()) {
                let system = system || real.starts_with("/System/");
                out.push(Bundle { path, real, system });
            }
        } else if kind.is_dir() && depth > 1 {
            collect(&path, system, depth.saturating_sub(1), ctx, seen, out);
        }
    }
}

/// A bundle with its `Info.plist` facts (bundles without one are dropped).
pub(super) fn with_info(bundles: Vec<Bundle>, threads: usize) -> Vec<(Bundle, BundleInfo)> {
    let infos = par_map(&bundles, threads, |b| plist_util::bundle_info(&b.real));
    bundles
        .into_iter()
        .zip(infos)
        .filter_map(|(b, info)| Some((b, info?)))
        .collect()
}

/// Cheap inventory (no sizes, icons or processes): what other apps are installed.
pub(super) fn installed(threads: usize, ctx: &JobCtx) -> Vec<(Bundle, BundleInfo)> {
    with_info(discover(ctx), threads)
}

/// A Homebrew cask.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Cask {
    /// Cask token (`brew uninstall --cask <token>`).
    pub(super) token: String,
    /// `zap` paths (`~` unexpanded, globs kept).
    pub(super) zap: Vec<String>,
}

/// Installed casks by the lowercase file name of the `.app` they install.
pub(super) fn casks() -> HashMap<String, Cask> {
    let mut out = HashMap::new();
    for prefix in BREW_PREFIXES {
        let room = Path::new(prefix).join("Caskroom");
        let Ok(entries) = std::fs::read_dir(&room) else {
            continue;
        };
        for entry in entries.flatten() {
            let token = entry.file_name().to_string_lossy().into_owned();
            if token.starts_with('.') {
                continue;
            }
            let dir = entry.path();
            let (mut apps, zap) = read_receipt(&dir.join(".metadata").join("INSTALL_RECEIPT.json"));
            apps.extend(version_apps(&dir));
            let cask = Cask {
                token: token.clone(),
                zap,
            };
            for app in apps {
                out.entry(app.to_lowercase())
                    .or_insert_with(|| cask.clone());
            }
        }
    }
    out
}

/// `.app` names left in the cask's version folders.
fn version_apps(cask_dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(versions) = std::fs::read_dir(cask_dir) else {
        return out;
    };
    for version in versions.flatten() {
        if version.file_name().as_encoded_bytes().first() == Some(&b'.') {
            continue;
        }
        let Ok(files) = std::fs::read_dir(version.path()) else {
            continue;
        };
        out.extend(
            files
                .flatten()
                .map(|f| f.file_name())
                .filter(|n| is_app_name(n))
                .map(|n| n.to_string_lossy().into_owned()),
        );
    }
    out
}

/// App names and zap paths from a cask's `INSTALL_RECEIPT.json`.
fn read_receipt(path: &Path) -> (Vec<String>, Vec<String>) {
    match std::fs::read(path) {
        Ok(bytes) => parse_receipt(&bytes),
        Err(err) => {
            tracing::debug!(%err, path = %path.display(), "no cask receipt");
            (Vec::new(), Vec::new())
        }
    }
}

/// [`read_receipt`] on the file's bytes.
pub(super) fn parse_receipt(bytes: &[u8]) -> (Vec<String>, Vec<String>) {
    let mut apps = Vec::new();
    let mut zap = Vec::new();
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return (apps, zap);
    };
    let artifacts = json
        .get("uninstall_artifacts")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    for artifact in artifacts {
        if let Some(list) = artifact.get("app").and_then(serde_json::Value::as_array) {
            let mut name: Option<String> = None;
            for v in list {
                if let Some(s) = v.as_str() {
                    name = Path::new(s)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned());
                } else if let Some(t) = v.get("target").and_then(serde_json::Value::as_str) {
                    name = Path::new(t)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned());
                }
            }
            apps.extend(name);
        }
        if let Some(stanzas) = artifact.get("zap").and_then(serde_json::Value::as_array) {
            for stanza in stanzas {
                for key in ["trash", "delete", "rmdir"] {
                    match stanza.get(key) {
                        Some(serde_json::Value::String(s)) => zap.push(s.clone()),
                        Some(serde_json::Value::Array(list)) => zap.extend(
                            list.iter()
                                .filter_map(serde_json::Value::as_str)
                                .map(str::to_owned),
                        ),
                        _ => {}
                    }
                }
            }
        }
    }
    (apps, zap)
}

/// `brew` executable, when Homebrew is installed.
pub(super) fn brew() -> Option<PathBuf> {
    BREW_PREFIXES
        .iter()
        .map(|p| Path::new(p).join("bin").join("brew"))
        .find(|p| p.is_file())
}

/// The outermost `.app` containing `exe`, lowercase (for case-insensitive lookup).
pub(super) fn outer_app(exe: &Path) -> Option<String> {
    let mut acc = PathBuf::new();
    for comp in exe.components() {
        acc.push(comp);
        if is_app_name(comp.as_os_str()) {
            return Some(acc.to_string_lossy().to_lowercase());
        }
    }
    None
}

/// Outermost app bundles of running processes (lowercase paths).
pub(super) fn running_bundles(procs: &[omc_scan::procs::Process]) -> HashSet<String> {
    procs
        .iter()
        .filter_map(|p| p.exe.as_deref().and_then(outer_app))
        .collect()
}

fn lower(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

/// Whether `bundle` is running, given [`running_bundles`].
pub(super) fn is_running(bundle: &Bundle, running: &HashSet<String>) -> bool {
    running.contains(&lower(&bundle.path)) || running.contains(&lower(&bundle.real))
}

/// Publisher from a copyright line (`Copyright © 2024 Foo Inc. All rights reserved.` →
/// `Foo Inc`).
pub(super) fn publisher_from_copyright(text: &str) -> Option<String> {
    // Licences and contact addresses are not publishers.
    let lowered = text.to_lowercase();
    if lowered.contains("license") || lowered.contains("licence") || text.contains('@') {
        return None;
    }
    let text = text
        .replace(['\u{a0}', '\u{3000}'], " ")
        .replace('（', "(")
        .replace('）', ")");
    let mut s = text.trim();
    loop {
        let before = s;
        for prefix in ["copyright", "©", "(c)"] {
            if let Some((head, tail)) = s.split_at_checked(prefix.len())
                && head.eq_ignore_ascii_case(prefix)
            {
                s = tail;
            }
        }
        s = s.trim_start_matches(|c: char| {
            c.is_ascii_digit() || c.is_whitespace() || matches!(c, '-' | ',' | '–' | '.')
        });
        if s == before {
            break;
        }
    }
    let lowered = s.to_ascii_lowercase();
    if let Some(at) = lowered.find("all rights reserved")
        && let Some((head, _)) = s.split_at_checked(at)
    {
        s = head;
    }
    let s = s.trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '.' | ',' | ';'));
    (!s.is_empty() && s.chars().count() <= 80).then(|| s.to_owned())
}

/// One Spotlight attribute of each path (`None` where unset), one `mdls` per chunk.
fn mdls(paths: &[&Path], attr: &str) -> Vec<Option<String>> {
    let mut out = Vec::with_capacity(paths.len());
    for chunk in paths.chunks(MDLS_CHUNK) {
        let mut cmd = cmd::command("/usr/bin/mdls");
        cmd.args(["-name", attr, "-raw"]);
        cmd.args(chunk);
        let values = match cmd::run_command("mdls", &mut cmd, MDLS_TIMEOUT) {
            Ok(o) => parse_mdls(&o.stdout, chunk.len()),
            Err(err) => {
                tracing::debug!(%err, "mdls failed");
                None
            }
        };
        out.extend(values.unwrap_or_else(|| vec![None; chunk.len()]));
    }
    out
}

/// NUL-separated `mdls -raw` values (`(null)` → `None`); `None` when the count does not
/// match.
pub(super) fn parse_mdls(stdout: &str, expected: usize) -> Option<Vec<Option<String>>> {
    let values: Vec<Option<String>> = stdout
        .split('\0')
        .take(expected)
        .map(|v| {
            let v = v.trim();
            (!v.is_empty() && v != "(null)").then(|| v.to_owned())
        })
        .collect();
    (values.len() == expected).then_some(values)
}

/// On-disk size of each bundle: Spotlight's `kMDItemPhysicalSize` (what Finder shows,
/// instant), measured with the walker only where Spotlight has no value.
fn bundle_sizes(bundles: &[Bundle], walker: &Walker, ctx: &JobCtx) -> Vec<Option<u64>> {
    let reals: Vec<&Path> = bundles.iter().map(|b| b.real.as_path()).collect();
    let mut sizes: Vec<Option<u64>> = mdls(&reals, "kMDItemPhysicalSize")
        .into_iter()
        .map(|v| v.and_then(|s| s.parse().ok()))
        .collect();
    let missing: Vec<usize> = sizes
        .iter()
        .enumerate()
        .filter_map(|(i, s)| s.is_none().then_some(i))
        .collect();
    if missing.is_empty() {
        return sizes;
    }
    let paths: Vec<PathBuf> = missing
        .iter()
        .filter_map(|&i| bundles.get(i).map(|b| b.real.clone()))
        .collect();
    let measured = walker.measure_all(&paths, ctx);
    for (&i, m) in missing.iter().zip(measured) {
        if let Some(slot) = sizes.get_mut(i) {
            *slot = (!m.missing).then_some(m.bytes);
        }
    }
    sizes
}

/// `2026-09-20 13:36:29 +0000` → Unix seconds.
pub(super) fn parse_mdls_date(text: &str) -> Option<i64> {
    let mut parts = text.trim().split(' ');
    let (date, time) = (parts.next()?, parts.next()?);
    let zone = parts.next().unwrap_or("+0000");
    let mut ymd = date.split('-').map(str::parse::<i64>);
    let (year, month, day) = (ymd.next()?.ok()?, ymd.next()?.ok()?, ymd.next()?.ok()?);
    let mut hms = time.split(':').map(str::parse::<i64>);
    let (hh, mm, ss) = (hms.next()?.ok()?, hms.next()?.ok()?, hms.next()?.ok()?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (sign, digits) = match zone.split_at_checked(1)? {
        ("+", d) => (1_i64, d),
        ("-", d) => (-1_i64, d),
        _ => return None,
    };
    let (zh, zm) = digits.split_at_checked(2)?;
    let offset = zh
        .parse::<i64>()
        .ok()?
        .checked_mul(3600)?
        .checked_add(zm.parse::<i64>().ok()?.checked_mul(60)?)?
        .checked_mul(sign)?;
    let days = days_from_civil(year, month, day)?;
    days.checked_mul(86_400)?
        .checked_add(hh.checked_mul(3600)?)?
        .checked_add(mm.checked_mul(60)?)?
        .checked_add(ss)?
        .checked_sub(offset)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> Option<i64> {
    let y = if m <= 2 { y.checked_sub(1)? } else { y };
    let era = y
        .checked_sub(if y >= 0 { 0 } else { 399 })?
        .checked_div(400)?;
    let yoe = y.checked_sub(era.checked_mul(400)?)?;
    let mp = if m > 2 {
        m.checked_sub(3)?
    } else {
        m.checked_add(9)?
    };
    let doy = mp
        .checked_mul(153)?
        .checked_add(2)?
        .checked_div(5)?
        .checked_add(d)?
        .checked_sub(1)?;
    let doe = yoe
        .checked_mul(365)?
        .checked_add(yoe.checked_div(4)?)?
        .checked_sub(yoe.checked_div(100)?)?
        .checked_add(doy)?;
    era.checked_mul(146_097)?
        .checked_add(doe)?
        .checked_sub(719_468)
}

/// Everything about one bundle that needs file-system access, gathered in parallel.
struct Facts {
    info: BundleInfo,
    mas: bool,
    installed: Option<i64>,
    needs_admin: bool,
    icon: Option<String>,
}

fn facts(bundle: &Bundle, icon_cache: Option<&Path>) -> Option<Facts> {
    let info = plist_util::bundle_info(&bundle.real)?;
    let mas = bundle
        .real
        .join("Contents")
        .join("_MASReceipt")
        .join("receipt")
        .is_file();
    let installed = std::fs::metadata(&bundle.real)
        .and_then(|m| m.created())
        .ok()
        .map(omc_scan::paths::unix_secs);
    let needs_admin = mas || access::needs_admin(&bundle.path);
    let icon = icon_cache.and_then(|cache| {
        icons::icon_for(
            cache,
            &bundle.real,
            info.id.as_deref(),
            info.icon.as_deref(),
        )
    });
    Some(Facts {
        info,
        mas,
        installed,
        needs_admin,
        icon,
    })
}

/// The installed apps (ids unassigned; the facade sorts and numbers them).
pub(super) fn list_apps(
    settings: &CleanSettings,
    walker: &Walker,
    ctx: &JobCtx,
) -> Result<Vec<AppRecord>> {
    ctx.set_phase(Phase::Scanning);
    let mut bundles = discover(ctx);
    if !settings.show_system_apps {
        // System apps are filtered by the facade anyway; skip their work early.
        bundles.retain(|b| !b.system);
    }
    if ctx.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let icon_cache = icons::cache_dir().filter(|dir| match std::fs::create_dir_all(dir) {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!(%err, dir = %dir.display(), "icon cache unavailable");
            false
        }
    });
    let threads = walker.options().threads;
    let facts = par_map(&bundles, threads, |b| facts(b, icon_cache.as_deref()));
    let casks = casks();
    let running = running_bundles(&omc_scan::procs::running());
    let paths: Vec<&Path> = bundles.iter().map(|b| b.path.as_path()).collect();
    let used: Vec<Option<i64>> = mdls(&paths, "kMDItemLastUsedDate")
        .into_iter()
        .map(|v| v.as_deref().and_then(parse_mdls_date))
        .collect();
    if ctx.is_cancelled() {
        return Err(Error::Cancelled);
    }
    ctx.set_phase(Phase::Measuring);
    let sizes = bundle_sizes(&bundles, walker, ctx);
    let mut out = Vec::with_capacity(bundles.len());
    for (((bundle, facts), used), bytes) in bundles.into_iter().zip(facts).zip(used).zip(sizes) {
        let Some(facts) = facts else { continue };
        let file_name = bundle
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let cask = casks.get(&file_name).cloned();
        let system = bundle.system
            || facts
                .info
                .id
                .as_deref()
                .is_some_and(|id| OS_APPS.contains(&id));
        let source = if facts.mas {
            AppSource::MacAppStore
        } else if cask.is_some() {
            AppSource::Homebrew
        } else {
            AppSource::MacBundle
        };
        ctx.add_bytes(bytes.unwrap_or(0));
        let info = AppInfo {
            id: 0,
            name: facts.info.name.clone(),
            version: facts.info.version.clone(),
            publisher: facts
                .info
                .copyright
                .as_deref()
                .and_then(publisher_from_copyright),
            ident: facts.info.id.clone(),
            location: Some(bundle.path.display().to_string()),
            bytes,
            source,
            system,
            running: is_running(&bundle, &running),
            last_used: used,
            installed: facts.installed,
            icon: facts.icon,
            needs_admin: facts.needs_admin,
        };
        out.push(AppRecord {
            info,
            detail: AppDetail {
                bundle: bundle.path,
                real: bundle.real,
                bundle_id: facts.info.id,
                executable: facts.info.executable,
                cask,
            },
        });
    }
    Ok(out)
}

/// Bundle ids of the bundles an app embeds (login items, XPC services, plug-ins,
/// privileged helpers, system extensions, helper apps).
pub(super) fn embedded_ids(bundle: &Path) -> Vec<String> {
    const DIRS: &[&[&str]] = &[
        &["Library", "LoginItems"],
        &["Library", "LaunchServices"],
        &["Library", "SystemExtensions"],
        &["Library", "QuickLook"],
        &["Library", "Spotlight"],
        &["XPCServices"],
        &["PlugIns"],
        &["Helpers"],
        &["Frameworks"],
    ];
    const EXTS: &[&str] = &[
        "app",
        "xpc",
        "appex",
        "systemextension",
        "qlgenerator",
        "mdimporter",
        "plugin",
        "bundle",
    ];
    let contents = bundle.join("Contents");
    let mut out = Vec::new();
    for rel in DIRS {
        let dir = rel.iter().fold(contents.clone(), |p, c| p.join(c));
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let helpers = rel.last() == Some(&"LaunchServices");
        for entry in entries.flatten() {
            let path = entry.path();
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if EXTS.contains(&ext.as_str()) {
                // Frameworks hold many bundles; only helper apps carry ids of interest.
                if rel == &["Frameworks"] && ext != "app" {
                    continue;
                }
                if let Some(id) = plist_util::bundle_info(&path).and_then(|i| i.id) {
                    out.push(id);
                }
            } else if helpers && entry.file_type().is_ok_and(|t| t.is_file()) {
                // Privileged helper tools are named by their launchd label.
                out.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "omc-mac-inv-{tag}-{}-{}",
                std::process::id(),
                omc_scan::paths::now_secs()
            ));
            let _ignored = std::fs::remove_dir_all(&dir);
            assert!(std::fs::create_dir_all(&dir).is_ok(), "create temp dir");
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ignored = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_plist(path: &Path, pairs: &[(&str, &str)]) {
        let mut d = plist::Dictionary::new();
        for (k, v) in pairs {
            d.insert((*k).to_owned(), plist::Value::from(*v));
        }
        if let Some(parent) = path.parent() {
            assert!(std::fs::create_dir_all(parent).is_ok(), "plist parent");
        }
        assert!(
            plist::Value::Dictionary(d).to_file_xml(path).is_ok(),
            "write plist"
        );
    }

    #[test]
    fn fixture_bundle_facts_and_embedded_ids() {
        let tmp = TempDir::new("bundle");
        let app = tmp.0.join("Foo Bar.app");
        write_plist(
            &app.join("Contents").join("Info.plist"),
            &[
                ("CFBundleIdentifier", "com.foo.bar"),
                ("CFBundleName", "Foo Bar"),
                ("CFBundleShortVersionString", "1.2.3"),
                ("CFBundleExecutable", "foobar"),
                (
                    "NSHumanReadableCopyright",
                    "Copyright © 2020-2024 Foo Inc. All rights reserved.",
                ),
            ],
        );
        write_plist(
            &app.join("Contents/Library/LoginItems/Foo Launcher.app/Contents/Info.plist"),
            &[("CFBundleIdentifier", "com.foo.bar.launcher")],
        );
        write_plist(
            &app.join("Contents/PlugIns/Share.appex/Contents/Info.plist"),
            &[("CFBundleIdentifier", "com.foo.bar.share")],
        );
        let helper = app.join("Contents/Library/LaunchServices/com.foo.bar.helper");
        if let Some(parent) = helper.parent() {
            assert!(std::fs::create_dir_all(parent).is_ok(), "helper dir");
        }
        assert!(std::fs::write(&helper, b"bin").is_ok(), "helper file");

        let bundle = Bundle {
            path: app.clone(),
            real: app.clone(),
            system: false,
        };
        let facts = facts(&bundle, None);
        assert!(facts.is_some(), "Info.plist is read");
        let Some(facts) = facts else { return };
        assert_eq!(facts.info.id.as_deref(), Some("com.foo.bar"), "bundle id");
        assert_eq!(facts.info.name, "Foo Bar", "name");
        assert_eq!(facts.info.version.as_deref(), Some("1.2.3"), "version");
        assert_eq!(
            facts.info.executable.as_deref(),
            Some("foobar"),
            "executable"
        );
        assert!(!facts.mas, "no App Store receipt");
        assert!(!facts.needs_admin, "own temp folder");
        assert_eq!(
            embedded_ids(&app),
            vec![
                "com.foo.bar.helper".to_owned(),
                "com.foo.bar.launcher".to_owned(),
                "com.foo.bar.share".to_owned()
            ],
            "login item, privileged helper and plug-in ids"
        );
        let mut seen = HashSet::new();
        let mut found = Vec::new();
        collect(&tmp.0, false, 2, &JobCtx::new(), &mut seen, &mut found);
        assert_eq!(found.len(), 1, "nested .app bundles are not listed");
    }

    #[test]
    fn copyright_lines_become_publishers() {
        assert_eq!(
            publisher_from_copyright("Copyright © 2020-2024 Foo Inc. All rights reserved.")
                .as_deref(),
            Some("Foo Inc"),
            "prefix, years and rights stripped"
        );
        assert_eq!(
            publisher_from_copyright("© 2019 Bar Labs").as_deref(),
            Some("Bar Labs"),
            "symbol"
        );
        assert_eq!(publisher_from_copyright("2024"), None, "nothing left");
        assert_eq!(publisher_from_copyright("MIT License"), None, "licence");
        assert_eq!(
            publisher_from_copyright("（c）2023 Foo").as_deref(),
            Some("Foo"),
            "full-width parentheses"
        );
        assert_eq!(
            publisher_from_copyright("Tencent\u{a0}Inc.\u{a0}All\u{a0}Rights\u{a0}Reserved")
                .as_deref(),
            Some("Tencent Inc"),
            "non-breaking spaces"
        );
    }

    #[test]
    fn mdls_dates_parse() {
        assert_eq!(
            parse_mdls_date("1970-01-02 00:00:10 +0000"),
            Some(86_410),
            "epoch arithmetic"
        );
        assert_eq!(
            parse_mdls_date("2024-02-29 12:00:00 +0100"),
            Some(1_709_204_400),
            "leap day with offset"
        );
        assert_eq!(parse_mdls_date("(null)"), None, "missing value");
        assert_eq!(
            parse_mdls("1970-01-01 00:00:01 +0000\0(null)", 2),
            Some(vec![Some("1970-01-01 00:00:01 +0000".to_owned()), None]),
            "NUL separated, (null) is unset"
        );
        assert_eq!(parse_mdls("", 2), None, "count mismatch");
    }

    #[test]
    fn cask_receipts_name_apps_and_zap_paths() {
        let json = br#"{"uninstall_artifacts":[
            {"uninstall":[{"quit":"com.x"}]},
            {"app":["Hidden Bar.app"]},
            {"app":["Foo.app",{"target":"Renamed.app"}]},
            {"zap":[{"trash":["~/Library/Containers/com.x","~/Library/Caches/com.x"],"delete":"~/x"}]}
        ]}"#;
        let (apps, zap) = parse_receipt(json);
        assert_eq!(apps, vec!["Hidden Bar.app", "Renamed.app"], "targets win");
        assert_eq!(zap.len(), 3, "trash and delete entries");
        assert_eq!(
            parse_receipt(b"not json"),
            (Vec::new(), Vec::new()),
            "garbage"
        );
    }

    #[test]
    fn outer_app_is_the_outermost_bundle() {
        let exe = Path::new(
            "/Applications/Google Chrome.app/Contents/Frameworks/X.framework/Helpers/H.app/Contents/MacOS/H",
        );
        assert_eq!(
            outer_app(exe).as_deref(),
            Some("/applications/google chrome.app"),
            "helper processes belong to the outer app"
        );
        assert_eq!(
            outer_app(Path::new("/usr/bin/true")),
            None,
            "not in a bundle"
        );
    }
}
