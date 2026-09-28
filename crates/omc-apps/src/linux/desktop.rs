//! Freedesktop desktop entries (`.desktop` files): parsing the `[Desktop Entry]` group with
//! locale fallback, extracting the program of `Exec`, and rewriting keys (autostart
//! overrides).

use std::path::{Path, PathBuf};

use super::common;

/// The parts of a desktop entry oh-my-clear uses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DesktopEntry {
    /// The file.
    pub(super) path: PathBuf,
    /// Desktop file id: the file name without `.desktop`.
    pub(super) id: String,
    /// `Name` in the best matching locale.
    pub(super) name: Option<String>,
    /// `Exec`.
    pub(super) exec: Option<String>,
    /// `TryExec`.
    pub(super) try_exec: Option<String>,
    /// `Icon`.
    pub(super) icon: Option<String>,
    /// `StartupWMClass`.
    pub(super) wm_class: Option<String>,
    /// `Type` (`Application`, `Link`, `Directory`).
    pub(super) kind: Option<String>,
    /// `NoDisplay=true`.
    pub(super) no_display: bool,
    /// `Hidden=true`.
    pub(super) hidden: bool,
    /// `X-GNOME-Autostart-enabled` is not `false`.
    pub(super) autostart_enabled: bool,
    /// `X-Flatpak` (the app id of an exported Flatpak entry).
    pub(super) flatpak: Option<String>,
    /// `X-SnapInstanceName`.
    pub(super) snap: Option<String>,
}

impl DesktopEntry {
    /// Reads and parses `path`; `None` (logged) when unreadable.
    pub(super) fn load(path: &Path, locales: &[String]) -> Option<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let mut entry = parse(&text, locales);
                entry.path = path.to_path_buf();
                entry.id = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                Some(entry)
            }
            Err(err) => {
                tracing::debug!(%err, path = %path.display(), "cannot read desktop entry");
                None
            }
        }
    }

    /// A launchable application shown in menus.
    pub(super) fn is_visible_app(&self) -> bool {
        self.kind.as_deref() == Some("Application") && !self.no_display && !self.hidden
    }

    /// The program `Exec` starts (see [`exec_program`]).
    pub(super) fn program(&self) -> Option<String> {
        self.exec.as_deref().and_then(exec_program)
    }

    /// `TryExec`, else the program of `Exec`, is missing. `false` when neither is set or
    /// the program cannot be determined.
    pub(super) fn target_missing(&self) -> bool {
        if let Some(try_exec) = self.try_exec.as_deref().map(str::trim)
            && !try_exec.is_empty()
        {
            return !common::program_exists(try_exec);
        }
        self.program().is_some_and(|p| !common::program_exists(&p))
    }
}

/// Locale candidates for `Name[…]`, most specific first (`de_DE.UTF-8` → `de_DE`, `de`).
pub(super) fn locales() -> Vec<String> {
    let raw = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|var| std::env::var(var).ok().filter(|v| !v.is_empty()))
        .unwrap_or_default();
    locales_of(&raw)
}

fn locales_of(raw: &str) -> Vec<String> {
    let base = raw.split(['.', '@']).next().unwrap_or_default();
    if base.is_empty() || base == "C" || base == "POSIX" {
        return Vec::new();
    }
    let mut out = vec![base.to_owned()];
    if let Some((lang, _)) = base.split_once('_') {
        out.push(lang.to_owned());
    }
    out
}

