//! Everything that belongs to one app: its bundle plus support files, caches, preferences,
//! containers, group containers, saved state, logs and crash reports, web data, plug-ins,
//! launch agents/daemons and privileged helpers, login items, installer receipts and
//! payload, command-line links and iCloud data.
//!
//! Every Library area is listed once (in parallel) and each name is judged by the
//! [`attribution`] engine against the app's [`Profile`] and every other installed app's.
//! Direct evidence — files the running app has open, its installer payload, launchd jobs
//! referencing it, its Homebrew `zap` list — is added on top. Only sizing walks trees.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::thread::ScopedJoinHandle;
use std::time::Duration;

use omc_proto::apps::{AppFile, AppFileKind, AppFilesReport, Confidence};
use omc_proto::jobs::{Location, Phase, SpecialAction};
use omc_proto::settings::CleanSettings;
use omc_scan::{JobCtx, Target, Walker};

use super::attribution::{self, Evidence, NameKind, Place, Profile, Verdict};
use super::sources::{self, Traits};
use super::{access, home, ident, inventory, par_map, plist_util, startup};
use crate::cmd;
use crate::{AppFiles, AppRecord, Error, Result};

/// `getconf` answers quickly; this bounds a wedged system.
const TOOL_TIMEOUT: Duration = Duration::from_secs(15);

/// Installer receipts.
const RECEIPTS: &str = "/private/var/db/receipts";

/// Children of a Chromium profile folder: a folder named like a Chromium-based app that
/// holds at least two of them is its data folder.
const CHROMIUM_MARKERS: &[&str] = &[
    "Local State",
    "Local Storage",
    "Session Storage",
    "Code Cache",
    "GPUCache",
    "IndexedDB",
    "Cookies",
    "Preferences",
    "blob_storage",
];

/// Children of `~/Library` and `/Library` that are standard folders, not vendor folders
/// (lowercase).
const STANDARD_LIBRARY: &[&str] = &[
    "accessibility",
    "accounts",
    "application scripts",
    "application support",
    "apple",
    "assistant",
    "audio",
    "autosave information",
    "biome",
    "caches",
    "calendars",
    "cloudstorage",
    "colorpickers",
    "colorsync",
    "components",
    "containers",
    "cookies",
    "coreanalytics",
    "coremediaio",
    "daemon containers",
    "developer",
    "dictionaries",
    "documentation",
    "extensions",
    "filesystems",
    "fonts",
    "frameworks",
    "group containers",
    "httpstorages",
    "input methods",
    "internet plug-ins",
    "keyboard layouts",
    "keychains",
    "launchagents",
    "launchdaemons",
    "logs",
    "mail",
    "messages",
    "metadata",
    "mobile documents",
    "preferencepanes",
    "preferences",
    "printers",
    "privilegedhelpertools",
    "quicklook",
    "receipts",
    "safari",
    "saved application state",
    "screen savers",
    "security",
    "services",
    "sharing",
    "shortcuts",
    "sounds",
    "spotlight",
    "startupitems",
    "suggestions",
    "systemextensions",
    "translation",
    "updates",
    "webkit",
];

/// One found item before measuring.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Found {
    /// File or folder (for launch jobs: the plist).
    path: PathBuf,
    kind: AppFileKind,
    confidence: Confidence,
    /// Removed through an OS action instead of deleting `path`.
    special: Option<SpecialAction>,
    /// Override of the computed admin requirement.
    admin: Option<bool>,
}

impl Found {
    fn path(path: PathBuf, kind: AppFileKind, confidence: Confidence) -> Self {
        Self {
            path,
            kind,
            confidence,
            special: None,
            admin: None,
        }
    }
}

/// Collects found items, one per path (highest confidence wins).
#[derive(Debug, Default)]
struct Collector {
    items: Vec<Found>,
    by_key: HashMap<String, usize>,
}

impl Collector {
    fn add(&mut self, found: Found) {
        let key = match &found.special {
            Some(SpecialAction::RemoveLoginItem { name }) => format!("login:{name}"),
            Some(SpecialAction::ForgetPackage { id }) => format!("pkg:{id}"),
            _ => found.path.to_string_lossy().to_lowercase(),
        };
        if let Some(existing) = self.by_key.get(&key).and_then(|&i| self.items.get_mut(i)) {
            if found.confidence > existing.confidence {
                existing.confidence = found.confidence;
            }
            // A launch job action supersedes a plain path for the same plist.
            if existing.special.is_none() && found.special.is_some() {
                existing.special = found.special;
                existing.kind = found.kind;
            }
        } else {
            self.by_key.insert(key, self.items.len());
            self.items.push(found);
        }
    }

    /// Adds `path` with the confidence of `verdict`, capped at `cap`.
    fn add_verdict(
        &mut self,
        path: PathBuf,
        kind: AppFileKind,
        verdict: &Verdict,
        cap: Option<Confidence>,
    ) {
        let Some(confidence) = verdict.confidence() else {
            return;
        };
        let confidence = cap.map_or(confidence, |c| confidence.min(c));
        self.add(Found::path(path, kind, confidence));
    }
}

/// How names in an area are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Style {
    /// Plain Library folder.
    Library {
        /// Per-app user data (Electron product names own their folder here).
        user_data: bool,
    },
    /// Per-user temp/cache folder under `/var/folders`.
    Temp,
    /// `~/Library` / `/Library`: vendor folders and named items, standard folders skipped.
    Root,
    /// `Mobile Documents`: `iCloud~com~foo~bar` folders.
    ICloud,
}

/// Where per-app files live and what they are.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Area {
    dir: PathBuf,
    kind: AppFileKind,
    style: Style,
    /// Look one level into vendor folders (`Application Support/Google/Chrome`).
    vendor_folders: bool,
    /// Highest confidence a name here can reach (user data outside Library folders).
    cap: Option<Confidence>,
    /// Name-matched folders may be read to corroborate them (never containers: reading
    /// them prompts for access to other apps' data).
    content: bool,
}

impl Area {
    fn new(dir: PathBuf, kind: AppFileKind, style: Style) -> Self {
        Self {
            dir,
            kind,
            style,
            vendor_folders: false,
            cap: None,
            content: matches!(style, Style::Library { user_data: true }),
        }
    }

    fn readable(mut self) -> Self {
        self.content = true;
        self
    }

    fn vendors(mut self) -> Self {
        self.vendor_folders = true;
        self
    }

    fn capped(mut self, cap: Confidence) -> Self {
        self.cap = Some(cap);
        self
    }

