//! System junk: OS and app caches, logs, crash reports, temp files, thumbnails, update and
//! package-manager caches, shader caches, mail downloads, device backups, old OS installs.
//! Browser and developer caches are left to their own areas (their paths are claimed).

use std::fs;
use std::path::Path;

use omc_proto::junk::{ItemTag, JunkKind};

use super::{Candidate, Cx, Os, Roots, join};

/// `~/Library/Caches` children that hold synced or account state: removable, but losing
/// them can trigger a full re-sync, so they are not preselected.
const MAC_REVIEW_CACHES: [&str; 6] = [
    "CloudKit",
    "com.apple.bird",
    "com.apple.cloudd",
    "com.apple.findmy.fmipcore",
    "FamilyCircle",
    "com.apple.HomeKit",
];

/// Folder names Chromium/Electron apps use for regenerable caches.
const APP_CACHE_DIRS: [(&str, ItemTag); 9] = [
    ("Cache", ItemTag::Cache),
    ("cache", ItemTag::Cache),
    ("Code Cache", ItemTag::CodeCache),
    ("GPUCache", ItemTag::GpuCache),
    ("DawnCache", ItemTag::GpuCache),
    ("DawnGraphiteCache", ItemTag::GpuCache),
    ("DawnWebGPUCache", ItemTag::GpuCache),
    ("GrShaderCache", ItemTag::GpuCache),
    ("ShaderCache", ItemTag::GpuCache),
];

pub(super) fn catalogue(cx: &mut Cx<'_>) -> Vec<Candidate> {
    let mut out = Vec::new();
    match cx.roots.os {
        Os::Mac => mac(cx, &mut out),
        Os::Windows => windows(cx, &mut out),
        Os::Linux => linux(cx, &mut out),
    }
    out
}

fn mac(cx: &mut Cx<'_>, out: &mut Vec<Candidate>) {
    let r = cx.roots;
    let s = cx.settings;

    // Per-app user caches, one item per folder.
    for e in cx.dirs(&r.cache) {
        let running = cx.running().owns(&e.name);
        let mut cand = Candidate::at(JunkKind::UserCache, e.path)
            .contents()
            .generic()
            .running(running, s);
        if MAC_REVIEW_CACHES.contains(&e.name.as_str()) {
            cand = cand.review();
        }
        out.push(cand);
    }
    let brew = r
        .env("HOMEBREW_CACHE")
        .map_or_else(|| r.cache.join("Homebrew"), Path::to_path_buf);
    out.push(Candidate::new(JunkKind::PackageCache, "Homebrew", brew).contents());

    // Sandboxed apps' caches.
    for e in cx.dirs(&r.home("Library/Containers")) {
        let running = cx.running().owns(&e.name);
        out.push(
            Candidate::new(
                JunkKind::UserCache,
                e.name,
                join(&e.path, "Data/Library/Caches"),
            )
            .contents()
            .generic()
            .running(running, s),
        );
    }
    app_caches(cx, &r.config, out);

    // System-wide caches.
    for e in cx.dirs(&r.sys("Library/Caches")) {
        out.push(
            Candidate::at(JunkKind::SystemCache, e.path)
                .contents()
                .generic(),
        );
    }

    // Logs and crash reports.
    logs(cx, &r.home("Library/Logs"), JunkKind::UserLog, out);
    logs(cx, &r.sys("Library/Logs"), JunkKind::SystemLog, out);
    out.push(
        Candidate::at(JunkKind::SystemLog, r.sys("private/var/log"))
            .contents()
            .aged(),
    );
    for dir in [
        r.home("Library/Logs/DiagnosticReports"),
        r.sys("Library/Logs/DiagnosticReports"),
        r.home("Library/Application Support/CrashReporter"),
    ] {
        out.push(Candidate::at(JunkKind::CrashReport, dir).contents());
    }

    // Temp files and the per-user cache folder next to `$TMPDIR` (`/var/folders/…/C`).
    out.push(
        Candidate::new(JunkKind::TempFiles, "TMPDIR", r.temp.clone())
            .contents()
            .aged(),
    );
    if r.temp.file_name().is_some_and(|n| n == "T")
        && let Some(user_dir) = r.temp.parent()
    {
        for e in cx.list(&user_dir.join("C")) {
            let kind = if e.name == "com.apple.QuickLook.thumbnailcache" {
                JunkKind::Thumbnails
            } else if e.name.starts_with("com.apple.metal") {
                JunkKind::ShaderCache
            } else {
                JunkKind::UserCache
            };
            let mut cand = Candidate::at(kind, e.path).aged();
            if e.dir {
                cand = cand.contents();
            }
            out.push(cand);
        }
    }

    // Updates, mail attachments, device backups and firmware.
    out.push(Candidate::at(JunkKind::UpdateCache, r.sys("Library/Updates")).contents());
    out.push(
        Candidate::new(
            JunkKind::MailDownloads,
            "Mail",
            r.home("Library/Containers/com.apple.mail/Data/Library/Mail Downloads"),
        )
        .contents()
        .review(),
    );
    for e in cx.dirs(&r.home("Library/Application Support/MobileSync/Backup")) {
        let name = device_name(&e.path.join("Info.plist")).unwrap_or(e.name);
        out.push(Candidate::new(JunkKind::DeviceBackups, name, e.path).review());
    }
    for device in ["iPhone", "iPad", "iPod"] {
        out.push(
            Candidate::at(
                JunkKind::DeviceBackups,
                r.home(&format!("Library/iTunes/{device} Software Updates")),
            )
            .contents(),
        );
    }
}

