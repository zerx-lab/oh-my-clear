//! Browser data: caches of every Chromium-family browser, Firefox-family browser and
//! Safari, per profile; cookies, history, site data and sessions only when the matching
//! setting is on (they sign the user out / lose tabs). Profiles are discovered from the
//! browser's own lists (Chromium `Local State`, Firefox `profiles.ini`) plus a folder scan.
//! A running browser's caches are listed but not preselected; its other data is left
//! alone entirely (removing it under a running browser corrupts the profile).

use std::fs;
use std::path::{Path, PathBuf};

use omc_proto::junk::{Browser, ItemTag, JunkKind};

use super::{Candidate, Cx, Os, Roots, join, json};

/// A Chromium-family browser installation.
#[derive(Debug, Clone)]
struct Chromium {
    browser: Browser,
    /// Prefix for item names of secondary channels (`Chrome Beta`).
    variant: Option<&'static str>,
    /// The "User Data" folder (profiles, `Local State`).
    data: PathBuf,
    /// Where the disk cache lives when separate (macOS `~/Library/Caches/…`, Linux
    /// `~/.cache/…`, Opera on Windows); same relative profile layout.
    cache: Option<PathBuf>,
    procs: &'static [&'static str],
}

/// A Firefox-family installation (`profiles.ini` folder plus local cache root).
#[derive(Debug, Clone)]
struct Firefox {
    data: PathBuf,
    cache: PathBuf,
    procs: &'static [&'static str],
}

const CHROME: &[&str] = &[
    "Google Chrome",
    "Google Chrome Beta",
    "Google Chrome Canary",
    "chrome",
    "google-chrome",
];
const CHROMIUM: &[&str] = &[
    "Chromium",
    "chromium",
    "chromium-browse",
    "chromium-browser",
];
const EDGE: &[&str] = &["Microsoft Edge", "msedge"];
const BRAVE: &[&str] = &["Brave Browser", "brave"];
const VIVALDI: &[&str] = &["Vivaldi", "vivaldi", "vivaldi-bin"];
const OPERA: &[&str] = &["Opera", "Opera GX", "opera"];
const ARC: &[&str] = &["Arc"];
const YANDEX: &[&str] = &["Yandex", "yandex-browser", "yandex_browser"];
const FIREFOX: &[&str] = &["firefox", "firefox-bin", "firefox-esr"];
const LIBREWOLF: &[&str] = &["librewolf", "librewolf-bin"];
const WATERFOX: &[&str] = &["waterfox", "waterfox-bin"];
const ZEN: &[&str] = &["zen", "zen-bin"];
const FLOORP: &[&str] = &["floorp", "floorp-bin"];
const SAFARI: &[&str] = &["Safari"];

/// Per-profile regenerable caches (relative to the profile, in the data or cache root).
const CHROMIUM_CACHES: [(&str, ItemTag); 9] = [
    ("Cache", ItemTag::Cache),
    ("Media Cache", ItemTag::Cache),
    ("Code Cache", ItemTag::CodeCache),
    ("GPUCache", ItemTag::GpuCache),
    ("DawnCache", ItemTag::GpuCache),
    ("DawnGraphiteCache", ItemTag::GpuCache),
    ("DawnWebGPUCache", ItemTag::GpuCache),
    ("Service Worker/CacheStorage", ItemTag::ServiceWorker),
    ("Service Worker/ScriptCache", ItemTag::ServiceWorker),
];

/// Caches shared by all profiles, directly in the user data folder.
const CHROMIUM_SHARED_CACHES: [&str; 3] = ["GrShaderCache", "ShaderCache", "GraphiteDawnCache"];

const CHROMIUM_COOKIES: [&str; 4] = [
    "Cookies",
    "Cookies-journal",
    "Network/Cookies",
    "Network/Cookies-journal",
];
const CHROMIUM_HISTORY: [&str; 5] = [
    "History",
    "History-journal",
    "Visited Links",
    "Top Sites",
    "Top Sites-journal",
];
const CHROMIUM_SITE_DATA: [&str; 6] = [
    "Local Storage",
    "IndexedDB",
    "Session Storage",
    "databases",
    "File System",
    "WebStorage",
];
const CHROMIUM_SESSIONS: [&str; 5] = [
    "Sessions",
    "Current Session",
    "Current Tabs",
    "Last Session",
    "Last Tabs",
];

