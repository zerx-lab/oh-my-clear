//! Pure parsing for the Windows platform module: command lines (`UninstallString`,
//! `ImagePath`, `Run` values), registry paths, product/vendor name normalisation, `.lnk`
//! link targets, `StartupApproved` values and the JSON printed by the PowerShell helpers.
//! Nothing here touches the OS, so every function is unit-tested.

use std::path::Path;

use omc_proto::apps::UninstallerOutcome;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::{Error, Result};

// ---------------------------------------------------------------------------------------
// Registry paths

/// Registry hives oh-my-clear reads and writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Hive {
    /// `HKEY_CURRENT_USER`.
    CurrentUser,
    /// `HKEY_LOCAL_MACHINE`.
    LocalMachine,
    /// `HKEY_CLASSES_ROOT`.
    ClassesRoot,
    /// `HKEY_USERS`.
    Users,
}

impl Hive {
    /// Short prefix used in reports and by `reg.exe` (`HKCU`, `HKLM`…).
    pub(crate) const fn short(self) -> &'static str {
        match self {
            Self::CurrentUser => "HKCU",
            Self::LocalMachine => "HKLM",
            Self::ClassesRoot => "HKCR",
            Self::Users => "HKU",
        }
    }

    /// `HKLM\path` (or the bare hive for an empty path).
    pub(crate) fn full(self, path: &str) -> String {
        if path.is_empty() {
            self.short().to_owned()
        } else {
            format!("{}\\{path}", self.short())
        }
    }
}

/// Splits `HKLM\SOFTWARE\X` (or `HKEY_LOCAL_MACHINE\…`, any case) into hive and sub path.
pub(crate) fn parse_reg_path(full: &str) -> Option<(Hive, &str)> {
    let full = full.trim().trim_matches('\\');
    let (head, rest) = full.split_once('\\').unwrap_or((full, ""));
    let hive = match head.to_ascii_uppercase().as_str() {
        "HKCU" | "HKEY_CURRENT_USER" => Hive::CurrentUser,
        "HKLM" | "HKEY_LOCAL_MACHINE" => Hive::LocalMachine,
        "HKCR" | "HKEY_CLASSES_ROOT" => Hive::ClassesRoot,
        "HKU" | "HKEY_USERS" => Hive::Users,
        _ => return None,
    };
    Some((hive, rest.trim_matches('\\')))
}

/// Parent and last component of a sub path (`a\b\c` gives `a\b` and `c`; a top-level key has an
/// empty parent).
pub(crate) fn split_key(path: &str) -> (&str, &str) {
    path.rsplit_once('\\').unwrap_or(("", path))
}

/// A file name for the `.reg` backup of `key` (unique per `stamp`).
pub(crate) fn backup_file_name(key: &str, stamp: u128) -> String {
    let mut name: String = key
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '{' | '}') {
                c
            } else {
                '_'
            }
        })
        .take(150)
        .collect();
    name.push('-');
    name.push_str(&stamp.to_string());
    name.push_str(".reg");
    name
}

// ---------------------------------------------------------------------------------------
// Command lines and paths

/// `C:\…` or `C:/…`.
pub(crate) fn is_absolute(path: &str) -> bool {
    matches!(path.as_bytes(), [d, b':', b'\\' | b'/', ..] if d.is_ascii_alphabetic())
}

