//! Windows programs installed with Wine. `winemenubuilder` mirrors each Start Menu
//! shortcut as `~/.local/share/applications/wine/Programs/<Folder>/<Name>.desktop`
//! (plus a `.menu` file in `~/.config/menus/applications-merged/` and a `.directory` file
//! per folder), whose `Exec` runs the shortcut (`.lnk`) or the program inside the Wine
//! prefix. One app per Start Menu folder; its install folder comes from the shortcut's
//! target (MS-SHLLINK `LinkInfo.LocalBasePath`).

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use super::common::{self, file_name};
use super::desktop::DesktopEntry;

/// Depth of `applications/wine` walked.
const MAX_DEPTH: usize = 6;
/// Largest shortcut read.
const MAX_LNK: u64 = 1024 * 1024;

/// What the Linux module keeps about a Wine app.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct WineDetail {
    /// The Wine prefix (`WINEPREFIX`, default `~/.wine`).
    pub(super) prefix: PathBuf,
    /// The program's install folder inside (or mapped into) the prefix.
    pub(super) dir: Option<PathBuf>,
    /// The Start Menu folder of its desktop entries (`…/wine/Programs/<Folder>`), when the
    /// app has one of its own.
    pub(super) folder: Option<PathBuf>,
    /// The Windows shortcuts its entries run.
    pub(super) links: Vec<PathBuf>,
}

/// One Wine app before it becomes an app record.
#[derive(Debug, Default)]
pub(super) struct WineApp {
    /// Its desktop entries, primary first.
    pub(super) entries: Vec<DesktopEntry>,
    /// Programs the entries start (Unix paths, existing or not).
    pub(super) programs: Vec<PathBuf>,
    /// Prefix, install folder, menu folder and shortcuts.
    pub(super) detail: WineDetail,
}

/// Splits an `Exec` value the way the shell `winemenubuilder` targets does: a backslash
/// escapes the next character everywhere (`C:\\Program\ Files` → `C:\Program Files`),
/// double quotes group.
pub(super) fn shell_args(exec: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut quoted = false;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
                started = true;
            }
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        args.push(current);
    }
    args
}

/// Whether `arg` is an absolute Windows path (`C:\…`).
fn is_windows_path(arg: &str) -> bool {
    let mut chars = arg.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.next() == Some(':')
        && chars.next() == Some('\\')
}

/// What a Wine `Exec` runs.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct WineExec {
    /// `WINEPREFIX=` value.
    pub(super) prefix: Option<PathBuf>,
    /// Windows paths started (programs or shortcuts), skipping `start.exe` itself.
    pub(super) windows: Vec<String>,
    /// Unix paths after `start /Unix`.
    pub(super) unix: Vec<PathBuf>,
}

/// Parses an `Exec` that runs `wine`; `None` when it does not.
pub(super) fn parse_exec(exec: &str) -> Option<WineExec> {
    let args = shell_args(exec);
    let wine = args.iter().position(|a| {
        file_name(Path::new(a)).is_some_and(|n| n == "wine" || n == "wine64" || n == "wine-stable")
    })?;
    let mut out = WineExec {
        prefix: args.iter().take(wine).find_map(|a| {
            let value = a.strip_prefix("WINEPREFIX=")?;
            Path::new(value).is_absolute().then(|| PathBuf::from(value))
        }),
        ..WineExec::default()
    };
    let mut rest = args.iter().skip(wine.saturating_add(1)).peekable();
    while let Some(arg) = rest.next() {
        if arg.eq_ignore_ascii_case("/unix") {
            out.unix.extend(rest.next().map(PathBuf::from));
        } else if is_windows_path(arg) {
            let lower = arg.to_lowercase();
            if !lower.ends_with("\\start.exe") {
                out.windows.push(arg.clone());
            }
        }
    }
    Some(out)
}

/// Unix path of a Windows path inside `prefix` (`C:` → `drive_c`, other drives through
/// `dosdevices/<x>:`).
pub(super) fn to_unix(prefix: &Path, windows: &str) -> Option<PathBuf> {
    let (drive, rest) = windows.split_once(":\\")?;
    let drive = drive.to_lowercase();
    if drive.len() != 1 {
        return None;
    }
    let mut path = if drive == "c" {
        prefix.join("drive_c")
    } else {
        prefix.join("dosdevices").join(format!("{drive}:"))
    };
    for part in rest.split('\\').filter(|p| !p.is_empty()) {
        if part == "." || part == ".." {
            return None;
        }
        path.push(part);
    }
    Some(path)
}

