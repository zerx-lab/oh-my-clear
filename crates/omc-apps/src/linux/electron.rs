//! Electron apps keep their data in `~/.config/<productName>` (`app.getPath("userData")`),
//! which rarely matches the package or binary name (`code` → `Code`, `1password` →
//! `1Password`). The names come from the app's `package.json`, read from
//! `resources/app.asar` (or an unpacked `resources/app/`) next to its binary or in its
//! package's file list.

use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Largest `app.asar` header read (headers of big apps are a few MiB).
const MAX_HEADER: u32 = 32 * 1024 * 1024;
/// Largest `package.json` read.
const MAX_PACKAGE_JSON: u64 = 1024 * 1024;

/// Names of an Electron app from its `package.json`.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ElectronNames {
    /// `productName`: the `userData` folder name when set.
    pub(super) product: Option<String>,
    /// `name`: the `userData` folder name without `productName`.
    pub(super) name: Option<String>,
    /// `desktopName` without `.desktop` (electron-builder's window class).
    pub(super) desktop: Option<String>,
}

/// Names of a `package.json` document; empty when it is not JSON.
pub(super) fn parse_package_json(text: &str) -> ElectronNames {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return ElectronNames::default();
    };
    let get = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    ElectronNames {
        product: get("productName"),
        name: get("name"),
        desktop: get("desktopName").map(|d| {
            d.strip_suffix(".desktop")
                .map_or_else(|| d.clone(), str::to_owned)
        }),
    }
}

/// Reads a little-endian `u32` at `at`.
fn le_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    let raw: [u8; 4] = bytes.get(at..end)?.try_into().ok()?;
    Some(u32::from_le_bytes(raw))
}

/// Layout of an asar archive from its first 16 bytes: (JSON header length, offset where
/// file data starts). The file starts with two Chromium pickles: `[4][header_size]` and
/// `[payload_size][json_len][json…]`; file offsets count from `8 + header_size`.
pub(super) fn asar_layout(prefix: &[u8]) -> Option<(u32, u64)> {
    if le_u32(prefix, 0)? != 4 {
        return None;
    }
    let header_size = le_u32(prefix, 4)?;
    let json_len = le_u32(prefix, 12)?;
    if json_len == 0 || json_len > MAX_HEADER || json_len > header_size.checked_sub(8)? {
        return None;
    }
    Some((json_len, u64::from(header_size).checked_add(8)?))
}

/// A file inside an asar archive.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct AsarFile {
    /// Offset from the start of the file data.
    pub(super) offset: u64,
    /// Length in bytes.
    pub(super) size: u64,
    /// Stored in `app.asar.unpacked/` instead.
    pub(super) unpacked: bool,
}

/// Looks `path` up in an asar JSON header (`{"files":{"a":{"files":{…}}}}`).
pub(super) fn asar_entry(header: &str, path: &[&str]) -> Option<AsarFile> {
    let root: Value = serde_json::from_str(header).ok()?;
    let mut node = &root;
    for part in path {
        node = node.get("files")?.get(*part)?;
    }
    let unpacked = node.get("unpacked").and_then(Value::as_bool) == Some(true);
    let size = node.get("size").and_then(Value::as_u64)?;
    let offset = match node.get("offset") {
        Some(Value::String(s)) => s.parse().ok()?,
        Some(v) => v.as_u64()?,
        None if unpacked => 0,
        None => return None,
    };
    Some(AsarFile {
        offset,
        size,
        unpacked,
    })
}

/// `package.json` of an `app.asar` archive.
fn read_asar_package(asar: &Path) -> Option<String> {
    let mut file = std::fs::File::open(asar).ok()?;
    let mut prefix = [0_u8; 16];
    file.read_exact(&mut prefix).ok()?;
    let (json_len, data_start) = asar_layout(&prefix)?;
    let mut header = vec![0_u8; usize::try_from(json_len).ok()?];
    file.read_exact(&mut header).ok()?;
    let header = String::from_utf8(header).ok()?;
    let entry = asar_entry(&header, &["package.json"])?;
    if entry.size > MAX_PACKAGE_JSON {
        return None;
    }
    if entry.unpacked {
        let mut unpacked = asar.as_os_str().to_owned();
        unpacked.push(".unpacked");
        return std::fs::read_to_string(Path::new(&unpacked).join("package.json")).ok();
    }
    file.seek(SeekFrom::Start(data_start.checked_add(entry.offset)?))
        .ok()?;
    let mut text = vec![0_u8; usize::try_from(entry.size).ok()?];
    file.read_exact(&mut text).ok()?;
    String::from_utf8(text).ok()
}

/// Names from an `app.asar` or `app/package.json` path.
fn names_from(path: &Path) -> Option<ElectronNames> {
    let text = if path.extension().is_some_and(|e| e == "asar") {
        read_asar_package(path)?
    } else {
        let meta = std::fs::metadata(path).ok()?;
        if meta.len() > MAX_PACKAGE_JSON {
            return None;
        }
        std::fs::read_to_string(path).ok()?
    };
    let names = parse_package_json(&text);
    (names.product.is_some() || names.name.is_some()).then_some(names)
}