/// Replaces `%NAME%` with `lookup(NAME)`; unknown variables are kept verbatim.
pub(crate) fn expand_env(text: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((before, after)) = rest.split_once('%') {
        out.push_str(before);
        match after.split_once('%') {
            Some((name, tail)) if !name.is_empty() && !name.contains(char::is_whitespace) => {
                if let Some(value) = lookup(name) {
                    out.push_str(&value);
                } else {
                    out.push('%');
                    out.push_str(name);
                    out.push('%');
                }
                rest = tail;
            }
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// A path stored in the registry: quotes, surrounding blanks and trailing separators
/// removed (`C:\` stays `C:\`). `None` when empty.
pub(crate) fn clean_path(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_matches('"').trim();
    let without = trimmed.trim_end_matches(['\\', '/']);
    let out = if without.len() == 2 && without.ends_with(':') {
        format!("{without}\\")
    } else {
        without.to_owned()
    };
    (!out.is_empty()).then_some(out)
}

/// `DisplayIcon` value (`"C:\x\app.exe",0`, `C:\x\app.ico`) → the file.
pub(crate) fn icon_path(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let unquoted = if let Some(rest) = trimmed.strip_prefix('"') {
        rest.split_once('"').map_or(rest, |(inside, _)| inside)
    } else {
        match trimmed.rsplit_once(',') {
            Some((file, index))
                if index
                    .trim()
                    .trim_start_matches('-')
                    .chars()
                    .all(|c| c.is_ascii_digit()) =>
            {
                file
            }
            _ => trimmed,
        }
    };
    clean_path(unquoted)
}

/// A command line split into program and verbatim arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandLine {
    /// Program (as written: absolute path or bare name).
    pub(crate) exe: String,
    /// Everything after it, trimmed.
    pub(crate) args: String,
}

impl CommandLine {
    /// Lower-case file name of the program (`msiexec.exe`).
    pub(crate) fn exe_name(&self) -> String {
        file_name(&self.exe).to_ascii_lowercase()
    }
}

/// Last component of a Windows path.
pub(crate) fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// Parent directory of a Windows path (`None` for a bare name).
pub(crate) fn parent_dir(path: &str) -> Option<&str> {
    let (parent, _) = path.rsplit_once(['\\', '/'])?;
    (!parent.is_empty()).then_some(parent)
}

/// Splits a command line the way `CreateProcess` resolves an unquoted program with
/// spaces: a quoted program ends at the closing quote; otherwise the longest prefix
/// ending before a blank that `is_file` accepts wins (`C:\Program Files\X\u.exe /S`),
/// then the first `.exe`, then the first word.
pub(crate) fn split_command(cmd: &str, is_file: impl Fn(&str) -> bool) -> Option<CommandLine> {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return None;
    }
    if let Some(rest) = cmd.strip_prefix('"') {
        let (exe, args) = rest.split_once('"').unwrap_or((rest, ""));
        let exe = exe.trim();
        return (!exe.is_empty()).then(|| CommandLine {
            exe: exe.to_owned(),
            args: args.trim().to_owned(),
        });
    }
    let make = |split: usize| {
        let (exe, args) = cmd.split_at_checked(split)?;
        Some(CommandLine {
            exe: exe.trim().to_owned(),
            args: args.trim().to_owned(),
        })
    };
    let mut blanks: Vec<usize> = cmd
        .char_indices()
        .filter(|(_, c)| c.is_whitespace())
        .map(|(i, _)| i)
        .collect();
    blanks.push(cmd.len());
    for &end in blanks.iter().rev() {
        if let Some(prefix) = cmd.get(..end)
            && is_file(prefix)
        {
            return make(end);
        }
    }
    let lower = cmd.to_ascii_lowercase();
    let mut from = 0;
    while let Some(found) = lower.get(from..).and_then(|tail| tail.find(".exe")) {
        let end = from.saturating_add(found).saturating_add(4);
        let next = lower.get(end..).and_then(|tail| tail.chars().next());
        if next.is_none_or(char::is_whitespace) {
            return make(end);
        }
        from = end;
    }
    make(blanks.first().copied().unwrap_or(cmd.len()))
}

/// The file whose absence proves that `cmd` can no longer run: the program itself, or
/// the DLL of a `rundll32` command. `None` when that cannot be told (bare names,
/// `msiexec`, script hosts).
pub(crate) fn command_target(cmd: &str, is_file: impl Fn(&str) -> bool) -> Option<String> {
    let line = split_command(cmd, is_file)?;
    let name = line.exe_name();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    match name {
        "rundll32" => {
            let arg = line.args.trim_start();
            let dll = match arg.strip_prefix('"') {
                Some(rest) => rest.split_once('"').map_or(rest, |(inside, _)| inside),
                None => arg.split(',').next().unwrap_or(arg),
            };
            let dll = dll.trim().trim_end_matches(',');
            is_absolute(dll).then(|| dll.to_owned())
        }
        "msiexec" | "cmd" | "powershell" | "pwsh" | "wscript" | "cscript" | "explorer"
        | "conhost" | "mshta" => None,
        _ => is_absolute(&line.exe).then_some(line.exe),
    }
}

/// `\??\C:\x`, `system32\drivers\x.sys`, `"C:\x\svc.exe" -k` → absolute program path of a
/// service `ImagePath` (`windir` resolves the relative `system32\…` form).
pub(crate) fn service_image(
    image: &str,
    windir: &str,
    is_file: impl Fn(&str) -> bool,
) -> Option<String> {
    let image = image.trim();
    let image = image.strip_prefix("\\??\\").unwrap_or(image);
    let line = split_command(image, is_file)?;
    let exe = line.exe;
    if is_absolute(&exe) {
        return Some(exe);
    }
    let lower = exe.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("\\systemroot\\") {
        return Some(format!(
            "{windir}\\{}",
            exe.get(exe.len().saturating_sub(rest.len())..)
                .unwrap_or(rest)
        ));
    }
    if lower.starts_with("system32\\") || lower.starts_with("syswow64\\") {
        return Some(format!("{windir}\\{exe}"));
    }
    None
}

/// Lower-cased path with a trailing separator, for "is inside this directory" tests on
/// command strings.
pub(crate) fn dir_needle(dir: &str) -> String {
    let mut needle = dir.trim_end_matches(['\\', '/']).to_lowercase();
    needle.push('\\');
    needle
}

/// `text` mentions a file inside `dir` (`dir_lower` from [`dir_needle`]) or `dir` itself.
pub(crate) fn mentions_dir(text: &str, dir_lower: &str) -> bool {
    let text = text.to_lowercase().replace('/', "\\");
    if text.contains(dir_lower) {
        return true;
    }
    let bare = dir_lower.trim_end_matches('\\');
    text.trim().trim_matches('"').trim_end_matches('\\') == bare
}

/// `path` (a Windows path string) is `dir` or inside it, case-insensitively.
pub(crate) fn path_within(path: &str, dir: &str) -> bool {
    let path = path
        .trim_end_matches(['\\', '/'])
        .to_lowercase()
        .replace('/', "\\");
    let dir = dir
        .trim_end_matches(['\\', '/'])
        .to_lowercase()
        .replace('/', "\\");
    if dir.is_empty() {
        return false;
    }
    path == dir
        || path
            .strip_prefix(&dir)
            .is_some_and(|rest| rest.starts_with('\\'))
}

// ---------------------------------------------------------------------------------------
// MSI product codes

/// `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`.
pub(crate) fn is_guid(text: &str) -> bool {
    text.len() == 38
        && text.bytes().enumerate().all(|(i, b)| match i {
            0 => b == b'{',
            37 => b == b'}',
            9 | 14 | 19 | 24 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// The first product code inside `text` (upper case).
pub(crate) fn find_guid(text: &str) -> Option<String> {
    text.char_indices()
        .filter(|(_, c)| *c == '{')
        .find_map(|(i, _)| {
            let candidate = text.get(i..i.checked_add(38)?)?;
            is_guid(candidate).then(|| candidate.to_ascii_uppercase())
        })
}

/// The "packed" (compressed) form Windows Installer uses as key name under
/// `Installer\Products`: each group of the GUID reversed, the last 8 bytes nibble-swapped.
pub(crate) fn packed_guid(guid: &str) -> Option<String> {
    if !is_guid(guid) {
        return None;
    }
    let hex: Vec<char> = guid
        .chars()
        .filter(char::is_ascii_hexdigit)
        .map(|c| c.to_ascii_uppercase())
        .collect();
    let mut out = String::with_capacity(32);
    let mut rest = hex.as_slice();
    for len in [8, 4, 4, 2, 2, 2, 2, 2, 2, 2, 2] {
        let (group, tail) = rest.split_at_checked(len)?;
        out.extend(group.iter().rev());
        rest = tail;
    }
    Some(out)
}

// ---------------------------------------------------------------------------------------
// Names

/// Suffixes of company names that folder and key names usually omit.
const LEGAL_SUFFIXES: &[&str] = &[
    "inc",
    "incorporated",
    "ltd",
    "limited",
    "llc",
    "gmbh",
    "corp",
    "corporation",
    "co",
    "company",
    "sa",
    "ag",
    "bv",
    "srl",
    "plc",
    "sro",
    "oy",
    "ab",
    "as",
    "pty",
    "kg",
    "se",
    "spa",
    "sas",
    "kk",
    "nv",
    "lp",
    "llp",
    "technologies",
    "software",
];

/// Tokens that describe a build, not the product.
fn is_version_token(token: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || c == '.');
    digits(token)
        || token.strip_prefix('v').is_some_and(digits)
        || matches!(
            token,
            "x64"
                | "x86"
                | "amd64"
                | "arm64"
                | "win64"
                | "win32"
                | "bit"
                | "64bit"
                | "32bit"
                | "enus"
                | "en"
                | "us"
        )
}

fn name_tokens(text: &str) -> Vec<String> {
    let mut depth = 0_u32;
    let mut cleaned = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '(' | '[' => depth = depth.saturating_add(1),
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => cleaned.push(c),
            _ => {}
        }
    }
    cleaned
        .split(|c: char| !c.is_alphanumeric() && c != '.')
        .map(|t| t.trim_matches('.').replace('.', "").to_lowercase())
        .filter(|t| !t.is_empty())
        .collect()
}

/// Product name for folder/key comparison: lower-case letters and digits only, without
/// parenthesised parts, trailing versions and bitness (`7-Zip 23.01 (x64)` → `7zip`,
/// `Python 3.12.1 (64-bit)` → `python`).
pub(crate) fn normalize_name(name: &str) -> String {
    let mut tokens = name_tokens(name);
    while tokens.len() > 1 && tokens.last().is_some_and(|t| is_version_token(t)) {
        tokens.pop();
    }
    tokens.retain(|t| {
        !matches!(
            t.as_str(),
            "x64" | "x86" | "amd64" | "arm64" | "64bit" | "32bit"
        )
    });
    tokens.concat()
}

/// Vendor name for comparison: [`normalize_name`] without legal suffixes
/// (`Microsoft Corporation` → `microsoft`, `JetBrains s.r.o.` → `jetbrains`).
pub(crate) fn normalize_publisher(publisher: &str) -> String {
    let mut tokens = name_tokens(publisher);
    while tokens.len() > 1
        && tokens
            .last()
            .is_some_and(|t| is_version_token(t) || LEGAL_SUFFIXES.contains(&t.as_str()))
    {
        tokens.pop();
    }
    tokens.concat()
}

/// Normalised names a product's folders and keys may use: the display name, the display
/// name without the vendor prefix (`Mozilla Firefox` → `firefox`), and the install
/// folder's own name. Names shorter than 3 characters are dropped.
pub(crate) fn product_keys(
    name: &str,
    publisher: Option<&str>,
    install_dir: Option<&str>,
) -> Vec<String> {
    let mut keys = vec![normalize_name(name)];
    if let Some(publisher) = publisher {
        let pub_tokens = name_tokens(publisher);
        let mut tokens = name_tokens(name);
        while tokens.len() > 1 && tokens.last().is_some_and(|t| is_version_token(t)) {
            tokens.pop();
        }
        if let Some(first) = pub_tokens.first()
            && tokens.len() > 1
            && tokens.first() == Some(first)
        {
            keys.push(tokens.get(1..).map(<[String]>::concat).unwrap_or_default());
        }
    }
    if let Some(dir) = install_dir {
        keys.push(normalize_name(file_name(dir)));
    }
    keys.retain(|k| k.chars().count() >= 3);
    keys.sort();
    keys.dedup();
    keys
}

/// `KB5031356` and similar update names.
pub(crate) fn is_update_name(name: &str) -> bool {
    let name = name.trim();
    let bytes = name.as_bytes();
    matches!(bytes, [b'K' | b'k', b'B' | b'b', d, ..] if d.is_ascii_digit())
}

/// `ReleaseType` values of Windows/Office updates.
pub(crate) fn is_update_release(release: &str) -> bool {
    matches!(
        release.trim().to_ascii_lowercase().as_str(),
        "update" | "hotfix" | "security update" | "update rollup" | "service pack"
    )
}

/// Microsoft runtimes and OS components other apps depend on (never uninstallable here).
pub(crate) fn is_system_app(name: &str, publisher: Option<&str>) -> bool {
    let Some(publisher) = publisher else {
        return false;
    };
    if !normalize_publisher(publisher).starts_with("microsoft") {
        return false;
    }
    let name = name.to_lowercase();
    (name.contains("visual c++") && (name.contains("redistributable") || name.contains("runtime")))
        || [
            "microsoft edge",
            "webview2",
            ".net",
            "windows sdk",
            "windows software development kit",
            "windows app runtime",
            "windowsappruntime",
            "windows desktop runtime",
            "asp.net",
            "directx",
            "vc_redist",
            "update health tools",
            "gameinput",
        ]
        .iter()
        .any(|needle| name.contains(needle))
}

/// Vendor key/folder names that belong to Windows or hardware drivers (never orphans).
pub(crate) fn is_os_vendor(normalized: &str) -> bool {
    matches!(
        normalized,
        "microsoft"
            | "classes"
            | "policies"
            | "wow6432node"
            | "clients"
            | "registeredapplications"
            | "appdatalow"
            | "intel"
            | "nvidia"
            | "nvidiacorporation"
            | "amd"
            | "ati"
            | "atitechnologies"
            | "realtek"
            | "realtekaudio"
            | "synaptics"
            | "elantech"
            | "dell"
            | "hp"
            | "hewlettpackard"
            | "lenovo"
            | "asus"
            | "acer"
            | "odbc"
            | "khronos"
            | "chromium"
            | "netscape"
            | "windows"
            | "windowsapps"
            | "packages"
            | "temp"
            | "crashdumps"
            | "connecteddevicesplatform"
            | "comms"
            | "d3dscache"
            | "virtualstore"
            | "programs"
            | "publisher"
            | "oem"
            | "vmware"
            | "vmwareinc"
            | "oracle"
            | "javasoft"
            | "google"
            | "mozilla"
            | "apple"
            | "appleinc"
            | "applecomputerinc"
            | "ssh"
            | "gnu"
            | "python"
            | "npm"
            | "npmcache"
            | "pip"
            | "nuget"
            | "package"
            | "packagecache"
            | "regid19910601commicrosoft"
            | "usoprivate"
            | "usoshared"
            | "softwaredistribution"
            | "ssl"
            | "sophos"
            | "defender"
            | "windowsdefender"
            | "identities"
            | "systemcertificates"
            | "startmenu"
            | "desktop"
            | "documents"
            | "cache"
            | "caches"
            | "logs"
            | "log"
    )
}

// ---------------------------------------------------------------------------------------
// Dates

/// `InstallDate` (`yyyymmdd`) → Unix seconds (midnight UTC).
pub(crate) fn parse_install_date(text: &str) -> Option<i64> {
    const MONTH_DAYS: [i64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let text = text.trim();
    if text.len() != 8 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let year: i64 = text.get(..4)?.parse().ok()?;
    let month: i64 = text.get(4..6)?.parse().ok()?;
    let day: i64 = text.get(6..)?.parse().ok()?;
    if !(1970..=2200).contains(&year) || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let leap = |y: i64| (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let mut days: i64 = 0;
    for y in 1970..year {
        days = days.checked_add(if leap(y) { 366 } else { 365 })?;
    }
    for (index, len) in (1_i64..).zip(MONTH_DAYS) {
        if index >= month {
            break;
        }
        days = days.checked_add(len)?;
        if index == 2 && leap(year) {
            days = days.checked_add(1)?;
        }
    }
    days = days.checked_add(day.checked_sub(1)?)?;
    days.checked_mul(86_400)
}

/// Unix seconds → `FILETIME` (100 ns ticks since 1601).
pub(crate) fn filetime(unix_secs: i64) -> u64 {
    u64::try_from(unix_secs.saturating_add(11_644_473_600))
        .unwrap_or(0)
        .saturating_mul(10_000_000)
}

// ---------------------------------------------------------------------------------------
// StartupApproved

/// Task Manager's `StartupApproved` value: even first byte = enabled, odd = disabled; a
/// missing value means enabled.
pub(crate) fn approved_enabled(value: Option<&[u8]>) -> bool {
    value.and_then(<[u8]>::first).is_none_or(|b| b % 2 == 0)
}

/// The 12-byte value Task Manager writes: `02` + zeros (enabled) or `03 00 00 00` + the
/// `FILETIME` of the change (disabled).
pub(crate) fn approved_value(enabled: bool, filetime: u64) -> [u8; 12] {
    if enabled {
        [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
    } else {
        let mut out = [0_u8; 12];
        for (slot, byte) in out
            .iter_mut()
            .zip([3, 0, 0, 0].into_iter().chain(filetime.to_le_bytes()))
        {
            *slot = byte;
        }
        out
    }
}

// ---------------------------------------------------------------------------------------
// System info

/// `ProductName` says "Windows 10" on Windows 11 (build ≥ 22000); fixes that and joins
/// edition, feature update and build: `Windows 11 Pro 24H2 (26100)`.
pub(crate) fn os_display(
    product: &str,
    display_version: Option<&str>,
    build: Option<&str>,
) -> String {
    let build_no: u32 = build.and_then(|b| b.trim().parse().ok()).unwrap_or(0);
    let mut name = product.trim().to_owned();
    if build_no >= 22_000 && name.contains("Windows 10") {
        name = name.replacen("Windows 10", "Windows 11", 1);
    }
    if name.is_empty() {
        "Windows".clone_into(&mut name);
    }
    if let Some(version) = display_version.map(str::trim).filter(|v| !v.is_empty()) {
        name.push(' ');
        name.push_str(version);
    }
    if let Some(build) = build.map(str::trim).filter(|b| !b.is_empty()) {
        name.push_str(" (");
        name.push_str(build);
        name.push(')');
    }
    name
}

/// `whoami /groups` lists the High or System mandatory level.
pub(crate) fn whoami_elevated(text: &str) -> bool {
    text.contains("S-1-16-12288") || text.contains("S-1-16-16384")
}

// ---------------------------------------------------------------------------------------
// Uninstallers

/// Processes that keep running after an uninstaller's launcher exited: NSIS (`Au_.exe`,
/// `Un_A.exe`), Inno Setup (`unins000.exe`, `_iu14D2N.tmp`), generic `*uninst*`, and the
/// launched program itself.
pub(crate) fn is_uninstaller_process(name: &str, launched: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let launched = launched.to_ascii_lowercase();
    name == "au_.exe"
        || name == "un_a.exe"
        || (name.starts_with("unins") && has_extension(&name, "exe"))
        || (name.starts_with("_iu") && has_extension(&name, "tmp"))
        || name.contains("uninst")
        || (!launched.is_empty() && name == launched)
}

fn has_extension(name: &str, ext: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

/// Uninstaller exit code → outcome (`0`, `3010` reboot required and `1641` reboot started
/// succeed; `1602` and `1223` are user cancellations).
pub(crate) fn exit_outcome(code: Option<i32>, detail: &str) -> UninstallerOutcome {
    match code {
        Some(0 | 3010 | 1641) => UninstallerOutcome::Succeeded,
        Some(1602 | 1223) => UninstallerOutcome::Cancelled,
        code => UninstallerOutcome::Failed {
            code,
            message: detail.trim().to_owned(),
        },
    }
}

/// Single-quoted PowerShell string literal.
pub(crate) fn ps_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

// ---------------------------------------------------------------------------------------
// PowerShell JSON

/// `ConvertTo-Json` prints one object for a single result and an array otherwise
/// (nothing at all for no result).
pub(crate) fn parse_json_list<T: DeserializeOwned>(text: &str) -> Result<Vec<T>> {
    let text = text.trim().trim_start_matches('\u{feff}');
    if text.is_empty() {
        return Ok(Vec::new());
    }
    if text.starts_with('[') {
        Ok(serde_json::from_str(text)?)
    } else {
        Ok(vec![serde_json::from_str(text)?])
    }
}

/// A JSON field that may be `null`, one string or an array of strings.
fn one_or_many<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Shape {
        One(String),
        Many(Vec<Option<String>>),
    }
    Ok(match Option::<Shape>::deserialize(de)? {
        None => Vec::new(),
        Some(Shape::One(s)) => vec![s],
        Some(Shape::Many(v)) => v.into_iter().flatten().collect(),
    })
}

/// `Get-AppxPackage | Select-Object Name,PackageFullName,…`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct AppxPackage {
    /// Package name (`Mozilla.Firefox`).
    pub(crate) name: String,
    /// Full name for `Remove-AppxPackage`.
    pub(crate) package_full_name: String,
    /// Family name (`…_8wekyb3d8bbwe`), also the `%LOCALAPPDATA%\Packages` folder.
    #[serde(default)]
    pub(crate) package_family_name: Option<String>,
    /// Version.
    #[serde(default)]
    pub(crate) version: Option<String>,
    /// Signer distinguished name (`CN=…, O=…`).
    #[serde(default)]
    pub(crate) publisher: Option<String>,
    /// Install folder under `WindowsApps`.
    #[serde(default)]
    pub(crate) install_location: Option<String>,
}

/// `Get-Process | Select-Object Id,Path`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct ProcessPath {
    /// Process id.
    pub(crate) id: u32,
    /// Executable path (missing for processes of other users/elevated ones).
    #[serde(default)]
    pub(crate) path: Option<String>,
}

/// `Get-CimInstance Win32_LogicalDisk | Select-Object DeviceID,VolumeName,Size,FreeSpace`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LogicalDisk {
    /// `C:`.
    #[serde(rename = "DeviceID")]
    pub(crate) device_id: String,
    /// Label.
    #[serde(rename = "VolumeName", default)]
    pub(crate) volume_name: Option<String>,
    /// Capacity.
    #[serde(rename = "Size", default)]
    pub(crate) size: Option<u64>,
    /// Free bytes.
    #[serde(rename = "FreeSpace", default)]
    pub(crate) free_space: Option<u64>,
}

/// One scheduled task as printed by the task listing script.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct TaskInfo {
    /// `\Vendor\Updater`.
    pub(crate) path: String,
    /// `Ready`, `Disabled`, `Running`, `Queued`.
    #[serde(default)]
    pub(crate) state: Option<String>,
    /// Principal the task runs as.
    #[serde(default)]
    pub(crate) user_id: Option<String>,
    /// `program arguments` of each exec action.
    #[serde(default, deserialize_with = "one_or_many")]
    pub(crate) exec: Vec<String>,
    /// CIM class of each trigger (`MSFT_TaskLogonTrigger`, `MSFT_TaskBootTrigger`…).
    #[serde(default, deserialize_with = "one_or_many")]
    pub(crate) triggers: Vec<String>,
}