fn le_u16(bytes: &[u8], at: usize) -> Option<u16> {
    let end = at.checked_add(2)?;
    let raw: [u8; 2] = bytes.get(at..end)?.try_into().ok()?;
    Some(u16::from_le_bytes(raw))
}

fn le_u32(bytes: &[u8], at: usize) -> Option<usize> {
    let end = at.checked_add(4)?;
    let raw: [u8; 4] = bytes.get(at..end)?.try_into().ok()?;
    usize::try_from(u32::from_le_bytes(raw)).ok()
}

/// NUL-terminated single-byte string at `at` (Latin-1 decoded).
fn ansi_at(bytes: &[u8], at: usize) -> Option<String> {
    let tail = bytes.get(at..)?;
    let end = tail.iter().position(|b| *b == 0)?;
    Some(tail.get(..end)?.iter().map(|b| char::from(*b)).collect())
}

/// NUL-terminated UTF-16LE string at `at`.
fn utf16_at(bytes: &[u8], at: usize) -> Option<String> {
    let tail = bytes.get(at..)?;
    let units: Vec<u16> = tail
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .take_while(|u| *u != 0)
        .collect();
    String::from_utf16(&units).ok()
}

/// Target of a Windows shortcut (`.lnk`, MS-SHLLINK): `LinkInfo` local base path plus
/// common path suffix; `None` without a local target.
pub(super) fn lnk_target(bytes: &[u8]) -> Option<String> {
    const HEADER: usize = 0x4C;
    const HAS_ID_LIST: usize = 0x1;
    const HAS_LINK_INFO: usize = 0x2;
    const VOLUME_AND_LOCAL_PATH: usize = 0x1;
    if le_u32(bytes, 0)? != HEADER {
        return None;
    }
    let flags = le_u32(bytes, 0x14)?;
    let mut info = HEADER;
    if flags & HAS_ID_LIST != 0 {
        let size = usize::from(le_u16(bytes, info)?);
        info = info.checked_add(2)?.checked_add(size)?;
    }
    if flags & HAS_LINK_INFO == 0 {
        return None;
    }
    let size = le_u32(bytes, info)?;
    let info_bytes = bytes.get(info..info.checked_add(size)?)?;
    let header_size = le_u32(info_bytes, 4)?;
    if le_u32(info_bytes, 8)? & VOLUME_AND_LOCAL_PATH == 0 {
        return None;
    }
    let unicode = if header_size >= 0x24 {
        let base = le_u32(info_bytes, 0x1C)?;
        let suffix = le_u32(info_bytes, 0x20)?;
        (base != 0)
            .then(|| {
                let b = utf16_at(info_bytes, base)?;
                let s = if suffix == 0 {
                    Some(String::new())
                } else {
                    utf16_at(info_bytes, suffix)
                };
                Some((b, s.unwrap_or_default()))
            })
            .flatten()
    } else {
        None
    };
    let (base, suffix) = if let Some(pair) = unicode {
        pair
    } else {
        let base = ansi_at(info_bytes, le_u32(info_bytes, 0x10)?)?;
        let suffix_at = le_u32(info_bytes, 0x18)?;
        let suffix = if suffix_at == 0 {
            String::new()
        } else {
            ansi_at(info_bytes, suffix_at).unwrap_or_default()
        };
        (base, suffix)
    };
    if base.is_empty() {
        return None;
    }
    Some(if suffix.is_empty() || base.ends_with('\\') {
        format!("{base}{suffix}")
    } else {
        format!("{base}\\{suffix}")
    })
}

/// Folders that hold many programs, never one app's install folder.
fn is_container_dir(name: &str) -> bool {
    const CONTAINERS: &[&str] = &[
        "appdata",
        "common files",
        "desktop",
        "dosdevices",
        "drive_c",
        "local",
        "program files",
        "program files (x86)",
        "programdata",
        "roaming",
        "start menu",
        "programs",
        "syswow64",
        "system32",
        "temp",
        "users",
        "windows",
    ];
    CONTAINERS.contains(&name.to_lowercase().as_str())
}

