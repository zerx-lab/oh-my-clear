//! Package managers: dpkg, rpm, pacman, Flatpak and Snap. Each source is one helper call
//! whose output is parsed by a pure function; desktop-file ownership is one call for all
//! paths.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use omc_proto::apps::AppSource;

use super::common::{self, LIST_TIMEOUT};

/// An installed distribution package.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Package {
    /// Package name.
    pub(super) name: String,
    /// Version.
    pub(super) version: Option<String>,
    /// Installed size in bytes.
    pub(super) bytes: Option<u64>,
    /// Maintainer / vendor / packager.
    pub(super) publisher: Option<String>,
    /// Install time (Unix seconds).
    pub(super) installed: Option<i64>,
    /// Essential or required by the distribution.
    pub(super) essential: bool,
}

/// An installed Flatpak app.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct FlatpakApp {
    /// Application id (`org.mozilla.firefox`).
    pub(super) id: String,
    /// Display name.
    pub(super) name: String,
    /// Version.
    pub(super) version: Option<String>,
    /// Installed size.
    pub(super) bytes: Option<u64>,
    /// Per-user installation (else system-wide).
    pub(super) user: bool,
    /// Remote it came from.
    pub(super) origin: Option<String>,
}

/// An installed snap.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct SnapApp {
    /// Snap name.
    pub(super) name: String,
    /// Version.
    pub(super) version: Option<String>,
    /// Revision.
    pub(super) rev: String,
    /// Publisher (verification marks stripped).
    pub(super) publisher: Option<String>,
    /// Base, snapd, core or gadget snap (part of the system).
    pub(super) system: bool,
}

fn non_empty(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty() && s != "(none)" && s != "-" && s != "None").then(|| s.to_owned())
}

/// `dpkg-query` format: name, version, `Installed-Size` (KiB), maintainer, status abbrev,
/// section, priority, essential.
pub(super) const DPKG_FORMAT: &str = "${Package}\t${Version}\t${Installed-Size}\t${Maintainer}\t${db:Status-Abbrev}\t${Section}\t${Priority}\t${Essential}\n";

/// Parses `dpkg-query -W -f` [`DPKG_FORMAT`] output; only installed packages.
pub(super) fn parse_dpkg(text: &str) -> Vec<Package> {
    text.lines()
        .filter_map(|line| {
            let mut f = line.split('\t');
            let name = non_empty(f.next()?)?;
            let version = f.next().and_then(non_empty);
            let kib = f.next().and_then(|s| s.trim().parse::<u64>().ok());
            let publisher = f.next().and_then(strip_email);
            let status = f.next().unwrap_or_default();
            if status.chars().nth(1) != Some('i') {
                return None;
            }
            let _section = f.next();
            let priority = f.next().unwrap_or_default().trim();
            let essential = f.next().unwrap_or_default().trim() == "yes"
                || matches!(priority, "required" | "important");
            Some(Package {
                name,
                version,
                bytes: kib.map(|k| k.saturating_mul(1024)),
                publisher,
                installed: None,
                essential,
            })
        })
        .collect()
}

/// `Name <mail>` → `Name`.
fn strip_email(s: &str) -> Option<String> {
    let s = s.split('<').next().unwrap_or(s);
    non_empty(s)
}

/// `rpm -qa --qf` format: name, version-release, size, vendor, install time.
pub(super) const RPM_FORMAT: &str =
    "%{NAME}\t%{VERSION}-%{RELEASE}\t%{SIZE}\t%{VENDOR}\t%{INSTALLTIME}\n";

/// Parses `rpm -qa --qf` [`RPM_FORMAT`] output.
pub(super) fn parse_rpm(text: &str) -> Vec<Package> {
    text.lines()
        .filter_map(|line| {
            let mut f = line.split('\t');
            let name = non_empty(f.next()?)?;
            Some(Package {
                name,
                version: f.next().and_then(non_empty),
                bytes: f.next().and_then(|s| s.trim().parse().ok()),
                publisher: f.next().and_then(non_empty),
                installed: f.next().and_then(|s| s.trim().parse().ok()),
                essential: false,
            })
        })
        .collect()
}

/// `expac` format: name, version, installed size (bytes), packager, install date.
pub(super) const EXPAC_FORMAT: &str = "%n\t%v\t%m\t%p\t%l";

