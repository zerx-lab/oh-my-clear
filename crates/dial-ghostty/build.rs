//! Builds libghostty-vt from the pinned `third_party/ghostty` submodule with zig and
//! links its static archive. See the crate docs (`src/lib.rs`) for the one-time setup.
//!
//! Guarantees:
//! - **No network.** Zig resolves packages only from the submodule's git-ignored
//!   `zig-pkg/` directory (`--system`); a missing package is a hard error with the
//!   one-time fetch command, never a download.
//! - **Clean submodule.** The zig cache lives in the cargo target dir and the install
//!   prefix in `OUT_DIR`, so nothing but the git-ignored `zig-pkg/` is written into
//!   `third_party/ghostty`.
//! - **Fast rebuilds across profiles.** The zig cache is shared by every profile of a
//!   target dir (`<target-dir>/ghostty-zig-cache`); only the first build compiles.
//! - **Pinned ABI.** The submodule HEAD must equal [`GHOSTTY_COMMIT`], the commit the
//!   checked-in bindings in `src/ffi/bindings.rs` were generated from.

use std::env;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// Ghostty commit the checked-in bindings (`src/ffi/bindings.rs`) were generated from.
/// Bump together with the submodule and regenerated bindings (see `src/ffi.rs`).
const GHOSTTY_COMMIT: &str = "b40acce58dcf77df52231c3798ea58e924647c89";

/// Passed as `-Dversion-string` so zig never shells out to `git describe`, whose
/// output changes (e.g. after a fetch) would invalidate the whole zig cache.
/// Base version from `third_party/ghostty/build.zig.zon`, build metadata = the pin.
const GHOSTTY_VERSION: &str = "1.3.2-dev+b40acce5";

/// Ghostty's `requireZig` needs this exact major.minor (patch may be newer).
const ZIG_MAJOR_MINOR: &str = "0.16";

/// Every system integration Ghostty's build exposes (`zig build --help`, "Available
/// System Integrations"). `--system` turns them all on; we turn them all back off so
/// the vendored SIMD code (simdutf, highway) is used. Re-check on every Ghostty bump.
const SYSTEM_INTEGRATIONS: [&str; 11] = [
    "freetype",
    "harfbuzz",
    "fontconfig",
    "libpng",
    "zlib",
    "oniguruma",
    "glslang",
    "spirv-cross",
    "simdutf",
    "gtk4-layer-shell",
    "highway",
];