    fn place(&self) -> Place<'static> {
        match self.style {
            Style::Library { user_data } => Place::Library { user_data },
            Style::Temp => Place::Temp,
            Style::Root | Style::ICloud => Place::Library { user_data: false },
        }
    }

    /// The name to judge for a child, `None` when the child is not a candidate.
    fn candidate(&self, name: &str) -> Option<String> {
        match self.style {
            Style::Root => {
                (!STANDARD_LIBRARY.contains(&name.to_lowercase().as_str())).then(|| name.to_owned())
            }
            Style::ICloud => {
                let rest = name.strip_prefix("iCloud~")?;
                Some(rest.replace('~', "."))
            }
            Style::Library { .. } | Style::Temp => Some(name.to_owned()),
        }
    }
}

/// Library folders whose children are named after apps.
fn areas(home: Option<&Path>, include_system: bool, temp_dirs: &[PathBuf]) -> Vec<Area> {
    use AppFileKind as K;
    let lib_style = Style::Library { user_data: false };
    let data_style = Style::Library { user_data: true };
    let mut out = Vec::new();
    if let Some(home) = home {
        let lib = home.join("Library");
        let support = lib.join("Application Support");
        out.extend([
            Area::new(support.clone(), K::Support, data_style).vendors(),
            Area::new(lib.join("Caches"), K::Cache, data_style).vendors(),
            Area::new(lib.join("Logs"), K::Logs, data_style).vendors(),
            Area::new(lib.join("Preferences"), K::Preferences, lib_style),
            Area::new(
                lib.join("Preferences").join("ByHost"),
                K::Preferences,
                lib_style,
            ),
            Area::new(lib.join("Containers"), K::Container, lib_style),
            Area::new(lib.join("Group Containers"), K::GroupContainer, lib_style),
            Area::new(
                lib.join("Saved Application State"),
                K::SavedState,
                lib_style,
            ),
            Area::new(lib.join("HTTPStorages"), K::WebData, lib_style),
            Area::new(lib.join("WebKit"), K::WebData, lib_style),
            Area::new(lib.join("Cookies"), K::WebData, lib_style),
            Area::new(lib.join("Application Scripts"), K::Support, lib_style),
            Area::new(lib.join("Autosave Information"), K::Support, lib_style),
            Area::new(
                support
                    .join("com.apple.sharedfilelist")
                    .join("com.apple.LSSharedFileList.ApplicationRecentDocuments"),
                K::Preferences,
                lib_style,
            ),
            Area::new(lib.join("Internet Plug-Ins"), K::Extension, lib_style),
            Area::new(lib.join("PreferencePanes"), K::Extension, lib_style),
            Area::new(lib.join("Services"), K::Extension, lib_style),
            Area::new(lib.join("QuickLook"), K::Extension, lib_style),
            Area::new(lib.join("Spotlight"), K::Extension, lib_style),
            Area::new(lib.join("Input Methods"), K::Extension, lib_style),
            Area::new(lib.join("Screen Savers"), K::Extension, lib_style),
            Area::new(lib.clone(), K::Support, Style::Root).vendors(),
            Area::new(lib.join("Mobile Documents"), K::Support, Style::ICloud)
                .capped(Confidence::Medium),
        ]);
        out.extend(plugin_formats(&lib.join("Audio").join("Plug-Ins")));
    }
    if include_system {
        let lib = Path::new("/Library");
        out.extend([
            Area::new(lib.join("Application Support"), K::Support, lib_style)
                .vendors()
                .readable(),
            Area::new(lib.join("Caches"), K::Cache, lib_style),
            Area::new(lib.join("Preferences"), K::Preferences, lib_style),
            Area::new(lib.join("Logs"), K::Logs, lib_style)
                .vendors()
                .readable(),
            Area::new(lib.join("PrivilegedHelperTools"), K::LaunchItem, lib_style),
            Area::new(lib.join("Internet Plug-Ins"), K::Extension, lib_style),
            Area::new(lib.join("PreferencePanes"), K::Extension, lib_style),
            Area::new(lib.join("Extensions"), K::Extension, lib_style),
            // Frameworks are shared by a vendor's apps: never preselected by name.
            Area::new(lib.join("Frameworks"), K::Extension, lib_style).capped(Confidence::Medium),
            Area::new(lib.join("Screen Savers"), K::Extension, lib_style),
            Area::new(lib.join("Input Methods"), K::Extension, lib_style),
            Area::new(lib.join("QuickLook"), K::Extension, lib_style),
            Area::new(lib.join("Spotlight"), K::Extension, lib_style),
            Area::new(lib.join("Services"), K::Extension, lib_style),
            Area::new(lib.join("Receipts"), K::Receipt, lib_style),
            Area::new(lib.to_path_buf(), K::Support, Style::Root).vendors(),
            Area::new(PathBuf::from("/Users/Shared"), K::Support, lib_style)
                .capped(Confidence::Medium),
        ]);
        out.extend(plugin_formats(&lib.join("Audio").join("Plug-Ins")));
    }
    out.extend(
        temp_dirs
            .iter()
            .map(|d| Area::new(d.clone(), K::Cache, Style::Temp)),
    );
    out
}

/// Every audio plug-in format folder (HAL, Components, VST, VST3, MAS, CLAP…).
fn plugin_formats(dir: &Path) -> Vec<Area> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| {
            Area::new(
                e.path(),
                AppFileKind::Extension,
                Style::Library { user_data: false },
            )
        })
        .collect()
}

/// `getconf DARWIN_USER_*_DIR`.
pub(super) fn getconf(var: &str) -> Option<PathBuf> {
    match cmd::run("/usr/bin/getconf", &[var], TOOL_TIMEOUT).and_then(|o| o.ok("getconf")) {
        Ok(out) => {
            let path = out.trim();
            (!path.is_empty()).then(|| PathBuf::from(path))
        }
        Err(err) => {
            tracing::debug!(%err, var, "getconf failed");
            None
        }
    }
}

/// The per-user folders under `/var/folders/xx/yyyy/`: `C` (caches), `T` (temp) and `0`
/// (user-level daemons' data), from one `getconf`.
pub(super) fn darwin_dirs() -> Vec<PathBuf> {
    let Some(temp) = getconf("DARWIN_USER_TEMP_DIR") else {
        return Vec::new();
    };
    let Some(base) = temp.parent() else {
        return vec![temp];
    };
    ["C", "T", "0"]
        .iter()
        .map(|d| base.join(d))
        .filter(|d| d.is_dir())
        .collect()
}

