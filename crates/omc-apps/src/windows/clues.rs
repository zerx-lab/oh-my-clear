//! Pure parsing and scoring for the related-item finders: evidence-weighted confidence,
//! Windows Installer component key paths and ownership, `SharedDLLs` reference counts,
//! firewall rule strings, Electron `app.asar` headers and `package.json` names,
//! `UserAssist`/`MUICache` value names, Prefetch and Windows Error Reporting file names,
//! and the `Package Cache` folder naming of Burn bundles and cached MSI packages.
//! Nothing here touches the OS, so every function is unit-tested.

use omc_proto::apps::Confidence;

use super::parse::{self, Hive};
use crate::userconf::{NameMatch, Names, Rival};

// ---------------------------------------------------------------------------------------
// Confidence

/// One piece of evidence tying an item to the app. Weights follow Bulk Crap Uninstaller's
/// confidence records (explicit connection, perfect product match, directory still used…):
/// the sum decides the level, so independent clues add up and contradicting ones cancel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Clue {
    /// Names or lives in the install folder, or names one of the app's programs.
    Reference,
    /// Windows Installer lists it as the key path of a component only this product owns.
    MsiOwned,
    /// The app's own manifest declares this name (Electron `productName`, `name`).
    Declared,
    /// Folder or key named after a product name of the app.
    ProductName,
    /// …equal to the display name or the install folder's name, not just normalised.
    ExactName,
    /// Name starts with a product name (`Product Crashes`, `product-updater`).
    NamePrefix,
    /// Lives below the app's vendor folder or key.
    VendorParent,
    /// Named after the app's vendor.
    VendorName,
    /// The matched name is short (< 5 characters): collisions are likely.
    ShortName,
    /// Another installed app claims it too (overlapping folder, shared component or DLL,
    /// same name).
    Shared,
}

impl Clue {
    const fn weight(self) -> i32 {
        match self {
            Self::Reference | Self::MsiOwned => 6,
            Self::Declared => 4,
            Self::ProductName | Self::VendorParent => 2,
            Self::ExactName | Self::NamePrefix | Self::VendorName => 1,
            Self::ShortName => -2,
            Self::Shared => -4,
        }
    }
}

/// Confidence from the summed clue weights (≥ 4 High, ≥ 2 Medium, ≥ 0 Low), capped at
/// `cap`; `None` when the evidence speaks against the item (drop it). No clue at all is
/// no evidence and also `None`.
pub(crate) fn confidence(clues: &[Clue], cap: Confidence) -> Option<Confidence> {
    if clues.is_empty() {
        return None;
    }
    let score = clues
        .iter()
        .fold(0_i32, |sum, clue| sum.saturating_add(clue.weight()));
    let level = match score {
        4.. => Confidence::High,
        2..=3 => Confidence::Medium,
        0..=1 => Confidence::Low,
        _ => return None,
    };
    Some(level.min(cap))
}

/// [`Clue::ShortName`] when the normalised `name` is shorter than 5 characters.
pub(crate) fn short_clue(name: &str) -> Option<Clue> {
    (parse::normalize_name(name).chars().count() < 5).then_some(Clue::ShortName)
}

/// Program names so common that a match by file name alone proves nothing.
pub(crate) fn is_generic_exe(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
    stem.chars().count() < 4
        || stem.starts_with("unins")
        || stem.starts_with("uninst")
        || stem.starts_with("setup")
        || matches!(
            stem,
            "update"
                | "updater"
                | "launcher"
                | "install"
                | "installer"
                | "helper"
                | "crashpad_handler"
                | "crashreporter"
                | "notification_helper"
                | "elevate"
                | "service"
                | "server"
                | "client"
                | "main"
                | "start"
                | "run"
                | "python"
                | "pythonw"
                | "java"
                | "javaw"
                | "node"
                | "electron"
                | "maintenancetool"
                | "vc_redist.x64"
                | "vc_redist.x86"
        )
}

// ---------------------------------------------------------------------------------------
// Per-user configuration

/// What a file of the install folder is to the per-user configuration search, in search
/// order (most telling first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum BinaryRole {
    /// The program `DisplayIcon` names.
    Main,
    /// An Electron `resources\app.asar` (never returned by [`binary_role`]).
    Archive,
    /// Another distinctive program.
    Program,
    /// A library named after the app or one of its programs.
    Library,
}

/// Name prefixes (lower case) of the runtime libraries Chromium/Electron, Qt, MSVC and
/// the usual toolkits ship: they hold no app strings.
const RUNTIME_LIBS: &[&str] = &[
    "api-ms-win",
    "chrome_elf",
    "concrt",
    "d3dcompiler",
    "dxcompiler",
    "dxil",
    "ffmpeg",
    "icudt",
    "icuin",
    "icuuc",
    "libc++",
    "libcef",
    "libcrypto",
    "libegl",
    "libglesv2",
    "libssl",
    "mfc1",
    "msvcp",
    "msvcr",
    "opengl32sw",
    "python3",
    "qt5",
    "qt6",
    "sqlite3",
    "ucrtbase",
    "vccorlib",
    "vcruntime",
    "vk_swiftshader",
    "vulkan-1",
    "zlib",
];

