//! Workspace automation, run as `cargo xtask <command>` (aliases in `.cargo/config.toml`).
//!
//! - `ci`: layering check, then every gate in order (fmt, clippy, nextest, deny); stops at the
//!   first failure.
//! - `layers`: crate layering, confined crates, and lint inheritance (see `layers.rs`).
//! - `new-crate <area> "<purpose>"`: scaffold `crates/dial-<area>` with workspace lints.

mod layers;

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Instant;

const USAGE: &str = "usage: cargo xtask <command>

commands:
  ci                             run the layering check, then all gates: fmt, clippy, nextest, deny
  layers                         check crate layering (ADR 0008) and lint inheritance
  new-crate <area> \"<purpose>\"   scaffold crates/dial-<area> (library, workspace lints)";

/// Gate sequence; keep in sync with AGENTS.md "Development Commands".
const GATES: &[&[&str]] = &[
    &["fmt", "--all", "--check"],
    &[
        "clippy",
        "--workspace",
        "--all-targets",
        "--all-features",
        "--locked",
        "--",
        "-D",
        "warnings",
    ],
    &[
        "nextest",
        "run",
        "--workspace",
        "--all-features",
        "--locked",
    ],
    &["deny", "--all-features", "check"],
];

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("{USAGE}")]
    Usage,
    #[error("unknown command `{0}`\n\n{USAGE}")]
    UnknownCommand(String),
    #[error(
        "invalid area `{0}`: use lowercase ASCII letters, digits and single hyphens, \
         starting with a letter and without a `dial` prefix (e.g. `engine`, `agent-acp`)"
    )]
    InvalidArea(String),
    #[error("purpose must be one non-empty line without quotes or backslashes")]
    InvalidPurpose,
    #[error("{} already exists", .0.display())]
    AlreadyExists(PathBuf),
    #[error("cannot start `cargo {args}`: {source}")]
    Spawn { args: String, source: io::Error },
    #[error("gate failed: `cargo {0}`")]
    GateFailed(String),
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("xtask must live one directory below the workspace root")]
    WorkspaceRoot,
    #[error("workspace structure violations:\n{0}")]
    Layering(String),
}

type Result<T, E = Error> = std::result::Result<T, E>;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            report(&format!("xtask: {err}"));
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<()> {
    match args {
        [] => Err(Error::Usage),
        [cmd, ..] if matches!(cmd.as_str(), "help" | "-h" | "--help") => {
            report(USAGE);
            Ok(())
        }
        [cmd] if cmd == "ci" => ci(&workspace_root()?),
        [cmd] if cmd == "layers" => layers::check(&workspace_root()?),
        [cmd, area, purpose] if cmd == "new-crate" => {
            let dir = new_crate(&workspace_root()?, area, purpose)?;
            report(&format!(
                "created {}; the `crates/*` glob makes it a workspace member. \
                 Add it to LAYERS in xtask/src/layers.rs (allowed internal deps), and to \
                 [workspace.dependencies] once another member depends on it.",
                dir.display()
            ));
            Ok(())
        }
        [cmd, ..] if matches!(cmd.as_str(), "ci" | "layers" | "new-crate") => Err(Error::Usage),
        [cmd, ..] => Err(Error::UnknownCommand(cmd.clone())),
    }
}

fn workspace_root() -> Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .ok_or(Error::WorkspaceRoot)
}

fn ci(root: &Path) -> Result<()> {
    // `cargo run` exports CARGO; reuse it so every gate runs on the same toolchain.
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let total = Instant::now();
    report("==> xtask layers");
    layers::check(root)?;
    for gate in GATES {
        let shown = gate.join(" ");
        report(&format!("==> cargo {shown}"));
        let started = Instant::now();
        let status = Command::new(&cargo)
            .args(gate.iter())
            .current_dir(root)
            .status()
            .map_err(|source| Error::Spawn {
                args: shown.clone(),
                source,
            })?;
        if !status.success() {
            return Err(Error::GateFailed(shown));
        }
        report(&format!(
            "    ok in {:.1}s",
            started.elapsed().as_secs_f64()
        ));
    }
    report(&format!(
        "all gates passed in {:.1}s",
        total.elapsed().as_secs_f64()
    ));
    Ok(())
}