const FIREFOX_CACHES: [(&str, ItemTag); 5] = [
    ("cache2", ItemTag::Cache),
    ("thumbnails", ItemTag::Cache),
    ("jumpListCache", ItemTag::Cache),
    ("startupCache", ItemTag::CodeCache),
    ("shader-cache", ItemTag::GpuCache),
];
const FIREFOX_COOKIES: [&str; 3] = ["cookies.sqlite", "cookies.sqlite-wal", "cookies.sqlite-shm"];
const FIREFOX_SITE_DATA: [&str; 3] = [
    "storage/default",
    "webappsstore.sqlite",
    "webappsstore.sqlite-wal",
];
const FIREFOX_SESSIONS: [&str; 2] = ["sessionstore.jsonlz4", "sessionstore-backups"];

#[expect(clippy::too_many_lines, reason = "declarative per-OS browser table")]
fn chromium_installs(r: &Roots) -> Vec<Chromium> {
    let spec = |browser, variant, data: PathBuf, cache: Option<PathBuf>, procs| Chromium {
        browser,
        variant,
        data,
        cache,
        procs,
    };
    match r.os {
        Os::Mac => [
            (Browser::Chrome, None, "Google/Chrome", CHROME),
            (
                Browser::Chrome,
                Some("Chrome Beta"),
                "Google/Chrome Beta",
                CHROME,
            ),
            (
                Browser::Chrome,
                Some("Chrome Canary"),
                "Google/Chrome Canary",
                CHROME,
            ),
            (Browser::Chromium, None, "Chromium", CHROMIUM),
            (Browser::Edge, None, "Microsoft Edge", EDGE),
            (Browser::Brave, None, "BraveSoftware/Brave-Browser", BRAVE),
            (Browser::Vivaldi, None, "Vivaldi", VIVALDI),
            (Browser::Opera, None, "com.operasoftware.Opera", OPERA),
            (
                Browser::Opera,
                Some("Opera GX"),
                "com.operasoftware.OperaGX",
                OPERA,
            ),
            (Browser::Arc, None, "Arc/User Data", ARC),
            (Browser::Yandex, None, "Yandex/YandexBrowser", YANDEX),
        ]
        .into_iter()
        .map(|(b, v, rel, procs)| {
            spec(b, v, join(&r.config, rel), Some(join(&r.cache, rel)), procs)
        })
        .collect(),
        Os::Windows => {
            let mut out: Vec<Chromium> = [
                (Browser::Chrome, None, "Google/Chrome/User Data", CHROME),
                (
                    Browser::Chrome,
                    Some("Chrome Beta"),
                    "Google/Chrome Beta/User Data",
                    CHROME,
                ),
                (
                    Browser::Chrome,
                    Some("Chrome Canary"),
                    "Google/Chrome SxS/User Data",
                    CHROME,
                ),
                (Browser::Chromium, None, "Chromium/User Data", CHROMIUM),
                (Browser::Edge, None, "Microsoft/Edge/User Data", EDGE),
                (
                    Browser::Brave,
                    None,
                    "BraveSoftware/Brave-Browser/User Data",
                    BRAVE,
                ),
                (Browser::Vivaldi, None, "Vivaldi/User Data", VIVALDI),
                (
                    Browser::Yandex,
                    None,
                    "Yandex/YandexBrowser/User Data",
                    YANDEX,
                ),
            ]
            .into_iter()
            .map(|(b, v, rel, procs)| spec(b, v, join(&r.cache, rel), None, procs))
            .collect();
            for (variant, rel) in [
                (None, "Opera Software/Opera Stable"),
                (Some("Opera GX"), "Opera Software/Opera GX Stable"),
            ] {
                out.push(spec(
                    Browser::Opera,
                    variant,
                    join(&r.config, rel),
                    Some(join(&r.cache, rel)),
                    OPERA,
                ));
            }
            out
        }
        Os::Linux => {
            let mut out: Vec<Chromium> = [
                (Browser::Chrome, None, "google-chrome", CHROME),
                (
                    Browser::Chrome,
                    Some("Chrome Beta"),
                    "google-chrome-beta",
                    CHROME,
                ),
                (
                    Browser::Chrome,
                    Some("Chrome Dev"),
                    "google-chrome-unstable",
                    CHROME,
                ),
                (Browser::Chromium, None, "chromium", CHROMIUM),
                (Browser::Edge, None, "microsoft-edge", EDGE),
                (Browser::Brave, None, "BraveSoftware/Brave-Browser", BRAVE),
                (Browser::Vivaldi, None, "vivaldi", VIVALDI),
                (Browser::Opera, None, "opera", OPERA),
                (Browser::Yandex, None, "yandex-browser", YANDEX),
            ]
            .into_iter()
            .map(|(b, v, rel, procs)| {
                spec(b, v, join(&r.config, rel), Some(join(&r.cache, rel)), procs)
            })
            .collect();
            out.push(spec(
                Browser::Chromium,
                Some("Chromium (Snap)"),
                r.home("snap/chromium/common/chromium"),
                Some(r.home("snap/chromium/common/.cache/chromium")),
                CHROMIUM,
            ));
            for (browser, app, rel, procs) in [
                (
                    Browser::Chrome,
                    "com.google.Chrome",
                    "google-chrome",
                    CHROME,
                ),
                (
                    Browser::Chromium,
                    "org.chromium.Chromium",
                    "chromium",
                    CHROMIUM,
                ),
                (
                    Browser::Brave,
                    "com.brave.Browser",
                    "BraveSoftware/Brave-Browser",
                    BRAVE,
                ),
                (Browser::Edge, "com.microsoft.Edge", "microsoft-edge", EDGE),
            ] {
                let base = r.home(".var/app").join(app);
                out.push(spec(
                    browser,
                    Some("Flatpak"),
                    join(&base.join("config"), rel),
                    Some(join(&base.join("cache"), rel)),
                    procs,
                ));
            }
            out
        }
    }
}

