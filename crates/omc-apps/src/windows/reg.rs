//! Thin helpers over `windows_registry`: opening keys by [`Hive`], reading values
//! leniently, mapping errors, and removing keys/values with an optional `.reg` backup.

use std::io;
use std::path::Path;
use std::time::Duration;

use omc_proto::jobs::Location;
use windows_registry::{CLASSES_ROOT, CURRENT_USER, Key, LOCAL_MACHINE, Type, USERS};

use super::parse::{self, Hive};
use crate::{Error, Result, cmd};

/// `DELETE` access right (needed to delete a subtree through its parent handle).
const DELETE: u32 = 0x0001_0000;

/// `HRESULT_FROM_WIN32(code)`.
const fn hresult(code: u32) -> i32 {
    i32::from_ne_bytes((0x8007_0000 | (code & 0xFFFF)).to_ne_bytes())
}

const HR_FILE_NOT_FOUND: i32 = hresult(2);
const HR_PATH_NOT_FOUND: i32 = hresult(3);
const HR_ACCESS_DENIED: i32 = hresult(5);

/// How a registry call failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegFailure {
    /// Key or value does not exist.
    Missing,
    /// Needs administrator rights.
    Denied,
    /// Anything else (`HRESULT`).
    Other(i32),
}

/// Classifies a `windows_registry` error.
pub(crate) fn failure<T>(result: &windows_registry::Result<T>) -> Option<RegFailure> {
    let err = result.as_ref().err()?;
    Some(match err.code().0 {
        HR_FILE_NOT_FOUND | HR_PATH_NOT_FOUND => RegFailure::Missing,
        HR_ACCESS_DENIED => RegFailure::Denied,
        other => RegFailure::Other(other),
    })
}

impl Hive {
    /// The predefined root key.
    pub(crate) fn root(self) -> &'static Key {
        match self {
            Self::CurrentUser => CURRENT_USER,
            Self::LocalMachine => LOCAL_MACHINE,
            Self::ClassesRoot => CLASSES_ROOT,
            Self::Users => USERS,
        }
    }
}

/// Opens `hive\path` for reading.
pub(crate) fn open(hive: Hive, path: &str) -> Option<Key> {
    hive.root().open(path).ok()
}

/// `hive\path` exists.
pub(crate) fn exists(hive: Hive, path: &str) -> bool {
    open(hive, path).is_some()
}