/// Crash report folders (files named `<Executable>_<date>…`, `<Executable>-<date>…`).
fn crash_dirs(home: Option<&Path>, include_system: bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = home {
        let lib = home.join("Library");
        let reports = lib.join("Logs").join("DiagnosticReports");
        out.push(reports.join("Retired"));
        out.push(reports);
        out.push(lib.join("Application Support").join("CrashReporter"));
    }
    if include_system {
        let reports = Path::new("/Library/Logs/DiagnosticReports");
        out.push(reports.join("Retired"));
        out.push(reports.to_path_buf());
        out.push(PathBuf::from("/Library/Application Support/CrashReporter"));
    }
    out
}

/// One child of a folder.
#[derive(Debug, Clone)]
struct Entry {
    name: String,
    path: PathBuf,
    is_dir: bool,
}

fn read_names(dir: &Path) -> Vec<Entry> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| Entry {
            name: e.file_name().to_string_lossy().into_owned(),
            path: e.path(),
            is_dir: e.file_type().is_ok_and(|t| t.is_dir()),
        })
        .collect()
}

/// The app and everything it is compared with.
struct Judge<'a> {
    me: &'a Profile,
    traits: &'a Traits,
    others: &'a [Profile],
}

impl Judge<'_> {
    /// Adds the content bonus when a name-matched folder holds the app's files.
    fn corroborate(&self, dir: &Path, verdict: &mut Verdict) {
        if !verdict.identity().is_some_and(Evidence::is_name) {
            return;
        }
        let children = read_names(dir);
        // Files named by the app's ids, or starting with one of its names
        // (`Ollama/ollama.pid`, `ToDesk/ToDeskUninstaller.app`).
        let own = children.iter().any(|c| {
            let v = attribution::score(self.me, &c.name, Place::Library { user_data: false });
            let normalized = ident::normalize(&c.name);
            v.has(Evidence::Id)
                || v.has(Evidence::SubId)
                || v.has(Evidence::GroupId)
                || self.me.names.iter().any(|(n, k)| {
                    *k != NameKind::IdTail && n.len() >= 5 && normalized.starts_with(n.as_str())
                })
        });
        let markers = self.traits.chromium
            && children
                .iter()
                .filter(|c| CHROMIUM_MARKERS.contains(&c.name.as_str()))
                .count()
                >= 2;
        if own || markers {
            verdict.add(Evidence::Content);
        }
    }

    /// Judges the children of one vendor folder; whether any was attributed.
    fn vendor_folder(&self, area: &Area, dir: &Path, word: &str, out: &mut Collector) -> bool {
        let mut any = false;
        for child in read_names(dir) {
            if let Some(v) =
                attribution::attribute(self.me, self.others, &child.name, Place::Vendor(word))
            {
                any = true;
                out.add_verdict(child.path, area.kind, &v, area.cap);
            }
        }
        any
    }
}

/// Judges the children of each area.
fn scan_areas(
    judge: &Judge<'_>,
    areas: &[Area],
    listings: Vec<Vec<Entry>>,
    ctx: &JobCtx,
    out: &mut Collector,
) {
    for (area, entries) in areas.iter().zip(listings) {
        if ctx.is_cancelled() {
            return;
        }
        let place = area.place();
        for entry in entries {
            ctx.add_items(1);
            let Some(name) = area.candidate(&entry.name) else {
                continue;
            };
            let normalized = ident::normalize(&name);
            let verdict = attribution::attribute(judge.me, judge.others, &name, place);
            let whole = verdict
                .as_ref()
                .is_some_and(|v| v.confidence() == Some(Confidence::High));
            if !whole
                && area.vendor_folders
                && entry.is_dir
                && judge.me.has_vendor_word(&normalized)
                && judge.vendor_folder(area, &entry.path, &normalized, out)
            {
                // `Google/Chrome` was attributed; the vendor folder itself is shared.
                continue;
            }
            let Some(mut verdict) = verdict else {
                continue;
            };
            if entry.is_dir && area.content && verdict.confidence() < Some(Confidence::High) {
                judge.corroborate(&entry.path, &mut verdict);
            }
            out.add_verdict(entry.path, area.kind, &verdict, area.cap);
        }
    }
}

/// `/private/var/…` → `/var/…` (`lsof` reports resolved paths, `getconf` the symlinked
/// ones).
fn unprivate(path: &Path) -> PathBuf {
    match path.strip_prefix("/private") {
        Ok(rest) if rest.starts_with("var") || rest.starts_with("tmp") => Path::new("/").join(rest),
        _ => path.to_path_buf(),
    }
}

/// Folders below areas that the running app has files open in.
fn scan_open_files(judge: &Judge<'_>, areas: &[Area], open: &[PathBuf], out: &mut Collector) {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for path in open {
        let path = unprivate(path);
        // The deepest area containing the path (`~/Library/Caches` before `~/Library`).
        let Some(area) = areas
            .iter()
            .filter(|a| path != a.dir && omc_scan::paths::is_within(&path, &a.dir))
            .max_by_key(|a| a.dir.components().count())
        else {
            continue;
        };
        let Ok(rel) = path.strip_prefix(&area.dir) else {
            continue;
        };
        let mut comps = rel.components().map(|c| c.as_os_str().to_string_lossy());
        let Some(first) = comps.next() else { continue };
        let Some(name) = area.candidate(&first) else {
            continue;
        };
        let normalized = ident::normalize(&name);
        let first_root = area.dir.join(first.as_ref());
        // Library-root folders hold unrelated tools' data: there the name must point at
        // the app too.
        let need_name = area.style == Style::Root;
        let mut root = first_root.clone();
        let verdict = if let Some(second) = comps.next()
            && area.vendor_folders
            && judge.me.has_vendor_word(&normalized)
        {
            // A vendor folder holds the vendor's other products: the product folder must
            // be named after the app; else the vendor folder itself must match.
            let child = attribution::attribute_direct(
                judge.me,
                judge.others,
                &second,
                Place::Vendor(normalized.as_str()),
                Evidence::OpenFile,
                true,
            );
            if child.is_some() {
                root = first_root.join(second.as_ref());
                child
            } else {
                attribution::attribute_direct(
                    judge.me,
                    judge.others,
                    &name,
                    area.place(),
                    Evidence::OpenFile,
                    true,
                )
            }
        } else {
            attribution::attribute_direct(
                judge.me,
                judge.others,
                &name,
                area.place(),
                Evidence::OpenFile,
                need_name,
            )
        };
        if !seen.insert(root.clone()) {
            continue;
        }
        if let Some(v) = verdict {
            out.add_verdict(root, area.kind, &v, area.cap);
        }
    }
}