fn firefox_installs(r: &Roots) -> Vec<Firefox> {
    let spec = |data: PathBuf, cache: PathBuf, procs| Firefox { data, cache, procs };
    match r.os {
        Os::Mac => [
            ("Firefox", FIREFOX),
            ("librewolf", LIBREWOLF),
            ("Waterfox", WATERFOX),
            ("zen", ZEN),
            ("Floorp", FLOORP),
        ]
        .into_iter()
        .map(|(rel, procs)| spec(join(&r.config, rel), join(&r.cache, rel), procs))
        .collect(),
        Os::Windows => [
            ("Mozilla/Firefox", FIREFOX),
            ("librewolf", LIBREWOLF),
            ("Waterfox", WATERFOX),
            ("zen", ZEN),
            ("Floorp", FLOORP),
        ]
        .into_iter()
        .map(|(rel, procs)| spec(join(&r.config, rel), join(&r.cache, rel), procs))
        .collect(),
        Os::Linux => {
            let mut out: Vec<Firefox> = [
                (".mozilla/firefox", "mozilla/firefox", FIREFOX),
                (".librewolf", "librewolf", LIBREWOLF),
                (".waterfox", "waterfox", WATERFOX),
                (".zen", "zen", ZEN),
                (".floorp", "floorp", FLOORP),
            ]
            .into_iter()
            .map(|(data, cache, procs)| spec(r.home(data), join(&r.cache, cache), procs))
            .collect();
            out.push(spec(
                join(&r.config, "mozilla/firefox"),
                join(&r.cache, "mozilla/firefox"),
                FIREFOX,
            ));
            out.push(spec(
                r.home("snap/firefox/common/.mozilla/firefox"),
                r.home("snap/firefox/common/.cache/mozilla/firefox"),
                FIREFOX,
            ));
            out.push(spec(
                r.home(".var/app/org.mozilla.firefox/.mozilla/firefox"),
                r.home(".var/app/org.mozilla.firefox/cache/mozilla/firefox"),
                FIREFOX,
            ));
            out
        }
    }
}

/// Safari's cache folders (macOS).
fn safari_caches(r: &Roots) -> Vec<PathBuf> {
    if r.os != Os::Mac {
        return Vec::new();
    }
    vec![
        r.cache.join("com.apple.Safari"),
        r.home("Library/Containers/com.apple.Safari/Data/Library/Caches"),
    ]
}

/// Every browser folder: other areas' broad globs must not report them.
pub(super) fn claimed(r: &Roots) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for c in chromium_installs(r) {
        out.push(c.data);
        out.extend(c.cache);
    }
    for f in firefox_installs(r) {
        out.push(f.data);
        out.push(f.cache);
    }
    out.extend(safari_caches(r));
    out
}

