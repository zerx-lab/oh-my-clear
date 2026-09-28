//! Friendly names for technical identifiers: bundle ids, launchd labels, package and folder
//! names become the installed app's display name and icon ([`Namer`]), or a readable
//! product name when no installed app matches ([`pretty`]).
//!
//! Only names change: annotated items keep their ids, groups and order, and the raw
//! identifier moves to `ident` so the UI can still show it as secondary text.

use std::collections::HashMap;
use std::path::Path;

use omc_proto::apps::{AppInfo, StartupItem, StartupKind};
use omc_proto::junk::{JunkKind, JunkReport};
use omc_proto::settings::CleanSettings;
use omc_scan::{JobCtx, Walker};

use crate::StartupRecord;

/// Longest friendly name produced by [`pretty`] (characters, before the ellipsis).
const MAX_CHARS: usize = 60;

/// Top-level components that start a reverse-DNS identifier.
const TLDS: &[&str] = &[
    "ai", "app", "at", "au", "be", "biz", "ca", "cc", "ch", "cloud", "cn", "co", "com", "company",
    "de", "design", "dev", "es", "eu", "fm", "fr", "games", "gg", "hk", "im", "in", "info", "io",
    "is", "it", "jp", "kr", "lab", "labs", "ly", "me", "net", "nl", "one", "org", "page", "pl",
    "pro", "ru", "run", "se", "sh", "site", "so", "space", "studio", "tech", "to", "top", "tv",
    "tw", "uk", "us", "xyz", "zone",
];

/// Code-hosting vendors: the product is the next component (`io.github.<user>.<product>`
/// keeps the user as vendor).
const HOSTS: &[&str] = &["github", "gitlab", "sourceforge", "bitbucket"];

/// Words that name a process role or build flavour, not a product (`…helper.GPU`,
/// `…desktop`, `…macos`, `Warp-Stable`).
const NOISE: &[&str] = &[
    "agent",
    "app",
    "cache",
    "client",
    "daemon",
    "desktop",
    "extension",
    "gpu",
    "gui",
    "helper",
    "launcher",
    "mac",
    "macclient",
    "macos",
    "osx",
    "plugin",
    "privhelper",
    "release",
    "renderer",
    "service",
    "settings",
    "stable",
    "startup",
    "updater",
    "wake",
    "xpcservice",
];

/// Product words too generic to stand alone: the vendor is kept (`com.soda.music` →
/// "Soda Music").
const GENERIC: &[&str] = &[
    "assistant",
    "browser",
    "calendar",
    "camera",
    "capture",
    "cast",
    "center",
    "chat",
    "code",
    "connect",
    "console",
    "creator",
    "dashboard",
    "docs",
    "downloader",
    "drive",
    "editor",
    "hub",
    "ide",
    "keystone",
    "mail",
    "manager",
    "maps",
    "meet",
    "meeting",
    "menubar",
    "messenger",
    "monitor",
    "music",
    "notes",
    "office",
    "pay",
    "photo",
    "photos",
    "player",
    "reader",
    "recorder",
    "remote",
    "scanner",
    "search",
    "share",
    "store",
    "studio",
    "sync",
    "terminal",
    "tools",
    "translate",
    "translator",
    "video",
    "viewer",
    "vpn",
    "workspace",
];

/// Lowercase words written in capitals.
const ACRONYMS: &[&str] = &[
    "ai", "api", "cli", "cn", "db", "dns", "ftp", "gpu", "hd", "http", "id", "ide", "jdk", "json",
    "nfc", "os", "pdf", "qq", "sdk", "sql", "ssh", "tv", "ui", "usb", "uu", "vpn", "wps", "xml",
];