/// Parses the `[Desktop Entry]` group (`path`/`id` are left empty).
pub(super) fn parse(text: &str, locales: &[String]) -> DesktopEntry {
    let mut entry = DesktopEntry {
        autostart_enabled: true,
        ..DesktopEntry::default()
    };
    // (rank, value): rank 0 = most specific locale, `locales.len()` = unlocalized.
    let mut name: Option<(usize, String)> = None;
    let mut in_group = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_group = line == "[Desktop Entry]";
            continue;
        }
        if !in_group || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = unescape(value.trim());
        if let Some(rest) = key.strip_prefix("Name") {
            let rank = if rest.is_empty() {
                Some(locales.len())
            } else {
                rest.strip_prefix('[')
                    .and_then(|r| r.strip_suffix(']'))
                    .and_then(|loc| locales.iter().position(|l| l == loc))
            };
            if let Some(rank) = rank
                && name.as_ref().is_none_or(|(best, _)| rank < *best)
            {
                name = Some((rank, value));
            }
            continue;
        }
        match key {
            "Exec" => entry.exec = Some(value),
            "TryExec" => entry.try_exec = Some(value),
            "Icon" => entry.icon = Some(value),
            "StartupWMClass" => entry.wm_class = Some(value),
            "Type" => entry.kind = Some(value),
            "NoDisplay" => entry.no_display = value == "true",
            "Hidden" => entry.hidden = value == "true",
            "X-GNOME-Autostart-enabled" => entry.autostart_enabled = value != "false",
            "X-Flatpak" => entry.flatpak = Some(value),
            "X-SnapInstanceName" => entry.snap = Some(value),
            _ => {}
        }
    }
    entry.name = name.map(|(_, v)| v).filter(|v| !v.is_empty());
    entry
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// Splits an `Exec` value into arguments (double quotes, backslash escapes inside them);
/// field codes (`%f`, `%U`…) are dropped.
pub(super) fn exec_args(exec: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut quoted = false;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            '\\' if quoted => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        args.push(current);
    }
    args.retain(|a| !(a.len() == 2 && a.starts_with('%')));
    args
}

/// Programs that run another program: the process name says nothing about the app.
pub(super) fn is_launcher(program: &str) -> bool {
    let name = Path::new(program)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    matches!(
        name.as_str(),
        "sh" | "bash"
            | "dash"
            | "zsh"
            | "env"
            | "java"
            | "mono"
            | "node"
            | "perl"
            | "ruby"
            | "gjs"
            | "wine"
            | "flatpak"
            | "snap"
            | "sudo"
            | "pkexec"
            | "gtk-launch"
            | "xdg-open"
            | "electron"
    ) || name.starts_with("python")
}

/// The program an `Exec` line starts, skipping a leading `env [-u NAME] VAR=value…`.
pub(super) fn exec_program(exec: &str) -> Option<String> {
    let mut iter = exec_args(exec).into_iter();
    let first = iter.next()?;
    if Path::new(&first).file_name().is_none_or(|n| n != "env") {
        return Some(first);
    }
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            // Options with a separate value.
            "-u" | "--unset" | "-C" | "--chdir" => {
                iter.next()?;
            }
            a if a.starts_with('-') || a.contains('=') => {}
            _ => return Some(arg),
        }
    }
    None
}

