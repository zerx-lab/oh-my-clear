//! Junk areas: system junk, browser data, developer junk, Trash and installers.
//!
//! Every area is a declarative, per-OS catalogue of candidate locations ([`Candidate`])
//! built from the user's well-known folders ([`Roots`]). Catalogues only list and read a
//! few small files (profile lists); all sizing happens afterwards in one parallel batch on
//! the walker's pool. The shared pipeline ([`finish`]) then:
//! 1. splits broad globs (`~/Library/Caches/*`) around locations another catalogue owns;
//! 2. drops excluded, guard-refused, missing, empty and (without `include_system`)
//!    admin-only places, and reports unreadable ones as [`Denied`];
//! 3. removes duplicates and paths nested in another reported path;
//! 4. measures everything in parallel (temp files and logs only count files older than
//!    the minimum age; see [`measure`] for other apps' sandbox containers);
//! 5. groups, sorts (largest first) and assigns item ids in final order, so
//!    `targets[id]` is the target of item `id`.
//!
//! Catalogues for all three OSes compile everywhere and are chosen at run time by
//! [`Os`], so each can be tested on any host against a fake home.

mod browser;
mod developer;
mod installers;
mod json;
mod measure;
mod system;
mod trash;

use std::cell::OnceCell;
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, Metadata};
use std::io;
use std::path::{Path, PathBuf};

use omc_proto::jobs::{DeleteMethod, Denied, Location, Phase, SpecialAction};
use omc_proto::junk::{ItemTag, JunkGroup, JunkItem, JunkKind, JunkReport, Safety};
use omc_proto::settings::CleanSettings;

use crate::ctx::JobCtx;
use crate::guard::Guard;
use crate::target::{Scanned, Target, push_target};
use crate::walk::{Measure, Walker};
use crate::{Error, Result, errors, paths, procs};

/// The junk areas [`scan`] knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JunkArea {
    /// OS and app caches, logs, crash reports, temp files, update and package caches.
    System,
    /// Browser caches and (opt-in) cookies, history, site data, sessions.
    Browser,
    /// Xcode data, package-manager and IDE caches, project build output.
    Developer,
    /// Trash / Recycle Bin.
    Trash,
    /// Installer packages and disk images in the user's folders.
    Installers,
}

/// Scans one junk area.
pub fn scan(
    area: JunkArea,
    settings: &CleanSettings,
    walker: &Walker,
    ctx: &JobCtx,
) -> Result<Scanned<JunkReport>> {
    ctx.set_phase(Phase::Scanning);
    let roots = Roots::detect().ok_or_else(|| Error::Invalid("no home folder".to_owned()))?;
    let mut cx = Cx::new(&roots, settings);
    let (candidates, claimed) = collect(area, &mut cx, walker, ctx);
    if ctx.is_cancelled() {
        return Err(Error::Cancelled);
    }
    finish(candidates, &claimed, cx, walker, ctx)
}

/// Runs the catalogue of `area`: its candidates plus the paths other areas own (a broad
/// glob of this area must not report them).
fn collect(
    area: JunkArea,
    cx: &mut Cx<'_>,
    walker: &Walker,
    ctx: &JobCtx,
) -> (Vec<Candidate>, Vec<PathBuf>) {
    match area {
        JunkArea::System => {
            let mut claimed = browser::claimed(cx.roots);
            claimed.extend(developer::claimed(cx.roots));
            (system::catalogue(cx), claimed)
        }
        JunkArea::Browser => (browser::catalogue(cx), Vec::new()),
        JunkArea::Developer => (developer::catalogue(cx, walker, ctx), Vec::new()),
        JunkArea::Trash => (trash::catalogue(cx), Vec::new()),
        JunkArea::Installers => (installers::catalogue(cx, walker, ctx), Vec::new()),
    }
}

// ---------------------------------------------------------------------------------------
// Roots

/// Operating system a catalogue is written for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Os {
    Mac,
    Windows,
    Linux,
}

impl Os {
    /// The host.
    pub(crate) const CURRENT: Self = if cfg!(target_os = "macos") {
        Self::Mac
    } else if cfg!(windows) {
        Self::Windows
    } else {
        Self::Linux
    };
}

/// Environment variables that move tool caches (`CARGO_HOME`, `GOCACHE`…).
const ENV_OVERRIDES: [&str; 17] = [
    "CARGO_HOME",
    "GOCACHE",
    "GOMODCACHE",
    "GOPATH",
    "npm_config_cache",
    "YARN_CACHE_FOLDER",
    "PIP_CACHE_DIR",
    "UV_CACHE_DIR",
    "GRADLE_USER_HOME",
    "BUN_INSTALL",
    "DENO_DIR",
    "PUB_CACHE",
    "HOMEBREW_CACHE",
    "NUGET_PACKAGES",
    "COMPOSER_CACHE_DIR",
    "PNPM_HOME",
    "POETRY_CACHE_DIR",
];