/// The role of the top-level install folder file `file_name` in the per-user
/// configuration search; `None`: not searched. `main` is the `DisplayIcon` program's file
/// name, `names` the app's normalised names ([`parse::normalize_name`]). Distinctive
/// programs are searched; libraries only when named after the app (`<name>*.dll`) and
/// never the shared runtime ones.
pub(crate) fn binary_role(
    file_name: &str,
    main: Option<&str>,
    names: &[String],
) -> Option<BinaryRole> {
    let lower = file_name.to_ascii_lowercase();
    if let Some(stem) = lower.strip_suffix(".exe") {
        if is_generic_exe(&lower) || stem.is_empty() {
            return None;
        }
        let is_main = main.is_some_and(|m| m.eq_ignore_ascii_case(file_name));
        return Some(if is_main {
            BinaryRole::Main
        } else {
            BinaryRole::Program
        });
    }
    let stem = lower.strip_suffix(".dll")?;
    if RUNTIME_LIBS.iter().any(|p| stem.starts_with(p)) {
        return None;
    }
    let n = parse::normalize_name(stem);
    names
        .iter()
        .any(|name| {
            name.chars().count() >= 3
                && (n == *name || (name.chars().count() >= 4 && n.starts_with(name.as_str())))
        })
        .then_some(BinaryRole::Library)
}

/// How other installed apps (`peers`: their names) compete for a per-user configuration
/// candidate named `stem` that this app matches as `ours`: matching it at least as
/// strongly is [`Rival::Equal`], more loosely [`Rival::Weaker`].
pub(crate) fn config_rival(peers: &Names, stem: &str, ours: NameMatch) -> Rival {
    match peers.relate(stem) {
        None => Rival::None,
        Some(theirs) if theirs >= ours => Rival::Equal,
        Some(_) => Rival::Weaker,
    }
}

// ---------------------------------------------------------------------------------------
// Windows Installer

/// A component's key path (the value data under `UserData\<SID>\Components\<packed>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KeyPath {
    /// A file.
    File(String),
    /// A folder (`CreateFolder` component; the data ends with `\`).
    Folder(String),
    /// A named registry value.
    Value {
        hive: Hive,
        /// Sub path below the hive.
        key: String,
        name: String,
        /// Written by a 32-bit component: on 64-bit Windows `HKLM\SOFTWARE\…` lives in
        /// the `WOW6432Node` view.
        wow32: bool,
    },
}

/// Parses a component key path: `C:\dir\file.dll`, `C:\dir\` (folder), or
/// `NN:\Key\Value` for registry key paths where `NN` is `00` `HKCR`, `01` `HKCU`,
/// `02` `HKLM`, `03` `HKU`, plus 20 for 64-bit components. `None` for run-from-source
/// (`C?\…`), default-value and `HKU` key paths, and anything malformed.
pub(crate) fn parse_key_path(raw: &str) -> Option<KeyPath> {
    let raw = raw.trim().trim_matches(char::from(0));
    if parse::is_absolute(raw) {
        return Some(if raw.ends_with('\\') {
            KeyPath::Folder(raw.trim_end_matches('\\').to_owned())
        } else {
            KeyPath::File(raw.to_owned())
        });
    }
    let (root, rest) = raw.split_once(":\\")?;
    let code: u8 = root.parse().ok()?;
    let (hive, wow32) = match code {
        0 => (Hive::ClassesRoot, true),
        1 => (Hive::CurrentUser, true),
        2 => (Hive::LocalMachine, true),
        20 => (Hive::ClassesRoot, false),
        21 => (Hive::CurrentUser, false),
        22 => (Hive::LocalMachine, false),
        _ => return None,
    };
    let (key, name) = rest.rsplit_once('\\')?;
    let key = key.trim_matches('\\');
    if key.is_empty() || name.is_empty() {
        return None;
    }
    Some(KeyPath::Value {
        hive,
        key: key.to_owned(),
        name: name.to_owned(),
        wow32,
    })
}

/// The `WOW6432Node` form of a 32-bit component's `HKLM\SOFTWARE\…` key (`None` when the
/// key is not redirected).
pub(crate) fn wow_key(key: &str) -> Option<String> {
    let (head, rest) = key.split_once('\\')?;
    (head.eq_ignore_ascii_case("software") && !rest.to_ascii_lowercase().starts_with("wow6432node"))
        .then(|| format!("{head}\\WOW6432Node\\{rest}"))
}

