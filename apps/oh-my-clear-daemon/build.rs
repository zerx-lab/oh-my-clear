//! Embeds the application icon (`resources/windows/oh-my-clear-daemon.rc`, the `.ico` from
//! the GUI's resources) into the Windows executable: the tray loads it at the notification
//! area's size, and Task Manager shows it. Other targets have nothing to compile.

use embed_resource::{CompilationResult, ParamsIncludeDirs};

fn main() -> Result<(), CompilationResult> {
    cargo("rerun-if-changed=resources/windows");
    cargo("rerun-if-changed=../oh-my-clear/resources/windows/oh-my-clear.ico");
    match embed_resource::compile(
        "resources/windows/oh-my-clear-daemon.rc",
        ParamsIncludeDirs(["../oh-my-clear/resources/windows"]),
    ) {
        // Cross-checking for Windows from another host without `llvm-rc` installed.
        CompilationResult::NotAttempted(why) if !cfg!(windows) => {
            cargo(&format!(
                "warning=the Windows icon is not embedded: no resource compiler ({why})"
            ));
            Ok(())
        }
        // On Windows the MSVC Build Tools are a prerequisite; a build without its icon is
        // a bug (the tray would have no icon).
        result => result.manifest_required(),
    }
}

fn cargo(directive: &str) {
    println!("cargo::{directive}");
}