pub(super) fn catalogue(cx: &mut Cx<'_>) -> Vec<Candidate> {
    let mut out = Vec::new();
    for install in chromium_installs(cx.roots) {
        chromium(cx, &install, &mut out);
    }
    for install in firefox_installs(cx.roots) {
        firefox(cx, &install, &mut out);
    }
    if cx.roots.os == Os::Mac {
        safari(cx, &mut out);
    }
    out
}

/// Data types a user opted into, with the file names each covers.
fn opted<'n>(cx: &Cx<'_>, sets: [(&'n [&'n str], ItemTag); 4]) -> Vec<(&'n [&'n str], ItemTag)> {
    let s = cx.settings;
    sets.into_iter()
        .filter(|(_, tag)| match tag {
            ItemTag::Cookies => s.browser_cookies,
            ItemTag::History => s.browser_history,
            ItemTag::SiteData => s.browser_site_data,
            ItemTag::Sessions => s.browser_sessions,
            _ => false,
        })
        .collect()
}

/// Pushes cache items (preselected unless the browser runs) and opted-in data items
/// (never preselected; skipped while the browser runs when `skip_running_apps`).
#[expect(
    clippy::too_many_arguments,
    reason = "one call site per browser family; a struct would only rename the arguments"
)]
fn profile_items(
    cx: &Cx<'_>,
    kind: JunkKind,
    name: &str,
    running: bool,
    cache_bases: &[&Path],
    data_base: &Path,
    caches: &[(&str, ItemTag)],
    data: &[(&[&str], ItemTag)],
    out: &mut Vec<Candidate>,
) {
    let s = cx.settings;
    for (rel, tag) in caches {
        for base in cache_bases {
            out.push(
                Candidate::new(kind, name, join(base, rel))
                    .contents()
                    .tag(*tag)
                    .running(running, s),
            );
        }
    }
    if running && s.skip_running_apps {
        return;
    }
    for (files, tag) in data {
        for rel in *files {
            let mut cand = Candidate::new(kind, name, join(data_base, rel))
                .tag(*tag)
                .review();
            cand.app_running = running;
            out.push(cand);
        }
    }
}

fn chromium(cx: &mut Cx<'_>, install: &Chromium, out: &mut Vec<Candidate>) {
    if !install.data.is_dir() && !install.cache.as_ref().is_some_and(|c| c.is_dir()) {
        return;
    }
    let kind = JunkKind::Browser(install.browser);
    let running = cx.running().any(install.procs);
    let data_sets = opted(
        cx,
        [
            (&CHROMIUM_COOKIES, ItemTag::Cookies),
            (&CHROMIUM_HISTORY, ItemTag::History),
            (&CHROMIUM_SITE_DATA, ItemTag::SiteData),
            (&CHROMIUM_SESSIONS, ItemTag::Sessions),
        ],
    );
    let label = install
        .variant
        .unwrap_or_else(|| browser_label(install.browser));
    for (dir, display) in chromium_profiles(cx, &install.data) {
        let profile = if dir.is_empty() {
            install.data.clone()
        } else {
            install.data.join(&dir)
        };
        let cache_profile = install.cache.as_ref().map(|c| {
            if dir.is_empty() {
                c.clone()
            } else {
                c.join(&dir)
            }
        });
        let name = match (install.variant, display) {
            (Some(variant), Some(display)) => format!("{variant}/{display}"),
            (None, Some(display)) => display,
            (_, None) => label.to_owned(),
        };
        let mut bases = vec![profile.as_path()];
        if let Some(cache) = cache_profile.as_deref() {
            bases.push(cache);
        }
        profile_items(
            cx,
            kind,
            &name,
            running,
            &bases,
            &profile,
            &CHROMIUM_CACHES,
            &data_sets,
            out,
        );
    }
    for rel in CHROMIUM_SHARED_CACHES {
        let name = match install.variant {
            Some(variant) => format!("{variant}/{rel}"),
            None => rel.to_owned(),
        };
        out.push(
            Candidate::new(kind, name, install.data.join(rel))
                .contents()
                .tag(ItemTag::GpuCache)
                .running(running, cx.settings),
        );
    }
}

