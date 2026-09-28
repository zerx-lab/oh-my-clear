//! Where an app's identifiers come from: its `Info.plist` names and ids, embedded bundles,
//! code signature (team, app and keychain groups, iCloud containers), Sparkle feed host,
//! Electron `package.json` (loose or inside `app.asar`) and `app-update.yml`; plus the
//! system's view of it: files its processes have open (`lsof`), and the payload of the
//! installer packages that installed it (`pkgutil`).
//!
//! The parsers are pure and unit-tested; the gatherers read the bundle and run the stock
//! tools with a deadline.

use std::collections::HashSet;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use super::attribution::{NameKind, Profile};
use super::{inventory, plist_util};
use crate::AppRecord;
use crate::cmd;

/// `codesign`, `pkgutil`, `lsof` answer quickly; this bounds a wedged system.
const TOOL_TIMEOUT: Duration = Duration::from_secs(15);

/// Largest `app.asar` header read (big apps have multi-megabyte headers).
const MAX_ASAR_HEADER: u32 = 64 << 20;

/// Largest `package.json` read.
const MAX_PACKAGE_JSON: u64 = 1 << 20;

/// Feed hosts shared by unrelated developers: no vendor.
const HOSTING: &[&str] = &[
    "amazonaws.com",
    "appspot.com",
    "azureedge.net",
    "bitbucket.io",
    "cloudfront.net",
    "dropboxusercontent.com",
    "firebaseapp.com",
    "github.com",
    "github.io",
    "githubusercontent.com",
    "gitlab.io",
    "googleapis.com",
    "herokuapp.com",
    "netlify.app",
    "pages.dev",
    "sourceforge.net",
    "vercel.app",
    "web.app",
    "workers.dev",
];

/// What the bundle says about the app beyond its [`Profile`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Traits {
    /// Chromium-based (Electron, CEF, Chrome): its data folders hold a Chromium profile.
    pub(super) chromium: bool,
    /// iCloud container ids (`iCloud.com.foo.bar`).
    pub(super) icloud: Vec<String>,
}

/// The app's full profile: every identifier the bundle reveals.
pub(super) fn profile(app: &AppRecord) -> (Profile, Traits) {
    let d = &app.detail;
    let mut p = Profile::default();
    let mut traits = Traits::default();
    let dict = plist_util::read_dict(&plist_util::info_plist_path(&d.real));
    let id = d.bundle_id.clone().or_else(|| {
        dict.as_ref()
            .and_then(|d| plist_util::string(d, "CFBundleIdentifier"))
    });
    if let Some(id) = &id {
        p.set_main_id(id);
    }
    p.add_name(&app.info.name, NameKind::Primary);
    if let Some(stem) = d.bundle.file_stem() {
        p.add_name(&stem.to_string_lossy(), NameKind::Primary);
    }
    if let Some(dict) = &dict {
        for key in ["CFBundleDisplayName", "CFBundleName"] {
            if let Some(name) = plist_util::string(dict, key) {
                p.add_name(&name, NameKind::Primary);
            }
        }
        if let Some(word) = plist_util::string(dict, "SUFeedURL")
            .as_deref()
            .and_then(feed_vendor)
            .and_then(|v| v.split('.').nth(1).map(str::to_owned))
        {
            p.add_vendor_word(&word);
        }
    }
    if let Some(exe) = &d.executable {
        p.add_name(exe, NameKind::Executable);
    }
    if let Some(word) = app
        .info
        .publisher
        .as_deref()
        .and_then(|pubr| super::ident::tokens(pubr).into_iter().next())
    {
        p.add_vendor_word(&word);
    }
    for embedded in inventory::embedded_ids(&d.real) {
        p.add_id(&embedded);
    }
    let signing = signing(&d.real);
    p.team = signing.team;
    for group in signing.groups.iter().chain(&signing.keychain) {
        p.add_exact(group);
    }
    for container in &signing.icloud {
        let id = container
            .strip_prefix("iCloud.")
            .unwrap_or(container.as_str());
        p.add_exact(id);
    }
    traits.icloud = signing.icloud;
    let electron = electron(&d.real);
    for name in &electron.products {
        p.add_name(name, NameKind::Product);
    }
    if let Some(dir) = &electron.updater {
        p.add_name(dir, NameKind::Updater);
    }
    traits.chromium = electron.chromium;
    (p, traits)
}

