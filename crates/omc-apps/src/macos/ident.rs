//! Name helpers shared by app-file attribution and the leftover scan: normalisation, name
//! tokens, bundle-id vendors, team prefixes and the suffixes macOS adds to per-app files.

/// Names shorter than this (after normalisation) never match by name.
const MIN_NAME: usize = 4;

/// Normalised names too generic to identify an app.
const GENERIC_NAMES: &[&str] = &[
    "agent",
    "app",
    "application",
    "applications",
    "apple",
    "browser",
    "cache",
    "caches",
    "chromium",
    "client",
    "codecache",
    "config",
    "crashpad",
    "crashreporter",
    "daemon",
    "data",
    "default",
    "desktop",
    "electron",
    "google",
    "helper",
    "installer",
    "launcher",
    "logs",
    "main",
    "microsoft",
    "preferences",
    "server",
    "service",
    "settings",
    "setup",
    "shared",
    "support",
    "uninstaller",
    "update",
    "updater",
    "updates",
    "user",
    "users",
];

/// Vendor prefixes shared by unrelated developers (never a vendor match).
const GENERIC_VENDORS: &[&str] = &[
    "com.apple",
    "com.company",
    "com.electron",
    "com.example",
    "com.github",
    "com.yourcompany",
    "io.github",
    "net.sourceforge",
    "org.example",
    "org.gnu",
];

/// Lowercase, without spaces, dashes, dots and underscores.
pub(super) fn normalize(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, ' ' | '-' | '.' | '_'))
        .flat_map(char::to_lowercase)
        .collect()
}

/// A normalised name usable for matching (long enough, not generic).
pub(super) fn usable_name(name: &str) -> Option<String> {
    let n = normalize(name);
    (n.chars().count() >= MIN_NAME && !GENERIC_NAMES.contains(&n.as_str())).then_some(n)
}

/// A normalised name is too generic to identify anything.
pub(super) fn is_generic(normalized: &str) -> bool {
    GENERIC_NAMES.contains(&normalized)
}

/// Lowercase words of a name: split at separators and `CamelCase` boundaries
/// (`"QQMusic Pro"` → `qq`, `music`, `pro`).
pub(super) fn tokens(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut out = Vec::new();
    let mut word = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if !word.is_empty() {
                out.push(std::mem::take(&mut word).to_lowercase());
            }
            continue;
        }
        let prev = i.checked_sub(1).and_then(|p| chars.get(p)).copied();
        let next = chars.get(i.saturating_add(1)).copied();
        let boundary = c.is_uppercase()
            && prev.is_some_and(|p| {
                p.is_lowercase() || (p.is_uppercase() && next.is_some_and(char::is_lowercase))
            });
        if boundary && !word.is_empty() {
            out.push(std::mem::take(&mut word).to_lowercase());
        }
        word.push(c);
    }
    if !word.is_empty() {
        out.push(word.to_lowercase());
    }
    out
}

/// First two components of a bundle id, lowercase (`com.google`), unless generic.
pub(super) fn vendor(id: &str) -> Option<String> {
    let mut parts = id.split('.');
    let (Some(a), Some(b), Some(_)) = (parts.next(), parts.next(), parts.next()) else {
        return None;
    };
    if a.is_empty() || b.is_empty() {
        return None;
    }
    let v = format!("{a}.{b}").to_lowercase();
    (!GENERIC_VENDORS.contains(&v.as_str())).then_some(v)
}

/// `TEAMID.` prefix: ten uppercase letters/digits and a dot.
pub(super) fn strip_team(name: &str) -> Option<(&str, &str)> {
    let (team, rest) = name.split_once('.')?;
    (team.len() == 10
        && team
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        && !rest.is_empty())
    .then_some((team, rest))
}

/// `name` without the extensions macOS adds to per-app files (`.plist`, `.savedState`,
/// `.binarycookies`, recent-document lists, logs) and without a `ByHost` host id.
pub(super) fn strip_suffixes(name: &str) -> &str {
    let mut stem = name;
    for ext in [
        ".plist",
        ".savedState",
        ".binarycookies",
        ".lockfile",
        ".sfl",
        ".sfl2",
        ".sfl3",
        ".sfl4",
        ".log",
    ] {
        if let Some(s) = strip_suffix_ignore_case(stem, ext) {
            stem = s;
        }
    }
    if let Some((head, host)) = stem.rsplit_once('.')
        && is_host_id(host)
    {
        stem = head;
    }
    stem
}

fn strip_suffix_ignore_case<'a>(s: &'a str, suffix: &str) -> Option<&'a str> {
    let at = s.len().checked_sub(suffix.len())?;
    let (head, tail) = s.split_at_checked(at)?;
    tail.eq_ignore_ascii_case(suffix).then_some(head)
}

/// A `ByHost` host id: a UUID or 12 hex digits.
fn is_host_id(s: &str) -> bool {
    let hex = s.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-');
    hex && (s.len() == 36 || s.len() == 12)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_drops_separators_and_case() {
        assert_eq!(normalize("Google Chrome"), "googlechrome", "spaces");
        assert_eq!(normalize("Foo-Bar.app_X"), "foobarappx", "separators");
        assert_eq!(usable_name("Qt"), None, "too short");
        assert_eq!(usable_name("Helper"), None, "generic");
        assert_eq!(
            usable_name("Rectangle").as_deref(),
            Some("rectangle"),
            "specific"
        );
    }

    #[test]
    fn tokens_split_camel_case_and_separators() {
        assert_eq!(tokens("Google Chrome"), vec!["google", "chrome"], "spaces");
        assert_eq!(tokens("QQMusic"), vec!["qq", "music"], "acronym then word");
        assert_eq!(tokens("CherryStudio"), vec!["cherry", "studio"], "camel");
        assert_eq!(tokens("draw.io"), vec!["draw", "io"], "dots");
        assert_eq!(tokens("wpsoffice"), vec!["wpsoffice"], "one word");
        assert!(tokens(" - ").is_empty(), "separators only");
    }

    #[test]
    fn suffixes_and_host_ids_are_stripped() {
        assert_eq!(strip_suffixes("com.foo.bar.plist"), "com.foo.bar", "plist");
        assert_eq!(
            strip_suffixes("com.foo.bar.savedState"),
            "com.foo.bar",
            "saved state"
        );
        assert_eq!(
            strip_suffixes("com.foo.bar.0A1B2C3D-0000-1111-2222-333344445555.plist"),
            "com.foo.bar",
            "ByHost uuid"
        );
        assert_eq!(strip_suffixes("com.foo.bar.sfl3"), "com.foo.bar", "sfl3");
        assert_eq!(
            strip_suffixes("com.foo.bar"),
            "com.foo.bar",
            "plain id kept"
        );
    }

    #[test]
    fn vendors_and_teams() {
        assert_eq!(
            vendor("com.google.Chrome").as_deref(),
            Some("com.google"),
            "vendor"
        );
        assert_eq!(vendor("com.github.foo"), None, "generic vendor");
        assert_eq!(vendor("com.foo"), None, "two components");
        assert_eq!(
            strip_team("EQHXZ8M8AV.com.google.Chrome"),
            Some(("EQHXZ8M8AV", "com.google.Chrome")),
            "team prefix"
        );
        assert_eq!(strip_team("group.com.foo"), None, "not a team");
    }
}