/// Profile folders of a Chromium user data folder with their display names: listed in
/// `Local State` (`profile.info_cache`) or recognised by a `Preferences` file. A user data
/// folder that is itself a profile (old Opera layout) yields `("", None)`.
fn chromium_profiles(cx: &mut Cx<'_>, data: &Path) -> Vec<(String, Option<String>)> {
    let names: Vec<(String, String)> = fs::read_to_string(data.join("Local State"))
        .ok()
        .and_then(|text| json::parse(&text))
        .map(|state| {
            state
                .get("profile")
                .and_then(|p| p.get("info_cache"))
                .map(|cache| {
                    cache
                        .members()
                        .iter()
                        .filter_map(|(dir, info)| {
                            let name = info.get("name").and_then(json::Value::as_str)?;
                            Some((dir.clone(), name.to_owned()))
                        })
                        .collect()
                })
                .unwrap_or_default()
        })
        .unwrap_or_default();
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    for e in cx.dirs(data) {
        let listed = names.iter().find(|(dir, _)| *dir == e.name);
        if listed.is_some() || e.path.join("Preferences").is_file() {
            let display = listed.map_or_else(|| e.name.clone(), |(_, n)| n.clone());
            out.push((e.name, Some(display)));
        }
    }
    if data.join("Preferences").is_file() {
        out.push((String::new(), None));
    }
    out
}

fn browser_label(browser: Browser) -> &'static str {
    match browser {
        Browser::Chrome => "Chrome",
        Browser::Chromium => "Chromium",
        Browser::Edge => "Edge",
        Browser::Brave => "Brave",
        Browser::Vivaldi => "Vivaldi",
        Browser::Opera => "Opera",
        Browser::Arc => "Arc",
        Browser::Firefox => "Firefox",
        Browser::Safari => "Safari",
        Browser::Yandex => "Yandex",
    }
}

fn firefox(cx: &mut Cx<'_>, install: &Firefox, out: &mut Vec<Candidate>) {
    let Some(profiles) = firefox_profiles(&install.data) else {
        return;
    };
    let kind = JunkKind::Browser(Browser::Firefox);
    let running = cx.running().any(install.procs);
    let data_sets = opted(
        cx,
        [
            (&FIREFOX_COOKIES, ItemTag::Cookies),
            // Firefox history lives in `places.sqlite` together with bookmarks: never
            // offered at file level.
            (&[], ItemTag::History),
            (&FIREFOX_SITE_DATA, ItemTag::SiteData),
            (&FIREFOX_SESSIONS, ItemTag::Sessions),
        ],
    );
    for profile in profiles {
        let (dir, cache) = match &profile.relative {
            Some(rel) => (join(&install.data, rel), join(&install.cache, rel)),
            None => (profile.path.clone(), profile.path.clone()),
        };
        profile_items(
            cx,
            kind,
            &profile.name,
            running,
            &[cache.as_path(), dir.as_path()],
            &dir,
            &FIREFOX_CACHES,
            &data_sets,
            out,
        );
    }
}

/// A Firefox profile from `profiles.ini`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FirefoxProfile {
    name: String,
    /// `Path=` when `IsRelative=1` (relative to the `profiles.ini` folder, `/`-separated).
    relative: Option<String>,
    /// Absolute path (for `IsRelative=0`).
    path: PathBuf,
}

/// Profiles listed in `<data>/profiles.ini`; `None` when there is no such file.
fn firefox_profiles(data: &Path) -> Option<Vec<FirefoxProfile>> {
    let text = fs::read_to_string(data.join("profiles.ini")).ok()?;
    Some(parse_profiles_ini(&text))
}

fn parse_profiles_ini(text: &str) -> Vec<FirefoxProfile> {
    #[derive(Default)]
    struct Section {
        name: Option<String>,
        path: Option<String>,
        relative: bool,
    }
    fn close(section: Option<Section>, out: &mut Vec<FirefoxProfile>) {
        let Some(section) = section else { return };
        let Some(path) = section.path.filter(|p| !p.is_empty()) else {
            return;
        };
        let path = path.replace('\\', "/");
        let name = section
            .name
            .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(path.as_str()).to_owned());
        out.push(if section.relative {
            FirefoxProfile {
                name,
                path: PathBuf::new(),
                relative: Some(path),
            }
        } else {
            FirefoxProfile {
                name,
                path: PathBuf::from(path),
                relative: None,
            }
        });
    }
    let mut out = Vec::new();
    let mut current: Option<Section> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            close(current.take(), &mut out);
            if header.starts_with("Profile") {
                current = Some(Section {
                    relative: true,
                    ..Section::default()
                });
            }
            continue;
        }
        let (Some(section), Some((key, value))) = (current.as_mut(), line.split_once('=')) else {
            continue;
        };
        match key.trim() {
            "Name" => section.name = Some(value.trim().to_owned()),
            "Path" => section.path = Some(value.trim().to_owned()),
            "IsRelative" => section.relative = value.trim() != "0",
            _ => {}
        }
    }
    close(current, &mut out);
    out
}