/// Children of a logs folder, one item each (`DiagnosticReports` is its own group).
fn logs(cx: &mut Cx<'_>, dir: &Path, kind: JunkKind, out: &mut Vec<Candidate>) {
    for e in cx.list(dir) {
        if e.name == "DiagnosticReports" {
            continue;
        }
        let mut cand = Candidate::at(kind, e.path).aged().generic();
        if e.dir {
            cand = cand.contents();
        }
        out.push(cand);
    }
}

/// Chromium/Electron-style cache folders of apps below `base` (`<app>/Cache`,
/// `<vendor>/<app>/GPUCache`…).
fn app_caches(cx: &mut Cx<'_>, base: &Path, out: &mut Vec<Candidate>) {
    let s = cx.settings;
    for app in cx.dirs(base) {
        let running = cx.running().owns(&app.name);
        let children = cx.dirs(&app.path);
        for child in children {
            if let Some(tag) = cache_tag(&child.name) {
                out.push(
                    Candidate::new(JunkKind::UserCache, app.name.clone(), child.path)
                        .tag(tag)
                        .generic()
                        .contents()
                        .running(running, s),
                );
                continue;
            }
            let child_running = running || cx.running().owns(&child.name);
            for grandchild in cx.dirs(&child.path) {
                if let Some(tag) = cache_tag(&grandchild.name) {
                    out.push(
                        Candidate::new(
                            JunkKind::UserCache,
                            format!("{}/{}", app.name, child.name),
                            grandchild.path,
                        )
                        .tag(tag)
                        .generic()
                        .contents()
                        .running(child_running, s),
                    );
                }
            }
        }
    }
}

fn cache_tag(name: &str) -> Option<ItemTag> {
    APP_CACHE_DIRS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, tag)| *tag)
}