/// Well-known system and vendor services whose identifiers say nothing to a beginner
/// (lowercase key → label). Checked before the generic rules; installed apps still win
/// (`com.apple.helpviewer` is the Tips app on current macOS).
const CURATED: &[(&str, &str)] = &[
    ("clang", "Xcode compiler cache"),
    ("cloudkit", "iCloud (CloudKit)"),
    ("com.apple.akd", "Apple Account (macOS)"),
    ("com.apple.amsaccountsd", "App Store services (macOS)"),
    ("com.apple.amsengagementd", "App Store services (macOS)"),
    ("com.apple.applemediaservices", "App Store services (macOS)"),
    ("com.apple.appleaccountd", "Apple Account (macOS)"),
    ("com.apple.appstoreagent", "App Store (macOS)"),
    ("com.apple.cloudtelemetry", "iCloud diagnostics (macOS)"),
    ("com.apple.controlcenter", "Control Center (macOS)"),
    ("com.apple.coremedia.videodecoder", "Video decoder (macOS)"),
    ("com.apple.ctcategories.service", "Screen Time (macOS)"),
    (
        "com.apple.dataaccess.dataaccessd",
        "Calendar & contacts sync (macOS)",
    ),
    ("com.apple.developertools", "Xcode developer tools"),
    ("com.apple.dock", "Dock (macOS)"),
    ("com.apple.duetexpertd", "Siri suggestions (macOS)"),
    ("com.apple.geoanalyticsd", "Maps analytics (macOS)"),
    ("com.apple.helpd", "Help (macOS)"),
    ("com.apple.helpviewer", "Help Viewer (macOS)"),
    ("com.apple.iconservices", "Icon cache (macOS)"),
    ("com.apple.icloudwebd", "iCloud web (macOS)"),
    ("com.apple.installer", "Installer (macOS)"),
    ("com.apple.languageassetd", "Language assets (macOS)"),
    ("com.apple.loginwindow", "Login window (macOS)"),
    ("com.apple.mediaanalysisd", "Media Analysis (macOS)"),
    ("com.apple.metal", "Metal shader cache (macOS)"),
    (
        "com.apple.notificationcenterui",
        "Notification Center (macOS)",
    ),
    ("com.apple.parsecd", "Spotlight suggestions (macOS)"),
    ("com.apple.python", "Python (macOS)"),
    ("com.apple.replayd", "Screen recording (macOS)"),
    ("com.apple.safari.safebrowsing", "Safari Safe Browsing"),
    ("com.apple.spotlight", "Spotlight (macOS)"),
    ("com.apple.storekitagent", "App Store (macOS)"),
    ("com.apple.textunderstandingd", "Text understanding (macOS)"),
    ("com.apple.wallpaper", "Wallpaper (macOS)"),
    ("com.apple.webkit", "WebKit (macOS)"),
    ("com.github.electron", "Electron"),
    ("com.google.googleupdater", "Google Updater"),
    ("com.google.keystone", "Google Software Update"),
    ("com.microsoft.autoupdate", "Microsoft AutoUpdate"),
    ("com.microsoft.autoupdate2", "Microsoft AutoUpdate"),
    ("geoservices", "Maps data (macOS)"),
    ("keystone", "Google Software Update"),
    ("org.llvm.clang", "Xcode compiler cache"),
    (
        "org.sparkle-project.downloaderservice",
        "App updates (Sparkle)",
    ),
    ("passkit", "Wallet (PassKit)"),
    ("tmpdir", "Temporary files"),
];

/// A friendly name and, when an installed app matched, its icon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// Display name.
    pub name: String,
    /// PNG of the app icon.
    pub icon: Option<String>,
}

/// One installed app as the namer knows it.
#[derive(Debug, Clone)]
struct Known {
    name: String,
    icon: Option<String>,
}

/// Installed apps indexed by identifier, bundle/install path and name; built once per job.
#[derive(Debug, Clone, Default)]
pub struct Namer {
    apps: Vec<Known>,
    /// Lowercase ident → app.
    ids: HashMap<String, usize>,
    /// Lowercase id prefix (≥ 3 components) shared by exactly one app's longer id → app
    /// (`None` when several apps share it).
    families: HashMap<String, Option<usize>>,
    /// Normalized display names, bundle/folder stems and aliases of non-system apps → app.
    names: HashMap<String, usize>,
    /// Lowercase bundle/install locations, longest first.
    locations: Vec<(String, usize)>,
}

impl Namer {
    /// The namer for the installed apps (system apps included). Never fails: without an
    /// inventory nothing resolves and names fall back to [`pretty`]. Uses its own progress
    /// context so the calling job's counters are left alone.
    pub fn build(settings: &CleanSettings, walker: &Walker, ctx: &JobCtx) -> Self {
        if ctx.is_cancelled() {
            return Self::default();
        }
        let settings = CleanSettings {
            show_system_apps: true,
            ..settings.clone()
        };
        match crate::list_apps(&settings, walker, &JobCtx::new()) {
            Ok(apps) => Self::from_apps(
                apps.iter()
                    .map(|app| (&app.info, crate::platform::app_aliases(app))),
            ),
            Err(err) => {
                tracing::warn!(%err, "app inventory for friendly names failed");
                Self::default()
            }
        }
    }