/// Install folder of `program`: its folder (the parent of `bin`/`x64`-style folders),
/// unless that holds many programs or lies outside `prefix` and `home`.
pub(super) fn install_dir(program: &Path, prefix: &Path, home: &Path) -> Option<PathBuf> {
    const ARCH_DIRS: &[&str] = &[
        "bin", "bin32", "bin64", "binaries", "win32", "win64", "x64", "x86",
    ];
    let mut dir = program.parent()?;
    if file_name(dir).is_some_and(|n| ARCH_DIRS.contains(&n.to_lowercase().as_str())) {
        dir = dir.parent()?;
    }
    let name = file_name(dir)?;
    let inside = dir.starts_with(prefix) || dir.starts_with(home);
    let too_wide = is_container_dir(name) || prefix.starts_with(dir) || home.starts_with(dir);
    let has_parent_refs = dir.components().any(|c| matches!(c, Component::ParentDir));
    (inside && !too_wide && !has_parent_refs).then(|| dir.to_path_buf())
}

/// `winemenubuilder`'s `.menu` file name of the desktop entry at `rel` (relative to the
/// applications folder): `wine/Programs/Foo/Bar.desktop` → `wine-Programs-Foo-Bar.menu`.
pub(super) fn menu_file_name(rel: &Path) -> Option<String> {
    let stem = rel.with_extension("");
    let parts: Vec<&str> = stem
        .components()
        .map(|c| match c {
            Component::Normal(n) => n.to_str(),
            _ => None,
        })
        .collect::<Option<_>>()?;
    (parts.len() >= 2).then(|| format!("{}.menu", parts.join("-")))
}

/// `.directory` file name of the menu folder at `rel`: `wine/Programs/Foo` →
/// `wine-Programs-Foo.directory`.
pub(super) fn directory_file_name(rel: &Path) -> Option<String> {
    menu_file_name(&rel.with_extension("desktop"))
        .and_then(|m| m.strip_suffix(".menu").map(|s| format!("{s}.directory")))
}

/// Shortcut names that are not the program itself.
fn is_auxiliary(name: &str) -> bool {
    const AUX: &[&str] = &[
        "changelog",
        "documentation",
        "help",
        "homepage",
        "licence",
        "license",
        "manual",
        "read me",
        "readme",
        "release notes",
        "support",
        "uninstall",
        "website",
    ];
    let lower = name.to_lowercase();
    AUX.iter().any(|a| lower.contains(a))
}

/// `.desktop` files under `dir`, `depth` levels deep at most.
fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    for child in common::children(dir) {
        if child.extension().is_some_and(|e| e == "desktop") {
            out.push(child);
        } else if depth > 0 && child.is_dir() {
            walk(&child, depth.saturating_sub(1), out);
        }
    }
}

/// Unix path of the program a shortcut at `path` (inside `prefix`) points to.
pub(super) fn link_program(path: &Path, prefix: &Path) -> Option<PathBuf> {
    read_lnk(path).and_then(|t| to_unix(prefix, &t))
}
/// Groups menu entry files by Start Menu folder: `…/Programs/Foo/**` → `…/Programs/Foo`;
/// files directly in `Programs` are groups of their own (`None` folder).
pub(super) fn group(
    root: &Path,
    files: Vec<PathBuf>,
) -> BTreeMap<PathBuf, (Option<PathBuf>, Vec<PathBuf>)> {
    let mut groups: BTreeMap<PathBuf, (Option<PathBuf>, Vec<PathBuf>)> = BTreeMap::new();
    for file in files {
        let Ok(rel) = file.strip_prefix(root) else {
            continue;
        };
        let parts: Vec<Component<'_>> = rel.components().collect();
        let (key, folder) = match parts.as_slice() {
            [top, sub, _, ..] => {
                let folder = root.join(top).join(sub);
                (folder.clone(), Some(folder))
            }
            _ => (file.clone(), None),
        };
        groups
            .entry(key)
            .or_insert_with(|| (folder, Vec::new()))
            .1
            .push(file);
    }
    groups
}

/// Reads a shortcut and returns its target.
fn read_lnk(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_LNK {
        return None;
    }
    lnk_target(&std::fs::read(path).ok()?)
}

