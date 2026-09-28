//! Reading the property lists macOS keeps about bundles and launchd jobs. `plist` reads XML
//! and binary lists alike; unreadable or malformed lists are skipped (logged at debug).

use std::path::{Path, PathBuf};

use plist::{Dictionary, Value};

/// The top-level dictionary of the property list at `path`.
pub(super) fn read_dict(path: &Path) -> Option<Dictionary> {
    match Value::from_file(path) {
        Ok(value) => value.into_dictionary(),
        Err(err) => {
            tracing::debug!(%err, path = %path.display(), "unreadable plist");
            None
        }
    }
}

/// A non-empty, trimmed string value.
pub(super) fn string(dict: &Dictionary, key: &str) -> Option<String> {
    dict.get(key)
        .and_then(Value::as_string)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Every string of an array value (non-strings skipped).
pub(super) fn strings(dict: &Dictionary, key: &str) -> Vec<String> {
    match dict.get(key) {
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(Value::as_string)
            .map(str::to_owned)
            .collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    }
}

/// What an `Info.plist` says about a bundle.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct BundleInfo {
    /// `CFBundleIdentifier`.
    pub(super) id: Option<String>,
    /// `CFBundleDisplayName`, `CFBundleName` or the file stem.
    pub(super) name: String,
    /// `CFBundleShortVersionString` (or `CFBundleVersion`).
    pub(super) version: Option<String>,
    /// `CFBundleExecutable`.
    pub(super) executable: Option<String>,
    /// `CFBundleIconFile` (or `CFBundleIconName`), as named in the plist.
    pub(super) icon: Option<String>,
    /// `NSHumanReadableCopyright`.
    pub(super) copyright: Option<String>,
}

/// The `Info.plist` of a bundle: `Contents/Info.plist`, or `Info.plist` at the top of flat
/// bundles.
pub(super) fn info_plist_path(bundle: &Path) -> PathBuf {
    let nested = bundle.join("Contents").join("Info.plist");
    if nested.is_file() {
        nested
    } else {
        bundle.join("Info.plist")
    }
}

/// Parses a bundle's `Info.plist`; `None` when it has none.
pub(super) fn bundle_info(bundle: &Path) -> Option<BundleInfo> {
    let dict = read_dict(&info_plist_path(bundle))?;
    Some(bundle_info_from(&dict, bundle))
}

/// [`bundle_info`] for an already parsed dictionary.
pub(super) fn bundle_info_from(dict: &Dictionary, bundle: &Path) -> BundleInfo {
    let stem = bundle
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    BundleInfo {
        id: string(dict, "CFBundleIdentifier"),
        name: string(dict, "CFBundleDisplayName")
            .or_else(|| string(dict, "CFBundleName"))
            .unwrap_or(stem),
        version: string(dict, "CFBundleShortVersionString")
            .or_else(|| string(dict, "CFBundleVersion")),
        executable: string(dict, "CFBundleExecutable"),
        icon: string(dict, "CFBundleIconFile").or_else(|| string(dict, "CFBundleIconName")),
        copyright: string(dict, "NSHumanReadableCopyright"),
    }
}

/// A launchd job definition (`LaunchAgents`/`LaunchDaemons` plist).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct LaunchJob {
    /// `Label` (the file stem when missing).
    pub(super) label: String,
    /// `Program`, else the first `ProgramArguments` entry.
    pub(super) program: Option<String>,
    /// `ProgramArguments` (or `[Program]`).
    pub(super) args: Vec<String>,
    /// `Disabled` key.
    pub(super) disabled: bool,
    /// `AssociatedBundleIdentifiers`.
    pub(super) associated: Vec<String>,
    /// `BundleProgram`: the program is relative to an app bundle (`SMAppService`).
    pub(super) bundle_program: bool,
}