    /// The namer for `apps`, each with its extra names (Electron product names,
    /// executables, desktop-entry ids).
    pub fn from_apps<'a>(apps: impl IntoIterator<Item = (&'a AppInfo, Vec<String>)>) -> Self {
        let mut namer = Self::default();
        for (index, (info, extra)) in apps.into_iter().enumerate() {
            namer.apps.push(Known {
                name: info.name.clone(),
                icon: info.icon.clone(),
            });
            if let Some(ident) = info.ident.as_deref().map(str::trim)
                && !ident.is_empty()
            {
                let lower = ident.to_lowercase();
                if !info.system {
                    // OS apps share prefixes with unrelated OS services.
                    namer.add_families(&lower, index);
                }
                namer.ids.entry(lower).or_insert(index);
            }
            if let Some(location) = info.location.as_deref()
                && is_specific_location(location)
            {
                namer.locations.push((location.to_lowercase(), index));
            }
            if info.system {
                continue;
            }
            let stem = info.location.as_deref().and_then(location_stem);
            for name in std::iter::once(info.name.as_str())
                .chain(stem)
                .chain(extra.iter().map(String::as_str))
            {
                let key = normalize(name);
                if key.chars().count() >= 2 {
                    namer.names.entry(key).or_insert(index);
                }
            }
        }
        namer
            .locations
            .sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        namer
    }

    /// Registers the proper prefixes (≥ 3 components) of `id` as its family.
    fn add_families(&mut self, id: &str, index: usize) {
        let parts: Vec<&str> = id.split('.').collect();
        for len in 3..parts.len() {
            let Some(prefix) = parts.get(..len) else {
                break;
            };
            let key = prefix.join(".");
            self.families
                .entry(key)
                .and_modify(|slot| {
                    if *slot != Some(index) {
                        *slot = None;
                    }
                })
                .or_insert(Some(index));
        }
    }

    /// The installed app `raw` names, if any: by identifier (exact, a parent id or the
    /// app's id family, case-insensitive), by a bundle/install `path` it lives in, or by
    /// the app's display, folder or product name.
    pub fn resolve(&self, raw: &str, path: Option<&Path>) -> Option<Resolved> {
        let key = unwrap_ident(raw.trim());
        if key.is_empty() {
            return None;
        }
        let lower = key.to_lowercase();
        if let Some(&app) = self.ids.get(&lower) {
            return Some(self.resolved(app, &[]));
        }
        if curated(&lower).is_none()
            && let Some(found) = self.by_parent(&lower, key)
        {
            return Some(found);
        }
        if let Some(path) = path
            && let Some(found) = self.by_path_text(&path.to_string_lossy())
        {
            return Some(found);
        }
        self.by_name(key)
            .or_else(|| pretty(key).and_then(|p| self.by_name(&p)))
            .map(|app| self.resolved(app, &[]))
    }

    /// The app a command line or path runs from: inside a known bundle or install folder,
    /// or else inside some `.app` bundle (named after it, no icon).
    pub fn resolve_program(&self, command: &str) -> Option<Resolved> {
        self.by_path_text(command)
    }

    fn resolved(&self, app: usize, suffix: &[String]) -> Resolved {
        let Some(known) = self.apps.get(app) else {
            return Resolved {
                name: String::new(),
                icon: None,
            };
        };
        let mut name = known.name.clone();
        let own: Vec<String> = known
            .name
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        for word in suffix {
            if !own.contains(&word.to_lowercase()) {
                name.push(' ');
                name.push_str(word);
            }
        }
        Resolved {
            name,
            icon: known.icon.clone(),
        }
    }

    /// Parent ids and id families: `com.tencent.xinWeChat.helper` → `WeChat`,
    /// `com.google.chrome.for.testing` → "Google Chrome For Testing".
    fn by_parent(&self, lower: &str, original: &str) -> Option<Resolved> {
        let parts: Vec<&str> = lower.split('.').collect();
        let original: Vec<&str> = original.split('.').collect();
        if parts.len() < 3 || parts.len() != original.len() {
            return None;
        }
        for len in (2..parts.len()).rev() {
            let (Some(prefix), Some(rest)) = (parts.get(..len), original.get(len..)) else {
                continue;
            };
            let key = prefix.join(".");
            let app = self
                .ids
                .get(&key)
                .copied()
                .or_else(|| self.families.get(&key).copied().flatten());
            if let Some(app) = app {
                let suffix: Vec<String> = rest
                    .iter()
                    .flat_map(|part| words(part))
                    .filter(|w| !is_noise(w))
                    .map(|w| title(&w))
                    .collect();
                return Some(self.resolved(app, &suffix));
            }
        }
        None
    }

    fn by_path_text(&self, text: &str) -> Option<Resolved> {
        self.app_by_path_text(text)
            .or_else(|| self.bundle_by_path_text(text))
    }

    /// The installed app whose bundle or install folder `text` mentions.
    fn app_by_path_text(&self, text: &str) -> Option<Resolved> {
        let lower = text.to_lowercase();
        self.locations
            .iter()
            .find(|(location, _)| contains_bounded(&lower, location))
            .map(|(_, app)| self.resolved(*app, &[]))
    }

    /// The `.app` bundle `text` mentions, named after its file (icon if it matches an
    /// installed app by name).
    fn bundle_by_path_text(&self, text: &str) -> Option<Resolved> {
        bundle_stem(text).map(|name| {
            let icon = self
                .names
                .get(&normalize(name))
                .and_then(|&app| self.apps.get(app))
                .and_then(|known| known.icon.clone());
            Resolved {
                name: name.to_owned(),
                icon,
            }
        })
    }

    fn by_name(&self, name: &str) -> Option<usize> {
        self.names.get(&normalize(name)).copied()
    }

    /// The friendly name of `raw`: an installed app, else [`pretty`]. `None` when nothing
    /// better than `raw` is known.
    pub fn friendly(&self, raw: &str, path: Option<&Path>) -> Option<Resolved> {
        self.resolve(raw, path)
            .or_else(|| pretty(raw).map(|name| Resolved { name, icon: None }))
    }
}

