//! Workspace structure invariants (ADR 0008), run by `cargo xtask layers` and as the first
//! step of `cargo xtask ci`:
//! - every member's internal dependencies (`omc-*`, `oh-my-clear*`; normal, dev, build,
//!   target-specific) follow [`LAYERS`];
//! - [`CONFINED`] external crates appear only in their owning members (the daemon never links
//!   gpui);
//! - every member inherits `[workspace.lints]` (so `unsafe_code = "forbid"` holds everywhere).
//!
//! Manifests are read as text: members follow the `name.workspace = true` convention, so a
//! line scanner is enough and xtask stays std-only.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use crate::{Error, Result};

/// Allowed internal dependencies per member. Every member must be listed; a new crate or
/// edge means editing this table (and ADR 0008 when the direction changes).
const LAYERS: &[(&str, &[&str])] = &[
    ("omc-proto", &[]),
    ("omc-ipc", &["omc-proto"]),
    ("omc-telemetry", &[]),
    ("omc-engine", &["omc-proto", "omc-ipc"]),
    ("omc-ui", &["omc-proto", "omc-ipc"]),
    (
        "oh-my-clear",
        &["omc-ui", "omc-ipc", "omc-proto", "omc-telemetry"],
    ),
    (
        "oh-my-clear-daemon",
        &["omc-engine", "omc-ipc", "omc-proto", "omc-telemetry"],
    ),
    ("xtask", &[]),
];

/// External crates that only the listed members may depend on directly: the GUI toolkit
/// stays out of the daemon (ADR 0008), the tray stays out of the UI (ADR 0020).
const CONFINED: &[(&str, &[&str])] = &[
    ("gpui-kit", &["omc-ui", "oh-my-clear"]),
    ("tray-icon", &["oh-my-clear-daemon"]),
    ("tao", &["oh-my-clear-daemon"]),
];

pub(crate) fn check(root: &Path) -> Result<()> {
    let mut manifests = Vec::new();
    for group in ["apps", "crates"] {
        let dir = root.join(group);
        let entries = fs::read_dir(&dir).map_err(|source| Error::Io {
            path: dir.clone(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| Error::Io {
                path: dir.clone(),
                source,
            })?;
            let manifest = entry.path().join("Cargo.toml");
            if manifest.is_file() {
                manifests.push(read(&manifest)?);
            }
        }
    }
    manifests.push(read(&root.join("xtask").join("Cargo.toml"))?);
    let problems = violations(&manifests);
    if problems.is_empty() {
        Ok(())
    } else {
        Err(Error::Layering(problems.join("\n")))
    }
}

fn read(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn violations(manifests: &[String]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut seen = BTreeSet::new();
    for text in manifests {
        let Some(name) = package_name(text) else {
            problems.push("a member manifest has no [package] name".to_owned());
            continue;
        };
        seen.insert(name.to_owned());
        let Some(allowed) = lookup(LAYERS, name) else {
            problems.push(format!(
                "`{name}` is missing from the LAYERS table in xtask/src/layers.rs"
            ));
            continue;
        };
        for dep in dependencies(text) {
            if is_internal(&dep) && !allowed.contains(&dep.as_str()) {
                problems.push(format!(
                    "`{name}` must not depend on `{dep}` (ADR 0008 layering)"
                ));
            }
            if let Some(owners) = lookup(CONFINED, &dep)
                && !owners.contains(&name)
            {
                problems.push(format!(
                    "`{dep}` is confined to {owners:?}; `{name}` must not depend on it"
                ));
            }
        }
        let inherits_lints = entries(text).any(|(section, line)| {
            section == "lints" && line.split_whitespace().collect::<String>() == "workspace=true"
        });
        if !inherits_lints {
            problems.push(format!("`{name}` must set `[lints] workspace = true`"));
        }
    }
    for (name, _) in LAYERS {
        if !seen.contains(*name) {
            problems.push(format!(
                "LAYERS lists `{name}`, but no workspace member has that name"
            ));
        }
    }
    problems
}

/// Whether `dep` names a workspace member.
fn is_internal(dep: &str) -> bool {
    dep.starts_with("omc-") || dep.starts_with("oh-my-clear")
}

fn lookup<'a>(table: &[(&str, &'a [&'a str])], key: &str) -> Option<&'a [&'a str]> {
    table
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, values)| *values)
}

/// `(section, line)` for every non-empty, non-header line; comments stripped.
fn entries(text: &str) -> impl Iterator<Item = (&str, &str)> {
    let mut section = "";
    text.lines().filter_map(move |raw| {
        let line = raw.split_once('#').map_or(raw, |(code, _)| code).trim();
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = header.trim();
            return None;
        }
        (!line.is_empty()).then_some((section, line))
    })
}

fn headers(text: &str) -> impl Iterator<Item = &str> {
    text.lines().filter_map(|raw| {
        raw.trim()
            .strip_prefix('[')
            .and_then(|l| l.strip_suffix(']'))
            .map(str::trim)
    })
}