/// Parses `expac --timefmt=%s -Q` [`EXPAC_FORMAT`] output.
pub(super) fn parse_expac(text: &str) -> Vec<Package> {
    text.lines()
        .filter_map(|line| {
            let mut f = line.split('\t');
            let name = non_empty(f.next()?)?;
            Some(Package {
                name,
                version: f.next().and_then(non_empty),
                bytes: f.next().and_then(|s| s.trim().parse().ok()),
                publisher: f.next().and_then(strip_email),
                installed: f.next().and_then(|s| s.trim().parse().ok()),
                essential: false,
            })
        })
        .collect()
}

/// Parses `pacman -Qi` (C locale) blocks. The install date is not parsed.
pub(super) fn parse_pacman_qi(text: &str) -> Vec<Package> {
    let mut out = Vec::new();
    let mut current: Option<Package> = None;
    for line in text.lines() {
        if line.trim().is_empty() {
            out.extend(current.take());
            continue;
        }
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "Name" => {
                out.extend(current.take());
                current = non_empty(value).map(|name| Package {
                    name,
                    ..Package::default()
                });
            }
            "Version" => {
                if let Some(p) = current.as_mut() {
                    p.version = non_empty(value);
                }
            }
            "Installed Size" => {
                if let Some(p) = current.as_mut() {
                    p.bytes = parse_size(value);
                }
            }
            "Packager" => {
                if let Some(p) = current.as_mut() {
                    p.publisher = strip_email(value);
                }
            }
            _ => {}
        }
    }
    out.extend(current);
    out
}

/// Parses human sizes: `12.34 MiB`, `1.2 GB`, `1.2\u{a0}kB`, `512 bytes`, `4 K`.
/// Decimal units are powers of 1000, binary units (`KiB`) powers of 1024.
pub(super) fn parse_size(text: &str) -> Option<u64> {
    let text = text.trim().replace(['\u{a0}', ','], " ");
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let (number, unit) = text.split_at_checked(split)?;
    let unit = unit.trim();
    let (int, frac) = number.split_once('.').unwrap_or((number, ""));
    let int: u128 = int.parse().ok()?;
    // Fraction as thousandths.
    let mut milli: u128 = 0;
    for (i, digit) in frac.chars().take(3).enumerate() {
        let d = u128::from(digit.to_digit(10)?);
        let scale = match i {
            0 => 100,
            1 => 10,
            _ => 1,
        };
        milli = milli.checked_add(d.checked_mul(scale)?)?;
    }
    let factor: u128 = match unit.to_ascii_lowercase().as_str() {
        "" | "b" | "byte" | "bytes" => 1,
        "k" | "kb" => 1000,
        "m" | "mb" => 1_000_000,
        "g" | "gb" => 1_000_000_000,
        "t" | "tb" => 1_000_000_000_000,
        "kib" => 1 << 10,
        "mib" => 1 << 20,
        "gib" => 1 << 30,
        "tib" => 1 << 40,
        _ => return None,
    };
    let total = int
        .checked_mul(1000)?
        .checked_add(milli)?
        .checked_mul(factor)?
        / 1000;
    u64::try_from(total).ok()
}

/// Columns requested from `flatpak list`.
pub(super) const FLATPAK_COLUMNS: &str =
    "--columns=application,name,version,size,installation,origin";

/// Parses `flatpak list --app` [`FLATPAK_COLUMNS`] output.
pub(super) fn parse_flatpak(text: &str) -> Vec<FlatpakApp> {
    text.lines()
        .filter_map(|line| {
            let mut f = line.split('\t');
            let id = non_empty(f.next()?)?;
            if id == "Application ID" {
                return None;
            }
            let name = f.next().and_then(non_empty).unwrap_or_else(|| id.clone());
            Some(FlatpakApp {
                name,
                version: f.next().and_then(non_empty),
                bytes: f.next().and_then(parse_size),
                user: f.next().is_some_and(|s| s.trim() == "user"),
                origin: f.next().and_then(non_empty),
                id,
            })
        })
        .collect()
}

/// Parses `flatpak ps --columns=application`: ids of running apps.
pub(super) fn parse_flatpak_ps(text: &str) -> HashSet<String> {
    text.lines()
        .filter_map(non_empty)
        .filter(|l| l != "Application")
        .collect()
}