impl TaskInfo {
    /// Starts at logon or boot.
    pub(crate) fn at_startup(&self) -> bool {
        self.triggers.iter().any(|t| {
            t.eq_ignore_ascii_case("MSFT_TaskLogonTrigger")
                || t.eq_ignore_ascii_case("MSFT_TaskBootTrigger")
        })
    }

    /// Not disabled.
    pub(crate) fn enabled(&self) -> bool {
        !self
            .state
            .as_deref()
            .is_some_and(|s| s.eq_ignore_ascii_case("disabled"))
    }

    /// Belongs to Windows (`\Microsoft\…`).
    pub(crate) fn is_microsoft(&self) -> bool {
        self.path.to_ascii_lowercase().starts_with("\\microsoft\\")
    }

    /// Last component of the path.
    pub(crate) fn name(&self) -> &str {
        file_name(&self.path)
    }
}

/// `DisplayName` from `AppxManifest.xml` when it is literal text (not `ms-resource:`).
pub(crate) fn manifest_display_name(xml: &str) -> Option<String> {
    manifest_property(xml, "DisplayName")
}

/// `PublisherDisplayName` from `AppxManifest.xml` when it is literal text.
pub(crate) fn manifest_publisher(xml: &str) -> Option<String> {
    manifest_property(xml, "PublisherDisplayName")
}

