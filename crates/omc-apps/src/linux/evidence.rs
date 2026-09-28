//! Evidence from the app's running processes: the folders under the home folder their
//! working directory and open files live in (`/proc/<pid>/cwd`, `/proc/<pid>/fd/*`).
//! `AppImage` processes are found by their `APPIMAGE` environment variable (the app runs
//! from a FUSE mount, not from the image).

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use omc_proto::apps::AppFileKind;
use omc_scan::procs;

use super::common::{self, file_name};

/// Open descriptors inspected per process.
const MAX_FDS: usize = 4096;
/// Bytes of `/proc/<pid>/environ` read.
const MAX_ENVIRON: u64 = 256 * 1024;

/// The per-user base folders open files are attributed to.
#[derive(Debug, Clone, Default)]
pub(super) struct Bases {
    /// The home folder.
    pub(super) home: PathBuf,
    /// `$XDG_CONFIG_HOME`.
    pub(super) config: Option<PathBuf>,
    /// `$XDG_DATA_HOME`.
    pub(super) data: Option<PathBuf>,
    /// `$XDG_STATE_HOME`.
    pub(super) state: Option<PathBuf>,
    /// `$XDG_CACHE_HOME`.
    pub(super) cache: Option<PathBuf>,
}

impl Bases {
    /// This user's folders; `None` without a home folder.
    pub(super) fn current() -> Option<Self> {
        Some(Self {
            home: common::home()?,
            config: common::config_home(),
            data: common::data_home(),
            state: common::state_home(),
            cache: common::cache_home(),
        })
    }
}

/// Folders and files every app touches (toolkit caches, sound, input methods, the
/// desktop's own databases) or that hold other programs (Wine prefixes, Steam): open
/// files there say nothing about the app.
pub(super) fn is_shared(name: &str) -> bool {
    const SHARED: &[&str] = &[
        ".cache",
        ".config",
        ".dbus",
        ".esd_auth",
        ".gnupg",
        ".icons",
        ".ICEauthority",
        ".java",
        ".local",
        ".mozilla",
        ".nv",
        ".pki",
        ".pulse",
        ".pulse-cookie",
        ".ssh",
        ".steam",
        ".themes",
        ".var",
        ".wine",
        ".Xauthority",
        ".xsession-errors",
        "at-spi",
        "dconf",
        "event-sound-cache.tdb",
        "fcitx",
        "fcitx5",
        "fontconfig",
        "gstreamer-1.0",
        "gtk-2.0",
        "gtk-3.0",
        "gtk-4.0",
        "gvfs-metadata",
        "ibus",
        "keyrings",
        "kxmlgui5",
        "lutris",
        "mesa_shader_cache",
        "mesa_shader_cache_db",
        "mime",
        "mimeapps.list",
        "nvidia",
        "pipewire",
        "pulse",
        "radv_builtin_shaders",
        "recently-used.xbel",
        "sounds",
        "steam",
        "thumbnails",
        "trash",
        "user-dirs.dirs",
        "user-dirs.locale",
        "wireplumber",
        "xdg-desktop-portal",
    ];
    let lower = name.to_lowercase();
    SHARED.iter().any(|s| s.eq_ignore_ascii_case(name))
        || lower.starts_with("ksycoca")
        || lower.starts_with("qtshadercache")
        || lower.starts_with("event-sound-cache")
        || common::is_generic_name(lower.trim_start_matches('.'))
}

/// The top folder (or file) of `target` directly inside a base folder, with its role;
/// `None` for paths outside the per-user bases, the bases themselves and shared folders.
pub(super) fn evidence_root(target: &Path, bases: &Bases) -> Option<(PathBuf, AppFileKind)> {
    let first = |base: &Path| -> Option<PathBuf> {
        let rest = target.strip_prefix(base).ok()?;
        match rest.components().next()? {
            Component::Normal(name) => Some(base.join(name)),
            _ => None,
        }
    };
    let xdg = [
        (bases.config.as_deref(), AppFileKind::Preferences),
        (bases.state.as_deref(), AppFileKind::Support),
        (bases.cache.as_deref(), AppFileKind::Cache),
        (bases.data.as_deref(), AppFileKind::Support),
    ];
    for (base, kind) in xdg {
        let Some(base) = base else { continue };
        if target.starts_with(base) {
            let root = first(base)?;
            return (!is_shared(file_name(&root)?)).then_some((root, kind));
        }
    }
    let var_app = bases.home.join(".var/app");
    if target.starts_with(&var_app) {
        return first(&var_app).map(|r| (r, AppFileKind::Container));
    }
    let root = first(&bases.home)?;
    let name = file_name(&root)?;
    (name.starts_with('.') && !is_shared(name)).then_some((root, AppFileKind::Support))
}

/// A `/proc/<pid>/…` link target as a path (`None` for sockets, pipes, `anon_inode:`).
pub(super) fn link_target(target: &Path) -> Option<PathBuf> {
    let text = target.to_str()?;
    let text = text.strip_suffix(" (deleted)").unwrap_or(text);
    text.starts_with('/').then(|| PathBuf::from(text))
}

