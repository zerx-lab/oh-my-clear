//! Well-known locations and path helpers, from the environment only (no `unsafe` OS calls).

use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The current user's home folder.
pub fn home() -> Option<PathBuf> {
    #[cfg(windows)]
    let var = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let var = std::env::var_os("HOME");
    var.filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// An environment variable as a path, when set and non-empty.
pub fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Expands a leading `~` (the home folder) in a user-entered path.
pub fn expand(input: &str) -> PathBuf {
    let trimmed = input.trim();
    if let Some(rest) = trimmed
        .strip_prefix("~/")
        .or_else(|| trimmed.strip_prefix("~\\"))
        && let Some(home) = home()
    {
        return home.join(rest);
    }
    if trimmed == "~"
        && let Some(home) = home()
    {
        return home;
    }
    PathBuf::from(trimmed)
}

/// `path` without `.` components and trailing separators; `None` when relative or when it
/// contains `..` (never resolved lexically: a symlink could make that wrong).
pub fn normalize(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => return None,
            other => out.push(other),
        }
    }
    Some(out)
}

/// Unix seconds of `time` (negative before 1970).
pub fn unix_secs(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_secs()).unwrap_or(i64::MAX),
        Err(before) => {
            i64::try_from(before.duration().as_secs()).map_or(i64::MIN, i64::saturating_neg)
        }
    }
}

/// Unix seconds now.
pub fn now_secs() -> i64 {
    unix_secs(SystemTime::now())
}

/// Case-insensitive ASCII comparison of two paths (Windows and default macOS volumes are
/// case-insensitive).
pub fn eq_ignore_case(a: &Path, b: &Path) -> bool {
    a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
}

/// `path` is `base` or below it, compared per component; case-insensitive on macOS and
/// Windows.
pub fn is_within(path: &Path, base: &Path) -> bool {
    let mut path = path.components();
    for want in base.components() {
        match path.next() {
            Some(got) if component_eq(got, want) => {}
            _ => return false,
        }
    }
    true
}

fn component_eq(a: Component<'_>, b: Component<'_>) -> bool {
    if cfg!(any(windows, target_os = "macos")) {
        a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
    } else {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_rejects_relative_and_parent_components() {
        let root = if cfg!(windows) { "C:\\" } else { "/" };
        let abs = PathBuf::from(root).join("a").join(".").join("b");
        assert_eq!(
            normalize(&abs),
            Some(PathBuf::from(root).join("a").join("b")),
            "`.` is dropped"
        );
        assert_eq!(normalize(Path::new("a/b")), None, "relative is rejected");
        let up = PathBuf::from(root).join("a").join("..").join("b");
        assert_eq!(normalize(&up), None, "`..` is rejected");
    }

    #[test]
    fn within_compares_whole_components() {
        let base = Path::new("/Users/me/Library");
        assert!(
            is_within(Path::new("/Users/me/Library/Caches"), base),
            "child"
        );
        assert!(is_within(base, base), "itself");
        assert!(
            !is_within(Path::new("/Users/me/LibraryX"), base),
            "prefix of a name is not containment"
        );
        assert!(!is_within(Path::new("/Users"), base), "ancestor");
    }

    #[test]
    fn unix_secs_handles_both_sides_of_the_epoch() {
        let later = UNIX_EPOCH.checked_add(std::time::Duration::from_secs(5));
        assert_eq!(later.map(unix_secs), Some(5), "after the epoch");
        let earlier = UNIX_EPOCH.checked_sub(std::time::Duration::from_secs(5));
        assert_eq!(earlier.map(unix_secs), Some(-5), "before the epoch");
    }
}