fn safari(cx: &mut Cx<'_>, out: &mut Vec<Candidate>) {
    let r = cx.roots;
    let kind = JunkKind::Browser(Browser::Safari);
    let running = cx.running().any(SAFARI);
    let data: [(&[&str], ItemTag); 4] = [
        (
            &[
                "Library/Containers/com.apple.Safari/Data/Library/Cookies/Cookies.binarycookies",
                "Library/Cookies/Cookies.binarycookies",
            ],
            ItemTag::Cookies,
        ),
        (
            &[
                "Library/Safari/History.db",
                "Library/Safari/History.db-wal",
                "Library/Safari/History.db-shm",
                "Library/Safari/History.db-lock",
            ],
            ItemTag::History,
        ),
        (
            &[
                "Library/Safari/LocalStorage",
                "Library/Safari/Databases",
                "Library/Containers/com.apple.Safari/Data/Library/WebKit/WebsiteData",
            ],
            ItemTag::SiteData,
        ),
        (
            &[
                "Library/Safari/LastSession.plist",
                "Library/Containers/com.apple.Safari/Data/Library/Safari/LastSession.plist",
            ],
            ItemTag::Sessions,
        ),
    ];
    let data_sets = opted(cx, data);
    let caches = safari_caches(r);
    let bases: Vec<&Path> = caches.iter().map(PathBuf::as_path).collect();
    // Both cache folders are the "profile"; `rel` "" means the folder itself.
    profile_items(
        cx,
        kind,
        "Safari",
        running,
        &bases,
        &r.home,
        &[("", ItemTag::Cache)],
        &data_sets,
        out,
    );
}

#[cfg(test)]
mod tests {
    use omc_proto::jobs::Location;
    use omc_proto::junk::Safety;
    use omc_proto::settings::CleanSettings;

    use super::super::Running;
    use super::super::tests::{items, put, run, temp_dir};
    use super::*;

    fn chrome_fixture(roots: &Roots) {
        let data = roots.home("Library/Application Support/Google/Chrome");
        put(&data.join("Local State"), 0);
        assert!(
            fs::write(
                data.join("Local State"),
                r#"{"profile":{"info_cache":{"Default":{"name":"Work"},"Profile 1":{"name":"Home"}}}}"#,
            )
            .is_ok(),
            "write Local State"
        );
        for profile in ["Default", "Profile 1"] {
            put(&data.join(profile).join("Preferences"), 10);
            put(&data.join(profile).join("Code Cache/js/a"), 4096);
            put(&data.join(profile).join("Cookies"), 4096);
            put(&data.join(profile).join("History"), 4096);
            put(&data.join(profile).join("Local Storage/leveldb/a"), 4096);
        }
        put(&data.join("GrShaderCache/a"), 4096);
        put(
            &roots.home("Library/Caches/Google/Chrome/Default/Cache/Cache_Data/a"),
            4096,
        );
    }

    fn scan(
        roots: &Roots,
        settings: &CleanSettings,
        running: &[&str],
    ) -> Vec<(String, Option<ItemTag>, Safety, bool, String)> {
        let mut cx = Cx::new(roots, settings);
        cx.set_running(Running::with(running));
        let cands = catalogue(&mut cx);
        let scanned = run(roots, settings, cands, &[]);
        items(&scanned.report)
            .into_iter()
            .map(|i| {
                let path = match &i.location {
                    Location::Path { path } => path.clone(),
                    other => other.display(),
                };
                (i.name.clone(), i.tag, i.safety, i.app_running, path)
            })
            .collect()
    }

