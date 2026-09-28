//! Login items (System Events) and third-party launch agents/daemons, with their enabled
//! state from `launchctl print-disabled` and the plist `Disabled` key.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use omc_proto::apps::{Scope, StartupChange, StartupItem, StartupKind};
use omc_proto::jobs::{Location, Phase, SpecialAction};
use omc_proto::settings::CleanSettings;
use omc_scan::JobCtx;

use super::{StartupDetail, access, plist_util};
use crate::cmd;
use crate::{Error, Result, StartupRecord};

/// `osascript`/`launchctl` bound (System Events can be slow to start).
const TIMEOUT: Duration = Duration::from_secs(20);

/// Which launchd domain a plist folder feeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Domain {
    /// `~/Library/LaunchAgents` → `gui/<uid>`.
    UserAgent,
    /// `/Library/LaunchAgents` → `gui/<uid>` of every user.
    SystemAgent,
    /// `/Library/LaunchDaemons` → `system`.
    Daemon,
}

/// Launchd plist folders.
pub(super) fn launch_dirs() -> Vec<(PathBuf, Domain)> {
    let mut out = Vec::new();
    if let Some(home) = omc_scan::paths::home() {
        out.push((home.join("Library").join("LaunchAgents"), Domain::UserAgent));
    }
    out.push((PathBuf::from("/Library/LaunchAgents"), Domain::SystemAgent));
    out.push((PathBuf::from("/Library/LaunchDaemons"), Domain::Daemon));
    out
}

/// The domain of a launchd plist from its folder.
pub(super) fn domain_of(plist: &Path) -> Domain {
    if plist.starts_with("/Library/LaunchDaemons") {
        Domain::Daemon
    } else if plist.starts_with("/Library/LaunchAgents") {
        Domain::SystemAgent
    } else {
        Domain::UserAgent
    }
}

/// `*.plist` files of a folder.
pub(super) fn plists(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("plist"))
        })
        .collect();
    out.sort();
    out
}

/// A login item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LoginItem {
    /// Name shown in System Settings.
    pub(super) name: String,
    /// What it opens.
    pub(super) path: Option<String>,
}

/// Lists login items through System Events. The first call triggers the Automation
/// permission prompt; a refusal is an `Err`.
pub(super) fn login_items() -> Result<Vec<LoginItem>> {
    let out = cmd::run(
        "/usr/bin/osascript",
        &[
            "-e",
            "set out to \"\"",
            "-e",
            "tell application \"System Events\"",
            "-e",
            "repeat with i in every login item",
            "-e",
            "set out to out & (name of i) & tab & (path of i) & linefeed",
            "-e",
            "end repeat",
            "-e",
            "end tell",
            "-e",
            "return out",
        ],
        TIMEOUT,
    )?
    .ok("osascript")?;
    Ok(parse_login_items(&out))
}

/// `name<TAB>path` lines.
pub(super) fn parse_login_items(text: &str) -> Vec<LoginItem> {
    text.lines()
        .filter_map(|line| {
            let (name, path) = line.split_once('\t').unwrap_or((line, ""));
            let name = name.trim();
            let path = path.trim();
            (!name.is_empty()).then(|| LoginItem {
                name: name.to_owned(),
                path: (!path.is_empty() && path != "missing value").then(|| path.to_owned()),
            })
        })
        .collect()
}