/// Who uses a Windows Installer component, from the value names (packed product codes)
/// of its `Components\<packed>` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ownership {
    /// Only this product: its key path goes with the product.
    Sole,
    /// This product and others (or the component is permanent): keep the key path.
    Shared,
    /// Not this product's component.
    Foreign,
}

/// Ownership of a component by the product whose packed code is `own` (the default
/// value, when present, is ignored). A permanent component is owned by the all-zero
/// "system" code and therefore never sole-owned.
pub(crate) fn ownership<'a>(owners: impl IntoIterator<Item = &'a str>, own: &str) -> Ownership {
    let mut mine = false;
    let mut others = false;
    for owner in owners.into_iter().filter(|o| !o.is_empty()) {
        if owner.eq_ignore_ascii_case(own) {
            mine = true;
        } else {
            others = true;
        }
    }
    match (mine, others) {
        (true, false) => Ownership::Sole,
        (true, true) => Ownership::Shared,
        (false, _) => Ownership::Foreign,
    }
}

/// `{GUID}` (Burn bundle) or `{GUID}v1.2.3` (cached MSI package) folder name below
/// `%ProgramData%\Package Cache` → the upper-case code.
pub(crate) fn package_cache_code(name: &str) -> Option<String> {
    let code = name.get(..38)?;
    let rest = name.get(38..)?;
    let versioned = rest
        .strip_prefix(['v', 'V'])
        .is_some_and(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.'));
    (parse::is_guid(code) && (rest.is_empty() || versioned)).then(|| code.to_ascii_uppercase())
}

// ---------------------------------------------------------------------------------------
// SharedDLLs

/// What the `SharedDLLs` reference count allows for a file inside an app's folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SharedDll {
    /// Count 0 or 1: only this app referenced it; the file and its counter may go.
    Owned,
    /// Count above 1, or unreadable: other installers still count on it; keep it.
    Shared,
}

/// Decision for a `SharedDLLs` value (`None` = the count could not be read).
pub(crate) fn shared_dll(count: Option<u32>) -> SharedDll {
    match count {
        Some(0 | 1) => SharedDll::Owned,
        Some(_) | None => SharedDll::Shared,
    }
}

// ---------------------------------------------------------------------------------------
// Firewall rules

/// A field of a firewall rule string (`v2.30|Action=Allow|Dir=In|App=C:\x\a.exe|…|`,
/// MS-GPFAS grammar); keys compare case-insensitively, empty values are `None`.
pub(crate) fn firewall_field<'a>(rule: &'a str, field: &str) -> Option<&'a str> {
    let mut parts = rule.split('|');
    let version = parts.next()?;
    if !version.trim().starts_with(['v', 'V']) {
        return None;
    }
    parts
        .find_map(|part| {
            let (key, value) = part.split_once('=')?;
            key.trim()
                .eq_ignore_ascii_case(field)
                .then_some(value.trim())
        })
        .filter(|value| !value.is_empty())
}

// ---------------------------------------------------------------------------------------
// Electron

/// Where the JSON header of an `app.asar` lives: it starts at byte 16; file offsets count
/// from `data_start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AsarLayout {
    pub(crate) json_len: usize,
    pub(crate) data_start: u64,
}

/// Largest header read (real apps have a few MiB at most).
const ASAR_MAX_HEADER: u32 = 64 << 20;

fn le_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let raw: [u8; 4] = bytes.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(raw))
}

/// Parses the 16-byte Chromium-pickle prefix of an `app.asar`: `4`, header pickle size,
/// header payload size, JSON length.
pub(crate) fn asar_layout(head: &[u8]) -> Option<AsarLayout> {
    if le_u32(head, 0)? != 4 {
        return None;
    }
    let header_size = le_u32(head, 4)?;
    let payload = le_u32(head, 8)?;
    let json_len = le_u32(head, 12)?;
    if header_size > ASAR_MAX_HEADER
        || payload.checked_add(4)? > header_size
        || json_len.checked_add(4)? > payload
    {
        return None;
    }
    Some(AsarLayout {
        json_len: usize::try_from(json_len).ok()?,
        data_start: u64::from(header_size).checked_add(8)?,
    })
}

/// A file inside an `app.asar`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AsarEntry {
    /// Offset from [`AsarLayout::data_start`].
    pub(crate) offset: u64,
    pub(crate) size: u64,
    /// Stored next to the archive in `app.asar.unpacked`.
    pub(crate) unpacked: bool,
}