/// Code-signing facts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Signing {
    /// Team id.
    pub(super) team: Option<String>,
    /// `com.apple.security.application-groups`.
    pub(super) groups: Vec<String>,
    /// `keychain-access-groups`.
    pub(super) keychain: Vec<String>,
    /// `com.apple.developer.icloud-container-identifiers`.
    pub(super) icloud: Vec<String>,
}

/// Team id and entitlements (two `codesign` runs, side by side: each takes long on big
/// bundles such as Xcode).
fn signing(bundle: &Path) -> Signing {
    let (team, ents) = std::thread::scope(|s| {
        let team = s.spawn(|| {
            let mut cmd = cmd::command("/usr/bin/codesign");
            cmd.args(["-dv", "--verbose=2"]).arg(bundle.as_os_str());
            match cmd::run_command("codesign", &mut cmd, TOOL_TIMEOUT) {
                // codesign prints the details on stderr.
                Ok(o) => parse_team_id(&o.stderr),
                Err(err) => {
                    tracing::debug!(%err, "codesign -dv failed");
                    None
                }
            }
        });
        let ents = s.spawn(|| {
            let mut cmd = cmd::command("/usr/bin/codesign");
            cmd.args(["-d", "--entitlements", "-", "--xml"])
                .arg(bundle.as_os_str());
            match cmd::run_command("codesign", &mut cmd, TOOL_TIMEOUT) {
                Ok(o) if !o.stdout.trim().is_empty() => {
                    match plist::Value::from_reader_xml(o.stdout.as_bytes()) {
                        Ok(value) => value.as_dictionary().map(entitlements).unwrap_or_default(),
                        Err(err) => {
                            tracing::debug!(%err, "entitlements plist");
                            Signing::default()
                        }
                    }
                }
                Ok(_) => Signing::default(),
                Err(err) => {
                    tracing::debug!(%err, "codesign entitlements failed");
                    Signing::default()
                }
            }
        });
        (
            team.join().unwrap_or_default(),
            ents.join().unwrap_or_default(),
        )
    });
    Signing {
        team: team.or(ents.team),
        ..ents
    }
}

/// The identifiers in an entitlements dictionary.
pub(super) fn entitlements(dict: &plist::Dictionary) -> Signing {
    Signing {
        team: plist_util::string(dict, "com.apple.developer.team-identifier"),
        groups: plist_util::strings(dict, "com.apple.security.application-groups"),
        keychain: plist_util::strings(dict, "keychain-access-groups"),
        icloud: plist_util::strings(dict, "com.apple.developer.icloud-container-identifiers"),
    }
}

/// `TeamIdentifier=ABCDE12345` from `codesign -dv` output (`not set` → `None`).
pub(super) fn parse_team_id(stderr: &str) -> Option<String> {
    stderr
        .lines()
        .find_map(|l| l.trim().strip_prefix("TeamIdentifier="))
        .map(str::trim)
        .filter(|t| super::ident::strip_team(&format!("{t}.x")).is_some())
        .map(str::to_owned)
}

/// Reverse-DNS vendor of an update feed URL (`https://u.keka.io/x` → `io.keka`); `None`
/// for shared hosting and bare IPs.
pub(super) fn feed_vendor(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority
        .rsplit('@')
        .next()?
        .split(':')
        .next()?
        .to_lowercase();
    let labels: Vec<&str> = host.split('.').filter(|l| !l.is_empty()).collect();
    if labels.len() < 2 || labels.iter().all(|l| l.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    let n = labels.len();
    let tld = labels.get(n.checked_sub(1)?)?;
    let second = labels.get(n.checked_sub(2)?)?;
    // `example.co.uk`, `example.com.cn`: the registrable name is one label further left.
    let (name, suffix) =
        if tld.len() == 2 && ["ac", "co", "com", "edu", "gov", "net", "org"].contains(second) {
            (*labels.get(n.checked_sub(3)?)?, format!("{second}.{tld}"))
        } else {
            (*second, (*tld).to_owned())
        };
    let registrable = format!("{name}.{suffix}");
    if HOSTING
        .iter()
        .any(|h| registrable == *h || host.ends_with(&format!(".{h}")))
    {
        return None;
    }
    Some(format!("{suffix}.{name}"))
}

/// What an Electron or Chromium-based bundle reveals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Electron {
    /// Chromium-based.
    pub(super) chromium: bool,
    /// `productName` and `name` of the app's `package.json`.
    pub(super) products: Vec<String>,
    /// electron-updater's `updaterCacheDirName`.
    pub(super) updater: Option<String>,
}