fn manifest_property(xml: &str, tag: &str) -> Option<String> {
    let (_, properties) = xml.split_once("<Properties")?;
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let (_, after) = properties.split_once(&open)?;
    let (value, _) = after.split_once(&close)?;
    let value = value
        .trim()
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'");
    (!value.is_empty() && !value.to_ascii_lowercase().starts_with("ms-resource:")).then_some(value)
}

/// Organisation (`O=`) or common name (`CN=`) of a signer distinguished name.
pub(crate) fn dn_name(dn: &str) -> Option<String> {
    let field = |key: &str| {
        dn.split(',').find_map(|part| {
            let (k, v) = part.split_once('=')?;
            k.trim()
                .eq_ignore_ascii_case(key)
                .then(|| v.trim().trim_matches('"').to_owned())
                .filter(|v| !v.is_empty())
        })
    };
    field("O").or_else(|| field("CN"))
}

// ---------------------------------------------------------------------------------------
// Shell links

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    let end = at.checked_add(2)?;
    let slice: [u8; 2] = bytes.get(at..end)?.try_into().ok()?;
    Some(u16::from_le_bytes(slice))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<usize> {
    let end = at.checked_add(4)?;
    let slice: [u8; 4] = bytes.get(at..end)?.try_into().ok()?;
    usize::try_from(u32::from_le_bytes(slice)).ok()
}