/// Looks up a top-level file (`package.json`) in the asar JSON header.
pub(crate) fn asar_entry(json: &str, name: &str) -> Option<AsarEntry> {
    let header: serde_json::Value = serde_json::from_str(json).ok()?;
    let entry = header.get("files")?.get(name)?;
    let size = entry.get("size")?.as_u64()?;
    if entry
        .get("unpacked")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return Some(AsarEntry {
            offset: 0,
            size,
            unpacked: true,
        });
    }
    let offset = match entry.get("offset")? {
        serde_json::Value::String(text) => text.parse().ok()?,
        other => other.as_u64()?,
    };
    Some(AsarEntry {
        offset,
        size,
        unpacked: false,
    })
}

/// A name usable as a single folder name (no separators, reserved characters or dots).
fn is_folder_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.ends_with(['.', ' '])
        && !name.contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|'])
        && !name.chars().any(char::is_control)
}

/// Folder names an Electron app uses for its data, from its `package.json`: `productName`
/// (`%APPDATA%\<productName>` is Electron's default `userData`) then `name` without an npm
/// scope. Unsafe names (separators, `..`) are dropped.
pub(crate) fn electron_names(package_json: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(package_json) else {
        return Vec::new();
    };
    let mut names: Vec<String> = Vec::new();
    for field in ["productName", "name"] {
        let Some(raw) = value.get(field).and_then(serde_json::Value::as_str) else {
            continue;
        };
        let raw = raw.trim();
        let name = raw.rsplit_once('/').map_or(raw, |(_, n)| n).trim();
        if is_folder_name(name) && !names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
            names.push(name.to_owned());
        }
    }
    names
}

// ---------------------------------------------------------------------------------------
// Usage traces

/// ROT13 (how `UserAssist` stores its value names).
pub(crate) fn rot13(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            'a'..='m' | 'A'..='M' => char::from(u8::try_from(c).map_or(0, |b| b.wrapping_add(13))),
            'n'..='z' | 'N'..='Z' => char::from(u8::try_from(c).map_or(0, |b| b.wrapping_sub(13))),
            other => other,
        })
        .collect()
}

/// Environment variable and sub folder of the known folders `UserAssist` paths start with.
fn known_folder(guid: &str) -> Option<(&'static str, &'static str)> {
    Some(match guid.to_ascii_uppercase().as_str() {
        "{6D809377-6AF0-444B-8957-A3773F02200E}" => ("ProgramFiles", ""),
        "{7C5A40EF-A0FB-4BFC-874A-C0F2E0B9FA8E}" => ("ProgramFiles(x86)", ""),
        "{F38BF404-1D43-42F2-9305-67DE0B28FC23}" => ("windir", ""),
        "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}" => ("windir", "System32"),
        "{D65231B0-B2F1-4857-A4CE-A8E7C6EA7D27}" => ("windir", "SysWOW64"),
        "{F1B32785-6FBA-4FCF-9D55-7B8E7F157091}" => ("LOCALAPPDATA", ""),
        "{3EB685DB-65F9-4CF6-A03A-E3EF65729F3D}" => ("APPDATA", ""),
        "{A77F5D77-2E2B-44C3-A6A2-ABA601054A51}" => {
            ("APPDATA", "Microsoft\\Windows\\Start Menu\\Programs")
        }
        "{0139D44E-6AFE-49F2-8690-3DAFCAE6FFB8}" => {
            ("ProgramData", "Microsoft\\Windows\\Start Menu\\Programs")
        }
        "{9E3995AB-1F9C-4F13-B827-48B24B6C7174}" => (
            "APPDATA",
            "Microsoft\\Internet Explorer\\Quick Launch\\User Pinned",
        ),
        _ => return None,
    })
}

/// A decoded `UserAssist` value name as a path: ROT13 undone and a leading known-folder
/// GUID replaced through `env` (`None` when the GUID or its variable is unknown).
pub(crate) fn user_assist_path(
    value_name: &str,
    env: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let decoded = rot13(value_name);
    let Some(guid) = decoded.get(..38).filter(|g| parse::is_guid(g)) else {
        return Some(decoded);
    };
    let rest = decoded
        .get(38..)
        .unwrap_or_default()
        .trim_start_matches('\\');
    let (var, sub) = known_folder(guid)?;
    let base = env(var)?;
    let base = base.trim_end_matches('\\');
    Some(match (sub.is_empty(), rest.is_empty()) {
        (true, true) => base.to_owned(),
        (true, false) => format!("{base}\\{rest}"),
        (false, true) => format!("{base}\\{sub}"),
        (false, false) => format!("{base}\\{sub}\\{rest}"),
    })
}

/// The program path of a `MUICache` value name (`C:\x\a.exe.FriendlyAppName`,
/// `….ApplicationCompany`, or the bare path); `None` for `LangID` and other values.
pub(crate) fn mui_cache_program(value_name: &str) -> Option<&str> {
    let path = [".FriendlyAppName", ".ApplicationCompany"]
        .iter()
        .find_map(|suffix| value_name.strip_suffix(suffix))
        .unwrap_or(value_name);
    parse::is_absolute(path).then_some(path)
}