/// Snaps that are part of the system, not apps.
fn is_system_snap(name: &str, notes: &str) -> bool {
    notes
        .split(',')
        .any(|n| matches!(n.trim(), "base" | "core" | "snapd" | "gadget" | "kernel"))
        || name.starts_with("core")
        || name.starts_with("gnome-")
        || name.starts_with("kde-frameworks")
        || matches!(
            name,
            "snapd" | "bare" | "gtk-common-themes" | "snapd-desktop-integration" | "mesa-2404"
        )
}

/// Parses `snap list` (columns `Name Version Rev Tracking Publisher Notes`).
pub(super) fn parse_snap(text: &str) -> Vec<SnapApp> {
    text.lines()
        .filter_map(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            let name = *cols.first()?;
            if name == "Name" {
                return None;
            }
            let publisher = cols
                .get(4)
                .and_then(|p| non_empty(p.trim_end_matches(['✓', '✪', '*', '!'])));
            let notes = cols.get(5).copied().unwrap_or_default();
            Some(SnapApp {
                name: name.to_owned(),
                version: cols.get(1).and_then(|v| non_empty(v)),
                rev: cols.get(2).copied().unwrap_or_default().to_owned(),
                publisher,
                system: is_system_snap(name, notes),
            })
        })
        .collect()
}

/// Parses `dpkg -S <paths…>`: `pkg1, pkg2:arch: /path` → path → first package.
pub(super) fn parse_dpkg_search(text: &str) -> BTreeMap<PathBuf, String> {
    text.lines()
        .filter(|l| !l.starts_with("diversion by"))
        .filter_map(|line| {
            let (pkgs, path) = line.split_once(": /")?;
            let pkg = pkgs.split(',').next()?.trim();
            let pkg = pkg.split(':').next().unwrap_or(pkg);
            let pkg = non_empty(pkg)?;
            Some((Path::new("/").join(path.trim()), pkg))
        })
        .collect()
}

/// `rpm -qf --qf` format listing each owning package's files with its name.
pub(super) const RPM_OWNER_FORMAT: &str = "[%{FILENAMES}\t%{NAME}\n]";

/// Parses `rpm -qf --qf` [`RPM_OWNER_FORMAT`] output, keeping `wanted` paths.
pub(super) fn parse_rpm_owners(text: &str, wanted: &HashSet<PathBuf>) -> BTreeMap<PathBuf, String> {
    text.lines()
        .filter_map(|line| {
            let (path, name) = line.split_once('\t')?;
            let path = PathBuf::from(path);
            wanted
                .contains(&path)
                .then(|| non_empty(name).map(|n| (path, n)))
                .flatten()
        })
        .collect()
}

/// Parses `pacman -Qo <paths…>`: `/path is owned by name version`.
pub(super) fn parse_pacman_owners(text: &str) -> BTreeMap<PathBuf, String> {
    text.lines()
        .filter_map(|line| {
            let (path, rest) = line.split_once(" is owned by ")?;
            let name = rest.split_whitespace().next()?;
            Some((PathBuf::from(path.trim()), name.to_owned()))
        })
        .collect()
}

/// Parses one-path-per-line listings (`dpkg-query -L`, `rpm -ql`/`-qc`, `pacman -Qlq`):
/// absolute paths only (`(contains no files)`, diversion notes and blanks skipped), with
/// pacman's trailing `/` on directories removed.
pub(super) fn parse_path_lines(text: &str) -> Vec<PathBuf> {
    text.lines()
        .map(str::trim)
        .filter(|l| l.starts_with('/'))
        .map(|l| {
            let trimmed = l.trim_end_matches('/');
            PathBuf::from(if trimmed.is_empty() { "/" } else { trimmed })
        })
        .collect()
}

/// Parses `dpkg-query -W -f '${Conffiles}\n' <pkg>`: ` /etc/x.conf <md5>[ obsolete]…`
/// (paths may contain spaces; the checksum is the first token after the path).
pub(super) fn parse_dpkg_conffiles(text: &str) -> Vec<PathBuf> {
    let is_sum =
        |t: &str| t == "newconffile" || (t.len() == 32 && t.chars().all(|c| c.is_ascii_hexdigit()));
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if !line.starts_with('/') {
                return None;
            }
            let tokens: Vec<&str> = line.split(' ').collect();
            let sum = tokens.iter().rposition(|t| is_sum(t))?;
            let path = tokens.get(..sum)?.join(" ");
            (!path.is_empty()).then(|| PathBuf::from(path))
        })
        .collect()
}