/// Crash reports named after the executable.
fn scan_crashes(app: &AppRecord, dirs: &[PathBuf], out: &mut Collector) {
    let Some(exe) = app.detail.executable.as_deref() else {
        return;
    };
    if ident::usable_name(exe).is_none() {
        return;
    }
    let exe = exe.to_lowercase();
    for dir in dirs {
        for entry in read_names(dir) {
            if entry.is_dir {
                continue;
            }
            let lower = entry.name.to_lowercase();
            let hit = lower
                .strip_prefix(exe.as_str())
                .is_some_and(|rest| rest.starts_with('_') || rest.starts_with('-'));
            if hit {
                out.add(Found::path(entry.path, AppFileKind::Logs, Confidence::High));
            }
        }
    }
}

/// Launch agents/daemons that belong to the app, and the privileged helpers they start.
fn scan_launchd(app: &AppRecord, judge: &Judge<'_>, include_system: bool, out: &mut Collector) {
    let within = |p: &str| {
        let p = Path::new(p);
        omc_scan::paths::is_within(p, &app.detail.real)
            || omc_scan::paths::is_within(p, &app.detail.bundle)
    };
    let lib = Place::Library { user_data: false };
    for (dir, domain) in startup::launch_dirs() {
        if !include_system && domain != startup::Domain::UserAgent {
            continue;
        }
        for plist in startup::plists(&dir) {
            let Some(job) = plist_util::launch_job(&plist) else {
                continue;
            };
            let referenced = job.program.as_deref().is_some_and(within)
                || job.args.iter().any(|a| within(a))
                || job.associated.iter().any(|a| {
                    let a = a.to_lowercase();
                    judge.me.ids.contains(&a) || judge.me.exact.contains(&a)
                });
            let stem = plist
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let verdict = if referenced {
                attribution::attribute_direct(
                    judge.me,
                    judge.others,
                    &job.label,
                    lib,
                    Evidence::Launchd,
                    false,
                )
            } else {
                attribution::attribute(judge.me, judge.others, &job.label, lib)
                    .or_else(|| attribution::attribute(judge.me, judge.others, &stem, lib))
            };
            let Some(confidence) = verdict.as_ref().and_then(Verdict::confidence) else {
                continue;
            };
            if let Some(program) = job.program.as_deref()
                && let Some(helper) = Path::new(program)
                    .strip_prefix("/Library/PrivilegedHelperTools")
                    .ok()
                    .and_then(|rest| rest.components().next())
            {
                let helper = Path::new("/Library/PrivilegedHelperTools").join(helper);
                if helper.exists() {
                    out.add(Found::path(helper, AppFileKind::LaunchItem, confidence));
                }
            }
            out.add(Found {
                special: Some(SpecialAction::UnloadLaunchJob {
                    label: job.label.clone(),
                    plist: plist.display().to_string(),
                }),
                admin: Some(domain != startup::Domain::UserAgent && !access::is_root()),
                ..Found::path(plist, AppFileKind::LaunchItem, confidence)
            });
        }
    }
}

/// Login items that open the app.
fn scan_login_items(
    app: &AppRecord,
    me: &Profile,
    items: Vec<startup::LoginItem>,
    out: &mut Collector,
) {
    for item in items {
        let by_path = item.path.as_deref().is_some_and(|p| {
            let p = Path::new(p);
            omc_scan::paths::is_within(p, &app.detail.bundle)
                || omc_scan::paths::is_within(p, &app.detail.real)
        });
        let normalized = ident::normalize(&item.name);
        let by_name = me.names.iter().any(|(n, _)| *n == normalized);
        let confidence = if by_path {
            Confidence::High
        } else if by_name {
            Confidence::Medium
        } else {
            continue;
        };
        let path = item.path.clone().map(PathBuf::from).unwrap_or_default();
        out.add(Found {
            special: Some(SpecialAction::RemoveLoginItem { name: item.name }),
            admin: Some(false),
            ..Found::path(path, AppFileKind::LoginItem, confidence)
        });
    }
}

/// What the installer database knows about the app.
#[derive(Debug, Default)]
struct Packages {
    /// Packages that installed the bundle.
    own: Vec<String>,
    /// Every installed package.
    all: Vec<String>,
    /// Top-level payload items of `own` packages.
    payload: Vec<PathBuf>,
}

fn packages(app: &AppRecord) -> Packages {
    let own = sources::bundle_pkgids(&app.detail.bundle);
    let mut payload = Vec::new();
    for id in &own {
        if id.to_lowercase().starts_with("com.apple.") {
            continue;
        }
        let files = sources::pkg_payload(id);
        payload.extend(payload_items(&sources::payload_roots(&files), &files));
    }
    Packages {
        own,
        all: sources::all_pkgids(),
        payload,
    }
}

/// Existing payload roots; a folder that also holds items the package did not install (a
/// vendor folder shared with other products) contributes only its payload children.
fn payload_items(roots: &[PathBuf], files: &[PathBuf]) -> Vec<PathBuf> {
    let listed: HashSet<String> = files
        .iter()
        .map(|f| f.to_string_lossy().to_lowercase())
        .collect();
    let is_listed = |p: &Path| listed.contains(&p.to_string_lossy().to_lowercase());
    let mut out = Vec::new();
    for root in roots {
        let Ok(meta) = std::fs::symlink_metadata(root) else {
            continue;
        };
        if !meta.is_dir() || root.extension().is_some() {
            out.push(root.clone());
            continue;
        }
        let children = read_names(root);
        if children.iter().all(|c| is_listed(&c.path)) {
            out.push(root.clone());
        } else {
            out.extend(
                children
                    .into_iter()
                    .filter(|c| is_listed(&c.path))
                    .map(|c| c.path),
            );
        }
    }
    out
}

/// Installer receipts (the packages that installed the bundle and packages named after
/// the app) and what those packages installed outside the bundle.
fn scan_receipts(judge: &Judge<'_>, pkgs: &Packages, out: &mut Collector) {
    let lib = Place::Library { user_data: false };
    let mut found: Vec<(String, Confidence)> = pkgs
        .own
        .iter()
        .map(|id| (id.clone(), Confidence::High))
        .collect();
    for id in &pkgs.all {
        if let Some(confidence) = attribution::attribute(judge.me, judge.others, id, lib)
            .as_ref()
            .and_then(Verdict::confidence)
        {
            found.push((id.clone(), confidence));
        }
    }
    for (id, confidence) in found {
        if !judge.me.apple && id.to_lowercase().starts_with("com.apple.") {
            continue;
        }
        let mut any = false;
        for ext in ["plist", "bom"] {
            let path = Path::new(RECEIPTS).join(format!("{id}.{ext}"));
            if path.is_file() {
                any = true;
                out.add(Found {
                    admin: Some(!access::is_root()),
                    ..Found::path(path, AppFileKind::Receipt, confidence)
                });
            }
        }
        if any {
            out.add(Found {
                special: Some(SpecialAction::ForgetPackage { id: id.clone() }),
                admin: Some(!access::is_root()),
                ..Found::path(
                    Path::new(RECEIPTS).join(format!("{id}.plist")),
                    AppFileKind::Receipt,
                    confidence,
                )
            });
        }
    }
    for path in &pkgs.payload {
        out.add(Found::path(
            path.clone(),
            kind_for_path(path),
            Confidence::High,
        ));
    }
}