fn package_name(text: &str) -> Option<&str> {
    entries(text).find_map(|(section, line)| {
        let (key, value) = line.split_once('=')?;
        (section == "package" && key.trim() == "name").then(|| value.trim().trim_matches('"'))
    })
}

/// Names of every dependency in `[dependencies]`, `[dev-dependencies]`,
/// `[build-dependencies]`, their `target.….` variants, and dotted `[dependencies.x]` tables.
fn dependencies(text: &str) -> BTreeSet<String> {
    let mut deps = BTreeSet::new();
    for (section, line) in entries(text) {
        if !section.ends_with("dependencies") {
            continue;
        }
        if let Some((key, _)) = line.split_once('=') {
            let key = key.trim();
            let key = key.strip_suffix(".workspace").unwrap_or(key);
            deps.insert(key.trim_matches('"').to_owned());
        }
    }
    for header in headers(text) {
        if let Some((_, dep)) = header.rsplit_once("dependencies.") {
            deps.insert(dep.trim_matches('"').to_owned());
        }
    }
    deps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(name: &str, deps: &str) -> String {
        format!("[package]\nname = \"{name}\"\n\n{deps}\n\n[lints]\nworkspace = true\n")
    }

    /// Minimal valid workspace: every LAYERS entry exists with no dependencies.
    fn baseline() -> Vec<String> {
        LAYERS.iter().map(|(name, _)| member(name, "")).collect()
    }

    fn with(name: &str, text: String) -> Vec<String> {
        let mut manifests: Vec<String> = baseline()
            .into_iter()
            .filter(|m| package_name(m) != Some(name))
            .collect();
        manifests.push(text);
        manifests
    }

    #[test]
    fn dependency_scanner_sees_every_table_form() {
        let text = "[package]\nname = \"x\"\ndescription = \"dependencies = none\"\n\n[dependencies]\nomc-telemetry.workspace = true\nserde = { workspace = true, features = [\"derive\"] }\n# omc-engine.workspace = true\n\n[dev-dependencies]\n\"omc-proto\".workspace = true\n\n[target.'cfg(windows)'.dependencies]\nwindows.workspace = true\n\n[dependencies.omc-ipc]\nworkspace = true\nfeatures = [\n  \"client\",\n]\n";
        let deps = dependencies(text);
        let expected: BTreeSet<String> =
            ["omc-telemetry", "serde", "omc-proto", "windows", "omc-ipc"]
                .into_iter()
                .map(str::to_owned)
                .collect();
        assert_eq!(
            deps, expected,
            "commented-out and non-dependency keys are ignored"
        );
    }

    #[test]
    fn declared_layering_passes() {
        let manifests = with(
            "omc-ui",
            member(
                "omc-ui",
                "[dependencies]\nomc-proto.workspace = true\ngpui-kit.workspace = true",
            ),
        );
        let problems = violations(&manifests);
        assert!(problems.is_empty(), "allowed edges must pass: {problems:?}");
    }

    #[test]
    fn ui_depending_on_engine_is_rejected_even_as_dev_dependency() {
        let manifests = with(
            "omc-ui",
            member("omc-ui", "[dev-dependencies]\nomc-engine.workspace = true"),
        );
        let problems = violations(&manifests);
        assert!(
            problems
                .iter()
                .any(|p| p.contains("`omc-ui` must not depend on `omc-engine`")),
            "UI → engine edge must be reported: {problems:?}"
        );
    }

    #[test]
    fn daemon_linking_gpui_is_rejected() {
        let manifests = with(
            "oh-my-clear-daemon",
            member(
                "oh-my-clear-daemon",
                "[dependencies]\ngpui-kit.workspace = true",
            ),
        );
        let problems = violations(&manifests);
        assert!(
            problems
                .iter()
                .any(|p| p.contains("`gpui-kit` is confined")),
            "daemon must never link gpui: {problems:?}"
        );
    }

    #[test]
    fn unknown_member_and_stale_table_entry_are_reported() {
        let mut manifests: Vec<String> = baseline()
            .into_iter()
            .filter(|m| package_name(m) != Some("omc-telemetry"))
            .collect();
        manifests.push(member("omc-surprise", ""));
        let problems = violations(&manifests);
        assert!(
            problems
                .iter()
                .any(|p| p.contains("`omc-surprise` is missing")),
            "unlisted member: {problems:?}"
        );
        assert!(
            problems
                .iter()
                .any(|p| p.contains("LAYERS lists `omc-telemetry`")),
            "stale table entry: {problems:?}"
        );
    }

    #[test]
    fn member_without_workspace_lints_is_rejected() {
        let manifests = with("omc-proto", "[package]\nname = \"omc-proto\"\n".to_owned());
        let problems = violations(&manifests);
        assert!(
            problems
                .iter()
                .any(|p| p.contains("`omc-proto` must set `[lints] workspace = true`")),
            "missing lint inheritance: {problems:?}"
        );
    }
}