/// Lowercase with spaces, dashes, underscores and dots removed: how app names are compared.
fn normalize(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, ' ' | '-' | '_' | '.'))
        .flat_map(char::to_lowercase)
        .collect()
}

/// A location specific enough to identify one app by prefix (not a drive or `/usr`).
fn is_specific_location(location: &str) -> bool {
    location
        .split(['/', '\\'])
        .filter(|part| !part.is_empty() && !part.ends_with(':'))
        .count()
        >= 2
}

/// File name of a location without its extension (`/Applications/WeChat.app` → `WeChat`).
fn location_stem(location: &str) -> Option<&str> {
    let name = location
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()?;
    let stem = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.len() <= 4 => stem,
        _ => name,
    };
    (!stem.is_empty()).then_some(stem)
}

/// `haystack` contains `needle` followed by a path separator, quote, space or the end.
fn contains_bounded(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    haystack.match_indices(needle).any(|(at, _)| {
        haystack
            .get(at.saturating_add(needle.len())..)
            .and_then(|rest| rest.chars().next())
            .is_none_or(|c| matches!(c, '/' | '\\' | '"' | '\'' | ' ' | '\t'))
    })
}

/// Name of the outermost `.app` bundle in a path or command line.
fn bundle_stem(text: &str) -> Option<&str> {
    let lower = text.to_ascii_lowercase();
    let at = lower.match_indices(".app").find_map(|(at, _)| {
        let end = at.saturating_add(4);
        lower
            .get(end..)
            .and_then(|rest| rest.chars().next())
            .is_none_or(|c| matches!(c, '/' | '"' | '\'' | ' '))
            .then_some(at)
    })?;
    let before = text.get(..at)?;
    let start = before.rfind('/').map_or(0, |i| i.saturating_add(1));
    let stem = before.get(start..)?.trim_start_matches(['"', '\'']);
    (!stem.is_empty()).then_some(stem)
}

/// The identifier inside decorations: the app after `+` (`com.apple.WebKit.GPU+<id>`),
/// without a team-id prefix (`J3CP9BBBN6.`) or `group.`.
fn unwrap_ident(raw: &str) -> &str {
    let mut key = raw.rsplit('+').next().unwrap_or(raw);
    if let Some((first, rest)) = key.split_once('.')
        && is_team_id(first)
    {
        key = rest;
    }
    if let Some(rest) = key.strip_prefix("group.")
        && rest.contains('.')
    {
        key = rest;
    }
    key
}

/// Apple team id: ten capital letters and digits, with at least one digit.
fn is_team_id(part: &str) -> bool {
    part.len() == 10
        && part
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && part.chars().any(|c| c.is_ascii_digit())
}

fn is_noise(word: &str) -> bool {
    NOISE.contains(&word.to_lowercase().as_str())
}

fn curated(lower: &str) -> Option<&'static str> {
    CURATED
        .iter()
        .find(|(key, _)| *key == lower)
        .map(|(_, label)| *label)
}