/// The folders catalogues are built from. Tests inject a fake tree.
#[derive(Debug, Clone)]
pub(crate) struct Roots {
    pub(crate) os: Os,
    /// Home folder.
    pub(crate) home: PathBuf,
    /// The user's temp folder (`$TMPDIR`, `%TEMP%`).
    pub(crate) temp: PathBuf,
    /// Prefix of absolute system paths: `/` on Unix, the system drive (`C:\`) on Windows.
    pub(crate) sys: PathBuf,
    /// Per-user cache base: `~/Library/Caches`, `$XDG_CACHE_HOME`, `%LOCALAPPDATA%`.
    pub(crate) cache: PathBuf,
    /// Per-user config base: `~/Library/Application Support`, `$XDG_CONFIG_HOME`,
    /// `%APPDATA%`.
    pub(crate) config: PathBuf,
    /// Per-user data base: `~/Library/Application Support`, `$XDG_DATA_HOME`,
    /// `%LOCALAPPDATA%`.
    pub(crate) data: PathBuf,
    /// Windows `%WINDIR%` (unused elsewhere).
    pub(crate) windir: PathBuf,
    /// Windows `%PROGRAMDATA%` (unused elsewhere).
    pub(crate) program_data: PathBuf,
    /// Windows: trees only administrators may modify.
    pub(crate) admin_trees: Vec<PathBuf>,
    /// Windows: roots of drives that have a Recycle Bin.
    pub(crate) drives: Vec<PathBuf>,
    /// Unix: the user's uid (owner of the home folder).
    pub(crate) uid: Option<u32>,
    /// User name (Linux `/run/media/<user>`).
    pub(crate) user: Option<String>,
    /// Windows: the user's SID (Recycle Bin folder name).
    pub(crate) sid: Option<String>,
    /// Tool cache overrides from the environment.
    pub(crate) env: Vec<(&'static str, PathBuf)>,
    /// macOS: trees whose folders the kernel checks with a sandbox upcall on open (other
    /// apps' containers); measured off the pool with a deadline (see [`measure`]).
    pub(crate) protected: Vec<PathBuf>,
}

impl Roots {
    /// The current user's folders; `None` without a home folder.
    fn detect() -> Option<Self> {
        let home = paths::home()?;
        let os = Os::CURRENT;
        let env = ENV_OVERRIDES
            .iter()
            .filter_map(|name| {
                paths::env_dir(name)
                    .filter(|p| p.is_absolute())
                    .map(|p| (*name, p))
            })
            .collect();
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .ok()
            .filter(|u| !u.is_empty())
            .or_else(|| home.file_name().map(|n| n.to_string_lossy().into_owned()));
        let mut temp = std::env::temp_dir();
        if os == Os::Mac {
            // `$TMPDIR` goes through the `/var` → `/private/var` symlink.
            temp = fs::canonicalize(&temp).unwrap_or(temp);
        }
        let mut roots = Self {
            os,
            temp,
            sys: PathBuf::from("/"),
            cache: home.join(".cache"),
            config: home.join(".config"),
            data: join(&home, ".local/share"),
            windir: PathBuf::new(),
            program_data: PathBuf::new(),
            admin_trees: Vec::new(),
            drives: Vec::new(),
            uid: home_uid(&home),
            user,
            sid: None,
            env,
            protected: Vec::new(),
            home,
        };
        match os {
            Os::Mac => {
                roots.cache = join(&roots.home, "Library/Caches");
                roots.config = join(&roots.home, "Library/Application Support");
                roots.data = roots.config.clone();
                roots.protected = vec![
                    join(&roots.home, "Library/Containers"),
                    join(&roots.home, "Library/Group Containers"),
                ];
            }
            Os::Linux => {
                if let Some(dir) = paths::env_dir("XDG_CACHE_HOME").filter(|p| p.is_absolute()) {
                    roots.cache = dir;
                }
                if let Some(dir) = paths::env_dir("XDG_CONFIG_HOME").filter(|p| p.is_absolute()) {
                    roots.config = dir;
                }
                if let Some(dir) = paths::env_dir("XDG_DATA_HOME").filter(|p| p.is_absolute()) {
                    roots.data = dir;
                }
            }
            Os::Windows => roots.detect_windows(),
        }
        Some(roots)
    }

    fn detect_windows(&mut self) {
        let drive = paths::env_dir("SystemDrive").unwrap_or_else(|| PathBuf::from("C:"));
        self.sys = PathBuf::from(format!("{}\\", drive.display()));
        self.windir = paths::env_dir("SystemRoot")
            .or_else(|| paths::env_dir("windir"))
            .unwrap_or_else(|| self.sys.join("Windows"));
        self.program_data =
            paths::env_dir("ProgramData").unwrap_or_else(|| self.sys.join("ProgramData"));
        self.cache =
            paths::env_dir("LOCALAPPDATA").unwrap_or_else(|| join(&self.home, "AppData/Local"));
        self.config =
            paths::env_dir("APPDATA").unwrap_or_else(|| join(&self.home, "AppData/Roaming"));
        self.data = self.cache.clone();
        self.admin_trees = vec![self.windir.clone(), self.program_data.clone()];
        for var in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
            if let Some(dir) = paths::env_dir(var) {
                self.admin_trees.push(dir);
            }
        }
        self.drives = (b'C'..=b'Z')
            .map(|letter| PathBuf::from(format!("{}:\\", char::from(letter))))
            .filter(|root| root.join("$Recycle.Bin").is_dir())
            .collect();
        self.sid = windows_sid();
    }

