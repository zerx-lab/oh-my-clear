//! Workspace structure invariants (ADR 0008, ADR 0010), run by `cargo xtask layers` and as
//! the first step of `cargo xtask ci`:
//! - every member's `dial*` dependencies (normal, dev, build, target-specific) follow [`LAYERS`];
//! - [`CONFINED`] external crates appear only in their owning members (the daemon never links
//!   gpui, the UI never links adapters);
//! - every member inherits `[workspace.lints]`, except [`UNSAFE_ISLANDS`], whose own table
//!   must equal the workspace table apart from `unsafe_code`.
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
    ("dial-proto", &[]),
    ("dial-core", &["dial-proto"]),
    ("dial-ipc", &["dial-proto"]),
    ("dial-telemetry", &[]),
    ("dial-process", &[]),
    ("dial-ghostty", &[]),
    ("dial-term", &["dial-ghostty", "dial-proto"]),
    ("dial-git", &["dial-process"]),
    ("dial-store", &["dial-proto"]),
    ("dial-llm", &[]),
    (
        "dial-agent",
        &["dial-proto", "dial-core", "dial-process", "dial-term"],
    ),
    (
        "dial-native",
        &[
            "dial-proto",
            "dial-core",
            "dial-agent",
            "dial-llm",
            "dial-process",
            "dial-git",
        ],
    ),
    ("dial-mcp", &["dial-proto", "dial-core"]),
    (
        "dial-engine",
        &[
            "dial-proto",
            "dial-core",
            "dial-ipc",
            "dial-process",
            "dial-term",
            "dial-git",
            "dial-store",
            "dial-llm",
            "dial-agent",
            "dial-native",
            "dial-mcp",
        ],
    ),
    (
        "dial-ui",
        &["dial-proto", "dial-core", "dial-term", "dial-ipc"],
    ),
    (
        "dial",
        &["dial-ui", "dial-ipc", "dial-proto", "dial-telemetry"],
    ),
    (
        "dial-daemon",
        &["dial-engine", "dial-ipc", "dial-proto", "dial-telemetry"],
    ),
    ("xtask", &[]),
];

/// External crates that only the listed members may depend on directly.
const CONFINED: &[(&str, &[&str])] = &[
    ("gpui-kit", &["dial-ui", "dial"]),
    ("alacritty_terminal", &["dial-process"]),
    ("agent-client-protocol-schema", &["dial-agent"]),
];

/// Members allowed to contain `unsafe` (ADR 0010). Cargo cannot override one key of an
/// inherited lint table, so each island carries a full copy.
const UNSAFE_ISLANDS: &[&str] = &["dial-ghostty"];