    #[test]
    fn chromium_profiles_use_local_state_names_and_opt_in_data() {
        let base = temp_dir("browser-chromium");
        let roots = Roots::fake(&base, Os::Mac);
        chrome_fixture(&roots);

        let got = scan(&roots, &CleanSettings::default(), &[]);
        let has =
            |name: &str, tag: ItemTag| got.iter().any(|(n, t, ..)| n == name && *t == Some(tag));
        assert!(
            has("Work", ItemTag::Cache),
            "disk cache in ~/Library/Caches: {got:?}"
        );
        assert!(has("Work", ItemTag::CodeCache), "code cache: {got:?}");
        assert!(
            has("Home", ItemTag::CodeCache),
            "second profile named from Local State: {got:?}"
        );
        assert!(
            has("GrShaderCache", ItemTag::GpuCache),
            "shared GPU cache: {got:?}"
        );
        assert!(
            !got.iter().any(|(_, t, ..)| matches!(
                t,
                Some(ItemTag::Cookies | ItemTag::History | ItemTag::SiteData)
            )),
            "opt-in data excluded by default: {got:?}"
        );
        assert!(
            got.iter().all(|(_, _, s, ..)| *s == Safety::Safe),
            "caches preselected"
        );

        let opted = CleanSettings {
            browser_cookies: true,
            browser_history: true,
            ..CleanSettings::default()
        };
        let got = scan(&roots, &opted, &[]);
        let cookies: Vec<_> = got
            .iter()
            .filter(|(_, t, ..)| *t == Some(ItemTag::Cookies))
            .collect();
        assert_eq!(cookies.len(), 2, "one cookie store per profile: {got:?}");
        assert!(
            cookies.iter().all(|(_, _, s, ..)| *s == Safety::Review),
            "cookies never preselected"
        );
        assert!(
            got.iter().any(|(_, t, ..)| *t == Some(ItemTag::History)),
            "history offered when enabled: {got:?}"
        );
        assert!(
            !got.iter().any(|(_, t, ..)| *t == Some(ItemTag::SiteData)),
            "site data still off: {got:?}"
        );

        let running = scan(&roots, &opted, &["Google Chrome"]);
        assert!(
            running.iter().all(|(_, t, s, r, _)| *r
                && *s == Safety::Review
                && matches!(
                    t,
                    Some(ItemTag::Cache | ItemTag::CodeCache | ItemTag::GpuCache)
                )),
            "running browser: caches only, marked and not preselected: {running:?}"
        );
        assert!(!running.is_empty(), "caches still listed");
    }

    #[test]
    fn firefox_profiles_come_from_profiles_ini() {
        let parsed = parse_profiles_ini(
            "[General]\nStartWithLastProfile=1\n\n[Profile1]\nName=dev\nIsRelative=0\nPath=/abs/dev\n\n[Profile0]\nName=default-release\nIsRelative=1\nPath=Profiles/abc.default-release\nDefault=1\n\n[Install4F96D1932A9F858E]\nDefault=Profiles/abc.default-release\n",
        );
        assert_eq!(
            parsed,
            vec![
                FirefoxProfile {
                    name: "dev".to_owned(),
                    relative: None,
                    path: PathBuf::from("/abs/dev"),
                },
                FirefoxProfile {
                    name: "default-release".to_owned(),
                    relative: Some("Profiles/abc.default-release".to_owned()),
                    path: PathBuf::new(),
                },
            ],
            "profiles parsed, install sections ignored"
        );

        let base = temp_dir("browser-firefox");
        let roots = Roots::fake(&base, Os::Windows);
        let data = roots.home("AppData/Roaming/Mozilla/Firefox");
        put(&data.join("profiles.ini"), 0);
        assert!(
            fs::write(
                data.join("profiles.ini"),
                "[Profile0]\nName=default-release\nIsRelative=1\nPath=Profiles/abc.default-release\n",
            )
            .is_ok(),
            "write profiles.ini"
        );
        let profile = data.join("Profiles/abc.default-release");
        put(&profile.join("places.sqlite"), 4096);
        put(&profile.join("cookies.sqlite"), 4096);
        put(&profile.join("sessionstore.jsonlz4"), 4096);
        put(
            &roots.home(
                "AppData/Local/Mozilla/Firefox/Profiles/abc.default-release/cache2/entries/a",
            ),
            4096,
        );
        let all = CleanSettings {
            browser_cookies: true,
            browser_history: true,
            browser_site_data: true,
            browser_sessions: true,
            ..CleanSettings::default()
        };
        let got = scan(&roots, &all, &[]);
        assert!(
            got.iter()
                .any(|(n, t, ..)| n == "default-release" && *t == Some(ItemTag::Cache)),
            "cache2 in the local profile folder: {got:?}"
        );
        assert!(
            got.iter().any(|(_, t, ..)| *t == Some(ItemTag::Cookies)),
            "cookies when enabled: {got:?}"
        );
        assert!(
            !got.iter().any(|(.., p)| p.contains("places.sqlite")),
            "places.sqlite (bookmarks) is never offered: {got:?}"
        );
    }
}
