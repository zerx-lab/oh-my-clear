//! `cargo omc` on macOS: the Dock and the app switcher take an app's icon and name from its
//! bundle, so a bare `target/<profile>/oh-my-clear` shows the generic executable icon. The
//! GUI runs from `target/debug/oh-my-clear.app` instead, with the daemon in a helper bundle
//! of its own (`omc_ipc::layout`, ADR 0020): it owns the menu bar icon, so it is a Cocoa
//! app, and inside the app's own bundle Launch Services would take it for the app.
//!
//! ```text
//! oh-my-clear.app/Contents/
//!   Info.plist                     apps/oh-my-clear/resources/macos/Info.plist, @VERSION@ filled
//!   MacOS/oh-my-clear              hard link to target/debug/oh-my-clear
//!   Resources/AppIcon.icns
//!   Helpers/oh-my-clear-daemon.app/Contents/
//!     Info.plist                   apps/oh-my-clear-daemon/resources/macos/Info.plist (LSUIElement)
//!     MacOS/oh-my-clear-daemon     hard link to target/debug/oh-my-clear-daemon
//!     Resources/AppIcon.icns
//! ```
//!
//! Hard links keep the daemon's modification time, and with it its build id: an unchanged
//! daemon keeps running across GUI restarts.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

const APP: &str = "oh-my-clear";
const DAEMON: &str = "oh-my-clear-daemon";

/// Assembles the bundle from the freshly built debug binaries in `target_dir` and returns
/// the path of its executable.
pub(crate) fn assemble(root: &Path, target_dir: &Path) -> Result<PathBuf> {
    let profile_dir = target_dir.join("debug");
    let icon_path = root.join("apps/oh-my-clear/resources/macos/AppIcon.icns");
    let icon = fs::read(&icon_path).map_err(|source| io_error(&icon_path, source))?;
    let contents = profile_dir.join(format!("{APP}.app")).join("Contents");
    let helper = contents
        .join("Helpers")
        .join(format!("{DAEMON}.app"))
        .join("Contents");
    for (bundle, package) in [(&contents, APP), (&helper, DAEMON)] {
        let resources = root.join("apps").join(package).join("resources/macos");
        bundle_binary(bundle, &resources, &icon, &profile_dir.join(package))?;
    }
    // The daemon's place before it moved into its helper bundle.
    remove_if_present(&contents.join("MacOS").join(DAEMON))?;
    Ok(contents.join("MacOS").join(APP))
}

/// Fills the bundle `contents` dir: `Info.plist` from `resources`, the icon, and a hard
/// link to `binary` in `MacOS/`.
fn bundle_binary(contents: &Path, resources: &Path, icon: &[u8], binary: &Path) -> Result<()> {
    let macos = contents.join("MacOS");
    let bundle_resources = contents.join("Resources");
    for dir in [&macos, &bundle_resources] {
        fs::create_dir_all(dir).map_err(|source| io_error(dir, source))?;
    }
    let plist_path = resources.join("Info.plist");
    let plist = fs::read_to_string(&plist_path)
        .map_err(|source| io_error(&plist_path, source))?
        .replace("@VERSION@", env!("CARGO_PKG_VERSION"));
    write_if_changed(&contents.join("Info.plist"), plist.as_bytes())?;
    write_if_changed(&bundle_resources.join("AppIcon.icns"), icon)?;
    let name = binary.file_name().ok_or_else(|| {
        io_error(
            binary,
            io::Error::new(io::ErrorKind::InvalidInput, "no file name"),
        )
    })?;
    link(binary, &macos.join(name))
}

fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error(path, source)),
    }
}

/// Cargo's target directory for the workspace at `root`.
pub(crate) fn target_dir(root: &Path) -> PathBuf {
    env::var_os("CARGO_TARGET_DIR")
        .or_else(|| env::var_os("CARGO_BUILD_TARGET_DIR"))
        .map_or_else(|| root.join("target"), |dir| root.join(dir))
}

/// Leaves unchanged files alone, so Launch Services does not re-read the bundle each run.
fn write_if_changed(path: &Path, contents: &[u8]) -> Result<()> {
    if fs::read(path).is_ok_and(|current| current == contents) {
        return Ok(());
    }
    fs::write(path, contents).map_err(|source| io_error(path, source))
}