/// Electron facts of a bundle (empty for native apps).
pub(super) fn electron(bundle: &Path) -> Electron {
    let contents = bundle.join("Contents");
    let resources = contents.join("Resources");
    let mut out = Electron::default();
    if let Ok(entries) = std::fs::read_dir(contents.join("Frameworks")) {
        out.chromium = entries.flatten().any(|e| {
            let name = e.file_name().to_string_lossy().to_lowercase();
            name.starts_with("electron framework")
                || name.starts_with("chromium embedded framework")
                || (name.contains("chrome") && name.ends_with(" framework.framework"))
        });
    }
    let package = std::fs::read(resources.join("app").join("package.json"))
        .ok()
        .or_else(|| read_asar_package(&resources.join("app.asar")));
    if let Some(bytes) = package {
        out.products = package_names(&bytes);
        out.chromium = true;
    }
    out.updater = std::fs::read_to_string(resources.join("app-update.yml"))
        .ok()
        .as_deref()
        .and_then(updater_cache_dir);
    out
}

/// `productName` and unscoped `name` of a `package.json`.
pub(super) fn package_names(bytes: &[u8]) -> Vec<String> {
    #[derive(Deserialize)]
    struct Package {
        #[serde(default, rename = "productName")]
        product_name: Option<String>,
        #[serde(default)]
        name: Option<String>,
    }
    let Ok(pkg) = serde_json::from_slice::<Package>(bytes) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for name in [pkg.product_name, pkg.name].into_iter().flatten() {
        let name = name.trim();
        if !name.is_empty() && !name.contains('/') && !out.iter().any(|n: &String| n == name) {
            out.push(name.to_owned());
        }
    }
    out
}