/// Every Wine app of this user's menu.
pub(super) fn apps(locales: &[String]) -> Vec<WineApp> {
    let (Some(home), Some(data)) = (common::home(), common::data_home()) else {
        return Vec::new();
    };
    let root = data.join("applications/wine");
    let mut files = Vec::new();
    walk(&root, MAX_DEPTH, &mut files);
    files.sort();
    let default_prefix = home.join(".wine");
    let mut out = Vec::new();
    for (_, (folder, files)) in group(&root, files) {
        let mut entries: Vec<DesktopEntry> = files
            .iter()
            .filter_map(|f| DesktopEntry::load(f, locales))
            .filter(DesktopEntry::is_visible_app)
            .collect();
        if entries.is_empty() {
            continue;
        }
        let folder_name = folder.as_deref().and_then(file_name).map(str::to_lowercase);
        entries.sort_by_key(|e| {
            let name = e.name.clone().unwrap_or_else(|| e.id.clone());
            (
                is_auxiliary(&name),
                folder_name.as_deref() != Some(name.to_lowercase().as_str()),
                name.len(),
            )
        });
        let mut app = WineApp {
            detail: WineDetail {
                prefix: default_prefix.clone(),
                folder,
                ..WineDetail::default()
            },
            ..WineApp::default()
        };
        let mut resolved_any = false;
        for (i, entry) in entries.iter().enumerate() {
            let Some(exec) = entry.exec.as_deref().and_then(parse_exec) else {
                continue;
            };
            let prefix = exec
                .prefix
                .clone()
                .unwrap_or_else(|| default_prefix.clone());
            if i == 0 {
                app.detail.prefix.clone_from(&prefix);
            }
            let unix = exec.windows.iter().filter_map(|w| to_unix(&prefix, w));
            for path in unix.chain(exec.unix.iter().cloned()) {
                let is_lnk = path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("lnk"));
                let program = if is_lnk {
                    let target = read_lnk(&path).and_then(|t| to_unix(&prefix, &t));
                    if !app.detail.links.contains(&path) {
                        app.detail.links.push(path);
                    }
                    target
                } else {
                    Some(path)
                };
                let Some(program) = program else { continue };
                resolved_any = true;
                if !is_auxiliary(file_name(&program).unwrap_or_default())
                    && !app.programs.contains(&program)
                {
                    app.programs.push(program);
                }
            }
        }
        // Programs whose files are all gone are leftovers, not installed apps.
        if resolved_any && !app.programs.iter().any(|p| p.exists()) {
            continue;
        }
        app.detail.dir = app
            .programs
            .iter()
            .filter(|p| p.exists())
            .find_map(|p| install_dir(p, &app.detail.prefix, &home));
        app.entries = entries;
        out.push(app);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_with_escaped_start_menu_link() {
        // Desktop-unescaped form of winemenubuilder's `C:\\\\ProgramData\\\\…\\ Menu`.
        let exec = r#"env WINEPREFIX="/home/u/.wine" wine C:\\ProgramData\\Microsoft\\Windows\\Start\ Menu\\Programs\\Foo\\Foo.lnk"#;
        let parsed = parse_exec(exec);
        assert_eq!(
            parsed,
            Some(WineExec {
                prefix: Some(PathBuf::from("/home/u/.wine")),
                windows: vec![
                    r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\Foo\Foo.lnk".to_owned()
                ],
                unix: Vec::new(),
            }),
            "prefix and link"
        );
    }

    #[test]
    fn exec_with_start_unix() {
        let exec = r#"env WINEPREFIX="/p" wine C:\\windows\\command\\start.exe /Unix /p/dosdevices/c:/users/Public/Desktop/Foo.lnk"#;
        let parsed = parse_exec(exec);
        assert!(
            parsed.as_ref().is_some_and(|p| p.windows.is_empty()
                && p.unix
                    == [PathBuf::from(
                        "/p/dosdevices/c:/users/Public/Desktop/Foo.lnk"
                    )]),
            "start.exe skipped, unix link kept: {parsed:?}"
        );
        assert_eq!(parse_exec("/usr/bin/foo %U"), None, "not wine");
    }

    #[test]
    fn windows_paths_map_into_the_prefix() {
        let prefix = Path::new("/home/u/.wine");
        assert_eq!(
            to_unix(prefix, r"C:\Program Files\Foo\foo.exe"),
            Some(PathBuf::from(
                "/home/u/.wine/drive_c/Program Files/Foo/foo.exe"
            )),
            "drive c"
        );
        assert_eq!(
            to_unix(prefix, r"D:\Games\x.exe"),
            Some(PathBuf::from("/home/u/.wine/dosdevices/d:/Games/x.exe")),
            "other drives"
        );
        assert_eq!(to_unix(prefix, r"C:\..\x"), None, "no parent refs");
        assert_eq!(to_unix(prefix, "relative"), None, "not absolute");
    }

    /// A minimal shortcut with an ANSI `LinkInfo` local base path.
    fn lnk(target: &str, with_id_list: bool) -> Vec<u8> {
        let mut out = vec![0_u8; 0x4C];
        out.splice(0..4, 0x4C_u32.to_le_bytes());
        let flags: u32 = if with_id_list { 0x3 } else { 0x2 };
        out.splice(0x14..0x18, flags.to_le_bytes());
        if with_id_list {
            out.extend_from_slice(&4_u16.to_le_bytes());
            out.extend_from_slice(&[9, 9, 9, 9]);
        }
        let path_off: u32 = 0x1C;
        let mut info = Vec::new();
        let path_len = u32::try_from(target.len()).unwrap_or(0);
        let suffix_off = path_off.saturating_add(path_len).saturating_add(1);
        let size = suffix_off.saturating_add(1);
        for v in [size, 0x1C, 1, 0, path_off, 0, suffix_off] {
            info.extend_from_slice(&v.to_le_bytes());
        }
        info.extend_from_slice(target.as_bytes());
        info.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&info);
        out
    }

    #[test]
    fn lnk_local_base_path() {
        let target = r"C:\Program Files\Foo\foo.exe";
        assert_eq!(
            lnk_target(&lnk(target, false)).as_deref(),
            Some(target),
            "plain"
        );
        assert_eq!(
            lnk_target(&lnk(target, true)).as_deref(),
            Some(target),
            "after id list"
        );
        assert_eq!(lnk_target(&[0; 8]), None, "not a shortcut");
        let mut truncated = lnk(target, false);
        truncated.truncate(0x50);
        assert_eq!(lnk_target(&truncated), None, "truncated");
    }

    #[test]
    fn install_dirs_skip_shared_folders() {
        let prefix = Path::new("/home/u/.wine");
        let home = Path::new("/home/u");
        let dir = |p: &str| install_dir(Path::new(p), prefix, home);
        assert_eq!(
            dir("/home/u/.wine/drive_c/Program Files/Foo/foo.exe"),
            Some(PathBuf::from("/home/u/.wine/drive_c/Program Files/Foo")),
            "program folder"
        );
        assert_eq!(
            dir("/home/u/.wine/drive_c/Program Files/Foo/bin/foo.exe"),
            Some(PathBuf::from("/home/u/.wine/drive_c/Program Files/Foo")),
            "bin folder climbs"
        );
        assert_eq!(
            dir("/home/u/.wine/drive_c/Program Files/foo.exe"),
            None,
            "Program Files"
        );
        assert_eq!(
            dir("/home/u/.wine/drive_c/windows/notepad.exe"),
            None,
            "windows"
        );
        assert_eq!(dir("/home/u/foo.exe"), None, "home itself");
        assert_eq!(dir("/usr/lib/foo/foo.exe"), None, "outside prefix and home");
    }

    #[test]
    fn menu_and_directory_names() {
        assert_eq!(
            menu_file_name(Path::new("wine/Programs/Foo/Bar App.desktop")).as_deref(),
            Some("wine-Programs-Foo-Bar App.menu"),
            "menu"
        );
        assert_eq!(
            directory_file_name(Path::new("wine/Programs/Foo")).as_deref(),
            Some("wine-Programs-Foo.directory"),
            "directory"
        );
    }

    #[test]
    fn groups_by_start_menu_folder() {
        let root = Path::new("/d/wine");
        let groups = group(
            root,
            vec![
                PathBuf::from("/d/wine/Programs/Foo/Foo.desktop"),
                PathBuf::from("/d/wine/Programs/Foo/Sub/Help.desktop"),
                PathBuf::from("/d/wine/Programs/Bar.desktop"),
            ],
        );
        assert_eq!(
            groups.len(),
            2,
            "one folder group, one loose entry: {groups:?}"
        );
        assert!(
            groups
                .get(Path::new("/d/wine/Programs/Foo"))
                .is_some_and(|(f, files)| f.is_some() && files.len() == 2),
            "nested entries join their top folder"
        );
    }

    #[test]
    fn auxiliary_shortcuts() {
        assert!(is_auxiliary("Uninstall Foo"), "uninstaller");
        assert!(is_auxiliary("Foo Readme"), "readme");
        assert!(!is_auxiliary("Foo"), "program");
    }
}