/// Working directory and open files of process `pid`.
fn open_paths(pid: u32) -> Vec<PathBuf> {
    let proc_dir = Path::new("/proc").join(pid.to_string());
    let mut out: Vec<PathBuf> = std::fs::read_link(proc_dir.join("cwd"))
        .ok()
        .and_then(|t| link_target(&t))
        .into_iter()
        .collect();
    if let Ok(fds) = std::fs::read_dir(proc_dir.join("fd")) {
        out.extend(
            fds.flatten()
                .take(MAX_FDS)
                .filter_map(|fd| std::fs::read_link(fd.path()).ok())
                .filter_map(|t| link_target(&t)),
        );
    }
    out
}

/// Whether a NUL-separated environment block sets `key` to `value`.
pub(super) fn environ_has(environ: &[u8], key: &str, value: &OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let value = value.as_bytes();
    environ.split(|b| *b == 0).any(|var| {
        var.strip_prefix(key.as_bytes())
            .and_then(|rest| rest.strip_prefix(b"="))
            .is_some_and(|v| v == value)
    })
}

/// Processes started from the `AppImage` file `image` (`APPIMAGE=<image>`).
pub(super) fn appimage_pids(image: &Path) -> Vec<u32> {
    use std::io::Read as _;
    let own = std::process::id();
    procs::running()
        .into_iter()
        .filter(|p| p.pid != own)
        .filter(|p| {
            let path = Path::new("/proc").join(p.pid.to_string()).join("environ");
            let mut environ = Vec::new();
            std::fs::File::open(path)
                .and_then(|f| f.take(MAX_ENVIRON).read_to_end(&mut environ))
                .is_ok()
                && environ_has(&environ, "APPIMAGE", image.as_os_str())
        })
        .map(|p| p.pid)
        .collect()
}

/// Folders (with roles) holding the working directories and open files of `pids`.
pub(super) fn open_roots(pids: &[u32], bases: &Bases) -> BTreeSet<(PathBuf, AppFileKind)> {
    pids.iter()
        .flat_map(|pid| open_paths(*pid))
        .filter_map(|p| evidence_root(&p, bases))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bases() -> Bases {
        Bases {
            home: PathBuf::from("/home/u"),
            config: Some(PathBuf::from("/home/u/.config")),
            data: Some(PathBuf::from("/home/u/.local/share")),
            state: Some(PathBuf::from("/home/u/.local/state")),
            cache: Some(PathBuf::from("/home/u/.cache")),
        }
    }

    #[test]
    fn roots_are_first_level_folders() {
        let b = bases();
        let root = |p: &str| evidence_root(Path::new(p), &b);
        assert_eq!(
            root("/home/u/.config/Code/User/settings.json"),
            Some((
                PathBuf::from("/home/u/.config/Code"),
                AppFileKind::Preferences
            )),
            "config folder"
        );
        assert_eq!(
            root("/home/u/.local/share/obsidian/db"),
            Some((
                PathBuf::from("/home/u/.local/share/obsidian"),
                AppFileKind::Support
            )),
            "data folder, not ~/.local"
        );
        assert_eq!(
            root("/home/u/.cache/spotify/Data/x"),
            Some((PathBuf::from("/home/u/.cache/spotify"), AppFileKind::Cache)),
            "cache folder"
        );
        assert_eq!(
            root("/home/u/.var/app/org.x.App/config/a"),
            Some((
                PathBuf::from("/home/u/.var/app/org.x.App"),
                AppFileKind::Container
            )),
            "flatpak data"
        );
        assert_eq!(
            root("/home/u/.thunderbird/p/x.sqlite"),
            Some((PathBuf::from("/home/u/.thunderbird"), AppFileKind::Support)),
            "dot folder"
        );
    }

    #[test]
    fn shared_and_user_files_are_not_evidence() {
        let b = bases();
        for path in [
            "/home/u/.cache/fontconfig/x.cache-8",
            "/home/u/.config/dconf/user",
            "/home/u/.local/share/recently-used.xbel",
            "/home/u/.cache/mesa_shader_cache/index",
            "/home/u/.wine/drive_c/x.exe",
            "/home/u/.cache/ksycoca6_en_abc",
            "/home/u/Documents/report.odt",
            "/home/u",
            "/home/u/.config",
            "/tmp/x",
        ] {
            assert_eq!(evidence_root(Path::new(path), &b), None, "{path}");
        }
    }

    #[test]
    fn link_targets_drop_pseudo_files() {
        assert_eq!(link_target(Path::new("socket:[123]")), None, "socket");
        assert_eq!(link_target(Path::new("anon_inode:[eventfd]")), None, "anon");
        assert_eq!(
            link_target(Path::new("/home/u/.config/x/lock (deleted)")),
            Some(PathBuf::from("/home/u/.config/x/lock")),
            "deleted suffix"
        );
    }

    #[test]
    fn environ_matches_exact_value() {
        let env = b"HOME=/home/u\0APPIMAGE=/home/u/Apps/Foo.AppImage\0APPDIR=/tmp/.mount_x\0";
        assert!(
            environ_has(env, "APPIMAGE", OsStr::new("/home/u/Apps/Foo.AppImage")),
            "exact"
        );
        assert!(
            !environ_has(env, "APPIMAGE", OsStr::new("/home/u/Apps/Foo")),
            "prefix only"
        );
        assert!(
            !environ_has(env, "APP", OsStr::new("/home/u/Apps/Foo.AppImage")),
            "key prefix only"
        );
    }
}