/// `Device Name` of an iOS backup's `Info.plist` (XML plists only).
fn device_name(info: &Path) -> Option<String> {
    let text = fs::read_to_string(info).ok()?;
    let (_, after) = text.split_once("<key>Device Name</key>")?;
    let (_, value) = after.split_once("<string>")?;
    let (name, _) = value.split_once("</string>")?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

#[expect(clippy::too_many_lines, reason = "declarative per-location table")]
fn windows(cx: &mut Cx<'_>, out: &mut Vec<Candidate>) {
    let r = cx.roots;
    let local = &r.cache;
    let windir = &r.windir;

    // Temp files.
    out.push(
        Candidate::new(JunkKind::TempFiles, "%TEMP%", r.temp.clone())
            .contents()
            .aged(),
    );
    out.push(
        Candidate::new(JunkKind::TempFiles, "Windows\\Temp", windir.join("Temp"))
            .contents()
            .aged()
            .admin(),
    );

    // Crash dumps and Windows Error Reporting.
    out.push(Candidate::at(JunkKind::CrashReport, local.join("CrashDumps")).contents());
    for sub in ["ReportArchive", "ReportQueue"] {
        out.push(
            Candidate::at(
                JunkKind::ErrorReports,
                join(local, "Microsoft/Windows/WER").join(sub),
            )
            .contents(),
        );
    }
    for sub in ["ReportArchive", "ReportQueue", "Temp"] {
        out.push(
            Candidate::at(
                JunkKind::ErrorReports,
                join(&r.program_data, "Microsoft/Windows/WER").join(sub),
            )
            .contents()
            .admin(),
        );
    }
    for sub in ["Minidump", "LiveKernelReports"] {
        out.push(
            Candidate::at(JunkKind::ErrorReports, windir.join(sub))
                .contents()
                .admin(),
        );
    }
    out.push(Candidate::at(JunkKind::ErrorReports, windir.join("MEMORY.DMP")).admin());

    // Windows Update downloads and Delivery Optimization.
    out.push(
        Candidate::new(
            JunkKind::UpdateCache,
            "Windows Update",
            join(windir, "SoftwareDistribution/Download"),
        )
        .contents()
        .admin(),
    );
    out.push(
        Candidate::new(
            JunkKind::UpdateCache,
            "Delivery Optimization",
            join(
                windir,
                "ServiceProfiles/NetworkService/AppData/Local/Microsoft/Windows/DeliveryOptimization/Cache",
            ),
        )
        .contents()
        .admin(),
    );

    // Servicing logs.
    for sub in ["CBS", "DISM"] {
        out.push(
            Candidate::at(JunkKind::SystemLog, join(windir, "Logs").join(sub))
                .contents()
                .aged()
                .admin(),
        );
    }

    // Explorer thumbnail and icon caches (locked while Explorer runs).
    let explorer = cx.running().any(&["explorer"]);
    for e in cx.list(&join(local, "Microsoft/Windows/Explorer")) {
        let lower = e.name.to_lowercase();
        if !e.dir
            && (lower.starts_with("thumbcache_") || lower.starts_with("iconcache_"))
            && Path::new(&lower).extension().is_some_and(|e| e == "db")
        {
            let mut cand = Candidate::at(JunkKind::Thumbnails, e.path).review();
            cand.app_running = explorer;
            out.push(cand);
        }
    }

    // GPU shader caches.
    for rel in [
        "D3DSCache",
        "NVIDIA/DXCache",
        "NVIDIA/GLCache",
        "AMD/DxCache",
        "AMD/DxcCache",
        "AMD/GLCache",
        "AMD/VkCache",
        "Intel/ShaderCache",
    ] {
        out.push(Candidate::new(JunkKind::ShaderCache, rel, join(local, rel)).contents());
    }
    for rel in [
        "NVIDIA/PerDriverVersion/DXCache",
        "NVIDIA/PerDriverVersion/GLCache",
    ] {
        out.push(
            Candidate::new(
                JunkKind::ShaderCache,
                rel,
                join(&r.home, "AppData/LocalLow").join(rel),
            )
            .contents(),
        );
    }

    // App caches: legacy internet cache, Electron apps, Store apps' temp state.
    out.push(
        Candidate::new(
            JunkKind::UserCache,
            "INetCache",
            join(local, "Microsoft/Windows/INetCache"),
        )
        .contents(),
    );
    app_caches(cx, &r.config, out);
    app_caches(cx, local, out);
    for e in cx.dirs(&local.join("Packages")) {
        for rel in ["TempState", "AC/INetCache"] {
            out.push(
                Candidate::new(JunkKind::UserCache, e.name.clone(), join(&e.path, rel))
                    .contents()
                    .generic(),
            );
        }
    }

    // Package-manager caches.
    for rel in [
        "chocolatey/lib-bad",
        "chocolatey/lib-bkp",
        "ChocolateyHttpCache",
    ] {
        out.push(
            Candidate::new(JunkKind::PackageCache, rel, join(&r.program_data, rel))
                .contents()
                .admin(),
        );
    }
    out.push(Candidate::new(JunkKind::PackageCache, "Scoop", r.home("scoop/cache")).contents());

    // Previous Windows installations and upgrade leftovers.
    for name in ["Windows.old", "$Windows.~BT", "$Windows.~WS"] {
        out.push(
            Candidate::at(JunkKind::OldOsInstall, r.sys.join(name))
                .review()
                .admin(),
        );
    }
}

fn linux(cx: &mut Cx<'_>, out: &mut Vec<Candidate>) {
    let r = cx.roots;
    let s = cx.settings;

    // Per-app user caches.
    for e in cx.dirs(&r.cache) {
        let kind = match e.name.as_str() {
            "thumbnails" => JunkKind::Thumbnails,
            "mesa_shader_cache" | "mesa_shader_cache_db" | "radv_builtin_shaders" | "nvidia" => {
                JunkKind::ShaderCache
            }
            _ => JunkKind::UserCache,
        };
        let running = cx.running().owns(&e.name);
        let mut cand = Candidate::at(kind, e.path).contents().running(running, s);
        if kind == JunkKind::UserCache {
            cand = cand.generic();
        }
        out.push(cand);
    }
    for rel in [".nv/GLCache", ".nv/ComputeCache"] {
        out.push(Candidate::new(JunkKind::ShaderCache, rel, r.home(rel)).contents());
    }
    app_caches(cx, &r.config, out);
    for e in cx.dirs(&r.home(".var/app")) {
        let running = cx.running().owns(&e.name);
        out.push(
            Candidate::new(JunkKind::UserCache, e.name, e.path.join("cache"))
                .contents()
                .generic()
                .running(running, s),
        );
    }
    out.push(Candidate::at(
        JunkKind::UserLog,
        r.home(".xsession-errors.old"),
    ));

    // Package-manager caches.
    for (name, rel) in [
        ("apt", "var/cache/apt/archives"),
        ("dnf", "var/cache/dnf"),
        ("yum", "var/cache/yum"),
        ("pacman", "var/cache/pacman/pkg"),
        ("zypper", "var/cache/zypp/packages"),
        ("snapd", "var/lib/snapd/cache"),
    ] {
        out.push(Candidate::new(JunkKind::PackageCache, name, r.sys(rel)).contents());
    }

    // Rotated logs and the systemd journal.
    rotated_logs(cx, &r.sys("var/log"), 0, out);
    for e in cx.dirs(&r.sys("var/log/journal")) {
        out.push(
            Candidate::new(JunkKind::SystemLog, format!("journal/{}", e.name), e.path)
                .contents()
                .aged(),
        );
    }

    // Crash dumps.
    for rel in ["var/crash", "var/lib/systemd/coredump"] {
        out.push(Candidate::new(JunkKind::CrashReport, rel, r.sys(rel)).contents());
    }

    // The user's own old entries in the shared temp folders.
    for rel in ["tmp", "var/tmp"] {
        user_temp(cx, &r.sys(rel), out);
    }
}

/// Rotated or compressed log files (`syslog.1`, `*.gz`, `messages-20240101`) in `dir`
/// and one level below.
fn rotated_logs(cx: &mut Cx<'_>, dir: &Path, depth: u32, out: &mut Vec<Candidate>) {
    for e in cx.list(dir) {
        if e.dir {
            if depth == 0 && e.name != "journal" {
                rotated_logs(cx, &e.path, 1, out);
            }
            continue;
        }
        if is_rotated_log(&e.name) {
            out.push(Candidate::at(JunkKind::SystemLog, e.path).aged());
        }
    }
}

fn is_rotated_log(name: &str) -> bool {
    const COMPRESSED: [&str; 6] = [".gz", ".xz", ".bz2", ".zst", ".old", ".lz4"];
    if COMPRESSED.iter().any(|ext| name.ends_with(ext)) {
        return true;
    }
    // `name.N` (logrotate numbering) or `name-YYYYMMDD` (dateext).
    let numbered = name.rsplit_once('.').is_some_and(|(stem, n)| {
        !stem.is_empty() && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
    });
    let dated = name.rsplit_once('-').is_some_and(|(stem, d)| {
        !stem.is_empty() && d.len() == 8 && d.bytes().all(|b| b.is_ascii_digit())
    });
    numbered || dated
}

/// Session sockets and runtime folders that live in `/tmp` but must never be removed.
const TEMP_KEEP_PREFIXES: [&str; 13] = [
    ".X",
    ".ICE-unix",
    ".font-unix",
    ".XIM-unix",
    ".Test-unix",
    "tmux-",
    "ssh-",
    "systemd-private-",
    "snap-private-tmp",
    "pulse-",
    "dbus-",
    "krb5cc",
    "omc-",
];

/// Entries of a shared temp folder owned by the user and untouched for the minimum age.
fn user_temp(cx: &mut Cx<'_>, dir: &Path, out: &mut Vec<Candidate>) {
    let r = cx.roots;
    let min_age = super::min_age_secs(cx.settings);
    let cutoff =
        crate::paths::now_secs().saturating_sub(i64::try_from(min_age).unwrap_or(i64::MAX));
    for e in cx.list(dir) {
        if TEMP_KEEP_PREFIXES.iter().any(|p| e.name.starts_with(p)) {
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(&e.path) else {
            continue;
        };
        if !(meta.is_dir() || meta.is_file()) || !owned_by(&meta, r) {
            continue;
        }
        let old = meta
            .modified()
            .ok()
            .map(crate::paths::unix_secs)
            .is_some_and(|m| m <= cutoff);
        if old {
            out.push(Candidate::at(JunkKind::TempFiles, e.path).aged());
        }
    }
}

#[cfg(unix)]
fn owned_by(meta: &fs::Metadata, roots: &Roots) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    roots.uid.is_some_and(|uid| meta.uid() == uid)
}

#[cfg(not(unix))]
fn owned_by(_meta: &fs::Metadata, _roots: &Roots) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use omc_proto::jobs::Location;
    use omc_proto::settings::CleanSettings;

    use super::super::tests::{items, put, run, temp_dir};
    use super::super::{Running, browser, developer};
    use super::*;

    fn system_scan(roots: &Roots) -> Vec<(JunkKind, String, String)> {
        let settings = CleanSettings::default();
        let mut cx = Cx::new(roots, &settings);
        cx.set_running(Running::with(&["Slack"]));
        let cands = catalogue(&mut cx);
        let mut claims = browser::claimed(roots);
        claims.extend(developer::claimed(roots));
        let scanned = run(roots, &settings, cands, &claims);
        scanned
            .report
            .groups
            .iter()
            .flat_map(|g| {
                g.items.iter().map(move |i| {
                    let path = match &i.location {
                        Location::Path { path } => path.clone(),
                        other => other.display(),
                    };
                    (g.kind, i.name.clone(), path)
                })
            })
            .collect()
    }

    #[test]
    fn mac_catalogue_expands_caches_and_leaves_owned_paths() {
        let base = temp_dir("sys-mac");
        let roots = Roots::fake(&base, Os::Mac);
        put(&roots.home("Library/Caches/com.example.app/data"), 4096);
        put(
            &roots.home("Library/Caches/Homebrew/downloads/x.tar.gz"),
            4096,
        );
        put(
            &roots.home("Library/Caches/Google/Chrome/Default/Cache/f"),
            4096,
        );
        put(&roots.home("Library/Caches/Google/Other/f"), 4096);
        put(&roots.home("Library/Caches/pip/http/f"), 4096);
        put(&roots.home("Library/Logs/DiagnosticReports/a.ips"), 4096);
        put(
            &roots.home("Library/Application Support/Slack/Cache/f"),
            4096,
        );
        put(
            &roots.home("Library/Application Support/Google/Chrome/Default/GPUCache/f"),
            4096,
        );
        let got = system_scan(&roots);
        let has = |kind: JunkKind, name: &str| got.iter().any(|(k, n, _)| *k == kind && n == name);
        assert!(
            has(JunkKind::UserCache, "com.example.app"),
            "app cache: {got:?}"
        );
        assert!(has(JunkKind::PackageCache, "Homebrew"), "Homebrew: {got:?}");
        assert!(
            has(JunkKind::UserCache, "Google/Other"),
            "split vendor dir: {got:?}"
        );
        assert!(
            has(JunkKind::CrashReport, "DiagnosticReports"),
            "crash reports: {got:?}"
        );
        assert!(
            has(JunkKind::UserCache, "Slack"),
            "Electron app cache: {got:?}"
        );
        assert!(
            !got.iter()
                .any(|(_, _, p)| p.contains("Chrome") || p.contains("pip")),
            "browser and developer caches belong to other areas: {got:?}"
        );
    }

    #[test]
    fn windows_and_linux_catalogues_find_their_places() {
        let base = temp_dir("sys-win");
        let roots = Roots::fake(&base, Os::Windows);
        put(&roots.home("AppData/Local/D3DSCache/x"), 4096);
        put(
            &roots.home("AppData/Local/Microsoft/Windows/Explorer/thumbcache_256.db"),
            4096,
        );
        put(&roots.sys.join("Windows").join("MEMORY.DMP"), 4096);
        let got = system_scan(&roots);
        assert!(
            got.iter()
                .any(|(k, n, _)| *k == JunkKind::ShaderCache && n == "D3DSCache"),
            "shader cache: {got:?}"
        );
        assert!(
            got.iter().any(|(k, _, _)| *k == JunkKind::Thumbnails),
            "thumbnail db: {got:?}"
        );

        let base = temp_dir("sys-linux");
        let roots = Roots::fake(&base, Os::Linux);
        put(&roots.home(".cache/thumbnails/large/a.png"), 4096);
        put(&roots.home(".cache/someapp/blob"), 4096);
        put(&roots.home(".cache/google-chrome/Default/Cache/f"), 4096);
        put(&roots.sys("var/cache/apt/archives/x.deb"), 4096);
        put(&roots.sys("var/log/syslog.2.gz"), 4096);
        put(&roots.sys("var/log/syslog"), 4096);
        let settings = CleanSettings {
            junk_min_age_hours: 0,
            ..CleanSettings::default()
        };
        let mut cx = Cx::new(&roots, &settings);
        cx.set_running(Running::default());
        let cands = catalogue(&mut cx);
        let scanned = run(&roots, &settings, cands, &browser::claimed(&roots));
        let names: Vec<&str> = items(&scanned.report)
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        for want in ["thumbnails", "someapp", "apt", "syslog.2.gz"] {
            assert!(names.contains(&want), "{want} in {names:?}");
        }
        assert!(!names.contains(&"syslog"), "live log untouched: {names:?}");
        assert!(
            !names.contains(&"google-chrome"),
            "browser cache left out: {names:?}"
        );
    }

    #[test]
    fn rotated_log_names() {
        for name in [
            "syslog.1",
            "kern.log.2.gz",
            "messages-20240101",
            "Xorg.0.log.old",
        ] {
            assert!(is_rotated_log(name), "{name} is rotated");
        }
        for name in ["syslog", "Xorg.0.log", "dpkg.log", "boot.log-x"] {
            assert!(!is_rotated_log(name), "{name} is live");
        }
    }
}