/// `DISCORD.EXE-1A2B3C4D.pf` → `discord.exe`.
pub(crate) fn prefetch_program(file_name: &str) -> Option<String> {
    let lower = file_name.to_ascii_lowercase();
    let stem = lower.strip_suffix(".pf")?;
    let (program, hash) = stem.rsplit_once('-')?;
    let is_exe = std::path::Path::new(program)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"));
    (hash.len() == 8 && hash.bytes().all(|b| b.is_ascii_hexdigit()) && is_exe)
        .then(|| program.to_owned())
}

/// Report labels Windows Error Reporting puts before the program name.
const WER_LABELS: [&str; 6] = [
    "AppCrash_",
    "AppHang_",
    "BEX64_",
    "BEX_",
    "Critical_",
    "NonCritical_",
];

/// The (possibly truncated) program name of a WER report folder
/// (`AppCrash_Discord.exe_3f2a…_1b2c…_cab_…`).
pub(crate) fn wer_program(folder_name: &str) -> Option<&str> {
    let rest = WER_LABELS.iter().find_map(|label| {
        let head = folder_name.get(..label.len())?;
        head.eq_ignore_ascii_case(label)
            .then(|| folder_name.get(label.len()..))
            .flatten()
    })?;
    // The program name may contain `_`; it ends before the first long hex hash.
    let mut search = 0_usize;
    while let Some(found) = rest.get(search..).and_then(|r| r.find('_')) {
        let at = search.checked_add(found)?;
        let after = rest.get(at.checked_add(1)?..)?;
        let hex_run = after.bytes().take_while(u8::is_ascii_hexdigit).count();
        if hex_run >= 16 {
            let program = rest.get(..at)?;
            return (program.chars().count() >= 4).then_some(program);
        }
        search = at.checked_add(1)?;
    }
    None
}

/// `app.exe` is (a truncation-aware match of) the WER report `program`.
pub(crate) fn wer_matches(program: &str, exe: &str) -> bool {
    exe.get(..program.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(program))
}