fn ansi_z(bytes: &[u8], at: usize) -> Option<String> {
    let tail = bytes.get(at..)?;
    let end = tail.iter().position(|b| *b == 0)?;
    let raw = tail.get(..end)?;
    // Non-ASCII ANSI text depends on the code page; decoding it wrong would make a live
    // target look missing, so such paths are "unknown".
    raw.is_ascii()
        .then(|| String::from_utf8_lossy(raw).into_owned())
}

fn utf16_z(bytes: &[u8], at: usize) -> Option<String> {
    let tail = bytes.get(at..)?;
    let units: Vec<u16> = tail
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .take_while(|u| *u != 0)
        .collect();
    String::from_utf16(&units).ok()
}

/// Local target path of a `.lnk` file (MS-SHLLINK `LinkInfo`: `LocalBasePath` +
/// `CommonPathSuffix`, Unicode variants when present). `None` for shell-item, network or
/// advertised (MSI) shortcuts and malformed files.
pub(crate) fn lnk_target(bytes: &[u8]) -> Option<String> {
    const HEADER: usize = 0x4C;
    const HAS_ID_LIST: usize = 0x1;
    const HAS_LINK_INFO: usize = 0x2;
    const VOLUME_AND_LOCAL_BASE: usize = 0x1;
    if u32_at(bytes, 0)? != HEADER {
        return None;
    }
    let flags = u32_at(bytes, 20)?;
    if flags & HAS_LINK_INFO == 0 {
        return None;
    }
    let mut at = HEADER;
    if flags & HAS_ID_LIST != 0 {
        let len = usize::from(u16_at(bytes, at)?);
        at = at.checked_add(2)?.checked_add(len)?;
    }
    let info = at;
    let header_size = u32_at(bytes, info.checked_add(4)?)?;
    let info_flags = u32_at(bytes, info.checked_add(8)?)?;
    if info_flags & VOLUME_AND_LOCAL_BASE == 0 {
        return None;
    }
    let base_off = u32_at(bytes, info.checked_add(16)?)?;
    let suffix_off = u32_at(bytes, info.checked_add(24)?)?;
    let (base, suffix) = if header_size >= 0x24 {
        let base_u = u32_at(bytes, info.checked_add(28)?)?;
        let suffix_u = u32_at(bytes, info.checked_add(32)?)?;
        let base = (base_u != 0)
            .then(|| utf16_z(bytes, info.checked_add(base_u)?))
            .flatten()
            .or_else(|| ansi_z(bytes, info.checked_add(base_off)?))?;
        let suffix = (suffix_u != 0)
            .then(|| utf16_z(bytes, info.checked_add(suffix_u)?))
            .flatten()
            .or_else(|| ansi_z(bytes, info.checked_add(suffix_off)?))
            .unwrap_or_default();
        (base, suffix)
    } else {
        let base = ansi_z(bytes, info.checked_add(base_off)?)?;
        let suffix = ansi_z(bytes, info.checked_add(suffix_off)?).unwrap_or_default();
        (base, suffix)
    };
    let mut path = base;
    if !suffix.is_empty() {
        if !path.ends_with('\\') {
            path.push('\\');
        }
        path.push_str(&suffix);
    }
    is_absolute(&path).then_some(path)
}