fn validate_area(area: &str) -> Result<()> {
    let valid = area.starts_with(|c: char| c.is_ascii_lowercase())
        && !area.ends_with('-')
        && !area.contains("--")
        && area != "dial"
        && !area.starts_with("dial-")
        && area
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if valid {
        Ok(())
    } else {
        Err(Error::InvalidArea(area.to_owned()))
    }
}

fn new_crate(root: &Path, area: &str, purpose: &str) -> Result<PathBuf> {
    validate_area(area)?;
    let purpose = purpose.trim();
    if purpose.is_empty() || purpose.contains(['\n', '\r', '"', '\\']) {
        return Err(Error::InvalidPurpose);
    }
    let name = format!("dial-{area}");
    let crates = root.join("crates");
    fs::create_dir_all(&crates).map_err(|source| Error::Io {
        path: crates.clone(),
        source,
    })?;
    let dir = crates.join(&name);
    // create_dir (not _all) fails atomically if the crate already exists.
    fs::create_dir(&dir).map_err(|source| {
        if source.kind() == io::ErrorKind::AlreadyExists {
            Error::AlreadyExists(dir.clone())
        } else {
            Error::Io {
                path: dir.clone(),
                source,
            }
        }
    })?;
    let src = dir.join("src");
    fs::create_dir(&src).map_err(|source| Error::Io {
        path: src.clone(),
        source,
    })?;
    write_file(&dir.join("Cargo.toml"), &manifest(&name, purpose))?;
    write_file(&src.join("lib.rs"), &format!("//! {purpose}\n"))?;
    Ok(dir)
}

fn manifest(name: &str, purpose: &str) -> String {
    format!(
        r#"[package]
name = "{name}"
description = "{purpose}"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
publish.workspace = true

[lib]
doctest = false # cargo-nextest is the only runner (ADR 0006)

[dependencies]

[lints]
workspace = true
"#
    )
}

fn write_file(path: &Path, contents: &str) -> Result<()> {
    fs::write(path, contents).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[expect(
    clippy::print_stderr,
    reason = "xtask is a CLI; stderr is its user interface"
)]
fn report(message: &str) {
    eprintln!("{message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn area_names_follow_crate_naming() {
        for ok in ["engine", "agent-acp", "store2"] {
            assert!(validate_area(ok).is_ok(), "`{ok}` should be accepted");
        }
        for bad in [
            "",
            "Engine",
            "2store",
            "-x",
            "x-",
            "a--b",
            "a_b",
            "a b",
            "dial",
            "dial-engine",
        ] {
            assert!(
                matches!(validate_area(bad), Err(Error::InvalidArea(_))),
                "`{bad}` should be rejected"
            );
        }
    }

    #[test]
    fn new_crate_scaffolds_once_and_never_overwrites() {
        let root = env::temp_dir().join(format!("dial-xtask-{}", std::process::id()));
        let first = new_crate(&root, "probe", "Probe crate.");
        let second = new_crate(&root, "probe", "Other purpose.");
        let lib = fs::read_to_string(root.join("crates/dial-probe/src/lib.rs"));
        let manifest_exists = root.join("crates/dial-probe/Cargo.toml").is_file();
        let cleanup = fs::remove_dir_all(&root);

        assert!(first.is_ok(), "first scaffold succeeds: {first:?}");
        assert!(
            matches!(second, Err(Error::AlreadyExists(_))),
            "second scaffold must refuse: {second:?}"
        );
        assert!(
            lib.is_ok_and(|text| text.contains("Probe crate.")),
            "existing lib.rs must keep the first purpose"
        );
        assert!(manifest_exists, "Cargo.toml is written");
        assert!(cleanup.is_ok(), "temp dir removed: {cleanup:?}");
    }

    #[test]
    fn new_crate_rejects_purpose_that_would_break_the_manifest() {
        let root = env::temp_dir().join(format!("dial-xtask-purpose-{}", std::process::id()));
        for bad in ["", "   ", "two\nlines", "has \"quotes\"", "back\\slash"] {
            assert!(
                matches!(new_crate(&root, "probe", bad), Err(Error::InvalidPurpose)),
                "purpose {bad:?} should be rejected"
            );
        }
        assert!(!root.exists(), "nothing is written for a rejected purpose");
    }
}
