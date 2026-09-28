//! Trash / Recycle Bin: the user's trash on the home volume and on other mounted volumes.
//! Emptying the trash is permanent by nature. When the trash cannot be read (macOS
//! without Full Disk Access) a size-less item asks the OS to empty it instead.

use std::fs;
use std::path::{Path, PathBuf};

use omc_proto::jobs::SpecialAction;
use omc_proto::junk::JunkKind;

use super::{Candidate, Cx, Os, is_absent, join};

pub(super) fn catalogue(cx: &mut Cx<'_>) -> Vec<Candidate> {
    let r = cx.roots;
    let mut out = Vec::new();
    match r.os {
        Os::Mac => {
            home_trash(cx, "Trash", r.home(".Trash"), &mut out);
            if let Some(uid) = r.uid {
                for v in cx.dirs(&r.sys("Volumes")) {
                    let dir = v.path.join(".Trashes").join(uid.to_string());
                    out.push(bin(v.name, dir));
                }
            }
        }
        Os::Windows => {
            for drive in &r.drives {
                let name = drive
                    .display()
                    .to_string()
                    .trim_end_matches(['\\', '/'])
                    .to_owned();
                let root = drive.join("$Recycle.Bin");
                match user_bin(cx, &root) {
                    Some(dir) => out.push(bin(name, dir)),
                    None if *drive == r.sys => out.push(special(name, root)),
                    None => {}
                }
            }
        }
        Os::Linux => {
            home_trash(cx, "Trash", join(&r.data, "Trash"), &mut out);
            if let Some(uid) = r.uid {
                let mut volumes = Vec::new();
                for user in cx.dirs(&r.sys("media")) {
                    volumes.extend(cx.dirs(&user.path));
                }
                if let Some(user) = &r.user {
                    volumes.extend(cx.dirs(&r.sys("run/media").join(user)));
                }
                volumes.extend(cx.dirs(&r.sys("mnt")));
                for v in volumes {
                    out.push(bin(v.name.clone(), v.path.join(format!(".Trash-{uid}"))));
                    out.push(bin(v.name, v.path.join(".Trash").join(uid.to_string())));
                }
            }
        }
    }
    out
}

/// A trash folder whose contents are removed permanently.
fn bin(name: String, dir: PathBuf) -> Candidate {
    let mut cand = Candidate::new(JunkKind::Trash, name, dir).contents();
    cand.permanent = true;
    cand
}

/// "Empty the Trash through the OS" (sized 0 when the trash is unreadable).
fn special(name: String, dir: PathBuf) -> Candidate {
    let mut cand = Candidate::new(JunkKind::Trash, name, dir);
    cand.permanent = true;
    cand.special = Some(SpecialAction::EmptyTrash);
    cand
}

/// The home trash: its contents when readable, else the OS action (and a denied entry).
fn home_trash(cx: &mut Cx<'_>, name: &str, dir: PathBuf, out: &mut Vec<Candidate>) {
    match fs::read_dir(&dir) {
        Ok(_) => out.push(bin(name.to_owned(), dir)),
        Err(err) if is_absent(&err) => {}
        Err(err) => {
            cx.deny(&dir, &err);
            out.push(special(name.to_owned(), dir));
        }
    }
}

/// The current user's folder in a drive's `$Recycle.Bin`: named by the SID when known,
/// otherwise the only user folder this account can list.
fn user_bin(cx: &mut Cx<'_>, root: &Path) -> Option<PathBuf> {
    if let Some(sid) = &cx.roots.sid {
        let dir = root.join(sid);
        return dir.is_dir().then_some(dir);
    }
    let candidates: Vec<PathBuf> = cx
        .dirs(root)
        .into_iter()
        .filter(|e| e.name.starts_with("S-1-5-21-"))
        .map(|e| e.path)
        .filter(|p| fs::read_dir(p).is_ok())
        .collect();
    if let [only] = candidates.as_slice() {
        return Some(only.clone());
    }
    if !candidates.is_empty() {
        tracing::debug!(
            root = %root.display(),
            count = candidates.len(),
            "several readable Recycle Bin folders; using the OS action"
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use omc_proto::jobs::{DeleteMethod, Location};
    use omc_proto::settings::CleanSettings;

    use super::super::Roots;
    use super::super::tests::{items, put, run, temp_dir};
    use super::*;

    #[test]
    fn trash_contents_are_removed_permanently() {
        let base = temp_dir("trash");
        let mut roots = Roots::fake(&base, Os::Linux);
        roots.uid = Some(1000);
        put(&roots.home(".local/share/Trash/files/a.txt"), 4096);
        put(&roots.home(".local/share/Trash/info/a.txt.trashinfo"), 10);
        put(&roots.sys("media/me/USB/.Trash-1000/files/b"), 4096);
        put(&roots.sys("media/me/USB/.Trash-1001/files/c"), 4096);
        let settings = CleanSettings::default();
        let mut cx = Cx::new(&roots, &settings);
        let cands = catalogue(&mut cx);
        let scanned = run(&roots, &settings, cands, &[]);
        let all = items(&scanned.report);
        let names: Vec<&str> = all.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names.len(), 2, "home trash + own volume trash: {names:?}");
        assert!(
            names.contains(&"USB"),
            "volume trash named by volume: {names:?}"
        );
        assert!(
            scanned
                .targets
                .iter()
                .all(|t| t.contents_only && t.method == DeleteMethod::Permanent),
            "trash is emptied permanently: {:?}",
            scanned.targets
        );
        assert!(
            !all.iter().any(
                |i| matches!(&i.location, Location::Path { path } if path.contains(".Trash-1001"))
            ),
            "other users' trash untouched"
        );
    }

    #[test]
    fn windows_bin_falls_back_to_the_os_action() {
        let base = temp_dir("trash-win");
        let roots = Roots::fake(&base, Os::Windows);
        assert!(
            fs::create_dir_all(roots.sys.join("$Recycle.Bin")).is_ok(),
            "mkdir bin"
        );
        let settings = CleanSettings::default();
        let mut cx = Cx::new(&roots, &settings);
        let cands = catalogue(&mut cx);
        let scanned = run(&roots, &settings, cands, &[]);
        let all = items(&scanned.report);
        assert!(
            matches!(
                all.first().map(|i| &i.location),
                Some(Location::Special {
                    action: SpecialAction::EmptyTrash
                })
            ) && all.len() == 1,
            "no user folder → empty through the OS: {all:?}"
        );
    }
}