    /// A system path below [`Self::sys`] (`rel` uses `/`).
    pub(crate) fn sys(&self, rel: &str) -> PathBuf {
        join(&self.sys, rel)
    }

    /// A path below the home folder (`rel` uses `/`).
    pub(crate) fn home(&self, rel: &str) -> PathBuf {
        join(&self.home, rel)
    }

    /// An environment override.
    pub(crate) fn env(&self, name: &str) -> Option<&Path> {
        self.env
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, p)| p.as_path())
    }

    /// A fake tree for tests: every base below `base`.
    #[cfg(test)]
    pub(crate) fn fake(base: &Path, os: Os) -> Self {
        let home = base.join("home");
        let (cache, config, data) = match os {
            Os::Mac => (
                join(&home, "Library/Caches"),
                join(&home, "Library/Application Support"),
                join(&home, "Library/Application Support"),
            ),
            Os::Linux => (
                home.join(".cache"),
                home.join(".config"),
                join(&home, ".local/share"),
            ),
            Os::Windows => (
                join(&home, "AppData/Local"),
                join(&home, "AppData/Roaming"),
                join(&home, "AppData/Local"),
            ),
        };
        let sys = base.join("sys");
        Self {
            os,
            temp: base.join("tmp"),
            windir: sys.join("Windows"),
            program_data: sys.join("ProgramData"),
            admin_trees: Vec::new(),
            drives: vec![sys.clone()],
            sys,
            cache,
            config,
            data,
            uid: None,
            user: Some("me".to_owned()),
            sid: None,
            env: Vec::new(),
            protected: Vec::new(),
            home,
        }
    }
}

#[cfg(unix)]
fn home_uid(home: &Path) -> Option<u32> {
    use std::os::unix::fs::MetadataExt as _;
    fs::metadata(home).ok().map(|m| m.uid())
}

#[cfg(not(unix))]
fn home_uid(_home: &Path) -> Option<u32> {
    None
}

/// The current user's SID from `whoami /user` (Recycle Bin folder name).
#[cfg(windows)]
fn windows_sid() -> Option<String> {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let output = std::process::Command::new("whoami")
        .args(["/user", "/fo", "csv", "/nh"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .split(',')
            .nth(1)
            .map(|s| s.trim().trim_matches('"').to_owned())
            .filter(|s| s.starts_with("S-1-")),
        Ok(out) => {
            tracing::debug!(status = %out.status, "whoami failed");
            None
        }
        Err(err) => {
            tracing::debug!(%err, "cannot run whoami");
            None
        }
    }
}

#[cfg(not(windows))]
fn windows_sid() -> Option<String> {
    None
}

/// `base` joined with a `/`-separated relative path, one component at a time (so the
/// same catalogue code builds Windows paths on any host).
pub(crate) fn join(base: &Path, rel: &str) -> PathBuf {
    rel.split('/')
        .filter(|c| !c.is_empty())
        .fold(base.to_path_buf(), |path, c| path.join(c))
}

// ---------------------------------------------------------------------------------------
// Candidates

/// One place a catalogue proposes, before measuring.
#[derive(Debug, Clone)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent per-item flags set by catalogue builders"
)]
pub(crate) struct Candidate {
    pub(crate) kind: JunkKind,
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) tag: Option<ItemTag>,
    pub(crate) safety: Safety,
    /// Remove the children, keep the folder.
    pub(crate) contents_only: bool,
    /// Temp files / logs: only files older than `junk_min_age_hours` count and are removed.
    pub(crate) aged: bool,
    /// `Some` forces the admin flag; `None` = derive it from ownership.
    pub(crate) admin: Option<bool>,
    pub(crate) app_running: bool,
    /// A glob result that yields to more specific locations inside it.
    pub(crate) generic: bool,
    /// A user file (installers): removed with `files_delete`.
    pub(crate) user_file: bool,
    /// Always permanent (Trash contents).
    pub(crate) permanent: bool,
    /// Removed through an OS action instead of the path; kept even when empty.
    pub(crate) special: Option<SpecialAction>,
    /// Already measured by the catalogue (installer files).
    pub(crate) known: Option<Measure>,
}