/// `HKLM\SOFTWARE\Microsoft\Tracing\<exe stem>_RASAPI32` → the stem.
pub(crate) fn tracing_program(key_name: &str) -> Option<&str> {
    let (stem, suffix) = key_name.rsplit_once('_')?;
    (suffix.eq_ignore_ascii_case("RASAPI32") || suffix.eq_ignore_ascii_case("RASMANCS"))
        .then_some(stem)
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Owner value Windows Installer writes for permanent (system-shared) components.
    const PERMANENT_OWNER: &str = "00000000000000000000000000000000";

    #[test]
    fn clues_sum_to_levels() {
        let cap = Confidence::High;
        assert_eq!(
            confidence(&[Clue::Reference], cap),
            Some(Confidence::High),
            "exact reference"
        );
        assert_eq!(
            confidence(&[Clue::MsiOwned], cap),
            Some(Confidence::High),
            "MSI ownership"
        );
        assert_eq!(
            confidence(&[Clue::VendorParent, Clue::ProductName], cap),
            Some(Confidence::High),
            "vendor\\product folder"
        );
        assert_eq!(
            confidence(&[Clue::ProductName], cap),
            Some(Confidence::Medium),
            "product name only"
        );
        assert_eq!(
            confidence(&[Clue::ProductName, Clue::ExactName], cap),
            Some(Confidence::Medium),
            "exact product name alone stays Medium"
        );
        assert_eq!(
            confidence(&[Clue::VendorName], cap),
            Some(Confidence::Low),
            "vendor only"
        );
        assert_eq!(
            confidence(&[Clue::ProductName, Clue::ShortName], cap),
            Some(Confidence::Low),
            "short product name"
        );
        assert_eq!(
            confidence(&[Clue::Reference, Clue::Shared], cap),
            Some(Confidence::Medium),
            "install folder shared with another app"
        );
        assert_eq!(
            confidence(&[Clue::ProductName, Clue::Shared], cap),
            None,
            "name shared with another installed app: dropped"
        );
        assert_eq!(
            confidence(&[Clue::Reference], Confidence::Low),
            Some(Confidence::Low),
            "cap wins (privacy traces)"
        );
        assert_eq!(confidence(&[], cap), None, "no evidence, no item");
    }

    #[test]
    fn generic_programs_and_short_names() {
        assert!(is_generic_exe("Update.exe"), "Squirrel updater");
        assert!(is_generic_exe("unins000.exe"), "Inno uninstaller");
        assert!(is_generic_exe("app.exe"), "short stem");
        assert!(!is_generic_exe("Discord.exe"), "distinctive program");
        assert_eq!(
            short_clue("7-Zip"),
            Some(Clue::ShortName),
            "`7zip` has 4 characters"
        );
        assert_eq!(short_clue("Firefox"), None, "long enough");
    }

    #[test]
    fn key_paths_parse_files_folders_and_registry() {
        assert_eq!(
            parse_key_path("C:\\Program Files\\Common Files\\X\\x.dll"),
            Some(KeyPath::File(
                "C:\\Program Files\\Common Files\\X\\x.dll".into()
            )),
            "file"
        );
        assert_eq!(
            parse_key_path("C:\\ProgramData\\X\\"),
            Some(KeyPath::Folder("C:\\ProgramData\\X".into())),
            "folder"
        );
        assert_eq!(
            parse_key_path("02:\\SOFTWARE\\Vendor\\Product\\InstallDir"),
            Some(KeyPath::Value {
                hive: Hive::LocalMachine,
                key: "SOFTWARE\\Vendor\\Product".into(),
                name: "InstallDir".into(),
                wow32: true,
            }),
            "32-bit HKLM value"
        );
        assert_eq!(
            parse_key_path("21:\\Software\\Vendor\\Path"),
            Some(KeyPath::Value {
                hive: Hive::CurrentUser,
                key: "Software\\Vendor".into(),
                name: "Path".into(),
                wow32: false,
            }),
            "64-bit HKCU value"
        );
        assert_eq!(
            parse_key_path("02:\\SOFTWARE\\Vendor\\"),
            None,
            "default value is not claimed"
        );
        assert_eq!(parse_key_path("03:\\S-1-5-18\\X\\Y"), None, "HKU skipped");
        assert_eq!(parse_key_path("C?\\setup\\x.dll"), None, "run from source");
        assert_eq!(parse_key_path(""), None, "empty");
        assert_eq!(
            wow_key("SOFTWARE\\Vendor\\Product"),
            Some("SOFTWARE\\WOW6432Node\\Vendor\\Product".into()),
            "redirected"
        );
        assert_eq!(
            wow_key("SOFTWARE\\WOW6432Node\\V"),
            None,
            "already 32-bit view"
        );
        assert_eq!(wow_key("SYSTEM\\X"), None, "not redirected");
    }

    #[test]
    fn component_ownership() {
        let own = "0123456789ABCDEF0123456789ABCDEF";
        assert_eq!(ownership([own], own), Ownership::Sole, "only us");
        assert_eq!(
            ownership(["", &own.to_ascii_lowercase()], own),
            Ownership::Sole,
            "default value ignored, case-insensitive"
        );
        assert_eq!(
            ownership([own, "FEDCBA9876543210FEDCBA9876543210"], own),
            Ownership::Shared,
            "another product too"
        );
        assert_eq!(
            ownership([own, PERMANENT_OWNER], own),
            Ownership::Shared,
            "permanent component"
        );
        assert_eq!(
            ownership([PERMANENT_OWNER], own),
            Ownership::Foreign,
            "not ours"
        );
        assert_eq!(ownership([], own), Ownership::Foreign, "no owners");
    }

    #[test]
    fn package_cache_folder_names() {
        let code = "{11111111-2222-3333-4444-555555555555}";
        assert_eq!(package_cache_code(code), Some(code.into()), "bundle folder");
        assert_eq!(
            package_cache_code("{aaaaaaaa-2222-3333-4444-555555555555}v14.38.33135"),
            Some("{AAAAAAAA-2222-3333-4444-555555555555}".into()),
            "cached MSI with version"
        );
        assert_eq!(package_cache_code(&format!("{code}x")), None, "junk suffix");
        assert_eq!(
            package_cache_code(&format!("{code}v")),
            None,
            "empty version"
        );
        assert_eq!(
            package_cache_code("0a1b2c3d4e5f"),
            None,
            "hash-named folder"
        );
    }

    #[test]
    fn shared_dll_counts() {
        assert_eq!(shared_dll(Some(1)), SharedDll::Owned, "only this app");
        assert_eq!(shared_dll(Some(0)), SharedDll::Owned, "already released");
        assert_eq!(
            shared_dll(Some(2)),
            SharedDll::Shared,
            "another installer counts on it"
        );
        assert_eq!(shared_dll(None), SharedDll::Shared, "unknown count is kept");
    }

    #[test]
    fn firewall_rule_fields() {
        let rule = "v2.30|Action=Allow|Active=TRUE|Dir=In|Protocol=6|App=%ProgramFiles%\\X\\x.exe|Name=X|Desc=a=b|";
        assert_eq!(
            firewall_field(rule, "App"),
            Some("%ProgramFiles%\\X\\x.exe"),
            "program path"
        );
        assert_eq!(
            firewall_field(rule, "name"),
            Some("X"),
            "case-insensitive key"
        );
        assert_eq!(
            firewall_field(rule, "Desc"),
            Some("a=b"),
            "value keeps later `=`"
        );
        assert_eq!(firewall_field(rule, "Svc"), None, "absent field");
        assert_eq!(firewall_field("v2.10|App=|", "App"), None, "empty value");
        assert_eq!(
            firewall_field("Action=Allow|App=C:\\x.exe|", "App"),
            None,
            "no version"
        );
    }

    fn asar_bytes(json: &str, data: &[u8]) -> Vec<u8> {
        let len = u32::try_from(json.len()).unwrap_or(0);
        let padded = len.div_ceil(4).saturating_mul(4);
        let payload = padded.saturating_add(4);
        let header = payload.saturating_add(4);
        let mut out = Vec::new();
        for n in [4, header, payload, len] {
            out.extend_from_slice(&n.to_le_bytes());
        }
        out.extend_from_slice(json.as_bytes());
        out.resize(usize::try_from(header.saturating_add(8)).unwrap_or(0), 0);
        out.extend_from_slice(data);
        out
    }

    #[test]
    fn asar_header_and_package_json() {
        let package = br#"{"name":"@scope/my-app","productName":"My App","version":"1.0.0"}"#;
        let json = format!(
            r#"{{"files":{{"package.json":{{"size":{},"offset":"0"}},"native.node":{{"size":9,"unpacked":true}}}}}}"#,
            package.len()
        );
        let bytes = asar_bytes(&json, package);
        let layout = asar_layout(bytes.get(..16).unwrap_or_default());
        assert!(layout.is_some(), "valid prefix");
        let Some(layout) = layout else { return };
        assert_eq!(layout.json_len, json.len(), "JSON length");
        let start = 16_usize;
        let text = bytes
            .get(start..start.saturating_add(layout.json_len))
            .and_then(|b| std::str::from_utf8(b).ok())
            .unwrap_or_default();
        let entry = asar_entry(text, "package.json");
        assert_eq!(
            entry,
            Some(AsarEntry {
                offset: 0,
                size: u64::try_from(package.len()).unwrap_or(0),
                unpacked: false
            }),
            "package.json entry"
        );
        let data_start = usize::try_from(layout.data_start).unwrap_or(0);
        let content = bytes
            .get(data_start..)
            .and_then(|b| std::str::from_utf8(b).ok())
            .unwrap_or_default();
        assert_eq!(
            electron_names(content),
            vec!["My App".to_owned(), "my-app".to_owned()],
            "names"
        );
        assert_eq!(
            asar_entry(text, "native.node").map(|e| e.unpacked),
            Some(true),
            "unpacked file"
        );
        assert_eq!(asar_entry(text, "missing.json"), None, "absent file");
        assert_eq!(
            asar_layout(&[5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            None,
            "bad magic"
        );
        assert_eq!(asar_layout(&[4, 0, 0]), None, "truncated");
        let mut lying = bytes.get(..16).map(<[u8]>::to_vec).unwrap_or_default();
        if let Some(b) = lying.get_mut(12) {
            *b = 0xFF;
        }
        assert_eq!(asar_layout(&lying), None, "JSON longer than its pickle");
    }

    #[test]
    fn electron_names_reject_unsafe() {
        assert_eq!(
            electron_names(r#"{"productName":"..\\..\\Windows","name":"ok-app"}"#),
            vec!["ok-app".to_owned()],
            "path traversal dropped"
        );
        assert_eq!(
            electron_names(r#"{"productName":"Same","name":"same"}"#),
            vec!["Same".to_owned()],
            "case-insensitive duplicate"
        );
        assert!(electron_names("not json").is_empty(), "malformed");
    }

    #[test]
    fn user_assist_and_mui_cache_names() {
        assert_eq!(rot13("P:\\Cebtenz Svyrf"), "C:\\Program Files", "rot13");
        let env = |var: &str| match var {
            "ProgramFiles" => Some("C:\\Program Files".to_owned()),
            "APPDATA" => Some("C:\\Users\\u\\AppData\\Roaming\\".to_owned()),
            _ => None,
        };
        assert_eq!(
            user_assist_path(
                &rot13("{6D809377-6AF0-444B-8957-A3773F02200E}\\Vendor\\app.exe"),
                env
            ),
            Some("C:\\Program Files\\Vendor\\app.exe".into()),
            "known folder resolved"
        );
        assert_eq!(
            user_assist_path(&rot13("{A77F5D77-2E2B-44C3-A6A2-ABA601054A51}\\X\\X.lnk"), env),
            Some("C:\\Users\\u\\AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs\\X\\X.lnk".into()),
            "known folder with sub folder"
        );
        assert_eq!(
            user_assist_path(&rot13("C:\\Tools\\x.exe"), env),
            Some("C:\\Tools\\x.exe".into()),
            "plain path"
        );
        assert_eq!(
            user_assist_path(&rot13("{00000000-0000-0000-0000-000000000000}\\x.exe"), env),
            None,
            "unknown folder"
        );
        assert_eq!(
            mui_cache_program("C:\\X\\a.exe.FriendlyAppName"),
            Some("C:\\X\\a.exe"),
            "friendly name value"
        );
        assert_eq!(
            mui_cache_program("C:\\X\\a.exe"),
            Some("C:\\X\\a.exe"),
            "bare path"
        );
        assert_eq!(mui_cache_program("LangID"), None, "not a program");
    }

    #[test]
    fn prefetch_wer_and_tracing_names() {
        assert_eq!(
            prefetch_program("DISCORD.EXE-1A2B3C4D.pf"),
            Some("discord.exe".into()),
            "prefetch"
        );
        assert_eq!(
            prefetch_program("MY-APP.EXE-0000ABCD.pf"),
            Some("my-app.exe".into()),
            "dash in name"
        );
        assert_eq!(prefetch_program("Layout.ini"), None, "not a prefetch file");
        assert_eq!(prefetch_program("X.EXE-12.pf"), None, "short hash");
        let hash = "3f2a9c1d7e6b5a4f3f2a9c1d7e6b5a4f3f2a9c1d";
        assert_eq!(
            wer_program(&format!(
                "AppCrash_my_tool.exe_{hash}_1b2c3d4e_cab_0a1b2c3d"
            )),
            Some("my_tool.exe"),
            "underscore in program name"
        );
        assert_eq!(
            wer_program(&format!("AppHang_Discord.ex_{hash}_x")),
            Some("Discord.ex"),
            "truncated name"
        );
        assert_eq!(wer_program("Kernel_141_abc"), None, "not an app report");
        assert!(wer_matches("Discord.ex", "discord.exe"), "truncation-aware");
        assert!(
            !wer_matches("Discord.exe", "disc.exe"),
            "longer report name"
        );
        assert_eq!(
            tracing_program("discord_RASAPI32"),
            Some("discord"),
            "tracing"
        );
        assert_eq!(
            tracing_program("my_app_RASMANCS"),
            Some("my_app"),
            "underscore in stem"
        );
        assert_eq!(tracing_program("_RASAPI32"), None, "empty stem");
        assert_eq!(tracing_program("FWCFG"), None, "other key");
    }

    #[test]
    fn config_binaries_are_the_apps_own() {
        let names = vec!["discord".to_owned(), "discordptb".to_owned()];
        let role = |file: &str| binary_role(file, Some("Discord.exe"), &names);
        assert_eq!(
            role("discord.exe"),
            Some(BinaryRole::Main),
            "DisplayIcon program, any case"
        );
        assert_eq!(
            role("DiscordHook.exe"),
            Some(BinaryRole::Program),
            "other program"
        );
        assert_eq!(role("Update.exe"), None, "generic program");
        assert_eq!(
            role("Discord_utils.dll"),
            Some(BinaryRole::Library),
            "named after the app"
        );
        assert_eq!(
            role("discordptb.DLL"),
            Some(BinaryRole::Library),
            "named after a program"
        );
        for runtime in [
            "ffmpeg.dll",
            "libEGL.dll",
            "vk_swiftshader.dll",
            "d3dcompiler_47.dll",
            "msvcp140.dll",
            "vcruntime140_1.dll",
        ] {
            assert_eq!(role(runtime), None, "runtime library {runtime}");
        }
        assert_eq!(role("other.dll"), None, "library not named after the app");
        assert_eq!(role("resources.pak"), None, "not a program or library");
        assert_eq!(
            binary_role("zed.dll", None, &["zed".to_owned()]),
            Some(BinaryRole::Library),
            "short name matches exactly"
        );
        assert_eq!(
            binary_role("zedit.dll", None, &["zed".to_owned()]),
            None,
            "short name is no prefix"
        );
        assert!(
            BinaryRole::Main < BinaryRole::Archive
                && BinaryRole::Archive < BinaryRole::Program
                && BinaryRole::Program < BinaryRole::Library,
            "search order: main program, archive, programs, libraries"
        );
    }

    #[test]
    fn config_rival_from_other_apps() {
        let mut peers = Names::default();
        peers.add("claudecode");
        peers.add("ghostty");
        assert_eq!(
            config_rival(&peers, "ghostty", NameMatch::Exact),
            Rival::Equal,
            "another app has the same name"
        );
        assert_eq!(
            config_rival(&peers, "claude", NameMatch::Exact),
            Rival::Weaker,
            "another app's name only extends it"
        );
        assert_eq!(
            config_rival(&peers, "claude", NameMatch::Partial),
            Rival::Equal,
            "both match it partially"
        );
        assert_eq!(
            config_rival(&peers, "zed", NameMatch::Exact),
            Rival::None,
            "no other app"
        );
    }
}