/// `text` with `keys` set inside `[Desktop Entry]` (existing lines replaced, missing
/// ones appended at the end of the group).
pub(super) fn set_keys(text: &str, keys: &[(&str, &str)]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_group = false;
    let mut group_end: Option<usize> = None;
    let mut done = vec![false; keys.len()];
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if in_group {
                group_end = Some(out.len());
            }
            in_group = trimmed == "[Desktop Entry]";
            out.push(line.to_owned());
            continue;
        }
        if in_group
            && let Some((key, _)) = trimmed.split_once('=')
            && let Some(pos) = keys.iter().position(|(k, _)| *k == key.trim())
            && let (Some(flag), Some((k, v))) = (done.get_mut(pos), keys.get(pos))
        {
            *flag = true;
            out.push(format!("{k}={v}"));
            continue;
        }
        out.push(line.to_owned());
    }
    if in_group {
        group_end = Some(out.len());
    }
    let missing: Vec<String> = keys
        .iter()
        .zip(&done)
        .filter(|(_, done)| !**done)
        .map(|((k, v), _)| format!("{k}={v}"))
        .collect();
    let mut insert_at = group_end.unwrap_or(out.len());
    if group_end.is_none() {
        out.push("[Desktop Entry]".to_owned());
        insert_at = out.len();
    }
    // Keep blank lines separating groups after the inserted keys.
    while insert_at > 0
        && out
            .get(insert_at.saturating_sub(1))
            .is_some_and(|l| l.trim().is_empty())
    {
        insert_at = insert_at.saturating_sub(1);
    }
    for (offset, line) in missing.into_iter().enumerate() {
        out.insert(insert_at.saturating_add(offset).min(out.len()), line);
    }
    let mut text = out.join("\n");
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIREFOX: &str = "\
[Desktop Entry]
Version=1.0
Name=Firefox Web Browser
Name[de]=Firefox-Webbrowser
Name[de_AT]=Firefox (AT)
Exec=env MOZ_ENABLE_WAYLAND=1 firefox %u
Icon=firefox
Type=Application
StartupWMClass=firefox
X-GNOME-Autostart-enabled=false

[Desktop Action new-window]
Name=New Window
Exec=firefox --new-window
NoDisplay=true
";

    #[test]
    fn parses_main_group_with_locale_fallback() {
        let e = parse(FIREFOX, &locales_of("de_DE.UTF-8"));
        assert_eq!(
            e.name.as_deref(),
            Some("Firefox-Webbrowser"),
            "language fallback"
        );
        let e = parse(FIREFOX, &locales_of("de_AT.UTF-8"));
        assert_eq!(e.name.as_deref(), Some("Firefox (AT)"), "exact locale wins");
        let e = parse(FIREFOX, &[]);
        assert_eq!(
            e.name.as_deref(),
            Some("Firefox Web Browser"),
            "unlocalized"
        );
        assert!(!e.no_display, "action group keys are ignored");
        assert!(e.is_visible_app(), "visible application");
        assert!(!e.autostart_enabled, "autostart switch read");
        assert_eq!(e.wm_class.as_deref(), Some("firefox"), "wm class");
        assert_eq!(
            e.program().as_deref(),
            Some("firefox"),
            "env prefix skipped"
        );
    }

    #[test]
    fn exec_arguments_honour_quotes_and_field_codes() {
        assert_eq!(
            exec_args(r#""/opt/My App/app" --flag "a \"q\"" %F"#),
            vec!["/opt/My App/app", "--flag", "a \"q\""],
            "quoted args"
        );
        assert_eq!(
            exec_program("env -u X A=b /snap/bin/code --x"),
            Some("/snap/bin/code".to_owned()),
            "env options and assignments skipped"
        );
        assert!(
            is_launcher("/usr/bin/python3.12"),
            "interpreters are launchers"
        );
        assert!(!is_launcher("/usr/bin/firefox"), "apps are not");
    }

    #[test]
    fn set_keys_replaces_and_appends_inside_main_group() {
        let out = set_keys(
            FIREFOX,
            &[("Hidden", "true"), ("X-GNOME-Autostart-enabled", "true")],
        );
        let e = parse(&out, &[]);
        assert!(e.hidden, "Hidden appended: {out}");
        assert!(e.autostart_enabled, "existing key replaced: {out}");
        assert_eq!(
            out.matches("X-GNOME-Autostart-enabled").count(),
            1,
            "no duplicate key: {out}"
        );
        let action = out
            .split("[Desktop Action new-window]")
            .nth(1)
            .unwrap_or_default();
        assert!(!action.contains("Hidden"), "action group untouched: {out}");
        let bare = set_keys("", &[("Hidden", "true")]);
        assert!(parse(&bare, &[]).hidden, "group created: {bare}");
    }

    #[test]
    fn unescapes_values() {
        assert_eq!(unescape(r"a\sb\\c"), "a b\\c", "escapes");
    }
}