/// Points `dest` at `src`'s current inode (cargo replaces the file on every rebuild).
fn link(src: &Path, dest: &Path) -> Result<()> {
    remove_if_present(dest)?;
    fs::hard_link(src, dest).map_err(|source| io_error(dest, source))
}

fn io_error(path: &Path, source: io::Error) -> Error {
    Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io;
    use std::path::Path;
    use std::time::SystemTime;

    fn write(path: &Path, contents: &[u8]) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(path, contents)
    }

    fn mtime(path: &Path) -> io::Result<SystemTime> {
        fs::metadata(path)?.modified()
    }

    #[test]
    fn bundle_tracks_rebuilt_binaries_and_keeps_the_daemon_build_id() {
        let root = std::env::temp_dir().join(format!("omc-xtask-bundle-{}", std::process::id()));
        let resources = root.join("apps/oh-my-clear/resources/macos");
        let daemon_resources = root.join("apps/oh-my-clear-daemon/resources/macos");
        let target = root.join("target");
        let debug = target.join("debug");
        let contents = debug.join("oh-my-clear.app/Contents");
        let helper = contents.join("Helpers/oh-my-clear-daemon.app/Contents");
        let stale_daemon = contents.join("MacOS/oh-my-clear-daemon");
        let setup = write(&resources.join("Info.plist"), b"<string>@VERSION@</string>")
            .and_then(|()| write(&resources.join("AppIcon.icns"), b"icns"))
            .and_then(|()| write(&daemon_resources.join("Info.plist"), b"daemon @VERSION@"))
            .and_then(|()| write(&debug.join("oh-my-clear"), b"gui v1"))
            .and_then(|()| write(&debug.join("oh-my-clear-daemon"), b"daemon"))
            .and_then(|()| write(&stale_daemon, b"old layout"));

        let first = super::assemble(&root, &target);
        let plist = fs::read_to_string(contents.join("Info.plist"));
        let icon = fs::read(contents.join("Resources/AppIcon.icns"));
        let helper_plist = fs::read_to_string(helper.join("Info.plist"));
        let helper_icon = fs::read(helper.join("Resources/AppIcon.icns"));
        let bundled_daemon = mtime(&helper.join("MacOS/oh-my-clear-daemon"));
        let built_daemon = mtime(&debug.join("oh-my-clear-daemon"));
        let stale_left = stale_daemon.exists();
        // cargo replaces the output file on a rebuild; the bundle must follow it.
        let rebuild = fs::remove_file(debug.join("oh-my-clear"))
            .and_then(|()| write(&debug.join("oh-my-clear"), b"gui v2"));
        let second = super::assemble(&root, &target);
        let gui = fs::read(contents.join("MacOS/oh-my-clear"));
        let cleanup = fs::remove_dir_all(&root);

        assert!(setup.is_ok(), "fixture written: {setup:?}");
        assert!(
            first.is_ok_and(|exe| exe == contents.join("MacOS/oh-my-clear")),
            "assemble returns the bundle executable"
        );
        assert!(
            plist.is_ok_and(|text| text
                == format!("<string>{}</string>", env!("CARGO_PKG_VERSION"))),
            "the plist gets the workspace version"
        );
        assert!(
            helper_plist.is_ok_and(|text| text == format!("daemon {}", env!("CARGO_PKG_VERSION"))),
            "the helper bundle gets the daemon's plist"
        );
        assert!(
            icon.is_ok_and(|bytes| bytes == b"icns")
                && helper_icon.is_ok_and(|bytes| bytes == b"icns"),
            "both bundles carry the icon"
        );
        assert!(
            matches!((&bundled_daemon, &built_daemon), (Ok(a), Ok(b)) if a == b),
            "the bundled daemon keeps the build's mtime (its build id): \
             {bundled_daemon:?} vs {built_daemon:?}"
        );
        assert!(!stale_left, "the daemon no longer sits next to the GUI");
        assert!(rebuild.is_ok(), "rebuild simulated: {rebuild:?}");
        assert!(second.is_ok(), "second assemble succeeds: {second:?}");
        assert!(
            gui.is_ok_and(|bytes| bytes == b"gui v2"),
            "the rebuilt GUI is bundled"
        );
        assert!(cleanup.is_ok(), "temp dir removed: {cleanup:?}");
    }
}