pub(crate) fn check(root: &Path) -> Result<()> {
    let workspace = read(&root.join("Cargo.toml"))?;
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
    let problems = violations(&workspace, &manifests);
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

fn violations(workspace: &str, manifests: &[String]) -> Vec<String> {
    let mut problems = Vec::new();
    let workspace_lints = lint_entries(workspace, "workspace.lints.");
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
            if dep.starts_with("dial") && !allowed.contains(&dep.as_str()) {
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
        problems.extend(lint_problems(name, text, &workspace_lints));
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

fn lint_problems(name: &str, text: &str, workspace_lints: &BTreeSet<String>) -> Vec<String> {
    let inherits = entries(text).any(|(section, line)| {
        section == "lints" && line.split_whitespace().collect::<String>() == "workspace=true"
    });
    if !UNSAFE_ISLANDS.contains(&name) {
        return if inherits {
            Vec::new()
        } else {
            vec![format!("`{name}` must set `[lints] workspace = true`")]
        };
    }
    let own = lint_entries(text, "lints.");
    let mut problems = Vec::new();
    for missing in workspace_lints.difference(&own) {
        problems.push(format!(
            "unsafe island `{name}` lacks workspace lint `{missing}`"
        ));
    }
    for extra in own.difference(workspace_lints) {
        problems.push(format!(
            "unsafe island `{name}` has `{extra}`, which differs from [workspace.lints]"
        ));
    }
    problems
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

/// `rust:key=value` / `clippy:key=value` entries (whitespace removed) of the lint tables
/// whose header starts with `prefix` (`workspace.lints.` or `lints.`), minus `unsafe_code`.
fn lint_entries(text: &str, prefix: &str) -> BTreeSet<String> {
    entries(text)
        .filter_map(|(section, line)| {
            let table = section.strip_prefix(prefix)?;
            let entry: String = line.split_whitespace().collect();
            (!entry.starts_with("unsafe_code=")).then(|| format!("{table}:{entry}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE: &str = "[workspace.lints.rust]\nunsafe_code = \"forbid\"\nunreachable_pub = \"warn\"\n\n[workspace.lints.clippy]\nunwrap_used = \"deny\" # no panics\n";

    fn member(name: &str, deps: &str) -> String {
        format!("[package]\nname = \"{name}\"\n\n{deps}\n\n[lints]\nworkspace = true\n")
    }

    /// Minimal valid workspace: every LAYERS entry exists with no dependencies.
    fn baseline() -> Vec<String> {
        LAYERS
            .iter()
            .map(|(name, _)| {
                if UNSAFE_ISLANDS.contains(name) {
                    format!(
                        "[package]\nname = \"{name}\"\n[lints.rust]\nunsafe_code = \"deny\"\nunreachable_pub = \"warn\"\n[lints.clippy]\nunwrap_used = \"deny\"\n"
                    )
                } else {
                    member(name, "")
                }
            })
            .collect()
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
        let text = "[package]\nname = \"x\"\ndescription = \"dependencies = none\"\n\n[dependencies]\ndial-core.workspace = true\nserde = { workspace = true, features = [\"derive\"] }\n# dial-engine.workspace = true\n\n[dev-dependencies]\n\"dial-proto\".workspace = true\n\n[target.'cfg(windows)'.dependencies]\nwindows.workspace = true\n\n[dependencies.dial-ipc]\nworkspace = true\nfeatures = [\n  \"client\",\n]\n";
        let deps = dependencies(text);
        let expected: BTreeSet<String> =
            ["dial-core", "serde", "dial-proto", "windows", "dial-ipc"]
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
            "dial-ui",
            member(
                "dial-ui",
                "[dependencies]\ndial-proto.workspace = true\ngpui-kit.workspace = true",
            ),
        );
        let problems = violations(WORKSPACE, &manifests);
        assert!(problems.is_empty(), "allowed edges must pass: {problems:?}");
    }

    #[test]
    fn ui_depending_on_engine_is_rejected_even_as_dev_dependency() {
        let manifests = with(
            "dial-ui",
            member(
                "dial-ui",
                "[dev-dependencies]\ndial-engine.workspace = true",
            ),
        );
        let problems = violations(WORKSPACE, &manifests);
        assert!(
            problems
                .iter()
                .any(|p| p.contains("`dial-ui` must not depend on `dial-engine`")),
            "UI → engine edge must be reported: {problems:?}"
        );
    }

    #[test]
    fn daemon_linking_gpui_is_rejected() {
        let manifests = with(
            "dial-daemon",
            member("dial-daemon", "[dependencies]\ngpui-kit.workspace = true"),
        );
        let problems = violations(WORKSPACE, &manifests);
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
            .filter(|m| package_name(m) != Some("dial-llm"))
            .collect();
        manifests.push(member("dial-surprise", ""));
        let problems = violations(WORKSPACE, &manifests);
        assert!(
            problems
                .iter()
                .any(|p| p.contains("`dial-surprise` is missing")),
            "unlisted member: {problems:?}"
        );
        assert!(
            problems
                .iter()
                .any(|p| p.contains("LAYERS lists `dial-llm`")),
            "stale table entry: {problems:?}"
        );
    }

    #[test]
    fn member_without_workspace_lints_is_rejected() {
        let manifests = with("dial-core", "[package]\nname = \"dial-core\"\n".to_owned());
        let problems = violations(WORKSPACE, &manifests);
        assert!(
            problems
                .iter()
                .any(|p| p.contains("`dial-core` must set `[lints] workspace = true`")),
            "missing lint inheritance: {problems:?}"
        );
    }

    #[test]
    fn unsafe_island_lint_drift_is_reported_both_ways() {
        let Some(island) = UNSAFE_ISLANDS.first() else {
            return;
        };
        let drifted = format!(
            "[package]\nname = \"{island}\"\n[lints.rust]\nunsafe_code = \"deny\"\n[lints.clippy]\nunwrap_used = \"deny\"\ntodo = \"allow\"\n"
        );
        let problems = violations(WORKSPACE, &with(island, drifted));
        assert!(
            problems
                .iter()
                .any(|p| p.contains("lacks workspace lint `rust:unreachable_pub=\"warn\"`")),
            "missing lint: {problems:?}"
        );
        assert!(
            problems
                .iter()
                .any(|p| p.contains("has `clippy:todo=\"allow\"`")),
            "extra lint: {problems:?}"
        );
    }
}