/// Package managers of distribution packages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Manager {
    /// dpkg / apt.
    Dpkg,
    /// rpm / dnf / zypper.
    Rpm,
    /// pacman.
    Pacman,
}

impl Manager {
    /// Every manager, in probing order.
    pub(super) const ALL: [Self; 3] = [Self::Dpkg, Self::Rpm, Self::Pacman];

    /// The manager of apps from `source`.
    pub(super) fn of(source: AppSource) -> Option<Self> {
        match source {
            AppSource::Deb => Some(Self::Dpkg),
            AppSource::Rpm => Some(Self::Rpm),
            AppSource::Pacman => Some(Self::Pacman),
            _ => None,
        }
    }

    /// Files `package` installed (one helper call; empty when unknown).
    pub(super) fn files(self, package: &str) -> Vec<PathBuf> {
        let (program, args): (&str, [&str; 2]) = match self {
            Self::Dpkg => ("dpkg-query", ["-L", package]),
            Self::Rpm => ("rpm", ["-ql", package]),
            Self::Pacman => ("pacman", ["-Qlq", package]),
        };
        common::run_stdout(program, &args, LIST_TIMEOUT, true)
            .map(|t| parse_path_lines(&t))
            .unwrap_or_default()
    }

    /// Configuration files a plain removal leaves behind (dpkg keeps conffiles until
    /// `purge`; rpm keeps modified ones as `.rpmsave`). pacman `-Rns` removes backup
    /// files itself, so it has none.
    pub(super) fn conffiles(self, package: &str) -> Vec<PathBuf> {
        match self {
            Self::Dpkg => common::run_stdout(
                "dpkg-query",
                &["-W", "-f", "${Conffiles}\n", package],
                LIST_TIMEOUT,
                true,
            )
            .map(|t| parse_dpkg_conffiles(&t))
            .unwrap_or_default(),
            Self::Rpm => common::run_stdout("rpm", &["-qc", package], LIST_TIMEOUT, true)
                .map(|t| parse_path_lines(&t))
                .unwrap_or_default(),
            Self::Pacman => Vec::new(),
        }
    }

    /// The manager is installed.
    pub(super) fn available(self) -> bool {
        common::which(match self {
            Self::Dpkg => "dpkg-query",
            Self::Rpm => "rpm",
            Self::Pacman => "pacman",
        })
        .is_some()
    }

    /// Every installed package; `None` when the database cannot be read.
    pub(super) fn packages(self) -> Option<Vec<Package>> {
        match self {
            Self::Dpkg => {
                common::run_stdout("dpkg-query", &["-W", "-f", DPKG_FORMAT], LIST_TIMEOUT, true)
                    .map(|t| parse_dpkg(&t))
            }
            Self::Rpm => {
                common::run_stdout("rpm", &["-qa", "--qf", RPM_FORMAT], LIST_TIMEOUT, true)
                    .map(|t| parse_rpm(&t))
            }
            Self::Pacman => {
                if common::which("expac").is_some()
                    && let Some(text) = common::run_stdout(
                        "expac",
                        &["--timefmt=%s", "-Q", EXPAC_FORMAT],
                        LIST_TIMEOUT,
                        true,
                    )
                {
                    return Some(parse_expac(&text));
                }
                common::run_stdout("pacman", &["-Qi"], LIST_TIMEOUT, true)
                    .map(|t| parse_pacman_qi(&t))
            }
        }
    }