/// Quotes a string for `AppleScript`.
pub(super) fn applescript_string(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Deletes a login item by name.
pub(super) fn remove_login_item(name: &str) -> Result<()> {
    let script = format!(
        "tell application \"System Events\" to delete login item {}",
        applescript_string(name)
    );
    cmd::run("/usr/bin/osascript", &["-e", &script], TIMEOUT)?.ok("osascript")?;
    Ok(())
}

/// `launchctl print-disabled <domain>` → label → disabled.
fn disabled_overrides(domain: &str) -> HashMap<String, bool> {
    match cmd::run("/bin/launchctl", &["print-disabled", domain], TIMEOUT)
        .and_then(|o| o.ok("launchctl"))
    {
        Ok(text) => parse_print_disabled(&text),
        Err(err) => {
            tracing::debug!(%err, domain, "launchctl print-disabled");
            HashMap::new()
        }
    }
}

/// Parses `"label" => enabled|disabled` (older systems: `=> false|true`).
pub(super) fn parse_print_disabled(text: &str) -> HashMap<String, bool> {
    text.lines()
        .filter_map(|line| {
            let (label, state) = line.split_once("=>")?;
            let label = label.trim().strip_prefix('"')?.strip_suffix('"')?;
            let disabled = match state.trim() {
                "disabled" | "true" => true,
                "enabled" | "false" => false,
                _ => return None,
            };
            Some((label.to_owned(), disabled))
        })
        .collect()
}

/// `gui/<uid>` of the console user.
pub(super) fn gui_domain() -> String {
    format!("gui/{}", access::console_uid())
}

fn domain_target(domain: Domain) -> String {
    match domain {
        Domain::Daemon => "system".to_owned(),
        Domain::UserAgent | Domain::SystemAgent => gui_domain(),
    }
}

/// Publisher guess from a reverse-DNS label: `com.google.keystone` → `Google`.
pub(super) fn publisher_from_label(label: &str) -> Option<String> {
    let mut parts = label.split('.');
    let (_, vendor, _) = (parts.next()?, parts.next()?, parts.next()?);
    let mut chars = vendor.chars();
    let first = chars.next()?;
    Some(first.to_uppercase().chain(chars).collect())
}

pub(super) fn list_startup(_settings: &CleanSettings, ctx: &JobCtx) -> Result<Vec<StartupRecord>> {
    ctx.set_phase(Phase::Scanning);
    let mut out = Vec::new();
    match login_items() {
        Ok(items) => {
            for item in items {
                ctx.add_items(1);
                let missing = item
                    .path
                    .as_deref()
                    .is_some_and(|p| std::fs::symlink_metadata(p).is_err());
                out.push(StartupRecord {
                    item: StartupItem {
                        id: 0,
                        name: item.name.clone(),
                        command: item.path.clone(),
                        location: Location::Special {
                            action: SpecialAction::RemoveLoginItem {
                                name: item.name.clone(),
                            },
                        },
                        kind: StartupKind::LoginItem,
                        scope: Scope::User,
                        enabled: true,
                        needs_admin: false,
                        publisher: None,
                        missing_target: missing,
                        ident: None,
                        icon: None,
                    },
                    detail: StartupDetail::LoginItem { name: item.name },
                });
            }
        }
        Err(err) => tracing::info!(%err, "login items skipped (Automation permission?)"),
    }
    let gui = disabled_overrides(&gui_domain());
    let system = disabled_overrides("system");
    for (dir, domain) in launch_dirs() {
        if ctx.is_cancelled() {
            return Err(Error::Cancelled);
        }
        for plist in plists(&dir) {
            ctx.add_items(1);
            let Some(job) = plist_util::launch_job(&plist) else {
                continue;
            };
            if job.label.to_lowercase().starts_with("com.apple.") {
                continue;
            }
            let overrides = if domain == Domain::Daemon {
                &system
            } else {
                &gui
            };
            let enabled = !overrides.get(&job.label).copied().unwrap_or(job.disabled);
            out.push(StartupRecord {
                item: StartupItem {
                    id: 0,
                    name: job.label.clone(),
                    command: job.command(),
                    location: Location::Path {
                        path: plist.display().to_string(),
                    },
                    kind: if domain == Domain::Daemon {
                        StartupKind::LaunchDaemon
                    } else {
                        StartupKind::LaunchAgent
                    },
                    scope: if domain == Domain::UserAgent {
                        Scope::User
                    } else {
                        Scope::System
                    },
                    enabled,
                    needs_admin: domain != Domain::UserAgent,
                    publisher: publisher_from_label(&job.label),
                    missing_target: job.target_missing(),
                    ident: None,
                    icon: None,
                },
                detail: StartupDetail::Launchd {
                    label: job.label,
                    plist,
                    domain,
                },
            });
        }
    }
    Ok(out)
}

fn launchctl(args: &[&str]) -> Result<()> {
    cmd::run("/bin/launchctl", args, TIMEOUT)?.ok("launchctl")?;
    Ok(())
}

/// Runs `launchctl`, logging (not failing) when the job was not loaded / already loaded.
fn launchctl_lenient(args: &[&str]) {
    if let Err(err) = launchctl(args) {
        tracing::debug!(%err, ?args, "launchctl (ignored)");
    }
}

/// `launchctl bootout` + delete the plist.
pub(super) fn unload_and_delete(label: &str, plist: &Path) -> Result<()> {
    let domain = domain_of(plist);
    if domain != Domain::UserAgent && !access::is_root() {
        return Err(Error::Elevation(format!(
            "removing {} needs administrator rights",
            plist.display()
        )));
    }
    let service = format!("{}/{label}", domain_target(domain));
    launchctl_lenient(&["bootout", &service]);
    match std::fs::remove_file(plist) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(Error::Io(err)),
    }
}