/// `updaterCacheDirName: foo-updater` of electron-builder's `app-update.yml`.
pub(super) fn updater_cache_dir(yml: &str) -> Option<String> {
    yml.lines()
        .find_map(|l| l.trim().strip_prefix("updaterCacheDirName:"))
        .map(|v| v.trim().trim_matches(['\'', '"']).trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// Where `package.json` is inside an `app.asar`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub(super) struct AsarEntry {
    /// Bytes.
    #[serde(default)]
    pub(super) size: u64,
    /// Offset after the header, as a decimal string.
    #[serde(default)]
    pub(super) offset: Option<String>,
    /// Stored in `app.asar.unpacked` instead.
    #[serde(default)]
    pub(super) unpacked: bool,
}

/// The top-level `package.json` entry of an asar header, skipping everything else without
/// building it.
pub(super) fn asar_package_entry(header: &[u8]) -> Option<AsarEntry> {
    use serde::de::{Deserializer, IgnoredAny, MapAccess, Visitor};

    struct Files(Option<AsarEntry>);

    impl<'de> Deserialize<'de> for Files {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct V;
            impl<'de> Visitor<'de> for V {
                type Value = Files;

                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("an asar file map")
                }

                fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Files, A::Error> {
                    let mut found = None;
                    while let Some(key) = map.next_key::<String>()? {
                        if key == "package.json" {
                            found = Some(map.next_value::<AsarEntry>()?);
                        } else {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                    Ok(Files(found))
                }
            }
            d.deserialize_map(V)
        }
    }

    #[derive(Deserialize)]
    struct Header {
        files: Files,
    }

    serde_json::from_slice::<Header>(header).ok()?.files.0
}

/// `package.json` of an `app.asar` archive: `[u32 4][u32 header pickle size]` then the
/// pickle `[u32 payload size][u32 json length][json]`; file data starts after the pickle.
fn read_asar_package(asar: &Path) -> Option<Vec<u8>> {
    let mut file = std::fs::File::open(asar).ok()?;
    let mut head = [0_u8; 16];
    file.read_exact(&mut head).ok()?;
    let word = |i: usize| -> Option<u32> {
        let bytes: [u8; 4] = head.get(i..i.checked_add(4)?)?.try_into().ok()?;
        Some(u32::from_le_bytes(bytes))
    };
    let pickle = word(4)?;
    let json_len = word(12)?;
    if json_len > MAX_ASAR_HEADER || json_len > pickle.saturating_sub(8) {
        return None;
    }
    let mut header = vec![0_u8; usize::try_from(json_len).ok()?];
    file.read_exact(&mut header).ok()?;
    let entry = asar_package_entry(&header)?;
    if entry.unpacked {
        let unpacked = asar.with_extension("asar.unpacked").join("package.json");
        return std::fs::read(unpacked).ok();
    }
    let offset: u64 = entry.offset.as_deref()?.parse().ok()?;
    if entry.size == 0 || entry.size > MAX_PACKAGE_JSON {
        return None;
    }
    let start = u64::from(pickle).checked_add(8)?.checked_add(offset)?;
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = vec![0_u8; usize::try_from(entry.size).ok()?];
    file.read_exact(&mut bytes).ok()?;
    Some(bytes)
}

/// Processes running from the bundle.
pub(super) fn app_pids(app: &AppRecord) -> Vec<u32> {
    omc_scan::procs::running()
        .into_iter()
        .filter(|p| {
            p.exe.as_deref().is_some_and(|exe| {
                omc_scan::paths::is_within(exe, &app.detail.real)
                    || omc_scan::paths::is_within(exe, &app.detail.bundle)
            })
        })
        .map(|p| p.pid)
        .collect()
}

/// Paths the processes have open (`lsof -Fn`).
pub(super) fn open_files(pids: &[u32]) -> Vec<PathBuf> {
    if pids.is_empty() {
        return Vec::new();
    }
    let list: Vec<String> = pids.iter().map(u32::to_string).collect();
    let list = list.join(",");
    // lsof exits non-zero when one of the processes is gone; its output is still valid.
    match cmd::run(
        "/usr/sbin/lsof",
        &["-w", "-n", "-P", "-Fn", "-p", &list],
        TOOL_TIMEOUT,
    ) {
        Ok(out) => parse_lsof(&out.stdout),
        Err(err) => {
            tracing::debug!(%err, "lsof failed");
            Vec::new()
        }
    }
}

/// The `n/absolute/path` lines of `lsof -F` output, deduplicated.
pub(super) fn parse_lsof(text: &str) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    text.lines()
        .filter_map(|l| l.strip_prefix('n'))
        .filter(|p| p.starts_with('/'))
        // Sockets and pipes carry ` (…)` or `->` annotations; files never contain `->`.
        .filter(|p| !p.contains("->"))
        .filter(|p| seen.insert(*p))
        .map(PathBuf::from)
        .collect()
}

/// Package ids that installed the bundle (`pkgutil --file-info`).
pub(super) fn bundle_pkgids(bundle: &Path) -> Vec<String> {
    let mut cmd = cmd::command("/usr/sbin/pkgutil");
    cmd.arg("--file-info").arg(bundle.as_os_str());
    match cmd::run_command("pkgutil", &mut cmd, TOOL_TIMEOUT).and_then(|o| o.ok("pkgutil")) {
        Ok(text) => parse_pkgids(&text),
        Err(err) => {
            tracing::debug!(%err, "pkgutil --file-info");
            Vec::new()
        }
    }
}

/// Every installed package id (`pkgutil --pkgs`).
pub(super) fn all_pkgids() -> Vec<String> {
    match cmd::run("/usr/sbin/pkgutil", &["--pkgs"], TOOL_TIMEOUT).and_then(|o| o.ok("pkgutil")) {
        Ok(text) => text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect(),
        Err(err) => {
            tracing::debug!(%err, "pkgutil --pkgs");
            Vec::new()
        }
    }
}

/// `pkgid: <id>` lines of `pkgutil --file-info`.
pub(super) fn parse_pkgids(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("pkgid:"))
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty())
        .collect()
}

