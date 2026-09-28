//! Per-user configuration outside `~/Library`: XDG folders (`~/.config/ghostty`) and home
//! dot entries (`~/.claude`, `~/.vscode`), judged by [`crate::userconf`] with the app's
//! bundle executables, bundled command-line tools and Electron `app.asar` as binary
//! evidence, its open files as direct evidence and the [`attribution`] engine for rival
//! apps.

use std::path::{Path, PathBuf};

use omc_proto::apps::{AppFileKind, Confidence};

use super::attribution::{self, LOW, NameKind, Place, Profile};
use super::sources;
use crate::AppRecord;
use crate::userconf::{self, AppSide, Lead, Limits, Names, Rival, Signals};

/// One attributed user-config item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Item {
    pub(super) path: PathBuf,
    pub(super) kind: AppFileKind,
    pub(super) confidence: Confidence,
}

/// What the bundle ships: files to search, most telling first, the names of its
/// executables and the names of its command-line tools.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Bundle {
    files: Vec<PathBuf>,
    programs: Vec<String>,
    tools: Vec<String>,
}

/// The app's names for user-config matching: display and bundle names, profile names
/// (the bundle-id tail and vendor words only hint), its executables and its command-line
/// tools.
fn names(app: &AppRecord, me: &Profile, bundle: &Bundle) -> Names {
    let mut out = Names::default();
    out.add_display(&app.info.name);
    if let Some(stem) = app.detail.bundle.file_stem() {
        out.add_display(&stem.to_string_lossy());
    }
    for (name, kind) in &me.names {
        match kind {
            NameKind::IdTail => out.add_hint(name),
            NameKind::Primary | NameKind::Product | NameKind::Updater | NameKind::Executable => {
                out.add(name);
            }
        }
    }
    if me.sole_vendor {
        for word in &me.vendor_words {
            out.add_hint(word);
        }
    }
    for program in &bundle.programs {
        out.add(program);
    }
    for tool in &bundle.tools {
        out.add_command(tool);
    }
    out
}

/// Regular files directly in `dir` (symlinks followed), sorted; only executable ones with
/// `executable`.
fn files_in(dir: &Path, executable: bool) -> Vec<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            std::fs::metadata(p)
                .is_ok_and(|m| m.is_file() && (!executable || m.permissions().mode() & 0o111 != 0))
        })
        .collect();
    out.sort();
    out
}

/// Folders named `bin` or `CLI` in `Resources` or one level below it (`Resources/CLI`,
/// `Resources/app/bin`); localisations skipped.
fn tool_dirs(resources: &Path) -> Vec<PathBuf> {
    let is_tool_dir =
        |name: &str| name.eq_ignore_ascii_case("bin") || name.eq_ignore_ascii_case("cli");
    let subdirs = |dir: &Path| -> Vec<(String, PathBuf)> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut out: Vec<(String, PathBuf)> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| Some((e.file_name().to_str()?.to_owned(), e.path())))
            .filter(|(_, path)| {
                path.extension()
                    .is_none_or(|ext| !ext.eq_ignore_ascii_case("lproj"))
            })
            .collect();
        out.sort();
        out
    };
    let mut out = Vec::new();
    for (name, path) in subdirs(resources) {
        if is_tool_dir(&name) {
            out.push(path);
            continue;
        }
        out.extend(
            subdirs(&path)
                .into_iter()
                .filter(|(child, _)| is_tool_dir(child))
                .map(|(_, p)| p),
        );
    }
    out
}

/// The bundle's executables (main first), its command-line tools (`bin`/`CLI` folders in
/// `Resources`, links into the bundle from a `bin` folder) and its Electron `app.asar`.
fn bundle(app: &AppRecord) -> Bundle {
    let contents = app.detail.real.join("Contents");
    let macos = contents.join("MacOS");
    let resources = contents.join("Resources");
    let mut out = Bundle::default();
    let file_name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned());
    if let Some(exe) = &app.detail.executable {
        let main = macos.join(exe);
        if main.is_file() {
            out.files.push(main);
        }
    }
    for file in files_in(&macos, false) {
        out.programs.extend(file_name(&file));
        if !out.files.contains(&file) {
            out.files.push(file);
        }
    }
    for dir in tool_dirs(&resources) {
        for file in files_in(&dir, true) {
            out.tools.extend(file_name(&file));
            if !out.files.contains(&file) {
                out.files.push(file);
            }
        }
    }
    let asar = resources.join("app.asar");
    if asar.is_file() {
        out.files.push(asar);
    }
    for link in sources::cli_links(&app.detail.bundle, &app.detail.real) {
        out.tools.extend(file_name(&link));
    }
    out
}

/// How strongly other installed apps claim `stem` compared with `me` (attribution points
/// of the name in the home folder).
pub(super) fn rival(me: &Profile, others: &[Profile], stem: &str) -> Rival {
    let mine = attribution::score(me, stem, Place::Home).points();
    let theirs = others
        .iter()
        .map(|o| attribution::score(o, stem, Place::Home).points())
        .max()
        .unwrap_or(0);
    if theirs < LOW {
        Rival::None
    } else if theirs >= mine {
        Rival::Equal
    } else {
        Rival::Weaker
    }
}

