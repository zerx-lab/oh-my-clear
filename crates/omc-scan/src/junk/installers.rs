//! Installers: disk images, installer packages and setup programs left in Downloads,
//! Desktop, Documents and the configured file roots (depth-limited). They are user files:
//! removed with `files_delete` and preselected only when they are plain installer formats
//! sitting in Downloads for over a week.

use std::io;
use std::path::{Path, PathBuf};

use omc_proto::jobs::Denied;
use omc_proto::junk::{ItemTag, JunkKind, Safety};
use parking_lot::Mutex;

use super::{Candidate, Cx, file_name};
use crate::ctx::JobCtx;
use crate::walk::{FileMeta, Measure, Visitor, Walker};
use crate::{errors, paths};

/// How deep below each root installers are searched.
const MAX_DEPTH: u32 = 4;
/// `.img` files smaller than this are rarely disk images.
const MIN_IMG_BYTES: u64 = 50 << 20;
/// Installers in Downloads older than this are preselected.
const SAFE_AGE_SECS: i64 = 7 * 86_400;
/// Formats that are only ever installers (safe to preselect when old and in Downloads).
const PLAIN_INSTALLERS: [&str; 8] = ["dmg", "pkg", "msi", "deb", "rpm", "msix", "appx", "iso"];

/// What a file named `name` of `len` bytes is, if an installer.
fn classify(name: &str, len: u64) -> Option<JunkKind> {
    let lower = name.to_lowercase();
    let (stem, ext) = lower.rsplit_once('.')?;
    let kind = match ext {
        "dmg" | "iso" | "xip" => JunkKind::DiskImage,
        "img" if len >= MIN_IMG_BYTES => JunkKind::DiskImage,
        "pkg" | "mpkg" | "msi" | "msix" | "msixbundle" | "appx" | "appxbundle" | "deb" | "rpm" => {
            JunkKind::InstallerPackage
        }
        "exe" | "zip"
            if ["setup", "install", "update"]
                .iter()
                .any(|w| stem.contains(w)) =>
        {
            JunkKind::SetupProgram
        }
        _ => return None,
    };
    Some(kind)
}

/// `path` has extension `ext` (any case).
fn has_ext(path: &Path, ext: &str) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

struct Finder<'a> {
    downloads: &'a Path,
    week_ago: i64,
    found: Mutex<Vec<Candidate>>,
    denied: Mutex<Vec<Denied>>,
}

impl Finder<'_> {
    fn push(&self, kind: JunkKind, path: &Path, known: Option<Measure>) {
        let name = file_name(path);
        let ext = name
            .rsplit_once('.')
            .map(|(_, e)| e.to_lowercase())
            .unwrap_or_default();
        let old = known
            .and_then(|m| m.newest)
            .is_some_and(|m| m <= self.week_ago);
        let safe = PLAIN_INSTALLERS.contains(&ext.as_str())
            && paths::is_within(path, self.downloads)
            && old;
        let mut cand = Candidate::new(kind, name, path.to_path_buf()).tag(ItemTag::Download);
        cand.safety = if safe { Safety::Safe } else { Safety::Review };
        cand.user_file = true;
        cand.known = known;
        self.found.lock().push(cand);
    }
}

impl Visitor for Finder<'_> {
    fn enter_dir(&self, dir: &Path, depth: u32) -> bool {
        let name = file_name(dir);
        if name.starts_with('.') || name == "node_modules" || has_ext(dir, "app") {
            return false;
        }
        // Old-style bundle packages are folders.
        if let Some(kind) = classify(&name, 0)
            && kind == JunkKind::InstallerPackage
        {
            self.push(kind, dir, None);
            return false;
        }
        depth < MAX_DEPTH
    }

    fn file(&self, path: &Path, meta: &FileMeta) {
        let name = file_name(path);
        if name.starts_with('.') {
            return;
        }
        if let Some(kind) = classify(&name, meta.len) {
            let known = Measure {
                bytes: meta.size,
                files: 1,
                newest: meta.modified,
                incomplete: false,
                missing: false,
            };
            self.push(kind, path, Some(known));
        }
    }

    fn denied(&self, path: &Path, err: &io::Error) {
        self.denied.lock().push(Denied {
            path: path.display().to_string(),
            reason: errors::classify(err, path),
        });
    }
}