    /// Owning package of each of `paths` (one helper call).
    pub(super) fn owners(self, paths: &[PathBuf]) -> BTreeMap<PathBuf, String> {
        if paths.is_empty() {
            return BTreeMap::new();
        }
        let strs: Vec<&str> = paths.iter().filter_map(|p| p.to_str()).collect();
        match self {
            Self::Dpkg => {
                let mut args = vec!["-S"];
                args.extend(&strs);
                common::run_stdout("dpkg", &args, LIST_TIMEOUT, false)
                    .map(|t| parse_dpkg_search(&t))
                    .unwrap_or_default()
            }
            Self::Rpm => {
                let mut args = vec!["-qf", "--qf", RPM_OWNER_FORMAT];
                args.extend(&strs);
                let wanted: HashSet<PathBuf> = paths.iter().cloned().collect();
                common::run_stdout("rpm", &args, LIST_TIMEOUT, false)
                    .map(|t| parse_rpm_owners(&t, &wanted))
                    .unwrap_or_default()
            }
            Self::Pacman => {
                let mut args = vec!["-Qo"];
                args.extend(&strs);
                common::run_stdout("pacman", &args, LIST_TIMEOUT, false)
                    .map(|t| parse_pacman_owners(&t))
                    .unwrap_or_default()
            }
        }
    }
}

/// Installed Flatpak apps; `None` when flatpak is missing or fails.
pub(super) fn flatpaks() -> Option<Vec<FlatpakApp>> {
    common::which("flatpak")?;
    common::run_stdout(
        "flatpak",
        &["list", "--app", FLATPAK_COLUMNS],
        LIST_TIMEOUT,
        true,
    )
    .map(|t| parse_flatpak(&t))
}

/// Ids of running Flatpak apps.
pub(super) fn flatpak_running() -> HashSet<String> {
    if common::which("flatpak").is_none() {
        return HashSet::new();
    }
    common::run_stdout(
        "flatpak",
        &["ps", "--columns=application"],
        common::QUICK_TIMEOUT,
        true,
    )
    .map(|t| parse_flatpak_ps(&t))
    .unwrap_or_default()
}