impl Candidate {
    /// A safe, whole-path candidate.
    pub(crate) fn new(kind: JunkKind, name: impl Into<String>, path: PathBuf) -> Self {
        Self {
            kind,
            name: name.into(),
            path,
            tag: None,
            safety: Safety::Safe,
            contents_only: false,
            aged: false,
            admin: None,
            app_running: false,
            generic: false,
            user_file: false,
            permanent: false,
            special: None,
            known: None,
        }
    }

    /// Named after the last component of `path`.
    pub(crate) fn at(kind: JunkKind, path: PathBuf) -> Self {
        let name = file_name(&path);
        Self::new(kind, name, path)
    }

    #[must_use]
    pub(crate) fn contents(mut self) -> Self {
        self.contents_only = true;
        self
    }

    #[must_use]
    pub(crate) fn aged(mut self) -> Self {
        self.aged = true;
        self
    }

    #[must_use]
    pub(crate) fn review(mut self) -> Self {
        self.safety = Safety::Review;
        self
    }

    #[must_use]
    pub(crate) fn tag(mut self, tag: ItemTag) -> Self {
        self.tag = Some(tag);
        self
    }

    #[must_use]
    pub(crate) fn admin(mut self) -> Self {
        self.admin = Some(true);
        self
    }

    #[must_use]
    pub(crate) fn generic(mut self) -> Self {
        self.generic = true;
        self
    }

    /// Marks the owning app as running; with `skip_running_apps` the item is not
    /// preselected.
    #[must_use]
    pub(crate) fn running(mut self, running: bool, settings: &CleanSettings) -> Self {
        if running {
            self.app_running = true;
            if settings.skip_running_apps {
                self.safety = Safety::Review;
            }
        }
        self
    }
}

/// Last component of `path` as display text.
pub(crate) fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

// ---------------------------------------------------------------------------------------
// Scan context

/// A directory entry (symlinks are never reported).
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) dir: bool,
}

/// Running processes, normalized for matching app folders.
#[derive(Debug, Default)]
pub(crate) struct Running {
    /// Lower-case process names without `.exe`.
    names: HashSet<String>,
    /// Lower-case bundle ids of running macOS apps.
    bundle_ids: HashSet<String>,
}

impl Running {
    fn detect() -> Self {
        let procs = procs::running();
        let mut bundles = HashSet::new();
        let mut running = Self::default();
        for p in &procs {
            running.names.insert(normalize_proc(&p.name));
            if let Some(app) = p.exe.as_deref().and_then(app_bundle)
                && bundles.insert(app.to_path_buf())
            {
                if let Some(name) = app.file_stem() {
                    running.names.insert(name.to_string_lossy().to_lowercase());
                }
                if let Some(id) = bundle_id(app) {
                    running.bundle_ids.insert(id.to_lowercase());
                }
            }
        }
        running
    }

    /// Any of `names` (process names, `.exe` optional, any case) is running.
    pub(crate) fn any(&self, names: &[&str]) -> bool {
        names
            .iter()
            .any(|n| self.names.contains(&normalize_proc(n)))
    }

    /// The app owning a cache folder named `dir_name` (app name, process name or bundle
    /// id) is running.
    pub(crate) fn owns(&self, dir_name: &str) -> bool {
        let lower = dir_name.to_lowercase();
        self.names.contains(&lower) || self.bundle_ids.contains(&lower)
    }

    #[cfg(test)]
    pub(crate) fn with(names: &[&str]) -> Self {
        Self {
            names: names.iter().map(|n| normalize_proc(n)).collect(),
            bundle_ids: HashSet::new(),
        }
    }
}

fn normalize_proc(name: &str) -> String {
    let lower = name.trim().to_lowercase();
    match lower.strip_suffix(".exe") {
        Some(stem) => stem.to_owned(),
        None => lower,
    }
}

/// The `.app` bundle an executable lives in.
fn app_bundle(exe: &Path) -> Option<&Path> {
    exe.ancestors()
        .find(|a| a.extension().is_some_and(|e| e.eq_ignore_ascii_case("app")))
}

/// `CFBundleIdentifier` of an app whose `Info.plist` is XML (binary plists are skipped:
/// the process name still matches).
fn bundle_id(app: &Path) -> Option<String> {
    let text = fs::read_to_string(app.join("Contents").join("Info.plist")).ok()?;
    let (_, after) = text.split_once("<key>CFBundleIdentifier</key>")?;
    let (_, value) = after.split_once("<string>")?;
    let (id, _) = value.split_once("</string>")?;
    Some(id.trim().to_owned())
}

/// What catalogues share while building candidates.
pub(crate) struct Cx<'a> {
    pub(crate) roots: &'a Roots,
    pub(crate) settings: &'a CleanSettings,
    running: OnceCell<Running>,
    /// Places that could not be read.
    pub(crate) denied: Vec<Denied>,
}

impl std::fmt::Debug for Cx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cx")
            .field("roots", &self.roots)
            .field("denied", &self.denied)
            .finish_non_exhaustive()
    }
}

impl<'a> Cx<'a> {
    pub(crate) fn new(roots: &'a Roots, settings: &'a CleanSettings) -> Self {
        Self {
            roots,
            settings,
            running: OnceCell::new(),
            denied: Vec::new(),
        }
    }