/// Lines of zig stderr kept in the error message.
const STDERR_TAIL_LINES: usize = 40;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            for line in error.to_string().lines() {
                emit(&format!("cargo::error={line}"));
            }
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), BuildError> {
    let manifest_dir = PathBuf::from(env_var("CARGO_MANIFEST_DIR")?);
    let out_dir = PathBuf::from(env_var("OUT_DIR")?);
    let target = env_var_string("TARGET")?;
    let host = env_var_string("HOST")?;
    let zig = env::var_os("ZIG").unwrap_or_else(|| OsString::from("zig"));

    emit("cargo::rerun-if-changed=build.rs");
    emit("cargo::rerun-if-env-changed=ZIG");

    let source = manifest_dir
        .join("..")
        .join("..")
        .join("third_party")
        .join("ghostty");
    let head_file = check_submodule(&source)?;
    emit(&format!("cargo::rerun-if-changed={}", head_file.display()));

    check_zig_version(&zig)?;
    let packages = source.join("zig-pkg");
    check_packages(&packages)?;

    let archive = ArchiveName::for_target(&target);
    let prefix = out_dir.join("ghostty-install");
    let mut build = Command::new(&zig);
    build
        .current_dir(&source)
        .arg("build")
        .arg("-Demit-lib-vt")
        .arg("-Doptimize=ReleaseFast")
        .arg("-Demit-xcframework=false")
        .arg(format!("-Dversion-string={GHOSTTY_VERSION}"))
        .arg("--system")
        .arg(&packages)
        .args(
            SYSTEM_INTEGRATIONS
                .iter()
                .map(|name| format!("-fno-sys={name}")),
        )
        .arg("--cache-dir")
        .arg(shared_cache_dir(&out_dir))
        .arg("--prefix")
        .arg(&prefix);
    if target == host {
        // Native build: keep zig's host libc/SDK detection (required for the MSVC ABI
        // on Windows) but compile for the baseline CPU so the binary is portable.
        build.arg("-Dcpu=baseline");
    } else {
        build.arg(format!("-Dtarget={}", zig_target(&target)?));
    }

    let output = build.output().map_err(|source| BuildError::ZigMissing {
        zig: zig.clone(),
        source,
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("package not found") {
            return Err(BuildError::PackagesMissing { dir: packages });
        }
        return Err(BuildError::ZigFailed {
            status: output.status.to_string(),
            stderr_tail: tail(&stderr, STDERR_TAIL_LINES),
        });
    }

    // Copy only the static archive into its own directory: when the shared library
    // sits next to it, the linker prefers the dylib/so for `-lghostty-vt`.
    let installed = prefix.join("lib").join(archive.file_name);
    if !installed.is_file() {
        return Err(BuildError::MissingArtifact(installed));
    }
    let link_dir = out_dir.join("lib");
    fs::create_dir_all(&link_dir).map_err(|source| BuildError::Io {
        path: link_dir.clone(),
        source,
    })?;
    let linked = link_dir.join(archive.file_name);
    fs::copy(&installed, &linked).map_err(|source| BuildError::Io {
        path: linked.clone(),
        source,
    })?;

    emit(&format!(
        "cargo::rustc-link-search=native={}",
        link_dir.display()
    ));
    emit(&format!(
        "cargo::rustc-link-lib=static={}",
        archive.link_name
    ));
    if target.contains("windows") {
        // Upstream CMakeLists.txt (ghostty-vt-static INTERFACE_LINK_LIBRARIES): the
        // Zig standard library inside the archive calls NT API and kernel32 functions.
        emit("cargo::rustc-link-lib=ntdll");
        emit("cargo::rustc-link-lib=kernel32");
    }
    // macOS needs only libSystem; Linux glibc needs libc/libm/librt. Rust's std
    // already links all of them.
    emit(&format!(
        "cargo::metadata=include={}",
        prefix.join("include").display()
    ));
    Ok(())
}

/// Archive file name as installed by `zig build` and the matching `-l` name.
struct ArchiveName {
    file_name: &'static str,
    link_name: &'static str,
}

impl ArchiveName {
    fn for_target(target: &str) -> Self {
        if target.contains("windows") {
            // Upstream names it `ghostty-vt-static.lib` so it doesn't collide with the
            // DLL import library `ghostty-vt.lib`.
            Self {
                file_name: "ghostty-vt-static.lib",
                link_name: "ghostty-vt-static",
            }
        } else {
            Self {
                file_name: "libghostty-vt.a",
                link_name: "ghostty-vt",
            }
        }
    }
}

/// Map dial's supported Rust targets to zig targets (baseline CPU implied).
fn zig_target(target: &str) -> Result<&'static str, BuildError> {
    match target {
        "aarch64-apple-darwin" => Ok("aarch64-macos"),
        "x86_64-apple-darwin" => Ok("x86_64-macos"),
        "aarch64-unknown-linux-gnu" => Ok("aarch64-linux-gnu"),
        "x86_64-unknown-linux-gnu" => Ok("x86_64-linux-gnu"),
        "aarch64-pc-windows-msvc" => Ok("aarch64-windows-msvc"),
        "x86_64-pc-windows-msvc" => Ok("x86_64-windows-msvc"),
        other => Err(BuildError::UnsupportedTarget(other.to_owned())),
    }
}

/// Verify the submodule is checked out at [`GHOSTTY_COMMIT`]; returns the git `HEAD`
/// file to watch for re-runs.
fn check_submodule(source: &Path) -> Result<PathBuf, BuildError> {
    let header = source.join("include").join("ghostty").join("vt.h");
    if !source.join("build.zig").is_file() || !header.is_file() {
        return Err(BuildError::SubmoduleMissing(source.to_path_buf()));
    }
    let dot_git = source.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        // Submodule checkouts have a `.git` file: "gitdir: <path relative to source>".
        let pointer = read_to_string(&dot_git)?;
        let relative = pointer
            .trim()
            .strip_prefix("gitdir:")
            .ok_or_else(|| BuildError::SubmoduleMissing(source.to_path_buf()))?
            .trim();
        source.join(relative)
    };
    let head_file = git_dir.join("HEAD");
    let head = read_to_string(&head_file)?;
    let head = head.trim();
    let commit = match head.strip_prefix("ref:") {
        // Attached HEAD (someone checked out a branch): resolve the loose ref.
        Some(reference) => read_to_string(&git_dir.join(reference.trim()))?
            .trim()
            .to_owned(),
        None => head.to_owned(),
    };
    if commit != GHOSTTY_COMMIT {
        return Err(BuildError::SubmoduleCommit { found: commit });
    }
    Ok(head_file)
}

fn check_zig_version(zig: &OsString) -> Result<(), BuildError> {
    let output = Command::new(zig)
        .arg("version")
        .output()
        .map_err(|source| BuildError::ZigMissing {
            zig: zig.clone(),
            source,
        })?;
    let found = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let matches = found
        .strip_prefix(ZIG_MAJOR_MINOR)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'));
    if output.status.success() && matches {
        Ok(())
    } else {
        Err(BuildError::ZigVersion { found })
    }
}