/// Role of a path from the Library folder it is in.
fn kind_for_path(path: &Path) -> AppFileKind {
    use AppFileKind as K;
    for comp in path.components().rev() {
        match comp.as_os_str().to_string_lossy().as_ref() {
            "Caches" => return K::Cache,
            "Preferences" => return K::Preferences,
            "Containers" => return K::Container,
            "Group Containers" => return K::GroupContainer,
            "Saved Application State" => return K::SavedState,
            "Logs" | "DiagnosticReports" => return K::Logs,
            "LaunchAgents" | "LaunchDaemons" | "PrivilegedHelperTools" => return K::LaunchItem,
            "HTTPStorages" | "WebKit" | "Cookies" => return K::WebData,
            "Receipts" | "receipts" => return K::Receipt,
            "Audio" | "Extensions" | "Frameworks" | "Internet Plug-Ins" | "PreferencePanes"
            | "QuickLook" | "Spotlight" | "Screen Savers" | "Input Methods" | "Services" => {
                return K::Extension;
            }
            "Application Support" | "Application Scripts" | "Mobile Documents" => {
                return K::Support;
            }
            _ => {}
        }
    }
    K::Other
}

/// `name` matches a shell-style pattern with `*` and `?` (case-insensitive).
fn wildcard(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    let (mut pi, mut ni) = (0_usize, 0_usize);
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        match p.get(pi) {
            Some('*') => {
                star = Some((pi, ni));
                pi = pi.saturating_add(1);
            }
            Some(&c) if c == '?' || Some(&c) == n.get(ni) => {
                pi = pi.saturating_add(1);
                ni = ni.saturating_add(1);
            }
            _ => match star {
                Some((sp, sn)) => {
                    pi = sp.saturating_add(1);
                    ni = sn.saturating_add(1);
                    star = Some((sp, ni));
                }
                None => return false,
            },
        }
    }
    p.get(pi..)
        .is_some_and(|rest| rest.iter().all(|&c| c == '*'))
}

/// Homebrew `zap` paths of the app's cask; `*`/`?` are expanded in the last component.
fn scan_zap(app: &AppRecord, out: &mut Collector) {
    let Some(cask) = &app.detail.cask else {
        return;
    };
    for raw in &cask.zap {
        let path = omc_scan::paths::expand(raw);
        let Some(path) = omc_scan::paths::normalize(&path) else {
            continue;
        };
        let (Some(parent), Some(pattern)) = (path.parent(), path.file_name()) else {
            continue;
        };
        let pattern = pattern.to_string_lossy();
        // Refuse anything shallow (`~`, `/Library`, `~/Library/Caches`) and globs above
        // the last component.
        if path.components().count() < 5 || parent.to_string_lossy().contains(['*', '?', '[']) {
            continue;
        }
        let matches: Vec<PathBuf> = if pattern.contains(['*', '?']) {
            read_names(parent)
                .into_iter()
                .filter(|e| wildcard(&pattern, &e.name))
                .map(|e| e.path)
                .collect()
        } else if pattern.contains('[') {
            Vec::new()
        } else if std::fs::symlink_metadata(&path).is_ok() {
            vec![path.clone()]
        } else {
            Vec::new()
        };
        for m in matches {
            let kind = kind_for_path(&m);
            out.add(Found::path(m, kind, Confidence::High));
        }
    }
}

/// Command-line links into the bundle (`/usr/local/bin/code`).
fn scan_cli_links(app: &AppRecord, out: &mut Collector) {
    for link in sources::cli_links(&app.detail.bundle, &app.detail.real) {
        out.add(Found::path(link, AppFileKind::Other, Confidence::High));
    }
}

/// XDG folders and home dot entries of the app ([`home`]).
fn scan_user_config(
    app: &AppRecord,
    judge: &Judge<'_>,
    open: &[PathBuf],
    home: &Path,
    settings: &CleanSettings,
    threads: usize,
    out: &mut Collector,
) {
    let open: Vec<PathBuf> = open.iter().map(|p| unprivate(p)).collect();
    let guard = omc_scan::Guard::new(settings);
    let items = home::user_config(app, judge.me, judge.others, &open, home, threads, |p| {
        guard.check(p).is_ok()
    });
    for item in items {
        out.add(Found::path(item.path, item.kind, item.confidence));
    }
}

/// Drops items inside another path item.
fn drop_nested(items: &mut Vec<Found>) {
    let dirs: Vec<PathBuf> = items
        .iter()
        .filter(|f| f.special.is_none() && f.kind != AppFileKind::Bundle)
        .map(|f| f.path.clone())
        .collect();
    items.retain(|f| {
        f.special.is_some()
            || !dirs
                .iter()
                .any(|d| d != &f.path && omc_scan::paths::is_within(&f.path, d))
    });
}

/// Every other installed app: its profile and bundle.
fn other_apps(app: &AppRecord, threads: usize, ctx: &JobCtx) -> (Vec<Profile>, Vec<PathBuf>) {
    let mut profiles = Vec::new();
    let mut bundles = Vec::new();
    for (bundle, info) in inventory::installed(threads, ctx) {
        if bundle.real == app.detail.real {
            continue;
        }
        let mut p = Profile::basic(info.id.as_deref(), &info.name, info.executable.as_deref());
        if let Some(stem) = bundle.path.file_stem() {
            p.add_name(&stem.to_string_lossy(), NameKind::Primary);
        }
        profiles.push(p);
        bundles.push(bundle.real);
    }
    (profiles, bundles)
}

/// No other installed app shares `me`'s vendor prefix or vendor words.
fn sole_vendor(me: &Profile, others: &[Profile]) -> bool {
    (me.vendor.is_some() || !me.vendor_words.is_empty())
        && !others.iter().any(|o| {
            (me.vendor.is_some() && o.vendor == me.vendor)
                || me.vendor_words.iter().any(|w| o.has_vendor_word(w))
        })
}