/// Judges `leads`: rival apps, open files (resolved paths) and the removal guard (`allowed`).
pub(super) fn judge(
    leads: Vec<Lead>,
    me: &Profile,
    others: &[Profile],
    open: &[PathBuf],
    allowed: impl Fn(&Path) -> bool,
) -> Vec<Item> {
    leads
        .into_iter()
        .filter_map(|lead| {
            let c = &lead.candidate;
            let signals = Signals {
                name: Some(lead.name),
                binary: lead.binary,
                open: open.iter().any(|p| omc_scan::paths::is_within(p, &c.path)),
                command: lead.command(),
                rival: rival(me, others, &c.stem),
            };
            let confidence = userconf::confidence(signals)?;
            let kind = c.kind();
            let keep = allowed(&c.path);
            keep.then_some(Item {
                kind,
                path: lead.candidate.path,
                confidence,
            })
        })
        .collect()
}

/// The app's user-config items below `home`.
pub(super) fn user_config(
    app: &AppRecord,
    me: &Profile,
    others: &[Profile],
    open: &[PathBuf],
    home: &Path,
    threads: usize,
    allowed: impl Fn(&Path) -> bool,
) -> Vec<Item> {
    let bundle = bundle(app);
    let names = names(app, me, &bundle);
    let own = [app.detail.real.clone(), app.detail.bundle.clone()];
    let side = AppSide {
        names: &names,
        files: &bundle.files,
        own: &own,
        wide: false,
    };
    let dirs = userconf::command_dirs(std::env::var_os("PATH").as_deref(), Some(home));
    let (leads, stats) = userconf::leads(
        &userconf::Bases::xdg(home),
        side,
        &dirs,
        Limits::default(),
        threads,
    );
    tracing::debug!(
        app = %app.info.name,
        leads = leads.len(),
        files = stats.files,
        bytes = stats.bytes,
        ms = stats.elapsed.as_millis(),
        "user config searched"
    );
    judge(leads, me, others, open, allowed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::userconf::{Base, Candidate, NameMatch};

    fn lead(home: &Path, name: &str, m: NameMatch, binary: bool, foreign: bool) -> Lead {
        Lead {
            candidate: Candidate {
                path: home.join(format!(".{name}")),
                name: name.to_owned(),
                stem: name.to_owned(),
                base: Base::Home,
                is_dir: true,
            },
            name: m,
            binary,
            foreign_command: foreign,
            own_command: false,
        }
    }

    #[test]
    fn home_entries_are_judged_by_evidence_rivals_and_commands() {
        let home = Path::new("/Users/u");
        let code = Profile::basic(
            Some("com.microsoft.VSCode"),
            "Visual Studio Code",
            Some("Electron"),
        );
        let claude = Profile::basic(Some("com.anthropic.claudefordesktop"), "Claude", None);
        let obsidian = Profile::basic(Some("md.obsidian"), "Obsidian", Some("Obsidian"));
        let others = [code.clone(), obsidian];
        let leads = vec![
            lead(home, "vscode", NameMatch::Partial, false, false),
            lead(home, "claude", NameMatch::Exact, true, true),
            lead(home, "obsidian", NameMatch::Exact, true, false),
            lead(home, "ghostty", NameMatch::Exact, true, false),
            lead(home, "blocked", NameMatch::Exact, true, false),
        ];
        let open = [home.join(".obsidian/db/0.mdb")];
        let items = judge(leads, &claude, &others, &open, |p| !p.ends_with(".blocked"));
        let conf = |n: &str| {
            let path = home.join(format!(".{n}"));
            items.iter().find(|i| i.path == path).map(|i| i.confidence)
        };
        assert_eq!(
            conf("vscode"),
            None,
            "VS Code claims ~/.vscode more strongly"
        );
        assert_eq!(
            conf("claude"),
            Some(Confidence::Low),
            "a `claude` command outside the bundle caps ~/.claude at low"
        );
        assert_eq!(
            conf("obsidian"),
            Some(Confidence::Medium),
            "direct evidence against an equal rival"
        );
        assert_eq!(
            conf("ghostty"),
            Some(Confidence::High),
            "binary evidence, no rival"
        );
        assert_eq!(conf("blocked"), None, "the removal guard wins");
        let alone = judge(
            vec![lead(home, "vscode", NameMatch::Partial, false, false)],
            &code,
            &[claude],
            &[],
            |_| true,
        );
        assert_eq!(
            alone.first().map(|i| i.confidence),
            Some(Confidence::Low),
            "~/.vscode by id tail alone is never preselected"
        );
    }

    #[test]
    fn bundled_tools_live_in_resources_bin_and_cli_folders() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("omc-home-tools-{}", std::process::id()));
        let _ignored = std::fs::remove_dir_all(&root);
        let res = root.join("Contents/Resources");
        for (rel, exec) in [
            ("CLI/ghostex", true),
            ("app/bin/code", true),
            ("app/bin/README", false),
            ("en.lproj/bin/x", true),
            ("a/b/bin/deep", true),
        ] {
            let path = res.join(rel);
            if let Some(parent) = path.parent() {
                assert!(std::fs::create_dir_all(parent).is_ok(), "parent created");
            }
            assert!(std::fs::write(&path, b"x").is_ok(), "file written");
            let mode = if exec { 0o755 } else { 0o644 };
            assert!(
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).is_ok(),
                "mode set"
            );
        }
        let tools: Vec<PathBuf> = tool_dirs(&res)
            .iter()
            .flat_map(|d| files_in(d, true))
            .collect();
        let _ignored = std::fs::remove_dir_all(&root);
        assert_eq!(
            tools,
            vec![res.join("CLI/ghostex"), res.join("app/bin/code")],
            "executables of Resources/{{bin,CLI}} and one level below; no localisations, no deeper folders"
        );
    }
}
