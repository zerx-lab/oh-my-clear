//! Display formatting shared by every page: sizes in the platform's convention (decimal on
//! macOS and Linux like Finder/GNOME Files, binary on Windows like Explorer), counts,
//! relative ages and home-relative paths.

use std::sync::LazyLock;

use gpui_kit::SharedString;

const DECIMAL: bool = !cfg!(windows);

/// `1.2 GB`, `834 MB`, `12 KB`, `0 bytes`-style size.
pub fn bytes(n: u64) -> SharedString {
    let unit: u64 = if DECIMAL { 1000 } else { 1024 };
    let names = ["KB", "MB", "GB", "TB", "PB"];
    if n < unit {
        return format!("{n} B").into();
    }
    let mut value = n;
    let mut scaled: usize = 0;
    // Integer tenths avoid float casts: value * 10 / unit^k.
    while value >= unit.saturating_mul(unit) && scaled.saturating_add(1) < names.len() {
        value = value.checked_div(unit).unwrap_or(0);
        scaled = scaled.saturating_add(1);
    }
    let tenths = value.saturating_mul(10).checked_div(unit).unwrap_or(0);
    let name = names.get(scaled).copied().unwrap_or("PB");
    let (whole, frac) = (tenths / 10, tenths % 10);
    if tenths >= 100 {
        format!("{whole} {name}").into()
    } else {
        format!("{whole}.{frac} {name}").into()
    }
}

/// `12,345`-style count.
pub fn count(n: u64) -> SharedString {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len().saturating_add(digits.len() / 3));
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len().saturating_sub(i)).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out.into()
}

/// Age of a Unix timestamp relative to `now` in whole days/months/years, localised through
/// the `time.*` keys; `None` for timestamps in the future.
pub fn age(then: i64, now: i64) -> Option<SharedString> {
    let secs = now.checked_sub(then)?;
    if secs < 0 {
        return None;
    }
    let days = secs / 86_400;
    let (months, years) = (days / 30, days / 365);
    let text = if days < 1 {
        rust_i18n::t!("time.today").to_string()
    } else if days < 31 {
        rust_i18n::t!("time.days", n = days).to_string()
    } else if days < 365 {
        rust_i18n::t!("time.months", n = months).to_string()
    } else {
        rust_i18n::t!("time.years", n = years).to_string()
    };
    Some(text.into())
}

/// Seconds since the Unix epoch now.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// The user's home folder without trailing separators, read once; `None` when unknown or a
/// filesystem root (a root "home" would turn every path into `~/…`).
static HOME: LazyLock<Option<String>> = LazyLock::new(|| {
    std::env::home_dir()
        .and_then(|home| home.to_str().map(str::to_owned))
        .and_then(|home| home_prefix(&home).map(str::to_owned))
});

/// Paths compare case-insensitively where the file system usually does (APFS/HFS+, NTFS).
const FOLD_CASE: bool = cfg!(any(target_os = "macos", windows));

/// `path` for display with the home folder shown as `~` (`~/Movies`, home itself `~`).
/// Only whole components match: with home `/Users/me`, `/Users/me2` stays as it is.
pub fn tilde(path: &str) -> SharedString {
    tilde_in(path, HOME.as_deref(), FOLD_CASE)
}

/// `home` usable as a prefix: trailing separators trimmed; `None` for roots (`/`, `C:\`).
fn home_prefix(home: &str) -> Option<&str> {
    Some(home.trim_end_matches(['/', '\\'])).filter(|h| h.contains(['/', '\\']))
}

fn tilde_in(path: &str, home: Option<&str>, fold_case: bool) -> SharedString {
    let rest = home.and_then(home_prefix).and_then(|home| {
        let (head, rest) = path.split_at_checked(home.len())?;
        let same = if fold_case {
            head.eq_ignore_ascii_case(home)
        } else {
            head == home
        };
        (same && (rest.is_empty() || rest.starts_with(['/', '\\']))).then_some(rest)
    });
    match rest {
        Some(rest) => format!("~{rest}").into(),
        None => SharedString::from(path.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_round_down_to_one_decimal() {
        assert_eq!(bytes(0).as_ref(), "0 B", "bytes");
        assert_eq!(bytes(999).as_ref(), "999 B", "below one unit");
        if DECIMAL {
            assert_eq!(bytes(1_500).as_ref(), "1.5 KB", "decimal kilo");
            assert_eq!(
                bytes(12_345_678).as_ref(),
                "12 MB",
                "two digits drop the decimal"
            );
            assert_eq!(bytes(1_234_567_890).as_ref(), "1.2 GB", "giga");
        } else {
            assert_eq!(bytes(1_536).as_ref(), "1.5 KB", "binary kilo");
            assert_eq!(bytes(1 << 30).as_ref(), "1.0 GB", "binary giga");
        }
    }

    #[test]
    fn counts_group_thousands() {
        assert_eq!(count(0).as_ref(), "0", "zero");
        assert_eq!(count(1_234).as_ref(), "1,234", "thousands");
        assert_eq!(count(1_234_567).as_ref(), "1,234,567", "millions");
        assert_eq!(count(123_456).as_ref(), "123,456", "no leading comma");
    }

    #[test]
    fn home_shows_as_tilde_on_whole_components() {
        let home = Some("/Users/me");
        assert_eq!(
            tilde_in("/Users/me", home, false).as_ref(),
            "~",
            "home itself"
        );
        assert_eq!(
            tilde_in("/Users/me/Library/Caches", home, false).as_ref(),
            "~/Library/Caches",
            "inside home"
        );
        assert_eq!(
            tilde_in("/Users/me2/x", home, false).as_ref(),
            "/Users/me2/x",
            "a sibling sharing the prefix is not inside home"
        );
        assert_eq!(
            tilde_in("/Users/me2", home, true).as_ref(),
            "/Users/me2",
            "sibling stays with case folding too"
        );
        assert_eq!(
            tilde_in("/Users/me/x", Some("/Users/me/"), false).as_ref(),
            "~/x",
            "trailing separator on home"
        );
        assert_eq!(
            tilde_in("/opt/x", home, false).as_ref(),
            "/opt/x",
            "outside home"
        );
        assert_eq!(
            tilde_in("~/Code", home, false).as_ref(),
            "~/Code",
            "already relative"
        );
        assert_eq!(
            tilde_in("/Users/me/x", None, false).as_ref(),
            "/Users/me/x",
            "unknown home"
        );
        assert_eq!(
            tilde_in("/home/ann/x", Some("/"), false).as_ref(),
            "/home/ann/x",
            "a root home shortens nothing"
        );
        assert_eq!(
            tilde_in(r"C:\x", Some(r"C:\"), true).as_ref(),
            r"C:\x",
            "a drive root home shortens nothing"
        );
    }

    #[test]
    fn home_matches_case_insensitively_only_when_folding() {
        let home = Some(r"C:\Users\Ann");
        assert_eq!(
            tilde_in(r"c:\users\ann\AppData", home, true).as_ref(),
            r"~\AppData",
            "case folded (macOS, Windows)"
        );
        assert_eq!(
            tilde_in("/home/Ann/x", Some("/home/ann"), false).as_ref(),
            "/home/Ann/x",
            "case-sensitive (Linux)"
        );
    }
}