pub(super) fn change_startup(
    record: &StartupRecord,
    change: StartupChange,
    _settings: &CleanSettings,
    _ctx: &JobCtx,
) -> Result<Option<StartupItem>> {
    match &record.detail {
        StartupDetail::LoginItem { name } => match change {
            StartupChange::Remove => {
                remove_login_item(name)?;
                Ok(None)
            }
            StartupChange::Enable | StartupChange::Disable => Err(Error::Unsupported(
                "login items cannot be disabled; remove the item instead".to_owned(),
            )),
        },
        StartupDetail::Launchd {
            label,
            plist,
            domain,
        } => {
            if *domain == Domain::Daemon && !access::is_root() {
                return Err(Error::Elevation(format!(
                    "changing the launch daemon {label} needs administrator rights"
                )));
            }
            let target = domain_target(*domain);
            let service = format!("{target}/{label}");
            let plist_arg = plist.display().to_string();
            match change {
                StartupChange::Enable => {
                    launchctl(&["enable", &service])?;
                    // Load it now; "already loaded" is fine.
                    launchctl_lenient(&["bootstrap", &target, &plist_arg]);
                    Ok(Some(StartupItem {
                        enabled: true,
                        ..record.item.clone()
                    }))
                }
                StartupChange::Disable => {
                    launchctl(&["disable", &service])?;
                    launchctl_lenient(&["bootout", &service]);
                    Ok(Some(StartupItem {
                        enabled: false,
                        ..record.item.clone()
                    }))
                }
                StartupChange::Remove => {
                    unload_and_delete(label, plist)?;
                    Ok(None)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_disabled_output_parses() {
        let text = "\tdisabled services = {\n\
            \t\t\"com.youqu.todesk.client.startup\" => enabled\n\
            \t\t\"com.apple.Siri.agent\" => disabled\n\
            \t\t\"org.old.style\" => true\n\
            \t}\n\tlogin item associations = {\n\t}\n";
        let map = parse_print_disabled(text);
        assert_eq!(
            map.get("com.youqu.todesk.client.startup"),
            Some(&false),
            "enabled"
        );
        assert_eq!(map.get("com.apple.Siri.agent"), Some(&true), "disabled");
        assert_eq!(
            map.get("org.old.style"),
            Some(&true),
            "legacy true = disabled"
        );
        assert_eq!(map.len(), 3, "only service lines");
    }

    #[test]
    fn login_item_lines_parse() {
        let items = parse_login_items("PixPin\t/Applications/PixPin.app\nGone\tmissing value\n\n");
        assert_eq!(items.len(), 2, "two items");
        assert_eq!(
            items.first().and_then(|i| i.path.as_deref()),
            Some("/Applications/PixPin.app"),
            "path"
        );
        assert_eq!(
            items.get(1).and_then(|i| i.path.clone()),
            None,
            "missing path"
        );
    }

    #[test]
    fn applescript_strings_are_escaped() {
        assert_eq!(
            applescript_string("a\"b\\c"),
            "\"a\\\"b\\\\c\"",
            "quotes and backslashes"
        );
    }

    #[test]
    fn labels_give_publishers_and_domains() {
        assert_eq!(
            publisher_from_label("com.google.keystone.agent").as_deref(),
            Some("Google"),
            "vendor"
        );
        assert_eq!(publisher_from_label("local"), None, "no vendor");
        assert_eq!(
            domain_of(Path::new("/Library/LaunchDaemons/x.plist")),
            Domain::Daemon,
            "daemon"
        );
        assert_eq!(
            domain_of(Path::new("/Users/me/Library/LaunchAgents/x.plist")),
            Domain::UserAgent,
            "user agent"
        );
    }
}