    /// Running processes (listed once per scan).
    pub(crate) fn running(&self) -> &Running {
        self.running.get_or_init(Running::detect)
    }

    #[cfg(test)]
    pub(crate) fn set_running(&mut self, running: Running) {
        self.running = OnceCell::from(running);
    }

    /// Records an unreadable place.
    pub(crate) fn deny(&mut self, path: &Path, err: &io::Error) {
        self.denied.push(Denied {
            path: path.display().to_string(),
            reason: errors::classify(err, path),
        });
    }

    /// Children of `dir` (no symlinks), sorted by name. A missing folder is empty; an
    /// unreadable one is recorded as denied.
    pub(crate) fn list(&mut self, dir: &Path) -> Vec<Entry> {
        match fs::read_dir(dir) {
            Ok(entries) => {
                let mut out: Vec<Entry> = entries
                    .filter_map(|entry| {
                        let entry = entry.ok()?;
                        let file_type = entry.file_type().ok()?;
                        if file_type.is_symlink() {
                            return None;
                        }
                        Some(Entry {
                            name: entry.file_name().to_string_lossy().into_owned(),
                            path: entry.path(),
                            dir: file_type.is_dir(),
                        })
                    })
                    .collect();
                out.sort_by(|a, b| a.name.cmp(&b.name));
                out
            }
            Err(err) if is_absent(&err) => Vec::new(),
            Err(err) => {
                self.deny(dir, &err);
                Vec::new()
            }
        }
    }

    /// Child folders of `dir`.
    pub(crate) fn dirs(&mut self, dir: &Path) -> Vec<Entry> {
        let mut out = self.list(dir);
        out.retain(|e| e.dir);
        out
    }
}

/// The error means "not there" (not a permission problem).
fn is_absent(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

// ---------------------------------------------------------------------------------------
// Pipeline

/// Filters, dedupes, measures and assembles candidates into a report.
fn finish(
    candidates: Vec<Candidate>,
    external_claims: &[PathBuf],
    mut cx: Cx<'_>,
    walker: &Walker,
    ctx: &JobCtx,
) -> Result<Scanned<JunkReport>> {
    let candidates = split_generics(candidates, external_claims, &mut cx);
    let candidates = admit(candidates, &mut cx, walker);
    let candidates = dedupe(candidates);
    if ctx.is_cancelled() {
        return Err(Error::Cancelled);
    }
    ctx.set_phase(Phase::Measuring);
    let min_age = min_age_secs(cx.settings);
    let measured = measure::measure(&candidates, min_age, &cx.roots.protected, walker, ctx);
    if ctx.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let mut kept = Vec::with_capacity(candidates.len());
    for (cand, (m, denied)) in candidates.into_iter().zip(measured) {
        if let Some(d) = denied {
            cx.denied.push(d);
        }
        if m.files > 0 || m.bytes > 0 || cand.special.is_some() {
            kept.push((cand, m));
        }
    }
    let Cx {
        settings, denied, ..
    } = cx;
    Ok(assemble(kept, denied, settings, min_age, walker))
}

/// `junk_min_age_hours` in seconds.
fn min_age_secs(settings: &CleanSettings) -> u64 {
    u64::from(settings.junk_min_age_hours).saturating_mul(3600)
}

/// Replaces generic candidates that contain a claimed path by their children (recursively,
/// only along the claimed branch) and drops generic candidates inside a claimed path.
fn split_generics(
    candidates: Vec<Candidate>,
    external_claims: &[PathBuf],
    cx: &mut Cx<'_>,
) -> Vec<Candidate> {
    let claims: Vec<PathBuf> = candidates
        .iter()
        .filter(|c| !c.generic && c.special.is_none())
        .map(|c| c.path.clone())
        .chain(external_claims.iter().cloned())
        .collect();
    let mut out = Vec::with_capacity(candidates.len());
    for cand in candidates {
        if cand.generic {
            split_one(cand, &claims, cx, &mut out);
        } else {
            out.push(cand);
        }
    }
    out
}

fn split_one(cand: Candidate, claims: &[PathBuf], cx: &mut Cx<'_>, out: &mut Vec<Candidate>) {
    if claims.iter().any(|k| paths::is_within(&cand.path, k)) {
        return;
    }
    if !claims.iter().any(|k| paths::is_within(k, &cand.path)) {
        out.push(cand);
        return;
    }
    for entry in cx.list(&cand.path) {
        let mut child = cand.clone();
        child.name = format!("{}/{}", cand.name, entry.name);
        child.contents_only = cand.contents_only && entry.dir;
        child.path = entry.path;
        split_one(child, claims, cx, out);
    }
}

/// Keeps candidates that exist, are allowed (exclusions, guard, `include_system`) and are
/// not symlinks; fills in the admin flag. Unreadable ones become denied entries.
fn admit(candidates: Vec<Candidate>, cx: &mut Cx<'_>, walker: &Walker) -> Vec<Candidate> {
    let guard = Guard::new(cx.settings);
    let mut out = Vec::with_capacity(candidates.len());
    for mut cand in candidates {
        if walker.options().is_excluded(&cand.path) {
            continue;
        }
        let meta = match fs::symlink_metadata(&cand.path) {
            Ok(meta) if meta.file_type().is_symlink() => continue,
            Ok(meta) => Some(meta),
            Err(err) => {
                if !is_absent(&err) {
                    cx.deny(&cand.path, &err);
                }
                None
            }
        };
        if cand.special.is_some() {
            // Removed through the OS; the path only sizes it.
            cand.admin = Some(cand.admin.unwrap_or(false));
            out.push(cand);
            continue;
        }
        let Some(meta) = meta else { continue };
        if measure::surely_empty(&meta) {
            continue;
        }
        if cand.contents_only && !meta.is_dir() {
            cand.contents_only = false;
        }
        let checked = if cand.contents_only {
            cand.path.join("x")
        } else {
            cand.path.clone()
        };
        if let Err(refusal) = guard.check(&checked) {
            tracing::debug!(path = %cand.path.display(), %refusal, "junk candidate refused by guard");
            continue;
        }
        let admin = cand
            .admin
            .unwrap_or_else(|| needs_admin(cx.roots, &cand.path, &meta, cand.contents_only));
        if admin && !cx.settings.include_system {
            continue;
        }
        cand.admin = Some(admin);
        out.push(cand);
    }
    out
}

/// Whether removing `path` (or its contents) needs administrator rights.
fn needs_admin(roots: &Roots, path: &Path, meta: &Metadata, contents_only: bool) -> bool {
    if roots.os == Os::Windows {
        return roots.admin_trees.iter().any(|t| paths::is_within(path, t));
    }
    unix_needs_admin(roots.uid, path, meta, contents_only)
}

#[cfg(unix)]
fn unix_needs_admin(uid: Option<u32>, path: &Path, meta: &Metadata, contents_only: bool) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let Some(uid) = uid else { return false };
    if uid == 0 {
        return false;
    }
    let writable =
        |m: &Metadata| (m.uid() == uid && m.mode() & 0o200 != 0) || m.mode() & 0o002 != 0;
    if contents_only {
        return meta.uid() != uid || !writable(meta);
    }
    let parent_ok = path
        .parent()
        .and_then(|p| fs::metadata(p).ok())
        .is_some_and(|m| writable(&m));
    !parent_ok || meta.uid() != uid
}