fn check_packages(packages: &Path) -> Result<(), BuildError> {
    let has_any = fs::read_dir(packages).is_ok_and(|mut entries| entries.next().is_some());
    if has_any {
        Ok(())
    } else {
        Err(BuildError::PackagesMissing {
            dir: packages.to_path_buf(),
        })
    }
}

/// `OUT_DIR` is `<dir>/<profile>/build/<pkg>-<hash>/out`; share the zig cache at
/// `<dir>/ghostty-zig-cache` so debug, release, test, and clippy builds reuse it.
/// Zig's cache is content-addressed and safe for concurrent use.
fn shared_cache_dir(out_dir: &Path) -> PathBuf {
    let mut ancestors = out_dir.ancestors();
    let build = ancestors.nth(2);
    let dir = ancestors.nth(1);
    match (build, dir) {
        (Some(build), Some(dir)) if build.file_name().is_some_and(|name| name == "build") => {
            dir.join("ghostty-zig-cache")
        }
        _ => out_dir.join("ghostty-zig-cache"),
    }
}

fn tail(text: &str, lines: usize) -> String {
    let mut kept: Vec<&str> = text.lines().rev().take(lines).collect();
    kept.reverse();
    kept.join("\n")
}

fn read_to_string(path: &Path) -> Result<String, BuildError> {
    fs::read_to_string(path).map_err(|source| BuildError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn env_var(name: &'static str) -> Result<OsString, BuildError> {
    env::var_os(name).ok_or(BuildError::Env(name))
}

fn env_var_string(name: &'static str) -> Result<String, BuildError> {
    env_var(name)?
        .into_string()
        .map_err(|_| BuildError::Env(name))
}

/// Cargo reads build-script directives from stdout (clippy exempts build scripts
/// from `print_stdout`).
fn emit(line: &str) {
    println!("{line}");
}

#[derive(Debug)]
enum BuildError {
    Env(&'static str),
    Io { path: PathBuf, source: io::Error },
    SubmoduleMissing(PathBuf),
    SubmoduleCommit { found: String },
    ZigMissing { zig: OsString, source: io::Error },
    ZigVersion { found: String },
    PackagesMissing { dir: PathBuf },
    UnsupportedTarget(String),
    ZigFailed { status: String, stderr_tail: String },
    MissingArtifact(PathBuf),
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const INIT: &str = "git submodule update --init --depth 1 third_party/ghostty";
        const FETCH: &str = "zig build --build-file third_party/ghostty/build.zig --fetch=all";
        match self {
            Self::Env(name) => write!(f, "cargo did not set {name} (or it is not UTF-8)"),
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::SubmoduleMissing(dir) => write!(
                f,
                "Ghostty sources are missing at {}.\nInitialise the submodule from the repo root:\n  {INIT}",
                dir.display()
            ),
            Self::SubmoduleCommit { found } => write!(
                f,
                "third_party/ghostty is at {found}, but dial-ghostty's bindings were generated \
                 for {GHOSTTY_COMMIT}.\nRestore the pinned commit:\n  {INIT}\nor bump the pin \
                 (GHOSTTY_COMMIT in build.rs + regenerate src/ffi/bindings.rs, see src/ffi.rs)."
            ),
            Self::ZigMissing { zig, source } => write!(
                f,
                "could not run `{}`: {source}\nInstall zig {ZIG_MAJOR_MINOR}.x (or point $ZIG at it).",
                zig.to_string_lossy()
            ),
            Self::ZigVersion { found } => write!(
                f,
                "zig {ZIG_MAJOR_MINOR}.x is required to build libghostty-vt, found `{found}`.\n\
                 Ghostty's build accepts only this major.minor; install it or point $ZIG at it."
            ),
            Self::PackagesMissing { dir } => write!(
                f,
                "Ghostty's Zig packages are missing or incomplete in {}.\n\
                 The build never downloads. Fetch them once from the repo root (needs network):\n  \
                 {FETCH}",
                dir.display()
            ),
            Self::UnsupportedTarget(target) => write!(
                f,
                "target {target} is not supported by dial-ghostty (supported: \
                 {{aarch64,x86_64}}-apple-darwin, {{aarch64,x86_64}}-unknown-linux-gnu, \
                 {{aarch64,x86_64}}-pc-windows-msvc)"
            ),
            Self::ZigFailed {
                status,
                stderr_tail,
            } => write!(f, "zig build failed ({status}):\n{stderr_tail}"),
            Self::MissingArtifact(path) => write!(
                f,
                "zig build succeeded but {} was not produced",
                path.display()
            ),
        }
    }
}