/// `haystack` contains `needle` as ASCII or UTF-16LE text, ignoring ASCII case (how
/// `.lnk` files store their target and working directory).
pub(crate) fn bytes_mention(haystack: &[u8], needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let ascii = needle.as_bytes();
    let wide: Vec<u8> = needle.encode_utf16().flat_map(u16::to_le_bytes).collect();
    haystack
        .windows(ascii.len())
        .any(|w| w.eq_ignore_ascii_case(ascii))
        || haystack
            .windows(wide.len())
            .any(|w| w.eq_ignore_ascii_case(&wide))
}

/// `path` points into a drive whose root exists (so a missing target is really gone, not
/// on an unplugged disk).
pub(crate) fn drive_root(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    match bytes {
        [d, b':', ..] if d.is_ascii_alphabetic() => Some(format!("{}:\\", char::from(*d))),
        _ => None,
    }
}

/// Convenience for callers holding a [`Path`].
pub(crate) fn path_str(path: &Path) -> String {
    path.display().to_string()
}

/// Wraps a parse failure.
pub(crate) fn parse_error(what: &str, message: impl Into<String>) -> Error {
    Error::Parse {
        what: what.to_owned(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_paths_parse_short_and_long_hives() {
        assert_eq!(
            parse_reg_path("HKEY_LOCAL_MACHINE\\SOFTWARE\\X"),
            Some((Hive::LocalMachine, "SOFTWARE\\X")),
            "long hive name"
        );
        assert_eq!(
            parse_reg_path("hkcu\\Software\\"),
            Some((Hive::CurrentUser, "Software")),
            "short, any case"
        );
        assert_eq!(parse_reg_path("HKXX\\a"), None, "unknown hive");
        assert_eq!(split_key("a\\b\\c"), ("a\\b", "c"), "parent and child");
        assert_eq!(split_key("a"), ("", "a"), "top-level key");
    }

    #[test]
    fn commands_split_quoted_unquoted_and_rundll32() {
        let files = ["C:\\Program Files\\X Y\\uninst.exe"];
        let is_file = |p: &str| files.iter().any(|f| f.eq_ignore_ascii_case(p));
        assert_eq!(
            split_command("\"C:\\Program Files\\X\\u.exe\" /S", is_file),
            Some(CommandLine {
                exe: "C:\\Program Files\\X\\u.exe".into(),
                args: "/S".into()
            }),
            "quoted program"
        );
        assert_eq!(
            split_command("C:\\Program Files\\X Y\\uninst.exe /S /D=1", is_file).map(|c| c.exe),
            Some(files[0].to_owned()),
            "longest existing prefix"
        );
        assert_eq!(
            split_command("C:\\Program Files\\Gone App\\un.exe /x", |_| false)
                .map(|c| (c.exe, c.args)),
            Some(("C:\\Program Files\\Gone App\\un.exe".into(), "/x".into())),
            "missing program: first .exe"
        );
        assert_eq!(
            command_target("rundll32.exe \"C:\\X\\a.dll\",Uninstall", |_| false),
            Some("C:\\X\\a.dll".into()),
            "rundll32 target is the dll"
        );
        assert_eq!(
            command_target(
                "MsiExec.exe /X{11111111-2222-3333-4444-555555555555}",
                |_| false
            ),
            None,
            "msiexec has no checkable target"
        );
    }

    #[test]
    fn env_and_paths() {
        let lookup = |n: &str| {
            (n.eq_ignore_ascii_case("ProgramFiles")).then(|| "C:\\Program Files".to_owned())
        };
        assert_eq!(
            expand_env("%ProgramFiles%\\A\\%X%\\100%", lookup),
            "C:\\Program Files\\A\\%X%\\100%",
            "expansion"
        );
        assert_eq!(
            clean_path("\"C:\\A\\\" "),
            Some("C:\\A".into()),
            "quotes and trailing slash"
        );
        assert_eq!(clean_path("C:\\"), Some("C:\\".into()), "drive root kept");
        assert_eq!(
            icon_path("\"C:\\A\\a.exe\",0"),
            Some("C:\\A\\a.exe".into()),
            "quoted icon"
        );
        assert_eq!(
            icon_path("C:\\A\\a.exe,-101"),
            Some("C:\\A\\a.exe".into()),
            "icon index"
        );
        assert!(path_within("C:\\A\\b", "c:\\a\\"), "inside");
        assert!(!path_within("C:\\AB", "C:\\A"), "sibling with same prefix");
        let needle = dir_needle("C:\\Program Files\\Foo");
        assert!(
            mentions_dir("\"C:\\Program Files\\Foo\\foo.exe\" %1", &needle),
            "command inside dir"
        );
        assert!(
            !mentions_dir("C:\\Program Files\\FooBar\\x.exe", &needle),
            "prefix is not containment"
        );
        assert_eq!(
            service_image(
                "\\SystemRoot\\System32\\drivers\\x.sys",
                "C:\\Windows",
                |_| false
            ),
            Some("C:\\Windows\\System32\\drivers\\x.sys".into()),
            "SystemRoot form"
        );
    }

    #[test]
    fn guids_pack_like_windows_installer() {
        let guid = "{12345678-ABCD-EF01-2345-6789ABCDEF01}";
        assert!(is_guid(guid), "valid");
        assert_eq!(
            find_guid("MsiExec.exe /I{12345678-abcd-EF01-2345-6789ABCDEF01}"),
            Some(guid.to_owned()),
            "found"
        );
        assert_eq!(
            packed_guid(guid),
            Some("87654321DCBA10FE32547698BADCFE10".into()),
            "packed"
        );
        assert_eq!(packed_guid("nope"), None, "invalid");
    }

    #[test]
    fn names_normalize() {
        assert_eq!(
            normalize_name("7-Zip 23.01 (x64)"),
            "7zip",
            "version and bitness"
        );
        assert_eq!(normalize_name("Python 3.12.1 (64-bit)"), "python", "python");
        assert_eq!(normalize_name("Notepad++"), "notepad", "punctuation");
        assert_eq!(
            normalize_publisher("Microsoft Corporation"),
            "microsoft",
            "legal suffix"
        );
        assert_eq!(
            normalize_publisher("JetBrains s.r.o."),
            "jetbrains",
            "dotted suffix"
        );
        let keys = product_keys(
            "Mozilla Firefox (x64 en-US)",
            Some("Mozilla"),
            Some("C:\\Program Files\\Mozilla Firefox"),
        );
        assert_eq!(
            keys,
            vec!["firefox".to_owned(), "mozillafirefox".to_owned()],
            "keys: {keys:?}"
        );
        assert!(is_update_name("KB5031356"), "update");
        assert!(!is_update_name("KeePass"), "not an update");
        assert!(
            is_system_app(
                "Microsoft Visual C++ 2015-2022 Redistributable (x64)",
                Some("Microsoft Corporation")
            ),
            "vc++"
        );
        assert!(
            !is_system_app(
                "Microsoft Visual Studio Code",
                Some("Microsoft Corporation")
            ),
            "vscode is an app"
        );
        assert!(
            !is_system_app("Microsoft Edge", Some("Evil Inc.")),
            "publisher must be Microsoft"
        );
    }

    #[test]
    fn dates_and_filetime() {
        assert_eq!(parse_install_date("19700101"), Some(0), "epoch");
        assert_eq!(
            parse_install_date("20240301"),
            Some(1_709_251_200),
            "after a leap day"
        );
        assert_eq!(parse_install_date("2024131"), None, "malformed");
        assert_eq!(filetime(0), 116_444_736_000_000_000, "epoch in FILETIME");
    }

    #[test]
    fn startup_approved_round_trips() {
        assert!(approved_enabled(None), "missing = enabled");
        assert!(approved_enabled(Some(&[6, 0])), "06 = enabled");
        assert!(!approved_enabled(Some(&[3, 0])), "03 = disabled");
        let off = approved_value(false, 0x0102_0304_0506_0708);
        assert_eq!(
            off,
            [3, 0, 0, 0, 8, 7, 6, 5, 4, 3, 2, 1],
            "disabled with FILETIME"
        );
        assert!(
            approved_enabled(Some(&approved_value(true, 5))),
            "enabled value"
        );
    }

    #[test]
    fn system_strings() {
        assert_eq!(
            os_display("Windows 10 Pro", Some("24H2"), Some("26100")),
            "Windows 11 Pro 24H2 (26100)",
            "Windows 11 fix"
        );
        assert_eq!(
            os_display("Windows 10 Home", None, Some("19045")),
            "Windows 10 Home (19045)",
            "Windows 10"
        );
        assert!(
            whoami_elevated("Mandatory Label\\High Mandatory Level Label S-1-16-12288"),
            "high"
        );
        assert!(
            !whoami_elevated("Mandatory Label\\Medium Mandatory Level Label S-1-16-8192"),
            "medium"
        );
        assert!(is_uninstaller_process("Au_.exe", ""), "nsis");
        assert!(is_uninstaller_process("_iu14D2N.tmp", ""), "inno");
        assert!(
            !is_uninstaller_process("chrome.exe", "setup.exe"),
            "unrelated"
        );
        assert_eq!(
            exit_outcome(Some(3010), ""),
            UninstallerOutcome::Succeeded,
            "reboot required"
        );
        assert_eq!(
            exit_outcome(Some(1602), ""),
            UninstallerOutcome::Cancelled,
            "msi cancel"
        );
        assert_eq!(ps_quote("it's"), "'it''s'", "quote");
    }

    #[test]
    fn json_lists_accept_object_or_array() {
        let one: Result<Vec<LogicalDisk>> = parse_json_list(
            "{\"DeviceID\":\"C:\",\"VolumeName\":\"OS\",\"Size\":100,\"FreeSpace\":40}",
        );
        assert!(
            one.is_ok_and(|v| v.len() == 1 && v.first().is_some_and(|d| d.size == Some(100))),
            "object"
        );
        let many: Result<Vec<TaskInfo>> = parse_json_list(
            "[{\"Path\":\"\\\\V\\\\Up\",\"State\":\"Disabled\",\"UserId\":null,\"Exec\":\"C:\\\\a.exe\",\"Triggers\":[\"MSFT_TaskLogonTrigger\"]},{\"Path\":\"\\\\Microsoft\\\\X\",\"Exec\":[],\"Triggers\":null}]",
        );
        assert!(
            many.is_ok_and(|v| v.len() == 2
                && v.first().is_some_and(|t| t.at_startup()
                    && !t.enabled()
                    && t.exec.len() == 1
                    && t.name() == "Up")
                && v.get(1)
                    .is_some_and(|t| t.is_microsoft() && t.exec.is_empty())),
            "array with one-or-many fields"
        );
        let empty: Result<Vec<AppxPackage>> = parse_json_list("  ");
        assert!(empty.is_ok_and(|v| v.is_empty()), "no output");
    }

    #[test]
    fn manifest_and_dn() {
        let xml = "<Package><Properties><DisplayName>Foo &amp; Bar</DisplayName><PublisherDisplayName>ms-resource:x</PublisherDisplayName></Properties></Package>";
        assert_eq!(
            manifest_display_name(xml),
            Some("Foo & Bar".into()),
            "display name"
        );
        assert_eq!(
            manifest_publisher(xml),
            None,
            "resource strings are ignored"
        );
        assert_eq!(
            dn_name("CN=Mozilla Corporation, O=Mozilla, C=US"),
            Some("Mozilla".into()),
            "O wins"
        );
    }

    fn lnk(base: &[u8], unicode: bool) -> Vec<u8> {
        let mut out = vec![0_u8; 0x4C];
        out[0..4].copy_from_slice(&0x4C_u32.to_le_bytes());
        out[20..24].copy_from_slice(&0x3_u32.to_le_bytes());
        out.extend(4_u16.to_le_bytes());
        out.extend([0xAA, 0xBB, 0, 0]);
        let header: u32 = if unicode { 0x24 } else { 0x1C };
        let base_off = header;
        let base_len = u32::try_from(base.len()).unwrap_or(0);
        let suffix_off = base_off.saturating_add(base_len).saturating_add(1);
        let mut info = Vec::new();
        info.extend(0_u32.to_le_bytes());
        info.extend(header.to_le_bytes());
        info.extend(1_u32.to_le_bytes());
        info.extend(0_u32.to_le_bytes());
        info.extend(base_off.to_le_bytes());
        info.extend(0_u32.to_le_bytes());
        info.extend(suffix_off.to_le_bytes());
        if unicode {
            let wide: Vec<u8> = "C:\\Wide\\w.exe"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect();
            let wide_off = suffix_off.saturating_add(1);
            info.extend(wide_off.to_le_bytes());
            info.extend(0_u32.to_le_bytes());
            info.extend(base);
            info.push(0);
            info.push(0);
            info.extend(wide);
            info.extend([0, 0]);
        } else {
            info.extend(base);
            info.push(0);
            info.push(0);
        }
        out.extend(info);
        out
    }

    #[test]
    fn lnk_targets_parse() {
        assert_eq!(
            lnk_target(&lnk(b"C:\\App\\a.exe", false)),
            Some("C:\\App\\a.exe".into()),
            "ansi path"
        );
        assert_eq!(
            lnk_target(&lnk(b"C:\\App\\a.exe", true)),
            Some("C:\\Wide\\w.exe".into()),
            "unicode wins"
        );
        assert_eq!(lnk_target(b"garbage"), None, "not a link");
        let bytes = lnk(b"C:\\Program Files\\Foo\\a.exe", false);
        assert!(
            bytes_mention(&bytes, "c:\\program files\\foo\\"),
            "ascii mention"
        );
        let wide: Vec<u8> = "C:\\X\\Y"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert!(bytes_mention(&wide, "c:\\x"), "utf-16 mention");
        assert_eq!(drive_root("d:\\x"), Some("d:\\".into()), "drive root");
    }
}