/// Absolute paths a package installed (`pkgutil --pkg-info` location + `--files`).
pub(super) fn pkg_payload(id: &str) -> Vec<PathBuf> {
    let location = match cmd::run("/usr/sbin/pkgutil", &["--pkg-info", id], TOOL_TIMEOUT)
        .and_then(|o| o.ok("pkgutil"))
    {
        Ok(text) => text
            .lines()
            .find_map(|l| l.strip_prefix("location:"))
            .map(|l| l.trim().to_owned())
            .unwrap_or_default(),
        Err(err) => {
            tracing::debug!(%err, id, "pkgutil --pkg-info");
            return Vec::new();
        }
    };
    let base = Path::new("/").join(location.trim_start_matches('/'));
    match cmd::run("/usr/sbin/pkgutil", &["--files", id], TOOL_TIMEOUT)
        .and_then(|o| o.ok("pkgutil"))
    {
        Ok(text) => text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| base.join(l))
            .collect(),
        Err(err) => {
            tracing::debug!(%err, id, "pkgutil --files");
            Vec::new()
        }
    }
}

/// Folders an installer payload only adds to (lowercase). Their direct children are the
/// payload's own items.
const CONTAINERS: &[&str] = &[
    "/",
    "/applications",
    "/etc",
    "/library",
    "/library/application support",
    "/library/audio",
    "/library/audio/plug-ins",
    "/library/audio/plug-ins/clap",
    "/library/audio/plug-ins/components",
    "/library/audio/plug-ins/hal",
    "/library/audio/plug-ins/mas",
    "/library/audio/plug-ins/vst",
    "/library/audio/plug-ins/vst3",
    "/library/caches",
    "/library/colorpickers",
    "/library/contextual menu items",
    "/library/coremediaio",
    "/library/coremediaio/plug-ins",
    "/library/coremediaio/plug-ins/dal",
    "/library/dictionaries",
    "/library/documentation",
    "/library/extensions",
    "/library/filesystems",
    "/library/fonts",
    "/library/frameworks",
    "/library/input methods",
    "/library/internet plug-ins",
    "/library/launchagents",
    "/library/launchdaemons",
    "/library/logs",
    "/library/preferencepanes",
    "/library/preferences",
    "/library/printers",
    "/library/privilegedhelpertools",
    "/library/quicklook",
    "/library/screen savers",
    "/library/services",
    "/library/spotlight",
    "/library/startupitems",
    "/library/widgets",
    "/opt",
    "/private",
    "/private/etc",
    "/private/tmp",
    "/private/var",
    "/tmp",
    "/users",
    "/users/shared",
    "/usr",
    "/usr/local",
    "/usr/local/bin",
    "/usr/local/etc",
    "/usr/local/include",
    "/usr/local/lib",
    "/usr/local/libexec",
    "/usr/local/sbin",
    "/usr/local/share",
    "/usr/local/share/doc",
    "/usr/local/share/man",
    "/usr/local/share/man/man1",
    "/var",
];