/// The app archive in a `resources/` folder next to `binary`.
fn beside(binary: &Path) -> Option<ElectronNames> {
    let resources = binary.parent()?.join("resources");
    names_from(&resources.join("app.asar"))
        .or_else(|| names_from(&resources.join("app/package.json")))
}

/// Whether `path` is an Electron app archive or unpacked `package.json`.
pub(super) fn is_app_archive(path: &Path) -> bool {
    path.ends_with("resources/app.asar") || path.ends_with("resources/app/package.json")
}

/// Names of the Electron app started by one of `binaries` (symlinks resolved), else the
/// first app archive in `package_files`.
pub(super) fn electron_names(
    binaries: &[PathBuf],
    package_files: impl FnOnce() -> Vec<PathBuf>,
) -> Option<ElectronNames> {
    binaries
        .iter()
        .flat_map(|b| [Some(b.clone()), std::fs::canonicalize(b).ok()])
        .flatten()
        .find_map(|b| beside(&b))
        .or_else(|| {
            package_files()
                .iter()
                .filter(|p| is_app_archive(p))
                .find_map(|p| names_from(p))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An asar archive holding `files` (name, content) at the root.
    fn asar(files: &[(&str, &str)]) -> Vec<u8> {
        let mut data = Vec::new();
        let mut entries = serde_json::Map::new();
        for (name, content) in files {
            entries.insert(
                (*name).to_owned(),
                serde_json::json!({ "size": content.len(), "offset": data.len().to_string() }),
            );
            data.extend_from_slice(content.as_bytes());
        }
        let json = serde_json::json!({ "files": entries }).to_string();
        let json_len = u32::try_from(json.len()).unwrap_or(u32::MAX);
        let padded = json.len().next_multiple_of(4);
        let header_size = u32::try_from(padded.saturating_add(8)).unwrap_or(u32::MAX);
        let mut out = Vec::new();
        out.extend_from_slice(&4_u32.to_le_bytes());
        out.extend_from_slice(&header_size.to_le_bytes());
        out.extend_from_slice(&header_size.saturating_sub(4).to_le_bytes());
        out.extend_from_slice(&json_len.to_le_bytes());
        out.extend_from_slice(json.as_bytes());
        out.resize(padded.saturating_add(16), 0);
        out.extend_from_slice(&data);
        out
    }

    #[test]
    fn package_json_names() {
        let names = parse_package_json(
            r#"{"name":"obsidian","productName":"Obsidian","desktopName":"obsidian.desktop"}"#,
        );
        assert_eq!(names.product.as_deref(), Some("Obsidian"), "productName");
        assert_eq!(names.name.as_deref(), Some("obsidian"), "name");
        assert_eq!(names.desktop.as_deref(), Some("obsidian"), "suffix dropped");
        assert_eq!(
            parse_package_json("not json"),
            ElectronNames::default(),
            "invalid JSON"
        );
        assert_eq!(
            parse_package_json(r#"{"productName":"  "}"#).product,
            None,
            "blank ignored"
        );
    }

    #[test]
    fn asar_layout_rejects_garbage() {
        assert_eq!(asar_layout(&[0; 16]), None, "zero header");
        assert_eq!(asar_layout(&[4, 0, 0]), None, "short prefix");
        let bytes = asar(&[("package.json", "{}")]);
        let layout = asar_layout(&bytes);
        assert!(
            layout.is_some_and(|(len, start)| len > 0 && start > u64::from(len)),
            "valid archive: {layout:?}"
        );
    }

    #[test]
    fn asar_entries_resolve_nested_and_unpacked() {
        let header = r#"{"files":{"package.json":{"size":12,"offset":"40"},"dist":{"files":{"a.js":{"size":3,"offset":"0"}}},"n.node":{"size":9,"unpacked":true}}}"#;
        assert_eq!(
            asar_entry(header, &["package.json"]),
            Some(AsarFile {
                offset: 40,
                size: 12,
                unpacked: false
            }),
            "string offset"
        );
        assert!(
            asar_entry(header, &["dist", "a.js"]).is_some(),
            "nested lookup"
        );
        assert!(
            asar_entry(header, &["n.node"]).is_some_and(|f| f.unpacked),
            "unpacked file"
        );
        assert_eq!(asar_entry(header, &["missing"]), None, "missing file");
    }

    #[test]
    fn reads_product_name_from_archive() {
        let dir = std::env::temp_dir().join(format!("omc-electron-{}", std::process::id()));
        let resources = dir.join("resources");
        let made = std::fs::create_dir_all(&resources).and_then(|()| {
            std::fs::write(
                resources.join("app.asar"),
                asar(&[
                    ("index.js", "console.log(1)"),
                    (
                        "package.json",
                        r#"{"name":"code-oss","productName":"Code - OSS"}"#,
                    ),
                ]),
            )
        });
        let names = electron_names(&[dir.join("code")], Vec::new);
        if let Err(err) = std::fs::remove_dir_all(&dir) {
            tracing::debug!(%err, "cleanup");
        }
        assert!(made.is_ok(), "fixture written: {made:?}");
        assert_eq!(
            names.and_then(|n| n.product).as_deref(),
            Some("Code - OSS"),
            "productName read from the archive"
        );
    }
}