#[cfg(not(unix))]
fn unix_needs_admin(
    _uid: Option<u32>,
    _path: &Path,
    _meta: &Metadata,
    _contents_only: bool,
) -> bool {
    false
}

/// Removes duplicates and candidates nested in another one (the outer one wins).
fn dedupe(mut candidates: Vec<Candidate>) -> Vec<Candidate> {
    candidates.sort_by(|a, b| a.path.cmp(&b.path));
    let mut out: Vec<Candidate> = Vec::with_capacity(candidates.len());
    let mut kept_paths: Vec<PathBuf> = Vec::new();
    for cand in candidates {
        let nested = kept_paths.last().is_some_and(|last| {
            paths::is_within(&cand.path, last) || paths::is_within(last, &cand.path)
        }) || (cfg!(any(windows, target_os = "macos"))
            && kept_paths
                .iter()
                .any(|k| paths::is_within(&cand.path, k) || paths::is_within(k, &cand.path)));
        if nested {
            continue;
        }
        kept_paths.push(cand.path.clone());
        out.push(cand);
    }
    out
}

/// A group's kind, total size and measured candidates.
type SizedGroup = (JunkKind, u64, Vec<(Candidate, Measure)>);

/// Groups, sorts (largest first) and numbers the items; builds one target per item.
fn assemble(
    kept: Vec<(Candidate, Measure)>,
    mut denied: Vec<Denied>,
    settings: &CleanSettings,
    min_age: u64,
    walker: &Walker,
) -> Scanned<JunkReport> {
    let mut groups: BTreeMap<JunkKind, Vec<(Candidate, Measure)>> = BTreeMap::new();
    for (cand, m) in kept {
        groups.entry(cand.kind).or_default().push((cand, m));
    }
    let mut groups: Vec<SizedGroup> = groups
        .into_iter()
        .map(|(kind, mut items)| {
            items.sort_by(|(a, am), (b, bm)| {
                bm.bytes
                    .cmp(&am.bytes)
                    .then_with(|| a.name.cmp(&b.name))
                    .then_with(|| a.path.cmp(&b.path))
            });
            let bytes = items
                .iter()
                .fold(0_u64, |sum, (_, m)| sum.saturating_add(m.bytes));
            (kind, bytes, items)
        })
        .collect();
    groups.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let mut targets = Vec::new();
    let mut report = JunkReport::default();
    for (kind, _, items) in groups {
        let mut out = Vec::with_capacity(items.len());
        for (cand, m) in items {
            let path = cand.path.display().to_string();
            let method = if cand.permanent {
                DeleteMethod::Permanent
            } else if cand.user_file {
                settings.files_delete
            } else {
                settings.junk_delete
            };
            let admin = cand.admin.unwrap_or(false);
            let (location, target) = if let Some(action) = &cand.special {
                let location = Location::Special {
                    action: action.clone(),
                };
                (location.clone(), Target::location(location, m.bytes))
            } else {
                let target = if cand.contents_only {
                    Target::contents(path.clone(), m.bytes, method)
                } else {
                    Target::path(path.clone(), m.bytes, method)
                };
                let age = if cand.aged { min_age } else { 0 };
                (Location::Path { path }, target.older_than(age))
            };
            let id = push_target(&mut targets, target.admin(admin));
            out.push(JunkItem {
                id,
                name: cand.name,
                location,
                tag: cand.tag,
                bytes: m.bytes,
                files: m.files,
                modified: m.newest,
                safety: cand.safety,
                needs_admin: admin,
                app_running: cand.app_running,
                ident: None,
                icon: None,
            });
        }
        report.groups.push(JunkGroup { kind, items: out });
    }
    let mut seen = HashSet::new();
    denied.retain(|d| {
        !walker.options().is_excluded(Path::new(&d.path)) && seen.insert(d.path.clone())
    });
    report.denied = denied;
    Scanned { report, targets }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::walk::WalkOptions;

    /// A unique, empty temp dir for one test, removed when dropped (also when the test
    /// fails or returns early).
    #[derive(Debug)]
    pub(crate) struct Fixture(PathBuf);

    impl std::ops::Deref for Fixture {
        type Target = Path;

        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            if let Err(err) = fs::remove_dir_all(&self.0) {
                tracing::debug!(%err, dir = %self.0.display(), "fixture cleanup");
            }
        }
    }

    pub(crate) fn temp_dir(name: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("omc-junk-{name}-{}", std::process::id()));
        let _ignored = fs::remove_dir_all(&dir);
        assert!(fs::create_dir_all(&dir).is_ok(), "mkdir {}", dir.display());
        Fixture(dir)
    }

    /// Writes `len` bytes to `path`, creating parents.
    pub(crate) fn put(path: &Path, len: usize) {
        if let Some(parent) = path.parent() {
            assert!(
                fs::create_dir_all(parent).is_ok(),
                "mkdir {}",
                parent.display()
            );
        }
        assert!(
            fs::write(path, vec![1_u8; len]).is_ok(),
            "write {}",
            path.display()
        );
    }

    pub(crate) fn walker() -> Walker {
        let walker = Walker::new(WalkOptions::default());
        assert!(walker.is_ok(), "pool builds");
        match walker {
            Ok(walker) => walker,
            Err(err) => panic_free_abort(&err),
        }
    }

    /// Tests cannot continue without a pool (the assert above already failed).
    fn panic_free_abort(err: &Error) -> ! {
        tracing::error!(%err, "no walker");
        std::process::abort()
    }

    /// Runs the pipeline on hand-made candidates.
    pub(crate) fn run(
        roots: &Roots,
        settings: &CleanSettings,
        candidates: Vec<Candidate>,
        claims: &[PathBuf],
    ) -> Scanned<JunkReport> {
        let cx = Cx::new(roots, settings);
        let result = finish(candidates, claims, cx, &walker(), &JobCtx::new());
        assert!(result.is_ok(), "pipeline runs");
        let Ok(scanned) = result else {
            return Scanned {
                report: JunkReport::default(),
                targets: Vec::new(),
            };
        };
        scanned
    }

    /// Every item of a report.
    pub(crate) fn items(report: &JunkReport) -> Vec<&JunkItem> {
        report.groups.iter().flat_map(|g| g.items.iter()).collect()
    }

    #[test]
    fn ids_match_targets_after_sorting() {
        let base = temp_dir("ids");
        let roots = Roots::fake(&base, Os::CURRENT);
        let caches = roots.home("caches");
        put(&caches.join("small/a"), 100);
        put(&caches.join("big/a"), 50_000);
        put(&caches.join("mid/a"), 5_000);
        put(&roots.home("logs/x/log"), 20_000);
        let mut cands: Vec<Candidate> = ["small", "big", "mid"]
            .iter()
            .map(|n| Candidate::at(JunkKind::UserCache, caches.join(n)).contents())
            .collect();
        cands.push(Candidate::at(JunkKind::UserLog, roots.home("logs/x")));
        cands.push(Candidate::at(JunkKind::UserLog, roots.home("logs/empty")));
        let scanned = run(&roots, &CleanSettings::default(), cands, &[]);
        let groups: Vec<JunkKind> = scanned.report.groups.iter().map(|g| g.kind).collect();
        assert_eq!(
            groups,
            vec![JunkKind::UserCache, JunkKind::UserLog],
            "groups largest first"
        );
        let names: Vec<&str> = items(&scanned.report)
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["big", "mid", "small", "x"],
            "items largest first"
        );
        for (n, item) in items(&scanned.report).iter().enumerate() {
            assert_eq!(
                usize::try_from(item.id).ok(),
                Some(n),
                "ids in display order"
            );
            let target = scanned.targets.get(n);
            assert_eq!(
                target.map(|t| &t.location),
                Some(&item.location),
                "targets[id] belongs to item {}",
                item.name
            );
            assert_eq!(target.map(|t| t.bytes), Some(item.bytes), "target size");
        }
        assert!(
            scanned.targets.first().is_some_and(|t| t.contents_only),
            "contents flag carried"
        );
    }

    #[test]
    fn nested_and_duplicate_paths_are_reported_once() {
        let base = temp_dir("dedupe");
        let roots = Roots::fake(&base, Os::CURRENT);
        let outer = roots.home("c/app");
        put(&outer.join("inner/f"), 1000);
        put(&outer.join("g"), 1000);
        let cands = vec![
            Candidate::at(JunkKind::UserCache, outer.join("inner")),
            Candidate::at(JunkKind::UserCache, outer.clone()),
            Candidate::at(JunkKind::UserLog, outer.clone()),
        ];
        let scanned = run(&roots, &CleanSettings::default(), cands, &[]);
        let all = items(&scanned.report);
        assert_eq!(all.len(), 1, "one item survives: {all:?}");
        assert_eq!(
            all.first().and_then(|i| i.location.as_path()),
            Some(outer.display().to_string().as_str()),
            "the outer path wins"
        );
        assert_eq!(
            all.first().map(|i| i.files),
            Some(2),
            "outer counts all files"
        );
    }

    #[test]
    fn generic_globs_split_around_claimed_paths() {
        let base = temp_dir("split");
        let roots = Roots::fake(&base, Os::CURRENT);
        let vendor = roots.home("caches/Google");
        put(&vendor.join("Chrome/Default/Cache/f"), 1000);
        put(&vendor.join("Drive/f"), 1000);
        put(&roots.home("caches/Homebrew/f"), 1000);
        let cands = vec![
            Candidate::at(JunkKind::UserCache, vendor.clone())
                .contents()
                .generic(),
            Candidate::at(JunkKind::UserCache, roots.home("caches/Homebrew"))
                .contents()
                .generic(),
            Candidate::at(JunkKind::PackageCache, roots.home("caches/Homebrew")).contents(),
        ];
        let scanned = run(
            &roots,
            &CleanSettings::default(),
            cands,
            &[vendor.join("Chrome")],
        );
        let all = items(&scanned.report);
        let names: Vec<(&str, JunkKind)> = scanned
            .report
            .groups
            .iter()
            .flat_map(|g| g.items.iter().map(move |i| (i.name.as_str(), g.kind)))
            .collect();
        assert_eq!(all.len(), 2, "Chrome is left to its owner: {names:?}");
        assert!(
            names.contains(&("Google/Drive", JunkKind::UserCache)),
            "sibling of the claimed path kept: {names:?}"
        );
        assert!(
            names.contains(&("Homebrew", JunkKind::PackageCache)),
            "specific entry wins over the glob: {names:?}"
        );
    }

    #[test]
    fn exclusions_and_age_limits_apply() {
        let base = temp_dir("age");
        let roots = Roots::fake(&base, Os::CURRENT);
        put(&roots.home("tmp/new"), 1000);
        put(&roots.home("ex/f"), 1000);
        let settings = CleanSettings {
            exclude: vec![roots.home("ex").display().to_string()],
            ..CleanSettings::default()
        };
        let walker = Walker::new(WalkOptions::from_settings(&settings));
        assert!(walker.is_ok(), "pool builds");
        let Ok(walker) = walker else { return };
        let cands = vec![
            Candidate::at(JunkKind::TempFiles, roots.home("tmp"))
                .contents()
                .aged(),
            Candidate::at(JunkKind::UserCache, roots.home("ex")),
        ];
        let result = finish(
            cands,
            &[],
            Cx::new(&roots, &settings),
            &walker,
            &JobCtx::new(),
        );
        assert!(
            result.as_ref().is_ok_and(|s| s.report.groups.is_empty()),
            "fresh temp files and excluded paths yield nothing: {result:?}"
        );
        let no_age = CleanSettings {
            junk_min_age_hours: 0,
            ..CleanSettings::default()
        };
        let scanned = run(
            &roots,
            &no_age,
            vec![
                Candidate::at(JunkKind::TempFiles, roots.home("tmp"))
                    .contents()
                    .aged(),
            ],
            &[],
        );
        assert_eq!(
            scanned
                .targets
                .first()
                .map(|t| (t.min_age_secs, t.contents_only)),
            Some((0, true)),
            "age 0 counts everything"
        );
    }
}