fn lower(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

/// The top-level items of an installer payload: paths directly inside a [`CONTAINERS`]
/// folder, outside app bundles and `/Applications`, not Apple's. Sorted, deduplicated.
pub(super) fn payload_roots(payload: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = payload
        .iter()
        .filter_map(|p| {
            let mut acc = PathBuf::new();
            for comp in p.components() {
                if !acc.as_os_str().is_empty() && !CONTAINERS.contains(&lower(&acc).as_str()) {
                    return None;
                }
                acc.push(comp);
                if !CONTAINERS.contains(&lower(&acc).as_str()) {
                    return Some(acc);
                }
            }
            None
        })
        .filter(|root| {
            let l = lower(root);
            let name = root
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            !l.starts_with("/applications/")
                && !root
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("app"))
                && !name.starts_with("com.apple.")
                && name != "apple"
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Symlinks in the command-line folders that point into the bundle.
pub(super) fn cli_links(bundle: &Path, real: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in ["/usr/local/bin", "/opt/homebrew/bin"] {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|t| t.is_symlink()) {
                continue;
            }
            let path = entry.path();
            let Ok(target) = std::fs::read_link(&path) else {
                continue;
            };
            let target = if target.is_absolute() {
                target
            } else {
                Path::new(dir).join(target)
            };
            if omc_scan::paths::is_within(&target, real)
                || omc_scan::paths::is_within(&target, bundle)
            {
                out.push(path);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "omc-mac-src-{tag}-{}-{}",
                std::process::id(),
                omc_scan::paths::now_secs()
            ));
            let _ignored = std::fs::remove_dir_all(&dir);
            assert!(std::fs::create_dir_all(&dir).is_ok(), "create temp dir");
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ignored = std::fs::remove_dir_all(&self.0);
        }
    }

    /// An asar archive holding `files` (name, content) at the top level.
    fn asar(files: &[(&str, &str)], unpacked: &[&str]) -> Vec<u8> {
        let mut data = Vec::new();
        let mut entries = serde_json::Map::new();
        for (name, content) in files {
            let mut e = serde_json::Map::new();
            e.insert("size".to_owned(), serde_json::Value::from(content.len()));
            if unpacked.contains(name) {
                e.insert("unpacked".to_owned(), serde_json::Value::Bool(true));
            } else {
                e.insert(
                    "offset".to_owned(),
                    serde_json::Value::from(data.len().to_string()),
                );
                data.extend_from_slice(content.as_bytes());
            }
            entries.insert((*name).to_owned(), serde_json::Value::Object(e));
        }
        let mut nested = serde_json::Map::new();
        nested.insert(
            "package.json".to_owned(),
            serde_json::json!({"size": 2, "offset": "0"}),
        );
        entries.insert(
            "node_modules".to_owned(),
            serde_json::json!({"files": {"dep": {"files": nested}}}),
        );
        let json = serde_json::json!({ "files": entries }).to_string();
        let json_len = u32::try_from(json.len()).unwrap_or_default();
        let padded = json_len.div_ceil(4).saturating_mul(4);
        let pickle = padded.saturating_add(8);
        let mut out = Vec::new();
        out.extend_from_slice(&4_u32.to_le_bytes());
        out.extend_from_slice(&pickle.to_le_bytes());
        out.extend_from_slice(&pickle.saturating_sub(4).to_le_bytes());
        out.extend_from_slice(&json_len.to_le_bytes());
        out.extend_from_slice(json.as_bytes());
        out.resize(
            out.len().saturating_add(
                usize::try_from(padded.saturating_sub(json_len)).unwrap_or_default(),
            ),
            0,
        );
        out.extend_from_slice(&data);
        out
    }

    #[test]
    fn asar_package_json_is_found_packed_and_unpacked() {
        let tmp = TempDir::new("asar");
        let packed = tmp.0.join("app.asar");
        let bytes = asar(
            &[
                ("main.js", "console.log(1)"),
                (
                    "package.json",
                    r#"{"productName":"Code","name":"code-oss"}"#,
                ),
            ],
            &[],
        );
        assert!(std::fs::write(&packed, bytes).is_ok(), "write asar");
        let pkg = read_asar_package(&packed);
        assert_eq!(
            pkg.as_deref().map(package_names),
            Some(vec!["Code".to_owned(), "code-oss".to_owned()]),
            "top-level package.json, nested ones ignored"
        );

        let dir = tmp.0.join("unpacked");
        assert!(
            std::fs::create_dir_all(dir.join("app.asar.unpacked")).is_ok(),
            "dir"
        );
        let json = r#"{"name":"apifox"}"#;
        let bytes = asar(&[("package.json", json)], &["package.json"]);
        assert!(
            std::fs::write(dir.join("app.asar"), bytes).is_ok(),
            "write asar"
        );
        assert!(
            std::fs::write(dir.join("app.asar.unpacked").join("package.json"), json).is_ok(),
            "write unpacked"
        );
        assert_eq!(
            read_asar_package(&dir.join("app.asar"))
                .as_deref()
                .map(package_names),
            Some(vec!["apifox".to_owned()]),
            "unpacked package.json"
        );
        assert!(
            std::fs::write(tmp.0.join("bad.asar"), b"\x04\0\0\0").is_ok(),
            "write"
        );
        assert_eq!(
            read_asar_package(&tmp.0.join("bad.asar")),
            None,
            "truncated archive"
        );
    }

    #[test]
    fn package_and_updater_names_parse() {
        assert_eq!(
            package_names(br#"{"name":"@scope/desktop","productName":"Paseo"}"#),
            vec!["Paseo".to_owned()],
            "scoped names are skipped"
        );
        assert!(package_names(b"not json").is_empty(), "garbage");
        assert_eq!(
            updater_cache_dir("owner: x\nupdaterCacheDirName: 'cherrystudio-updater'\n").as_deref(),
            Some("cherrystudio-updater"),
            "quoted value"
        );
        assert_eq!(updater_cache_dir("provider: github\n"), None, "absent");
    }

    #[test]
    fn feed_hosts_become_reverse_dns_vendors() {
        assert_eq!(
            feed_vendor("https://u.keka.io/appcast").as_deref(),
            Some("io.keka"),
            "subdomain dropped"
        );
        assert_eq!(
            feed_vendor("https://rectangleapp.com/downloads/updates.xml").as_deref(),
            Some("com.rectangleapp"),
            "plain host"
        );
        assert_eq!(
            feed_vendor("https://updates.example.co.uk:8443/feed").as_deref(),
            Some("co.uk.example"),
            "two-label suffix and port"
        );
        assert_eq!(
            feed_vendor("https://raw.githubusercontent.com/x/y/main/appcast.xml"),
            None,
            "shared hosting"
        );
        assert_eq!(
            feed_vendor("https://zerx-lab.github.io/feed.xml"),
            None,
            "pages"
        );
        assert_eq!(feed_vendor("http://10.0.0.1/feed"), None, "ip address");
    }

    #[test]
    fn lsof_output_parses_to_paths() {
        let text = "p123\nfcwd\nn/\nftxt\nn/Applications/X.app/Contents/MacOS/X\n\
                    f3\nn/Users/me/Library/Application Support/X/db\n\
                    f4\nn->0x1234\nf5\nn/Users/me/Library/Application Support/X/db\n\
                    p124\nf6\nnlocalhost:443\n";
        assert_eq!(
            parse_lsof(text),
            vec![
                PathBuf::from("/"),
                PathBuf::from("/Applications/X.app/Contents/MacOS/X"),
                PathBuf::from("/Users/me/Library/Application Support/X/db"),
            ],
            "absolute paths once, sockets skipped"
        );
    }

    #[test]
    fn payload_roots_are_top_level_items() {
        let payload: Vec<PathBuf> = [
            "/Applications",
            "/Applications/ToDesk.app",
            "/Applications/ToDesk.app/Contents/MacOS/ToDesk",
            "/Library",
            "/Library/Audio/Plug-Ins/HAL",
            "/Library/Audio/Plug-Ins/HAL/ToDeskOutputDriver.driver",
            "/Library/Audio/Plug-Ins/HAL/ToDeskOutputDriver.driver/Contents/Info.plist",
            "/Library/LaunchDaemons/com.youqu.todesk.service.plist",
            "/Library/Application Support/Vendor/Product/data.bin",
            "/Library/Application Support/Apple/x",
            "/usr/local/bin/todesk",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        assert_eq!(
            payload_roots(&payload),
            vec![
                PathBuf::from("/Library/Application Support/Vendor"),
                PathBuf::from("/Library/Audio/Plug-Ins/HAL/ToDeskOutputDriver.driver"),
                PathBuf::from("/Library/LaunchDaemons/com.youqu.todesk.service.plist"),
                PathBuf::from("/usr/local/bin/todesk"),
            ],
            "bundle, containers and Apple skipped"
        );
    }

    #[test]
    fn team_ids_and_pkgids_parse() {
        let err = "Executable=/A.app/Contents/MacOS/A\nTeamIdentifier=EQHXZ8M8AV\nSealed Resources";
        assert_eq!(parse_team_id(err).as_deref(), Some("EQHXZ8M8AV"), "team id");
        assert_eq!(
            parse_team_id("TeamIdentifier=not set"),
            None,
            "ad-hoc signed"
        );
        let info = "volume: /\npath: /Applications/X.app\n\npkgid: com.x.mac\npkg-version: 1\n";
        assert_eq!(parse_pkgids(info), vec!["com.x.mac".to_owned()], "pkgid");
    }

    #[test]
    fn entitlements_list_groups_and_icloud() {
        let mut d = plist::Dictionary::new();
        d.insert(
            "com.apple.security.application-groups".to_owned(),
            plist::Value::Array(vec![plist::Value::from("FN2V63AD2J.com.tencent")]),
        );
        d.insert(
            "com.apple.developer.icloud-container-identifiers".to_owned(),
            plist::Value::Array(vec![plist::Value::from("iCloud.com.foo.bar")]),
        );
        let s = entitlements(&d);
        assert_eq!(
            s.groups,
            vec!["FN2V63AD2J.com.tencent".to_owned()],
            "groups"
        );
        assert_eq!(s.icloud, vec!["iCloud.com.foo.bar".to_owned()], "icloud");
        assert_eq!(s.team, None, "no team key");
    }
}
