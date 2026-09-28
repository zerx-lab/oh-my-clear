//! Embeds the application icon (`resources/windows/oh-my-clear.rc`) into the Windows
//! executable. Other targets have nothing to compile.

use embed_resource::{CompilationResult, ParamsIncludeDirs};

fn main() -> Result<(), CompilationResult> {
    cargo("rerun-if-changed=resources/windows");
    match embed_resource::compile(
        "resources/windows/oh-my-clear.rc",
        ParamsIncludeDirs(["resources/windows"]),
    ) {
        // Cross-checking for Windows from another host without `llvm-rc` installed.
        CompilationResult::NotAttempted(why) if !cfg!(windows) => {
            cargo(&format!(
                "warning=the Windows icon is not embedded: no resource compiler ({why})"
            ));
            Ok(())
        }
        // On Windows the MSVC Build Tools are a prerequisite; a build without its icon is
        // a bug.
        result => result.manifest_required(),
    }
}

fn cargo(directive: &str) {
    println!("cargo::{directive}");
}