fn join<T: Default>(handle: ScopedJoinHandle<'_, T>, what: &str) -> T {
    handle.join().unwrap_or_else(|_| {
        tracing::error!(what, "app files worker panicked");
        T::default()
    })
}

/// Everything that belongs to `app`.
pub(super) fn app_files(
    app: &AppRecord,
    settings: &CleanSettings,
    walker: &Walker,
    ctx: &JobCtx,
) -> Result<AppFiles> {
    ctx.set_phase(Phase::Scanning);
    let home = omc_scan::paths::home();
    let threads = walker.options().threads;
    // Independent lookups run side by side; the slowest (login items via System Events)
    // bounds the wait.
    let (profile, (others, other_bundles), open, login, pkgs, (areas, listings)) =
        std::thread::scope(|s| {
            let profile = s.spawn(|| sources::profile(app));
            let others = s.spawn(|| other_apps(app, threads, ctx));
            let open = s.spawn(|| sources::open_files(&sources::app_pids(app)));
            let login = s.spawn(|| match startup::login_items() {
                Ok(items) => items,
                Err(err) => {
                    tracing::info!(%err, "login items unavailable");
                    Vec::new()
                }
            });
            let pkgs = s.spawn(|| packages(app));
            let listed = s.spawn(|| {
                let areas = areas(home.as_deref(), settings.include_system, &darwin_dirs());
                let listings = par_map(&areas, threads, |a| read_names(&a.dir));
                (areas, listings)
            });
            (
                join(profile, "profile"),
                join(others, "others"),
                join(open, "open files"),
                join(login, "login items"),
                join(pkgs, "packages"),
                join(listed, "areas"),
            )
        });
    if ctx.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let (mut me, traits) = profile;
    me.sole_vendor = sole_vendor(&me, &others);
    let judge = Judge {
        me: &me,
        traits: &traits,
        others: &others,
    };
    let mut found = Collector::default();
    scan_areas(&judge, &areas, listings, ctx, &mut found);
    scan_open_files(&judge, &areas, &open, &mut found);
    scan_crashes(
        app,
        &crash_dirs(home.as_deref(), settings.include_system),
        &mut found,
    );
    scan_launchd(app, &judge, settings.include_system, &mut found);
    scan_login_items(app, &me, login, &mut found);
    scan_receipts(&judge, &pkgs, &mut found);
    scan_zap(app, &mut found);
    scan_cli_links(app, &mut found);
    if let Some(home) = home.as_deref() {
        scan_user_config(app, &judge, &open, home, settings, threads, &mut found);
    }
    if ctx.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let mut items = found.items;
    items.retain(|f| {
        // Nothing inside the bundle (it is removed as a whole), nothing excluded, and
        // nothing inside or around another installed app.
        f.special.is_some()
            || (!omc_scan::paths::is_within(&f.path, &app.detail.real)
                && !walker.options().is_excluded(&f.path)
                && !other_bundles.iter().any(|b| {
                    omc_scan::paths::is_within(&f.path, b) || omc_scan::paths::is_within(b, &f.path)
                }))
    });
    drop_nested(&mut items);
    items.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.path.cmp(&b.path)));
    items.insert(
        0,
        Found {
            admin: Some(app.info.needs_admin),
            ..Found::path(
                app.detail.bundle.clone(),
                AppFileKind::Bundle,
                Confidence::High,
            )
        },
    );
    let (report_items, targets) = assemble(app, items, settings, walker, ctx);
    let uninstaller = app
        .detail
        .cask
        .as_ref()
        .map(|c| format!("brew uninstall --cask {}", c.token));
    Ok(AppFiles {
        report: AppFilesReport {
            app: app.info.clone(),
            uninstaller,
            items: report_items,
        },
        targets,
        app: app.clone(),
    })
}

