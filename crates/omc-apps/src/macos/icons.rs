//! App icons: the best PNG embedded in the bundle's `.icns`, cached as a PNG file the UI
//! can load. `.icns` is a sequence of records (4-byte type, 4-byte big-endian length that
//! includes the 8-byte header); modern sizes store PNG data directly.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

/// PNG signature.
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

/// Icon types by preference: 256 px, 128 px, then larger, then smaller.
const PREFERENCE: &[&[u8; 4]] = &[
    b"ic08", // 256
    b"ic13", // 128@2x = 256
    b"ic07", // 128
    b"ic12", // 64@2x = 128
    b"ic14", // 256@2x = 512
    b"ic09", // 512
    b"ic10", // 512@2x = 1024
    b"ic11", // 32@2x = 64
];

/// The preferred PNG image inside an `.icns` file.
pub(super) fn best_png(data: &[u8]) -> Option<&[u8]> {
    let (magic, body) = data.split_at_checked(8)?;
    if magic.get(..4)? != b"icns" {
        return None;
    }
    let mut rest = body;
    let mut best: Option<(usize, &[u8])> = None;
    while let Some((header, after)) = rest.split_at_checked(8) {
        let Some((kind, len)) = header.split_at_checked(4) else {
            break;
        };
        let Some(len) = len
            .try_into()
            .ok()
            .map(u32::from_be_bytes)
            .and_then(|l| usize::try_from(l).ok())
        else {
            break;
        };
        // Malformed record: keep what was found so far.
        let Some((payload, next)) = len
            .checked_sub(8)
            .and_then(|payload_len| after.split_at_checked(payload_len))
        else {
            break;
        };
        if payload.starts_with(PNG_MAGIC)
            && let Some(rank) = PREFERENCE.iter().position(|p| p.as_slice() == kind)
            && best.is_none_or(|(r, _)| rank < r)
        {
            best = Some((rank, payload));
        }
        rest = next;
    }
    best.map(|(_, png)| png)
}

/// The `.icns` a bundle names in `CFBundleIconFile` (extension optional).
pub(super) fn icns_path(bundle: &Path, icon: Option<&str>) -> Option<PathBuf> {
    let resources = bundle.join("Contents").join("Resources");
    let name = icon?;
    let file = if Path::new(name).extension().is_some() {
        resources.join(name)
    } else {
        resources.join(format!("{name}.icns"))
    };
    file.is_file().then_some(file)
}

/// Icon cache folder: `~/Library/Caches/dev.zerx.oh-my-clear/icons`.
pub(super) fn cache_dir() -> Option<PathBuf> {
    Some(
        omc_scan::paths::home()?
            .join("Library")
            .join("Caches")
            .join("dev.zerx.oh-my-clear")
            .join("icons"),
    )
}

/// Cache file name for an app: its bundle id (sanitised) or a hash of its path.
fn cache_name(bundle: &Path, id: Option<&str>) -> String {
    match id {
        Some(id) if !id.is_empty() => {
            let clean: String = id
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            format!("{clean}.png")
        }
        _ => {
            let mut h = DefaultHasher::new();
            bundle.hash(&mut h);
            format!("{:016x}.png", h.finish())
        }
    }
}

/// The cached PNG icon of `bundle`, extracting it when missing or older than the `.icns`.
pub(super) fn icon_for(
    cache: &Path,
    bundle: &Path,
    id: Option<&str>,
    icon: Option<&str>,
) -> Option<String> {
    let icns = icns_path(bundle, icon)?;
    let out = cache.join(cache_name(bundle, id));
    let source_time = std::fs::metadata(&icns).and_then(|m| m.modified()).ok();
    let cached_time = std::fs::metadata(&out).and_then(|m| m.modified()).ok();
    if let (Some(src), Some(cached)) = (source_time, cached_time)
        && cached >= src
    {
        return Some(out.display().to_string());
    }
    let data = match std::fs::read(&icns) {
        Ok(d) => d,
        Err(err) => {
            tracing::debug!(%err, path = %icns.display(), "unreadable icns");
            return None;
        }
    };
    let png = best_png(&data)?;
    // Write then rename, so a concurrent reader never sees half a file.
    let tmp = out.with_extension("png.tmp");
    if let Err(err) = std::fs::write(&tmp, png).and_then(|()| std::fs::rename(&tmp, &out)) {
        tracing::debug!(%err, path = %out.display(), "icon cache write failed");
        return None;
    }
    Some(out.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = kind.to_vec();
        let len = u32::try_from(payload.len().saturating_add(8)).unwrap_or(u32::MAX);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    fn icns(records: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = records.concat();
        let mut out = b"icns".to_vec();
        let len = u32::try_from(body.len().saturating_add(8)).unwrap_or(u32::MAX);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&body);
        out
    }

    fn png(tag: u8) -> Vec<u8> {
        let mut p = PNG_MAGIC.to_vec();
        p.push(tag);
        p
    }

    #[test]
    fn prefers_256_px_png() {
        let data = icns(&[
            record(*b"TOC ", b"xxxx"),
            record(*b"ic10", &png(1)),
            record(*b"ic07", &png(2)),
            record(*b"ic08", &png(3)),
        ]);
        assert_eq!(best_png(&data), Some(png(3).as_slice()), "ic08 wins");
    }

    #[test]
    fn skips_non_png_chunks_and_falls_back() {
        let data = icns(&[
            record(*b"ic08", b"\0\0\0\x0cjP  JPEG2000"),
            record(*b"ic09", &png(9)),
        ]);
        assert_eq!(
            best_png(&data),
            Some(png(9).as_slice()),
            "JPEG 2000 is skipped"
        );
    }

    #[test]
    fn rejects_malformed_containers() {
        assert_eq!(best_png(b"nope"), None, "too short");
        assert_eq!(best_png(b"icnx\0\0\0\x08"), None, "wrong magic");
        let mut bad = icns(&[record(*b"ic08", &png(1))]);
        // Claim a record longer than the file.
        if let Some(b) = bad.get_mut(12) {
            *b = 0xff;
        }
        assert_eq!(best_png(&bad), None, "truncated record");
        let tiny = icns(&[b"ic08\0\0\0\x04".to_vec()]);
        assert_eq!(best_png(&tiny), None, "length below header size");
    }

    #[test]
    fn cache_names_are_safe() {
        let p = Path::new("/Applications/A.app");
        assert_eq!(
            cache_name(p, Some("com.foo/bar")),
            "com.foo_bar.png",
            "sanitised"
        );
        assert!(
            Path::new(&cache_name(p, None))
                .extension()
                .is_some_and(|e| e == "png"),
            "hash fallback is a png name"
        );
    }
}