impl LaunchJob {
    /// Program and arguments for display.
    pub(super) fn command(&self) -> Option<String> {
        if self.args.is_empty() {
            return self.program.clone();
        }
        let quoted: Vec<String> = self
            .args
            .iter()
            .map(|a| {
                if a.contains(' ') {
                    format!("\"{a}\"")
                } else {
                    a.clone()
                }
            })
            .collect();
        Some(quoted.join(" "))
    }

    /// The program is an absolute path that does not exist.
    pub(super) fn target_missing(&self) -> bool {
        if self.bundle_program {
            return false;
        }
        self.program.as_deref().is_some_and(|p| {
            let path = Path::new(p);
            path.is_absolute() && std::fs::symlink_metadata(path).is_err()
        })
    }
}

/// Parses a launchd plist.
pub(super) fn launch_job(plist: &Path) -> Option<LaunchJob> {
    let dict = read_dict(plist)?;
    Some(launch_job_from(&dict, plist))
}

/// [`launch_job`] for an already parsed dictionary.
pub(super) fn launch_job_from(dict: &Dictionary, plist: &Path) -> LaunchJob {
    let label = string(dict, "Label").unwrap_or_else(|| {
        plist
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let args = strings(dict, "ProgramArguments");
    let program = string(dict, "Program").or_else(|| args.first().cloned());
    let args = if args.is_empty() {
        program.iter().cloned().collect()
    } else {
        args
    };
    LaunchJob {
        label,
        program,
        args,
        disabled: dict
            .get("Disabled")
            .and_then(Value::as_boolean)
            .unwrap_or(false),
        associated: strings(dict, "AssociatedBundleIdentifiers"),
        bundle_program: dict.contains_key("BundleProgram"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(pairs: &[(&str, Value)]) -> Dictionary {
        let mut d = Dictionary::new();
        for (k, v) in pairs {
            d.insert((*k).to_owned(), v.clone());
        }
        d
    }

    #[test]
    fn bundle_names_fall_back_in_order() {
        let path = Path::new("/Applications/Foo Bar.app");
        let full = dict(&[
            ("CFBundleDisplayName", Value::from("Display")),
            ("CFBundleName", Value::from("Short")),
            ("CFBundleIdentifier", Value::from("com.foo.bar")),
        ]);
        assert_eq!(
            bundle_info_from(&full, path).name,
            "Display",
            "display name wins"
        );
        let short = dict(&[("CFBundleName", Value::from("Short"))]);
        assert_eq!(
            bundle_info_from(&short, path).name,
            "Short",
            "then bundle name"
        );
        let blank = dict(&[("CFBundleName", Value::from("  "))]);
        assert_eq!(
            bundle_info_from(&blank, path).name,
            "Foo Bar",
            "then file stem"
        );
    }

    #[test]
    fn launch_jobs_read_program_arguments() {
        let d = dict(&[
            ("Label", Value::from("com.foo.agent")),
            (
                "ProgramArguments",
                Value::Array(vec![
                    Value::from("/Applications/Foo App.app/Contents/MacOS/foo"),
                    Value::from("--bg"),
                ]),
            ),
            ("Disabled", Value::Boolean(true)),
        ]);
        let job = launch_job_from(&d, Path::new("/x/com.foo.agent.plist"));
        assert_eq!(
            job.program.as_deref(),
            Some("/Applications/Foo App.app/Contents/MacOS/foo"),
            "first argument is the program"
        );
        assert!(job.disabled, "Disabled key is read");
        assert_eq!(
            job.command().as_deref(),
            Some("\"/Applications/Foo App.app/Contents/MacOS/foo\" --bg"),
            "arguments with spaces are quoted"
        );
        let unlabeled = launch_job_from(&Dictionary::new(), Path::new("/x/org.y.z.plist"));
        assert_eq!(
            unlabeled.label, "org.y.z",
            "label falls back to the file stem"
        );
        assert!(
            !unlabeled.target_missing(),
            "no program: nothing is missing"
        );
    }
}