/// Words of one identifier component: split on dashes and underscores (camelCase is
/// kept: it is how brands write themselves, `ForkLift`, `CherryStudio`).
fn words(part: &str) -> Vec<String> {
    part.split(['-', '_', ' '])
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// A word as a name: mixed-case words keep their casing, acronyms are capitalized, other
/// lowercase words get an initial capital.
fn title(word: &str) -> String {
    if word.chars().any(char::is_uppercase) {
        return word.to_owned();
    }
    if ACRONYMS.contains(&word) {
        return word.to_uppercase();
    }
    let mut chars = word.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// `words` without trailing noise words and repeats, title-cased and joined.
fn join_words(words: Vec<String>) -> Option<String> {
    let mut kept: Vec<String> = Vec::new();
    for word in words {
        if !kept.iter().any(|k| k.eq_ignore_ascii_case(&word)) {
            kept.push(word);
        }
    }
    while kept.last().is_some_and(|w| is_noise(w)) {
        kept.pop();
    }
    if kept.is_empty() {
        return None;
    }
    let joined = kept.iter().map(|w| title(w)).collect::<Vec<_>>().join(" ");
    Some(cap(joined))
}

/// At most [`MAX_CHARS`] characters, with an ellipsis when cut.
fn cap(name: String) -> String {
    match name.char_indices().nth(MAX_CHARS) {
        Some((at, _)) => {
            let mut short = name.get(..at).unwrap_or(&name).trim_end().to_owned();
            short.push('…');
            short
        }
        None => name,
    }
}

/// A readable product name for an identifier that matches no installed app, or `None`
/// when `raw` is already the best name.
///
/// Reverse-DNS ids lose the domain and vendor (`net.kovidgoyal.kitty` → "Kitty") unless
/// the product word is generic (`com.soda.music.helper` → "Soda Music") or missing
/// (`com.raycast.macos` → "Raycast"); role/flavour suffixes (`helper`, `GPU`, `agent`,
/// `desktop`…), team-id and `group.` prefixes are dropped; macOS services get
/// "(macOS)". Well-known services use a curated label. Plain names only lose a noise
/// suffix (`yuque-desktop` → "Yuque"); everything else stays as it is.
pub fn pretty(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let key = unwrap_ident(trimmed);
    let lower = key.to_lowercase();
    if let Some(label) = curated(&lower) {
        return Some(label.to_owned());
    }
    let parts: Vec<&str> = key.split('.').filter(|p| !p.is_empty()).collect();
    let is_dns = parts.len() >= 2
        && parts
            .first()
            .is_some_and(|tld| TLDS.contains(&tld.to_lowercase().as_str()));
    let name = if is_dns {
        dns_name(&parts)
    } else {
        plain_name(key)
    };
    name.filter(|n| n != trimmed)
}

fn dns_name(parts: &[&str]) -> Option<String> {
    // Curated parents with noise removed: `com.google.keystone.agent`.
    let mut len = parts.len();
    while len > 2
        && parts
            .get(len.saturating_sub(1))
            .is_some_and(|p| is_noise(p))
    {
        len = len.saturating_sub(1);
    }
    let stripped = parts.get(..len)?.join(".").to_lowercase();
    if let Some(label) = curated(&stripped) {
        return Some(label.to_owned());
    }
    let mut vendor_at = 1;
    if parts
        .get(1)
        .is_some_and(|v| HOSTS.contains(&v.to_lowercase().as_str()))
        && parts.len() > 3
    {
        vendor_at = 2;
    }
    let vendor = parts.get(vendor_at).copied()?;
    let product: Vec<String> = parts
        .get(vendor_at.saturating_add(1)..)
        .unwrap_or(&[])
        .iter()
        .flat_map(|p| words(p))
        .collect();
    let apple = vendor.eq_ignore_ascii_case("apple");
    let mut product_words = product;
    while product_words.last().is_some_and(|w| is_noise(w)) {
        product_words.pop();
    }
    let chosen = if product_words.is_empty() {
        if apple || is_noise(vendor) {
            return None;
        }
        words(vendor)
    } else if !apple
        && product_words.len() == 1
        && product_words
            .first()
            .is_some_and(|w| GENERIC.contains(&w.to_lowercase().as_str()))
    {
        let mut both = words(vendor);
        both.extend(product_words);
        both
    } else {
        product_words
    };
    let name = join_words(chosen)?;
    Some(if apple {
        cap(format!("{name} (macOS)"))
    } else {
        name
    })
}

/// Non-reverse-DNS names change only when they end in a noise word after a dash or
/// underscore (`yuque-desktop` → "Yuque").
fn plain_name(key: &str) -> Option<String> {
    let parts = words(key);
    if parts.len() < 2 || !parts.last().is_some_and(|w| is_noise(w)) {
        return None;
    }
    join_words(parts)
}

/// Junk kinds whose item names are app identifiers or folder names.
fn names_apps(kind: JunkKind) -> bool {
    matches!(
        kind,
        JunkKind::UserCache
            | JunkKind::SystemCache
            | JunkKind::UserLog
            | JunkKind::SystemLog
            | JunkKind::CrashReport
            | JunkKind::TempFiles
            | JunkKind::ShaderCache
            | JunkKind::OrphanFiles
            | JunkKind::OrphanLaunchItems
            | JunkKind::BrokenUninstallEntries
            | JunkKind::BrokenShortcuts
            | JunkKind::OrphanRegistry
    )
}

/// Gives the items of `report` friendly names: `name` becomes the app or product name,
/// the raw name moves to `ident` (only when it changed and no ident is set yet), `icon`
/// is the app's. Ids, groups and order are unchanged.
pub fn annotate_junk(report: &mut JunkReport, namer: &Namer) {
    for group in &mut report.groups {
        if !names_apps(group.kind) {
            continue;
        }
        for item in &mut group.items {
            let raw = item.name.as_str();
            let found = match raw.split_once('/') {
                // `CherryStudio/Shared Dictionary`: name the app part only.
                Some((head, rest)) => namer.resolve(head, None).map(|r| Resolved {
                    name: format!("{}/{rest}", r.name),
                    icon: r.icon,
                }),
                None => namer.friendly(raw, None),
            };
            let Some(found) = found else { continue };
            apply(&mut item.name, &mut item.ident, &mut item.icon, found);
        }
    }
}

/// Gives startup items friendly names: launchd labels become the app they belong to (by
/// label, else by the program's bundle) or a readable product name; other kinds already
/// carry names and only gain the app's icon.
pub fn annotate_startup(items: &mut [StartupRecord], namer: &Namer) {
    annotate_startup_items(items.iter_mut().map(|record| &mut record.item), namer);
}

fn annotate_startup_items<'a>(items: impl Iterator<Item = &'a mut StartupItem>, namer: &Namer) {
    for item in items {
        let command = item.command.as_deref();
        match item.kind {
            StartupKind::LaunchAgent | StartupKind::LaunchDaemon => {
                // Installed app by label, then by program; a readable label beats the
                // file name of an unknown bundle (`GoogleUpdater.app`).
                let found = namer
                    .resolve(&item.name, None)
                    .or_else(|| command.and_then(|c| namer.app_by_path_text(c)))
                    .or_else(|| pretty(&item.name).map(|name| Resolved { name, icon: None }))
                    .or_else(|| command.and_then(|c| namer.bundle_by_path_text(c)));
                if let Some(found) = found {
                    apply(&mut item.name, &mut item.ident, &mut item.icon, found);
                }
            }
            _ => {
                if item.icon.is_none() {
                    item.icon = command
                        .and_then(|c| namer.resolve_program(c))
                        .and_then(|r| r.icon);
                }
            }
        }
    }
}

fn apply(
    name: &mut String,
    ident: &mut Option<String>,
    icon: &mut Option<String>,
    found: Resolved,
) {
    if icon.is_none() {
        *icon = found.icon;
    }
    if found.name.is_empty() || found.name == *name {
        return;
    }
    let raw = std::mem::replace(name, found.name);
    if ident.is_none() {
        *ident = Some(raw);
    }
}

#[cfg(test)]
mod tests {
    use omc_proto::apps::{AppInfo, AppSource, Scope, StartupItem};
    use omc_proto::jobs::Location;
    use omc_proto::junk::{JunkGroup, JunkItem, Safety};

    use super::*;

    fn app(name: &str, ident: &str, location: &str, system: bool) -> AppInfo {
        AppInfo {
            id: 0,
            name: name.to_owned(),
            version: None,
            publisher: None,
            ident: Some(ident.to_owned()),
            location: Some(location.to_owned()),
            bytes: None,
            source: AppSource::MacBundle,
            system,
            running: false,
            last_used: None,
            installed: None,
            icon: Some(format!("/icons/{name}.png")),
            needs_admin: false,
        }
    }

    fn namer() -> Namer {
        let apps = [
            app(
                "WeChat",
                "com.tencent.xinWeChat",
                "/Applications/WeChat.app",
                false,
            ),
            app(
                "Google Chrome",
                "com.google.Chrome",
                "/Applications/Google Chrome.app",
                false,
            ),
            app(
                "ToDesk",
                "com.youqu.todesk.mac",
                "/Applications/ToDesk.app",
                false,
            ),
            app(
                "Cherry Studio",
                "com.kangfenmao.CherryStudio",
                "/Applications/Cherry Studio.app",
                false,
            ),
            app("语雀", "com.yuque.app", "/Applications/语雀.app", false),
            app(
                "Tips",
                "com.apple.helpviewer",
                "/System/Applications/Tips.app",
                true,
            ),
            app(
                "Music",
                "com.apple.Music",
                "/System/Applications/Music.app",
                true,
            ),
            app(
                "AweSun",
                "com.oray.sunlogin.macclient",
                "/Applications/AweSun.app",
                false,
            ),
        ];
        Namer::from_apps(apps.iter().map(|info| {
            let aliases = if info.name == "语雀" {
                vec!["yuque-desktop".to_owned()]
            } else {
                Vec::new()
            };
            (info, aliases)
        }))
    }

    fn name_of(namer: &Namer, raw: &str) -> Option<String> {
        namer.resolve(raw, None).map(|r| r.name)
    }

    #[test]
    fn pretty_names_reverse_dns_ids() {
        let cases = [
            ("ai.opencode.desktop.helper", Some("Opencode")),
            ("com.soda.music.helper", Some("Soda Music")),
            ("com.usebruno.app.helper.GPU", Some("Usebruno")),
            ("net.kovidgoyal.kitty", Some("Kitty")),
            ("com.raycast.macos", Some("Raycast")),
            ("com.binarynights.ForkLift", Some("ForkLift")),
            ("J3CP9BBBN6.com.binarynights.ForkLift", Some("ForkLift")),
            (
                "4K6FWZU8C4.group.com.better365.menubar",
                Some("Better365 Menubar"),
            ),
            ("com.google.keystone.agent", Some("Google Software Update")),
            ("com.citrolabs.keystone.agent", Some("Citrolabs Keystone")),
            ("com.cocos.creator.helper", Some("Cocos Creator")),
            ("com.tencent.qq", Some("QQ")),
            ("dev.atlas.ide", Some("Atlas IDE")),
            ("dev.warp.Warp-Stable", Some("Warp")),
            (
                "org.pqrs.Karabiner-Elements.Settings",
                Some("Karabiner Elements"),
            ),
            (
                "io.github.clash-verge-rev.clash-verge-rev.service",
                Some("Clash Verge Rev"),
            ),
            (
                "com.apple.WebKit.GPU+com.canva.affinity-cn",
                Some("Affinity CN"),
            ),
            ("com.apple.mediaanalysisd", Some("Media Analysis (macOS)")),
            ("com.apple.dock.helper", Some("Dock (macOS)")),
            ("com.apple.FollowUpUI", Some("FollowUpUI (macOS)")),
            (
                "com.microsoft.autoupdate.helper",
                Some("Microsoft AutoUpdate"),
            ),
            ("clang", Some("Xcode compiler cache")),
            ("GeoServices", Some("Maps data (macOS)")),
        ];
        for (raw, want) in cases {
            assert_eq!(pretty(raw).as_deref(), want, "pretty({raw})");
        }
    }

    #[test]
    fn pretty_never_makes_plain_names_worse() {
        let cases = [
            ("Citro Labs", None),
            ("123pan", None),
            ("node-gyp", None),
            ("cargo-zigbuild", None),
            ("DiscRecording.log", None),
            ("draw.io", None),
            ("bun", None),
            ("", None),
            ("helper", None),
            ("yuque-desktop", Some("Yuque")),
            ("dashboard-app", Some("Dashboard")),
        ];
        for (raw, want) in cases {
            assert_eq!(pretty(raw).as_deref(), want, "pretty({raw:?})");
        }
    }

    #[test]
    fn pretty_edge_cases() {
        assert_eq!(
            pretty("com.helper"),
            None,
            "all-noise id keeps its raw name"
        );
        assert_eq!(
            pretty("com.apple.helper"),
            None,
            "all-noise Apple id keeps its raw name"
        );
        assert_eq!(
            pretty("com.example"),
            Some("Example".to_owned()),
            "vendor-only id"
        );
        assert_eq!(
            pretty("com.例子.音乐"),
            Some("音乐".to_owned()),
            "non-ASCII words survive"
        );
        let long = format!("com.vendor.{}", "x".repeat(200));
        let name = pretty(&long).unwrap_or_default();
        assert_eq!(
            name.chars().count(),
            MAX_CHARS + 1,
            "long names are capped: {name}"
        );
        assert!(
            name.ends_with('…'),
            "cut names end with an ellipsis: {name}"
        );
    }

    #[test]
    fn resolves_ids_parents_and_families() {
        let n = namer();
        assert_eq!(
            name_of(&n, "com.tencent.xinWeChat").as_deref(),
            Some("WeChat"),
            "exact"
        );
        assert_eq!(
            name_of(&n, "COM.TENCENT.XINWECHAT").as_deref(),
            Some("WeChat"),
            "case"
        );
        assert_eq!(
            name_of(&n, "com.tencent.xinWeChat.helper").as_deref(),
            Some("WeChat"),
            "parent"
        );
        assert_eq!(
            name_of(&n, "com.google.chrome.for.testing.helper").as_deref(),
            Some("Google Chrome For Testing"),
            "sibling products keep their distinguishing words",
        );
        assert_eq!(
            name_of(&n, "com.youqu.todesk.UninstallerHelper").as_deref(),
            Some("ToDesk UninstallerHelper"),
            "id family",
        );
        assert_eq!(
            name_of(&n, "com.apple.WebKit.GPU+com.tencent.xinWeChat").as_deref(),
            Some("WeChat"),
            "WebKit process of an app",
        );
        assert_eq!(
            name_of(&n, "com.tencent.qq"),
            None,
            "a vendor prefix alone is not a match"
        );
        let wechat = n.resolve("com.tencent.xinWeChat.helper", None);
        assert_eq!(
            wechat.and_then(|r| r.icon).as_deref(),
            Some("/icons/WeChat.png"),
            "the app's icon comes along",
        );
    }

    #[test]
    fn resolves_paths_and_names() {
        let n = namer();
        let program =
            n.resolve_program("/Applications/AweSun.app/Contents/Helpers/AweSun --mod=service");
        assert_eq!(
            program.map(|r| r.name).as_deref(),
            Some("AweSun"),
            "program inside a bundle"
        );
        let quoted = n.resolve_program("/usr/bin/open -a /Applications/ToDesk.app --args");
        assert_eq!(
            quoted.map(|r| r.name).as_deref(),
            Some("ToDesk"),
            "bundle as an argument"
        );
        let other = n.resolve_program(
            "\"/Users/me/Library/Support/EgoUpdater.app/Contents/MacOS/EgoUpdater\" --wake",
        );
        assert_eq!(
            other,
            Some(Resolved {
                name: "EgoUpdater".to_owned(),
                icon: None
            }),
            "unknown bundle: its name, no icon",
        );
        assert_eq!(
            n.resolve_program("/usr/bin/true"),
            None,
            "no bundle, no name"
        );
        let path = Path::new("/Applications/WeChat.app/Contents/MacOS/WeChat");
        assert_eq!(
            n.resolve("wechat-helper-x", Some(path))
                .map(|r| r.name)
                .as_deref(),
            Some("WeChat"),
            "path"
        );
        assert_eq!(
            name_of(&n, "CherryStudio").as_deref(),
            Some("Cherry Studio"),
            "folder name"
        );
        assert_eq!(
            name_of(&n, "yuque-desktop").as_deref(),
            Some("语雀"),
            "product alias"
        );
        assert_eq!(
            name_of(&n, "Music"),
            None,
            "system apps are not matched by name"
        );
        assert_eq!(
            name_of(&n, "com.apple.helpviewer").as_deref(),
            Some("Tips"),
            "system apps match by id"
        );
    }

    fn junk_item(id: u32, name: &str) -> JunkItem {
        JunkItem {
            id,
            name: name.to_owned(),
            location: Location::Path {
                path: format!("/tmp/{name}"),
            },
            tag: None,
            bytes: 1,
            files: 1,
            modified: None,
            safety: Safety::Safe,
            needs_admin: false,
            app_running: false,
            ident: None,
            icon: None,
        }
    }

    #[test]
    fn annotate_junk_renames_only() {
        let mut report = JunkReport {
            groups: vec![
                JunkGroup {
                    kind: JunkKind::UserCache,
                    items: vec![
                        junk_item(0, "com.tencent.xinWeChat"),
                        junk_item(1, "Claude"),
                        junk_item(2, "CherryStudio/Shared Dictionary"),
                        junk_item(3, "com.soda.music.helper"),
                        junk_item(4, "Cherry Studio"),
                    ],
                },
                JunkGroup {
                    kind: JunkKind::Trash,
                    items: vec![junk_item(5, "com.tencent.xinWeChat")],
                },
            ],
            denied: Vec::new(),
        };
        annotate_junk(&mut report, &namer());
        let got: Vec<(u32, &str, Option<&str>, Option<&str>)> = report
            .groups
            .iter()
            .flat_map(|g| &g.items)
            .map(|i| (i.id, i.name.as_str(), i.ident.as_deref(), i.icon.as_deref()))
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    0,
                    "WeChat",
                    Some("com.tencent.xinWeChat"),
                    Some("/icons/WeChat.png")
                ),
                (1, "Claude", None, None),
                (
                    2,
                    "Cherry Studio/Shared Dictionary",
                    Some("CherryStudio/Shared Dictionary"),
                    Some("/icons/Cherry Studio.png"),
                ),
                (3, "Soda Music", Some("com.soda.music.helper"), None),
                (4, "Cherry Studio", None, Some("/icons/Cherry Studio.png")),
                (5, "com.tencent.xinWeChat", None, None),
            ],
            "ids and order kept; ident only when the name changed; skipped groups untouched",
        );
    }

    #[test]
    fn annotate_startup_names_launchd_labels() {
        let item = |name: &str, command: Option<&str>, kind: StartupKind| StartupItem {
            id: 0,
            name: name.to_owned(),
            command: command.map(str::to_owned),
            location: Location::Path {
                path: "/tmp/x.plist".to_owned(),
            },
            kind,
            scope: Scope::User,
            enabled: true,
            needs_admin: false,
            publisher: None,
            missing_target: false,
            ident: None,
            icon: None,
        };
        let mut items = [
            item(
                "com.oray.awesun.helper",
                Some("/Applications/AweSun.app/Contents/Helpers/AweSun_Helper -m server"),
                StartupKind::LaunchDaemon,
            ),
            item("com.youqu.todesk.service", None, StartupKind::LaunchDaemon),
            item(
                "com.citrolabs.keystone.agent",
                None,
                StartupKind::LaunchAgent,
            ),
            item(
                "Bob",
                Some("/Applications/WeChat.app"),
                StartupKind::LoginItem,
            ),
        ];
        annotate_startup_items(items.iter_mut(), &namer());
        let got: Vec<(&str, Option<&str>, Option<&str>)> = items
            .iter()
            .map(|r| (r.name.as_str(), r.ident.as_deref(), r.icon.as_deref()))
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    "AweSun",
                    Some("com.oray.awesun.helper"),
                    Some("/icons/AweSun.png")
                ),
                (
                    "ToDesk",
                    Some("com.youqu.todesk.service"),
                    Some("/icons/ToDesk.png")
                ),
                (
                    "Citrolabs Keystone",
                    Some("com.citrolabs.keystone.agent"),
                    None
                ),
                ("Bob", None, Some("/icons/WeChat.png")),
            ],
            "labels resolve by id, then program bundle, then prettified; login items keep names",
        );
    }
}
