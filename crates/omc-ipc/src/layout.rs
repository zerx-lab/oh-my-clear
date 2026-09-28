//! Where the two executables sit relative to each other, so each can start the other: the
//! UI spawns the daemon (ADR 0008), the daemon's tray launches the UI (ADR 0020).
//!
//! - Side by side (`target/<profile>/`, Windows/Linux install dirs):
//!   `oh-my-clear[.exe]` next to `oh-my-clear-daemon[.exe]`.
//! - macOS app bundle: the daemon is a Cocoa app (it owns the menu bar icon), so it
//!   gets its own helper bundle with its own identifier; sharing the app's bundle would make
//!   Launch Services treat the daemon as a running instance of the app:
//!
//! ```text
//! oh-my-clear.app/Contents/MacOS/oh-my-clear
//! oh-my-clear.app/Contents/Helpers/oh-my-clear-daemon.app/Contents/MacOS/oh-my-clear-daemon
//! ```

use std::path::{Path, PathBuf};

/// The GUI executable's file stem.
pub const UI_NAME: &str = "oh-my-clear";
/// The daemon executable's file stem.
pub const DAEMON_NAME: &str = "oh-my-clear-daemon";

/// The daemon that belongs to the GUI at `ui_exe`. `None` when `ui_exe` has no parent.
pub fn daemon_exe(ui_exe: &Path) -> Option<PathBuf> {
    let dir = ui_exe.parent()?;
    let file = executable(DAEMON_NAME);
    Some(match bundle_contents(dir) {
        Some(contents) => contents
            .join("Helpers")
            .join(format!("{DAEMON_NAME}.app"))
            .join("Contents/MacOS")
            .join(file),
        None => dir.join(file),
    })
}

/// The GUI that belongs to the daemon at `daemon_exe`. `None` when `daemon_exe` has no
/// parent.
pub fn ui_exe(daemon_exe: &Path) -> Option<PathBuf> {
    let dir = daemon_exe.parent()?;
    let file = executable(UI_NAME);
    let app = bundle_contents(dir)
        .and_then(Path::parent)
        .filter(|helper| {
            helper
                .file_name()
                .is_some_and(|name| name == format!("{DAEMON_NAME}.app").as_str())
        })
        .and_then(Path::parent)
        .filter(|helpers| helpers.file_name().is_some_and(|name| name == "Helpers"))
        .and_then(Path::parent);
    Some(match app {
        Some(contents) => contents.join("MacOS").join(file),
        None => dir.join(file),
    })
}

/// `X.app/Contents` when `dir` is `X.app/Contents/MacOS`.
fn bundle_contents(dir: &Path) -> Option<&Path> {
    let contents = dir
        .file_name()
        .is_some_and(|name| name == "MacOS")
        .then(|| dir.parent())??;
    let app = contents
        .file_name()
        .is_some_and(|name| name == "Contents")
        .then(|| contents.parent())??;
    app.extension()
        .is_some_and(|ext| ext == "app")
        .then_some(contents)
}

fn executable(stem: &str) -> String {
    format!("{stem}{}", std::env::consts::EXE_SUFFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exe(path: &str) -> PathBuf {
        PathBuf::from(format!("{path}{}", std::env::consts::EXE_SUFFIX))
    }

    #[test]
    fn side_by_side_outside_a_bundle() {
        let ui = exe("/work/target/debug/oh-my-clear");
        let daemon = exe("/work/target/debug/oh-my-clear-daemon");
        assert_eq!(daemon_exe(&ui), Some(daemon.clone()), "UI → sibling daemon");
        assert_eq!(ui_exe(&daemon), Some(ui), "daemon → sibling UI");
    }

    #[test]
    fn macos_bundle_puts_the_daemon_in_a_helper_bundle() {
        let ui = exe("/Applications/oh-my-clear.app/Contents/MacOS/oh-my-clear");
        let daemon = exe(
            "/Applications/oh-my-clear.app/Contents/Helpers/oh-my-clear-daemon.app/Contents/MacOS/oh-my-clear-daemon",
        );
        assert_eq!(daemon_exe(&ui), Some(daemon.clone()), "UI → helper daemon");
        assert_eq!(ui_exe(&daemon), Some(ui), "helper daemon → bundle UI");
    }

    #[test]
    fn a_bare_macos_dir_that_is_no_bundle_stays_side_by_side() {
        let ui = exe("/tmp/Contents/MacOS/oh-my-clear");
        assert_eq!(
            daemon_exe(&ui),
            Some(exe("/tmp/Contents/MacOS/oh-my-clear-daemon")),
            "no `.app` above `Contents`"
        );
        let daemon = exe("/x/other.app/Contents/MacOS/oh-my-clear-daemon");
        assert_eq!(
            ui_exe(&daemon),
            Some(exe("/x/other.app/Contents/MacOS/oh-my-clear")),
            "a daemon not inside the helper bundle looks next to itself"
        );
    }
}