/// Downloads, Desktop, Documents and the file roots, nested roots removed.
fn search_roots(cx: &Cx<'_>) -> Vec<PathBuf> {
    let r = cx.roots;
    let mut roots: Vec<PathBuf> = ["Downloads", "Desktop", "Documents"]
        .iter()
        .map(|d| r.home.join(d))
        .chain(
            cx.settings
                .file_roots
                .iter()
                .filter_map(|p| paths::normalize(&paths::expand(p))),
        )
        .collect();
    roots.sort();
    let mut out: Vec<PathBuf> = Vec::with_capacity(roots.len());
    for root in roots {
        if !out.iter().any(|kept| paths::is_within(&root, kept)) {
            out.push(root);
        }
    }
    out
}

pub(super) fn catalogue(cx: &mut Cx<'_>, walker: &Walker, ctx: &JobCtx) -> Vec<Candidate> {
    let roots = search_roots(cx);
    let downloads = cx.roots.home.join("Downloads");
    let finder = Finder {
        downloads: &downloads,
        week_ago: paths::now_secs().saturating_sub(SAFE_AGE_SECS),
        found: Mutex::new(Vec::new()),
        denied: Mutex::new(Vec::new()),
    };
    walker.walk(&roots, ctx, &finder);
    cx.denied.extend(finder.denied.into_inner());
    finder.found.into_inner()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{Duration, SystemTime};

    use omc_proto::jobs::DeleteMethod;
    use omc_proto::settings::CleanSettings;

    use super::super::tests::{items, put, run, temp_dir, walker};
    use super::super::{Os, Roots};
    use super::*;

    #[test]
    fn installer_names_are_classified() {
        assert_eq!(classify("App.dmg", 1), Some(JunkKind::DiskImage), "dmg");
        assert_eq!(classify("disk.img", 1), None, "small img");
        assert_eq!(
            classify("disk.img", MIN_IMG_BYTES),
            Some(JunkKind::DiskImage),
            "big img"
        );
        assert_eq!(
            classify("tool.MSI", 1),
            Some(JunkKind::InstallerPackage),
            "case-insensitive"
        );
        assert_eq!(
            classify("VSCodeUserSetup-x64.exe", 1),
            Some(JunkKind::SetupProgram),
            "setup exe"
        );
        assert_eq!(classify("game.exe", 1), None, "plain exe is an app");
        assert_eq!(classify("photos.zip", 1), None, "plain zip");
        assert_eq!(
            classify("Thing.AppImage", 1),
            None,
            "AppImage may be the app"
        );
    }

    #[test]
    fn old_downloads_are_preselected_others_reviewed() {
        let base = temp_dir("installers");
        let roots = Roots::fake(&base, Os::CURRENT);
        let old_dmg = roots.home("Downloads/Old.dmg");
        put(&old_dmg, 4096);
        let old = SystemTime::now().checked_sub(Duration::from_hours(30 * 24));
        let file = fs::File::options().write(true).open(&old_dmg);
        assert!(old.is_some() && file.is_ok(), "open dmg");
        if let (Some(old), Ok(file)) = (old, file) {
            assert!(file.set_modified(old).is_ok(), "age dmg");
        }
        put(&roots.home("Downloads/New.pkg"), 4096);
        put(&roots.home("Desktop/sub/setup.exe"), 4096);
        put(&roots.home("Documents/a/b/c/d/e/deep.dmg"), 4096);
        put(&roots.home("Documents/.hidden/x.dmg"), 4096);
        let settings = CleanSettings::default();
        let mut cx = Cx::new(&roots, &settings);
        let cands = catalogue(&mut cx, &walker(), &JobCtx::new());
        let scanned = run(&roots, &settings, cands, &[]);
        let all = items(&scanned.report);
        let find = |name: &str| all.iter().find(|i| i.name == name);
        assert_eq!(
            find("Old.dmg").map(|i| i.safety),
            Some(Safety::Safe),
            "old dmg in Downloads: {all:?}"
        );
        assert_eq!(
            find("New.pkg").map(|i| i.safety),
            Some(Safety::Review),
            "fresh pkg: {all:?}"
        );
        assert_eq!(
            find("setup.exe").map(|i| i.safety),
            Some(Safety::Review),
            "setup exe: {all:?}"
        );
        assert!(
            find("deep.dmg").is_none(),
            "beyond the depth limit: {all:?}"
        );
        assert!(find("x.dmg").is_none(), "hidden folders skipped: {all:?}");
        assert!(
            scanned
                .targets
                .iter()
                .all(|t| t.method == DeleteMethod::Trash && !t.contents_only),
            "installers go to the Trash by default: {:?}",
            scanned.targets
        );
    }
}