/// Non-empty trimmed string value (`REG_SZ` or unexpanded `REG_EXPAND_SZ`).
pub(crate) fn string(key: &Key, name: &str) -> Option<String> {
    let value = key.get_string(name).ok()?;
    let value = value.trim_matches(char::from(0)).trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// `REG_DWORD` value; also accepts a decimal string.
pub(crate) fn dword(key: &Key, name: &str) -> Option<u32> {
    key.get_u32(name)
        .ok()
        .or_else(|| string(key, name).and_then(|s| s.parse().ok()))
}

/// Raw bytes of a value.
pub(crate) fn bytes(key: &Key, name: &str) -> Option<Vec<u8>> {
    key.get_value(name).ok().map(|v| v.to_vec())
}

/// Names of the subkeys (empty when unreadable).
pub(crate) fn subkeys(key: &Key) -> Vec<String> {
    key.keys().map(Iterator::collect).unwrap_or_default()
}

/// String values of a key: `(name, data)`.
pub(crate) fn string_values(key: &Key) -> Vec<(String, String)> {
    let Ok(values) = key.values() else {
        return Vec::new();
    };
    values
        .filter(|(_, v)| matches!(v.ty(), Type::String | Type::ExpandString))
        .filter_map(|(name, value)| Some((name, String::try_from(value).ok()?)))
        .map(|(name, data)| (name, data.trim_matches(char::from(0)).trim().to_owned()))
        .collect()
}

/// Value names of a key (empty when unreadable).
pub(crate) fn value_names(key: &Key) -> Vec<String> {
    key.values()
        .map(|values| values.map(|(name, _)| name).collect())
        .unwrap_or_default()
}

/// `REG_DWORD` values of a key: `(name, count)`; other types give `None`.
pub(crate) fn dword_values(key: &Key) -> Vec<(String, Option<u32>)> {
    let Ok(values) = key.values() else {
        return Vec::new();
    };
    values
        .map(|(name, value)| {
            let count = matches!(value.ty(), Type::U32).then(|| u32::try_from(value).ok());
            (name, count.flatten())
        })
        .collect()
}

/// Value `name` of `hive\path` exists.
pub(crate) fn value_exists(hive: Hive, path: &str, name: &str) -> bool {
    open(hive, path).is_some_and(|key| key.get_value(name).is_ok())
}

/// Expands `%VAR%` from the process environment.
pub(crate) fn expand(text: &str) -> String {
    parse::expand_env(text, |name| std::env::var(name).ok())
}

/// `Ok(Some)` on success, `Ok(None)` when the key/value is missing, else the mapped
/// I/O error.
fn checked<T>(result: windows_registry::Result<T>) -> Result<Option<T>> {
    let kind = failure(&result);
    match result {
        Ok(value) => Ok(Some(value)),
        Err(_) if kind == Some(RegFailure::Missing) => Ok(None),
        Err(err) => Err(io_error(kind, &err.message())),
    }
}

/// Writes a `REG_BINARY` value, creating the key.
pub(crate) fn set_binary(hive: Hive, path: &str, name: &str, data: &[u8]) -> Result<()> {
    let key = checked(hive.root().create(path))?
        .ok_or_else(|| io_error(Some(RegFailure::Missing), path))?;
    checked(key.set_bytes(name, Type::Bytes, data))?;
    Ok(())
}

/// Deletes one value; a missing key or value is success.
pub(crate) fn delete_value(hive: Hive, path: &str, name: &str) -> Result<()> {
    let Some(key) = checked(hive.root().options().read().write().open(path))? else {
        return Ok(());
    };
    checked(key.remove_value(name))?;
    Ok(())
}

/// Deletes a key and its subtree; a missing key is success.
pub(crate) fn delete_tree(hive: Hive, path: &str) -> Result<()> {
    let (parent, child) = parse::split_key(path);
    if child.is_empty() {
        return Err(parse::parse_error(
            "registry key",
            format!("refusing to delete {}", hive.full(path)),
        ));
    }
    let Some(key) = checked(
        hive.root()
            .options()
            .read()
            .write()
            .access(DELETE)
            .open(parent),
    )?
    else {
        return Ok(());
    };
    checked(key.remove_tree(child))?;
    Ok(())
}

/// Maps a registry failure to the I/O error the executor classifies.
pub(crate) fn io_error(failure: Option<RegFailure>, message: &str) -> Error {
    match failure {
        Some(RegFailure::Denied) => Error::Io(io::Error::from_raw_os_error(5)),
        Some(RegFailure::Missing) => {
            Error::Io(io::Error::new(io::ErrorKind::NotFound, message.to_owned()))
        }
        _ => Error::Io(io::Error::other(message.to_owned())),
    }
}

/// Top-level registry keys of a system hive a removal must never touch.
fn is_protected(hive: Hive, path: &str) -> bool {
    let lower = path.trim_matches('\\').to_ascii_lowercase();
    let depth = lower.split('\\').filter(|c| !c.is_empty()).count();
    if depth < 2 && hive != Hive::ClassesRoot {
        return true;
    }
    matches!(
        lower.as_str(),
        "software\\microsoft"
            | "software\\classes"
            | "software\\policies"
            | "software\\wow6432node"
            | "software\\wow6432node\\microsoft"
            | "software\\microsoft\\windows"
            | "software\\microsoft\\windows\\currentversion"
            | "software\\microsoft\\windows\\currentversion\\uninstall"
            | "software\\microsoft\\windows\\currentversion\\run"
            | "system\\currentcontrolset"
            | "system\\currentcontrolset\\services"
    ) || (hive == Hive::ClassesRoot && matches!(lower.as_str(), "" | "clsid" | "installer"))
}

/// Removes a [`Location::RegistryKey`] or [`Location::RegistryValue`]; exports the key to
/// `backup_dir` first when given (and fails without deleting when that export fails).
/// Already missing = `Ok`; access denied = `Io(raw os error 5)`.
pub(crate) fn remove(location: &Location, backup_dir: Option<&Path>) -> Result<()> {
    let (full, value) = match location {
        Location::RegistryKey { key } => (key.as_str(), None),
        Location::RegistryValue { key, name } => (key.as_str(), Some(name.as_str())),
        other => {
            return Err(Error::Unsupported(format!(
                "not a registry location: {}",
                other.display()
            )));
        }
    };
    let (hive, path) =
        parse::parse_reg_path(full).ok_or_else(|| parse::parse_error("registry key", full))?;
    if value.is_none() && is_protected(hive, path) {
        return Err(parse::parse_error(
            "registry key",
            format!("refusing to delete {full}"),
        ));
    }
    if !exists(hive, path) {
        return Ok(());
    }
    if let Some(dir) = backup_dir {
        backup(hive, path, dir)?;
    }
    match value {
        Some(name) => delete_value(hive, path, name),
        None => delete_tree(hive, path),
    }
}

/// `reg export <key> <dir>\<name>.reg /y`.
fn backup(hive: Hive, path: &str, dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let full = hive.full(path);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let file = dir.join(parse::backup_file_name(&full, stamp));
    let file = file.display().to_string();
    let out = cmd::run(
        "reg",
        &["export", &full, &file, "/y"],
        Duration::from_secs(120),
    )?;
    out.ok("reg export").map(|_| ()).map_err(|err| {
        tracing::warn!(%err, key = %full, "registry backup failed; not deleting");
        err
    })
}