/// Installed snaps; `None` when snap is missing or fails.
pub(super) fn snaps() -> Option<Vec<SnapApp>> {
    common::which("snap")?;
    common::run_stdout("snap", &["list"], LIST_TIMEOUT, true).map(|t| parse_snap(&t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpkg_keeps_installed_and_flags_essential() {
        let text = "firefox\t128.0\t250000\tUbuntu Devs <u@x>\tii \tweb\toptional\tno\n\
                    old\t1\t10\tX\trc \tlibs\toptional\tno\n\
                    bash\t5.2\t1800\tY\tii \tshells\trequired\tyes\n";
        let got = parse_dpkg(text);
        assert_eq!(got.len(), 2, "config-files package dropped: {got:?}");
        assert_eq!(
            got.first().map(|p| (
                p.name.as_str(),
                p.bytes,
                p.publisher.as_deref(),
                p.essential
            )),
            Some(("firefox", Some(256_000_000), Some("Ubuntu Devs"), false)),
            "KiB, email stripped"
        );
        assert!(
            got.get(1).is_some_and(|p| p.essential),
            "required priority is essential"
        );
    }

    #[test]
    fn rpm_and_expac_rows() {
        let rpm = parse_rpm("gimp\t2.10-1.fc40\t123\t(none)\t1700000000\n");
        assert_eq!(
            rpm.first()
                .map(|p| (p.bytes, p.publisher.clone(), p.installed)),
            Some((Some(123), None, Some(1_700_000_000))),
            "rpm fields: {rpm:?}"
        );
        let expac = parse_expac("vlc\t3.0-1\t4096\tA B <a@b>\t1600000000\n");
        assert_eq!(
            expac.first().map(|p| (p.publisher.clone(), p.installed)),
            Some((Some("A B".to_owned()), Some(1_600_000_000))),
            "expac fields: {expac:?}"
        );
    }

    #[test]
    fn pacman_qi_blocks() {
        let text = "Name            : vlc\nVersion         : 3.0.20-1\nInstalled Size  : 12.50 MiB\nPackager        : Arch <a@b>\nDepends On      : a  b\n                  c\n\nName            : zsh\nVersion         : 5.9-1\n";
        let got = parse_pacman_qi(text);
        assert_eq!(got.len(), 2, "two blocks: {got:?}");
        assert_eq!(
            got.first().and_then(|p| p.bytes),
            Some(13_107_200),
            "binary units"
        );
    }

    #[test]
    fn sizes_parse_both_unit_systems() {
        assert_eq!(parse_size("1.2 GB"), Some(1_200_000_000), "decimal");
        assert_eq!(parse_size("1.2\u{a0}kB"), Some(1200), "nbsp");
        assert_eq!(parse_size("4 KiB"), Some(4096), "binary");
        assert_eq!(parse_size("512 bytes"), Some(512), "bytes");
        assert_eq!(parse_size("junk"), None, "garbage");
        assert_eq!(parse_size("3 parsecs"), None, "unknown unit");
    }

    #[test]
    fn flatpak_and_snap_listings() {
        let fp = parse_flatpak(
            "org.gimp.GIMP\tGNU Image Manipulation Program\t2.10.38\t1.2\u{a0}GB\tsystem\tflathub\n\
             com.x.Y\tY\t\t10 MB\tuser\tflathub\n",
        );
        assert_eq!(fp.len(), 2, "rows: {fp:?}");
        assert!(
            fp.first()
                .is_some_and(|a| !a.user && a.bytes == Some(1_200_000_000)),
            "system install: {fp:?}"
        );
        assert!(
            fp.get(1).is_some_and(|a| a.user && a.version.is_none()),
            "user install"
        );

        let snaps = parse_snap(
            "Name    Version   Rev    Tracking       Publisher   Notes\n\
             core22  2024      1380   latest/stable  canonical✓  base\n\
             firefox 130.0     4848   latest/stable  mozilla✓    -\n\
             snapd   2.63      21759  latest/stable  canonical✓  snapd\n",
        );
        let apps: Vec<&str> = snaps
            .iter()
            .filter(|s| !s.system)
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(apps, vec!["firefox"], "system snaps flagged: {snaps:?}");
        assert_eq!(
            snaps.get(1).and_then(|s| s.publisher.clone()),
            Some("mozilla".to_owned()),
            "verification mark stripped"
        );
    }

    #[test]
    fn ownership_outputs() {
        let dpkg = parse_dpkg_search(
            "diversion by x from: /usr/share/applications/a.desktop\n\
             firefox:amd64: /usr/share/applications/firefox.desktop\n\
             gimp, gimp-data: /usr/share/applications/gimp.desktop\n",
        );
        assert_eq!(
            dpkg.get(Path::new("/usr/share/applications/firefox.desktop"))
                .map(String::as_str),
            Some("firefox"),
            "arch stripped: {dpkg:?}"
        );
        assert_eq!(dpkg.len(), 2, "diversions skipped");

        let wanted: HashSet<PathBuf> =
            [PathBuf::from("/usr/share/applications/gimp.desktop")].into();
        let rpm = parse_rpm_owners(
            "/usr/bin/gimp\tgimp\n/usr/share/applications/gimp.desktop\tgimp\nfile /x is not owned by any package\n",
            &wanted,
        );
        assert_eq!(rpm.len(), 1, "only wanted paths: {rpm:?}");

        let pac =
            parse_pacman_owners("/usr/share/applications/vlc.desktop is owned by vlc 3.0-1\n");
        assert_eq!(
            pac.values().next().map(String::as_str),
            Some("vlc"),
            "pacman owner"
        );
    }

    #[test]
    fn path_listings_keep_absolute_paths() {
        let paths = parse_path_lines(
            "/.\n/usr/bin/foo\n/usr/share/foo/\n(contains no files)\ndiverted by x to: /y\n\n",
        );
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/."),
                PathBuf::from("/usr/bin/foo"),
                PathBuf::from("/usr/share/foo"),
            ],
            "notes skipped, trailing slash dropped"
        );
        assert_eq!(
            parse_path_lines("/\n"),
            vec![PathBuf::from("/")],
            "root kept"
        );
    }

    #[test]
    fn dpkg_conffiles_handle_spaces_and_flags() {
        let text = " /etc/foo/foo.conf 0123456789abcdef0123456789abcdef\n /etc/foo/my file.conf 0123456789abcdef0123456789abcdef obsolete\n /etc/foo/new.conf newconffile\nnot a path\n";
        assert_eq!(
            parse_dpkg_conffiles(text),
            vec![
                PathBuf::from("/etc/foo/foo.conf"),
                PathBuf::from("/etc/foo/my file.conf"),
                PathBuf::from("/etc/foo/new.conf"),
            ],
            "paths before the checksum"
        );
        assert!(
            parse_dpkg_conffiles(" /etc/x\n").is_empty(),
            "lines without checksum skipped"
        );
    }
}