/// Measures the items (the bundle's size is already known from the app list) and builds
/// the report items with their removal targets (`targets[id]`).
fn assemble(
    app: &AppRecord,
    items: Vec<Found>,
    settings: &CleanSettings,
    walker: &Walker,
    ctx: &JobCtx,
) -> (Vec<AppFile>, Vec<Target>) {
    ctx.set_phase(Phase::Measuring);
    let measure_paths: Vec<PathBuf> = items
        .iter()
        .map(|f| {
            let known_bundle = f.kind == AppFileKind::Bundle && app.info.bytes.is_some();
            let no_space = matches!(
                f.special,
                Some(SpecialAction::RemoveLoginItem { .. } | SpecialAction::ForgetPackage { .. })
            );
            if known_bundle || no_space {
                // Nothing to measure (an empty path measures as missing).
                PathBuf::new()
            } else if f.kind == AppFileKind::Bundle {
                app.detail.real.clone()
            } else {
                f.path.clone()
            }
        })
        .collect();
    let sizes = walker.measure_all(&measure_paths, ctx);
    let mut report_items = Vec::with_capacity(items.len());
    let mut targets = Vec::with_capacity(items.len());
    for (found, size) in items.into_iter().zip(sizes) {
        let bytes = match (found.kind, app.info.bytes) {
            (AppFileKind::Bundle, Some(known)) => known,
            _ if size.missing => 0,
            _ => size.bytes,
        };
        let needs_admin = found
            .admin
            .unwrap_or_else(|| access::needs_admin(&found.path));
        let (location, target) = if let Some(action) = found.special {
            let location = Location::Special { action };
            let target = Target::location(location.clone(), bytes).admin(needs_admin);
            (location, target)
        } else {
            let path = found.path.display().to_string();
            let target =
                Target::path(path.clone(), bytes, settings.files_delete).admin(needs_admin);
            (Location::Path { path }, target)
        };
        let id = omc_scan::target::push_target(&mut targets, target);
        ctx.add_bytes(bytes);
        report_items.push(AppFile {
            id,
            location,
            kind: found.kind,
            bytes,
            confidence: found.confidence,
            needs_admin,
        });
    }
    (report_items, targets)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "omc-mac-files-{tag}-{}-{}",
                std::process::id(),
                omc_scan::paths::now_secs()
            ));
            let _ignored = std::fs::remove_dir_all(&dir);
            assert!(std::fs::create_dir_all(&dir).is_ok(), "create temp dir");
            Self(dir)
        }

        fn dir(&self, rel: &str) -> PathBuf {
            let p = self.0.join(rel);
            assert!(std::fs::create_dir_all(&p).is_ok(), "fixture dir {rel}");
            p
        }

        fn file(&self, rel: &str) -> PathBuf {
            let p = self.0.join(rel);
            if let Some(parent) = p.parent() {
                assert!(
                    std::fs::create_dir_all(parent).is_ok(),
                    "fixture parent {rel}"
                );
            }
            assert!(std::fs::write(&p, b"x").is_ok(), "fixture file {rel}");
            p
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ignored = std::fs::remove_dir_all(&self.0);
        }
    }

    fn chrome() -> Profile {
        let mut p = Profile::basic(
            Some("com.google.Chrome"),
            "Google Chrome",
            Some("Google Chrome"),
        );
        p.add_id("com.google.Chrome.helper");
        p
    }

    fn code() -> Profile {
        let mut p = Profile::basic(Some("com.microsoft.VSCode"), "Code", Some("Electron"));
        p.add_name("Visual Studio Code", NameKind::Primary);
        p.add_name("Code", NameKind::Product);
        p
    }

    fn teams() -> Profile {
        Profile::basic(
            Some("com.microsoft.teams2"),
            "Microsoft Teams",
            Some("MSTeams"),
        )
    }

    fn edge() -> Profile {
        Profile::basic(
            Some("com.microsoft.edgemac"),
            "Microsoft Edge",
            Some("Microsoft Edge"),
        )
    }

    fn studio() -> Profile {
        Profile::basic(
            Some("com.google.android.studio"),
            "Android Studio",
            Some("studio"),
        )
    }

    /// A fake home with Chrome-like, Electron-like, Microsoft-like and Apple-shared files.
    fn fake_home() -> TempDir {
        let t = TempDir::new("home");
        for d in [
            "Library/Application Support/Google/Chrome/Default",
            "Library/Application Support/Google/Chrome for Testing",
            "Library/Application Support/Google/GoogleUpdater",
            "Library/Application Support/Google/AndroidStudio2026.1",
            "Library/Caches/Google/Chrome",
            "Library/Caches/org.sparkle-project.Sparkle",
            "Library/Application Support/Code/User",
            "Library/Application Support/Microsoft/Teams",
            "Library/Application Support/Microsoft/Office",
            "Library/Application Support/Microsoft Edge/Local Storage",
            "Library/Application Support/Microsoft Edge/Code Cache",
            "Library/Application Support/CrashReporter",
            "Library/Application Support/com.apple.sharedfilelist",
            "Library/Containers/com.microsoft.teams2",
            "Library/Google",
            ".vscode/extensions",
            ".cargo",
        ] {
            t.dir(d);
        }
        for f in [
            "Library/Preferences/com.google.Chrome.plist",
            "Library/Preferences/com.google.chrome.for.testing.plist",
            "Library/Preferences/com.google.Keystone.Agent.plist",
            "Library/Preferences/com.microsoft.VSCode.plist",
            "Library/Application Support/Microsoft Edge/Local State",
            "Library/Google/Google Chrome Brand.plist",
        ] {
            t.file(f);
        }
        t
    }

    fn scan(me: &Profile, traits: &Traits, others: &[Profile], home: &Path) -> Vec<Found> {
        let areas = areas(Some(home), false, &[]);
        let listings: Vec<Vec<Entry>> = areas.iter().map(|a| read_names(&a.dir)).collect();
        let judge = Judge { me, traits, others };
        let mut out = Collector::default();
        scan_areas(&judge, &areas, listings, &JobCtx::new(), &mut out);
        out.items
    }

    fn conf_of(items: &[Found], home: &Path, rel: &str) -> Option<Confidence> {
        let path = home.join(rel);
        items.iter().find(|f| f.path == path).map(|f| f.confidence)
    }

    #[test]
    fn chrome_owns_its_vendor_product_folders_only() {
        let home = fake_home();
        let h = &home.0;
        let mut me = chrome();
        let others = vec![studio(), code(), teams(), edge()];
        me.sole_vendor = sole_vendor(&me, &others);
        let items = scan(&me, &Traits::default(), &others, h);
        for (rel, want) in [
            (
                "Library/Application Support/Google/Chrome",
                Some(Confidence::High),
            ),
            ("Library/Caches/Google/Chrome", Some(Confidence::High)),
            (
                "Library/Preferences/com.google.Chrome.plist",
                Some(Confidence::High),
            ),
            ("Library/Google/Google Chrome Brand.plist", None),
            (
                "Library/Application Support/Google/Chrome for Testing",
                None,
            ),
            ("Library/Application Support/Google/GoogleUpdater", None),
            ("Library/Application Support/Google", None),
            (
                "Library/Preferences/com.google.chrome.for.testing.plist",
                None,
            ),
            ("Library/Preferences/com.google.Keystone.Agent.plist", None),
            ("Library/Caches/org.sparkle-project.Sparkle", None),
            ("Library/Application Support/CrashReporter", None),
            ("Library/Application Support/com.apple.sharedfilelist", None),
        ] {
            assert_eq!(conf_of(&items, h, rel), want, "{rel}");
        }
    }

    #[test]
    fn electron_data_folder_is_the_product_name() {
        let home = fake_home();
        let h = &home.0;
        let me = code();
        let others = vec![chrome(), studio(), teams(), edge()];
        let items = scan(&me, &Traits::default(), &others, h);
        assert_eq!(
            conf_of(&items, h, "Library/Application Support/Code"),
            Some(Confidence::High),
            "Application Support/<productName>"
        );
        assert_eq!(
            conf_of(&items, h, "Library/Preferences/com.microsoft.VSCode.plist"),
            Some(Confidence::High),
            "bundle id"
        );
        assert_eq!(
            conf_of(&items, h, "Library/Application Support/Microsoft/Teams"),
            None,
            "another Microsoft product"
        );
    }

    #[test]
    fn same_vendor_apps_split_the_vendor_folder() {
        let home = fake_home();
        let h = &home.0;
        let mut me = teams();
        let others = vec![edge(), code(), chrome(), studio()];
        me.sole_vendor = sole_vendor(&me, &others);
        assert!(!me.sole_vendor, "Edge shares the vendor");
        let items = scan(&me, &Traits::default(), &others, h);
        assert_eq!(
            conf_of(&items, h, "Library/Application Support/Microsoft/Teams"),
            Some(Confidence::High),
            "vendor/product"
        );
        assert_eq!(
            conf_of(&items, h, "Library/Containers/com.microsoft.teams2"),
            Some(Confidence::High),
            "container"
        );
        for rel in [
            "Library/Application Support/Microsoft/Office",
            "Library/Application Support/Microsoft",
            "Library/Application Support/Microsoft Edge",
        ] {
            assert_eq!(conf_of(&items, h, rel), None, "{rel} is not Teams'");
        }

        let me = edge();
        let others = vec![teams(), code(), chrome(), studio()];
        let chromium = Traits {
            chromium: true,
            icloud: Vec::new(),
        };
        let items = scan(&me, &chromium, &others, h);
        assert_eq!(
            conf_of(&items, h, "Library/Application Support/Microsoft Edge"),
            Some(Confidence::High),
            "name + Chromium profile inside"
        );
        let items = scan(&me, &Traits::default(), &others, h);
        assert_eq!(
            conf_of(&items, h, "Library/Application Support/Microsoft Edge"),
            Some(Confidence::Medium),
            "name only"
        );
    }

    #[test]
    fn open_files_mark_their_top_folder() {
        let home = fake_home();
        let h = &home.0;
        let me = chrome();
        let others = vec![studio()];
        let traits = Traits::default();
        let judge = Judge {
            me: &me,
            traits: &traits,
            others: &others,
        };
        let areas = areas(Some(h), false, &[]);
        let open = vec![
            h.join("Library/Application Support/Google/Chrome/Default/History"),
            h.join("Library/Application Support/Google/AndroidStudio2026.1/x"),
            h.join(".cargo/registry"),
            h.join("Library/Caches/org.sparkle-project.Sparkle/x"),
            PathBuf::from("/System/Library/Fonts/SFNS.ttf"),
        ];
        let mut out = Collector::default();
        scan_open_files(&judge, &areas, &open, &mut out);
        let paths: Vec<PathBuf> = out.items.iter().map(|f| f.path.clone()).collect();
        assert_eq!(
            paths,
            vec![h.join("Library/Application Support/Google/Chrome")],
            "only the app's own folder"
        );
    }

    #[test]
    fn vendor_named_data_folders_are_taken_whole() {
        let t = TempDir::new("vendorword");
        let h = &t.0;
        t.dir("Library/Application Support/apifox/apifox-data");
        t.file("Library/Application Support/Zed/db/0.mdb");
        let mut apifox = Profile::basic(Some("cn.apifox.app"), "Apifox", Some("Apifox"));
        apifox.add_name("apifox", NameKind::Product);
        let items = scan(&apifox, &Traits::default(), &[], h);
        assert_eq!(
            conf_of(&items, h, "Library/Application Support/apifox"),
            Some(Confidence::High),
            "Electron data folder named like the vendor"
        );
        assert_eq!(
            conf_of(&items, h, "Library/Application Support/apifox/apifox-data"),
            None,
            "not split into children"
        );

        let mut zed = Profile::basic(Some("dev.zed.Zed-Preview"), "Zed Preview", Some("zed"));
        zed.sole_vendor = true;
        let items = scan(&zed, &Traits::default(), &[], h);
        assert_eq!(
            conf_of(&items, h, "Library/Application Support/Zed"),
            Some(Confidence::Medium),
            "sole vendor folder by name"
        );
        let traits = Traits::default();
        let judge = Judge {
            me: &zed,
            traits: &traits,
            others: &[],
        };
        let mut out = Collector::default();
        let open = vec![h.join("Library/Application Support/Zed/db/0.mdb")];
        scan_open_files(&judge, &areas(Some(h), false, &[]), &open, &mut out);
        assert!(
            out.items
                .iter()
                .any(|f| f.path == h.join("Library/Application Support/Zed")
                    && f.confidence == Confidence::High),
            "open files confirm the vendor folder"
        );
    }

    #[test]
    fn payload_folders_shared_with_other_products_give_their_children() {
        let t = TempDir::new("payload");
        let own = t.dir("Vendor/Product");
        t.dir("Vendor/Other");
        let solo = t.dir("Solo");
        let file = t.file("Solo/data");
        let files = vec![t.0.join("Vendor"), own.clone(), solo.clone(), file];
        let roots = vec![t.0.join("Vendor"), solo.clone(), t.0.join("Missing")];
        assert_eq!(
            payload_items(&roots, &files),
            vec![own, solo],
            "shared folder → payload child; own folder whole; missing skipped"
        );
    }

    #[test]
    fn zap_globs_match_the_last_component() {
        assert!(
            wildcard(
                "com.google.Chrome.app.*.savedState",
                "com.google.Chrome.app.abc.savedState"
            ),
            "star"
        );
        assert!(wildcard("com.x.sfl*", "com.x.sfl3"), "trailing star");
        assert!(wildcard("a?c", "ABC"), "question mark, case-insensitive");
        assert!(!wildcard("com.x.*", "com.y.z"), "prefix differs");
        assert!(!wildcard("abc", "abcd"), "no implicit suffix");
    }

    #[test]
    fn library_paths_map_to_kinds() {
        assert_eq!(
            kind_for_path(Path::new("/Users/me/Library/Caches/com.x")),
            AppFileKind::Cache,
            "caches"
        );
        assert_eq!(
            kind_for_path(Path::new("/Users/me/Library/Application Support/X/Caches")),
            AppFileKind::Cache,
            "innermost folder wins"
        );
        assert_eq!(
            kind_for_path(Path::new("/Library/Audio/Plug-Ins/HAL/X.driver")),
            AppFileKind::Extension,
            "audio driver"
        );
        assert_eq!(
            kind_for_path(Path::new("/Users/me/.x")),
            AppFileKind::Other,
            "dotfile"
        );
    }

    #[test]
    fn collector_dedupes_and_keeps_best_confidence() {
        let mut c = Collector::default();
        let p = PathBuf::from("/Users/me/Library/LaunchAgents/com.x.plist");
        c.add(Found::path(
            p.clone(),
            AppFileKind::LaunchItem,
            Confidence::Low,
        ));
        c.add(Found {
            special: Some(SpecialAction::UnloadLaunchJob {
                label: "com.x".to_owned(),
                plist: p.display().to_string(),
            }),
            ..Found::path(p, AppFileKind::LaunchItem, Confidence::High)
        });
        assert_eq!(c.items.len(), 1, "same path once");
        assert!(
            c.items
                .first()
                .is_some_and(|f| f.confidence == Confidence::High && f.special.is_some()),
            "best confidence and the launch action win"
        );
    }

    #[test]
    fn nested_items_are_dropped() {
        let mut items = vec![
            Found::path(
                PathBuf::from("/L/Support/Foo"),
                AppFileKind::Support,
                Confidence::Medium,
            ),
            Found::path(
                PathBuf::from("/L/Support/Foo/x"),
                AppFileKind::Support,
                Confidence::High,
            ),
            Found::path(
                PathBuf::from("/L/Support/Foobar"),
                AppFileKind::Support,
                Confidence::High,
            ),
        ];
        drop_nested(&mut items);
        assert_eq!(items.len(), 2, "child of an item is removed with it");
    }
}
