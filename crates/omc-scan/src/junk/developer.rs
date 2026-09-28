//! Developer junk: Xcode data, language package-manager caches, IDE caches, container/VM
//! tool caches, and build output inside projects (`node_modules`, `target`…).
//!
//! Projects are found with a directory-only parallel walk (no per-file `stat`): marker
//! files (`package.json`, `Cargo.toml`…) are seen in the parent's listing, found artifacts
//! are never entered, and hidden folders, app bundles and `~/Library`/`AppData` are
//! skipped. A project's age is its folder's mtime and its top-level entries' mtimes.

use std::fs;
use std::path::{Path, PathBuf};

use omc_proto::jobs::Denied;
use omc_proto::junk::{ItemTag, JunkKind};
use parking_lot::Mutex;

use super::{Candidate, Cx, Os, Roots, file_name, join, json};
use crate::ctx::JobCtx;
use crate::walk::{WalkOptions, Walker};
use crate::{errors, paths};

/// VS Code-family editors: folder name (in the config base) and process names.
const EDITORS: [(&str, &[&str]); 5] = [
    ("Code", &["Code", "Visual Studio Code", "code"]),
    (
        "Code - Insiders",
        &[
            "Code - Insiders",
            "Visual Studio Code - Insiders",
            "code-insiders",
        ],
    ),
    ("Cursor", &["Cursor", "cursor"]),
    ("VSCodium", &["VSCodium", "codium"]),
    ("Windsurf", &["Windsurf", "windsurf"]),
];

/// Regenerable folders of a VS Code-family editor.
const EDITOR_CACHES: [(&str, ItemTag); 10] = [
    ("Cache", ItemTag::Cache),
    ("CachedData", ItemTag::CodeCache),
    ("CachedExtensionVSIXs", ItemTag::Packages),
    ("CachedProfilesData", ItemTag::Cache),
    ("Code Cache", ItemTag::CodeCache),
    ("GPUCache", ItemTag::GpuCache),
    ("DawnGraphiteCache", ItemTag::GpuCache),
    ("DawnWebGPUCache", ItemTag::GpuCache),
    ("Service Worker/CacheStorage", ItemTag::ServiceWorker),
    ("logs", ItemTag::Logs),
];

/// JetBrains product folder prefixes and their launcher process names.
const JETBRAINS: [(&str, &str); 15] = [
    ("IntelliJIdea", "idea"),
    ("IdeaIC", "idea"),
    ("PyCharm", "pycharm"),
    ("WebStorm", "webstorm"),
    ("CLion", "clion"),
    ("GoLand", "goland"),
    ("Rider", "rider"),
    ("DataGrip", "datagrip"),
    ("PhpStorm", "phpstorm"),
    ("RubyMine", "rubymine"),
    ("RustRover", "rustrover"),
    ("DataSpell", "dataspell"),
    ("AndroidStudio", "studio"),
    ("Aqua", "aqua"),
    ("Fleet", "fleet"),
];

/// Candidates at fixed paths (no listing needed).
#[expect(clippy::too_many_lines, reason = "declarative per-tool table")]
fn fixed(r: &Roots) -> Vec<Candidate> {
    let mut out = Vec::new();
    let mac = r.os == Os::Mac;
    let win = r.os == Os::Windows;
    let pick = |mac_rel: &str, linux_rel: &str, win_rel: &str| match r.os {
        Os::Mac => join(&r.cache, mac_rel),
        Os::Linux => join(&r.cache, linux_rel),
        Os::Windows => join(&r.cache, win_rel),
    };
    let env_or = |name: &str, default: PathBuf| r.env(name).map_or(default, Path::to_path_buf);
    let pkg = |name: &str, path: PathBuf| {
        Candidate::new(JunkKind::DevPackageCache, name, path)
            .contents()
            .tag(ItemTag::Packages)
    };

    // JavaScript.
    let npm = r.env("npm_config_cache").map_or_else(
        || {
            if win {
                r.cache.join("npm-cache")
            } else {
                r.home(".npm")
            }
        },
        Path::to_path_buf,
    );
    out.push(pkg("npm", npm.join("_cacache")));
    out.push(pkg(
        "yarn",
        env_or("YARN_CACHE_FOLDER", pick("Yarn", "yarn", "Yarn/Cache")),
    ));
    out.push(pkg("yarn berry", r.home(".yarn/berry/cache")));
    let pnpm_store = r.env("PNPM_HOME").map_or_else(
        || match r.os {
            Os::Mac => r.home("Library/pnpm/store"),
            Os::Linux => join(&r.data, "pnpm/store"),
            Os::Windows => join(&r.cache, "pnpm/store"),
        },
        |home| home.join("store"),
    );
    out.push(pkg("pnpm store", pnpm_store));
    out.push(pkg("pnpm", pick("pnpm", "pnpm", "pnpm-cache")));
    let bun = env_or("BUN_INSTALL", r.home(".bun"));
    out.push(pkg("bun", join(&bun, "install/cache")));
    out.push(pkg(
        "deno",
        env_or("DENO_DIR", pick("deno", "deno", "deno")),
    ));
    out.push(pkg(
        "electron",
        pick("electron", "electron", "electron/Cache"),
    ));
    out.push(pkg(
        "electron-builder",
        pick(
            "electron-builder",
            "electron-builder",
            "electron-builder/Cache",
        ),
    ));
    out.push(pkg(
        "node-gyp",
        pick("node-gyp", "node-gyp", "node-gyp/Cache"),
    ));
    out.push(
        pkg(
            "Playwright",
            pick("ms-playwright", "ms-playwright", "ms-playwright"),
        )
        .review(),
    );
    out.push(pkg("Puppeteer", r.home(".cache/puppeteer")).review());

    // Rust.
    let cargo = env_or("CARGO_HOME", r.home(".cargo"));
    for rel in ["registry/cache", "registry/src", "git/checkouts"] {
        out.push(pkg(&format!("cargo {rel}"), join(&cargo, rel)));
    }
    out.push(pkg(
        "sccache",
        pick("Mozilla.sccache", "sccache", "Mozilla/sccache/cache"),
    ));

    // Python.
    out.push(pkg(
        "pip",
        env_or("PIP_CACHE_DIR", pick("pip", "pip", "pip/Cache")),
    ));
    let uv = r.env("UV_CACHE_DIR").map_or_else(
        || {
            if win {
                join(&r.cache, "uv/cache")
            } else {
                r.home(".cache/uv")
            }
        },
        Path::to_path_buf,
    );
    out.push(pkg("uv", uv));
    let poetry = env_or(
        "POETRY_CACHE_DIR",
        pick("pypoetry", "pypoetry", "pypoetry/Cache"),
    );
    // `virtualenvs` next to these holds live project environments: not offered.
    for rel in ["cache", "artifacts"] {
        out.push(pkg(&format!("poetry {rel}"), poetry.join(rel)));
    }

    // JVM.
    let gradle = env_or("GRADLE_USER_HOME", r.home(".gradle"));
    out.push(pkg("Gradle", gradle.join("caches")));
    out.push(pkg("Gradle wrapper", join(&gradle, "wrapper/dists")).review());
    out.push(pkg("Maven", r.home(".m2/repository")).review());

    // Go.
    out.push(pkg(
        "go build",
        env_or("GOCACHE", pick("go-build", "go-build", "go-build")),
    ));
    let gomod = r.env("GOMODCACHE").map_or_else(
        || join(&env_or("GOPATH", r.home("go")), "pkg/mod"),
        Path::to_path_buf,
    );
    out.push(pkg("go modules", gomod).review());

    // .NET, PHP, Dart, Apple.
    out.push(pkg("NuGet", env_or("NUGET_PACKAGES", r.home(".nuget/packages"))).review());
    for rel in ["v3-cache", "plugins-cache"] {
        let base = if win {
            join(&r.cache, "NuGet")
        } else {
            r.home(".local/share/NuGet")
        };
        out.push(pkg(&format!("NuGet {rel}"), base.join(rel)));
    }
    out.push(pkg(
        "Composer",
        env_or(
            "COMPOSER_CACHE_DIR",
            pick("composer", "composer", "Composer"),
        ),
    ));
    out.push(pkg("Composer", r.home(".composer/cache")));
    let pub_cache = r.env("PUB_CACHE").map_or_else(
        || {
            if win {
                join(&r.cache, "Pub/Cache")
            } else {
                r.home(".pub-cache")
            }
        },
        Path::to_path_buf,
    );
    out.push(pkg("pub", pub_cache).review());
    if mac {
        out.push(pkg("CocoaPods", r.cache.join("CocoaPods")));
        out.push(pkg("Carthage", r.cache.join("org.carthage.CarthageKit")));
        out.push(
            Candidate::new(JunkKind::Xcode, "Xcode", r.cache.join("com.apple.dt.Xcode"))
                .contents()
                .tag(ItemTag::Cache),
        );
        for (name, rel) in [
            ("CoreSimulator", "Library/Developer/CoreSimulator/Caches"),
            ("Previews", "Library/Developer/Xcode/UserData/Previews"),
        ] {
            out.push(
                Candidate::new(JunkKind::Xcode, name, r.home(rel))
                    .contents()
                    .tag(ItemTag::Simulator),
            );
        }
        out.push(
            Candidate::new(
                JunkKind::IdeCache,
                "Code",
                r.cache.join("com.microsoft.VSCode.ShipIt"),
            )
            .contents()
            .tag(ItemTag::Cache),
        );
    }

    // Container / VM tools.
    let tool =
        |name: &str, path: PathBuf| Candidate::new(JunkKind::ToolCache, name, path).contents();
    out.push(tool("Vagrant boxes", r.home(".vagrant.d/boxes")).review());
    out.push(tool("Vagrant", r.home(".vagrant.d/tmp")));
    out.push(tool("minikube", r.home(".minikube/cache")));
    out.push(tool("Android", r.home(".android/cache")));
    out
}

/// Folders whose children the developer catalogue lists.
fn listed_parents(r: &Roots) -> Vec<PathBuf> {
    let mut out = vec![r.cache.join("JetBrains")];
    if r.os == Os::Mac {
        out.push(r.home("Library/Logs/JetBrains"));
        out.push(r.home("Library/Developer"));
    }
    out.extend(EDITORS.iter().map(|(dir, _)| r.config.join(dir)));
    out
}

/// Android Studio folders (`<cache>/Google/AndroidStudio*`).
fn android_studio_dirs(r: &Roots) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(r.cache.join("Google")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| e.file_name().to_string_lossy().starts_with("AndroidStudio"))
        .map(|e| e.path())
        .collect()
}

/// Every developer-cache folder: the system area's globs must not report them.
pub(super) fn claimed(r: &Roots) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fixed(r).into_iter().map(|c| c.path).collect();
    out.extend(listed_parents(r));
    out.extend(android_studio_dirs(r));
    // The package caches' parents that only hold caches.
    out.push(r.home(".npm"));
    out
}

pub(super) fn catalogue(cx: &mut Cx<'_>, walker: &Walker, ctx: &JobCtx) -> Vec<Candidate> {
    let r = cx.roots;
    let mut out = fixed(r);
    if r.os == Os::Mac {
        xcode(cx, &mut out);
    }
    ides(cx, &mut out);
    let prune: Vec<PathBuf> = out
        .iter()
        .map(|c| c.path.clone())
        .chain(listed_parents(r))
        .collect();
    projects(cx, walker, ctx, &prune, &mut out);
    out
}

fn xcode(cx: &mut Cx<'_>, out: &mut Vec<Candidate>) {
    let r = cx.roots;
    for e in cx.dirs(&r.home("Library/Developer/Xcode/DerivedData")) {
        let name = derived_data_name(&e.name);
        out.push(Candidate::new(JunkKind::Xcode, name, e.path).tag(ItemTag::BuildOutput));
    }
    for day in cx.dirs(&r.home("Library/Developer/Xcode/Archives")) {
        for e in cx.dirs(&day.path) {
            let name = e
                .name
                .strip_suffix(".xcarchive")
                .unwrap_or(&e.name)
                .to_owned();
            out.push(
                Candidate::new(JunkKind::Xcode, name, e.path)
                    .tag(ItemTag::Archives)
                    .review(),
            );
        }
    }
    for platform in ["iOS", "watchOS", "tvOS", "visionOS", "macOS"] {
        let dir = r.home(&format!("Library/Developer/Xcode/{platform} DeviceSupport"));
        for e in cx.dirs(&dir) {
            out.push(
                Candidate::new(JunkKind::Xcode, format!("{platform} {}", e.name), e.path)
                    .tag(ItemTag::DeviceSupport)
                    .review(),
            );
        }
    }
}

/// `MyApp-abcdefghijklmnopqrstuvwxyzab` → `MyApp`.
fn derived_data_name(dir: &str) -> String {
    match dir.rsplit_once('-') {
        Some((stem, hash))
            if !stem.is_empty()
                && hash.len() == 28
                && hash.bytes().all(|b| b.is_ascii_lowercase()) =>
        {
            stem.to_owned()
        }
        _ => dir.to_owned(),
    }
}

fn ides(cx: &mut Cx<'_>, out: &mut Vec<Candidate>) {
    let r = cx.roots;
    let s = cx.settings;
    let jetbrains_running = |cx: &Cx<'_>, product: &str| {
        JETBRAINS
            .iter()
            .find(|(prefix, _)| product.starts_with(prefix))
            .is_some_and(|(_, proc)| {
                let wide = format!("{proc}64");
                cx.running().any(&[proc, wide.as_str()])
            })
    };
    let mut products: Vec<(String, PathBuf)> = cx
        .dirs(&r.cache.join("JetBrains"))
        .into_iter()
        .map(|e| (e.name, e.path))
        .collect();
    products.extend(
        android_studio_dirs(r)
            .into_iter()
            .map(|p| (file_name(&p), p)),
    );
    for (product, dir) in products {
        let running = jetbrains_running(cx, &product);
        if r.os == Os::Windows {
            for (sub, tag) in [("caches", ItemTag::Cache), ("log", ItemTag::Logs)] {
                out.push(
                    Candidate::new(JunkKind::IdeCache, product.clone(), dir.join(sub))
                        .contents()
                        .tag(tag)
                        .running(running, s),
                );
            }
        } else {
            out.push(
                Candidate::new(JunkKind::IdeCache, product, dir)
                    .contents()
                    .tag(ItemTag::Cache)
                    .running(running, s),
            );
        }
    }
    if r.os == Os::Mac {
        for e in cx.dirs(&r.home("Library/Logs/JetBrains")) {
            let running = jetbrains_running(cx, &e.name);
            out.push(
                Candidate::new(JunkKind::IdeCache, e.name, e.path)
                    .contents()
                    .tag(ItemTag::Logs)
                    .running(running, s),
            );
        }
    }
    for (dir, procs) in EDITORS {
        let base = r.config.join(dir);
        if !base.is_dir() {
            continue;
        }
        let running = cx.running().any(procs);
        for (rel, tag) in EDITOR_CACHES {
            out.push(
                Candidate::new(JunkKind::IdeCache, dir, join(&base, rel))
                    .contents()
                    .tag(tag)
                    .running(running, s),
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// Project artifacts

/// Marker files seen in one folder listing.
#[derive(Debug, Default, Clone, Copy)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one flag per marker file, filled from a single listing"
)]
struct Markers {
    package_json: bool,
    cargo: bool,
    maven: bool,
    sbt: bool,
    gradle: bool,
    pubspec: bool,
    podfile: bool,
    dotnet: bool,
    zig: bool,
    swift: bool,
    cmake: bool,
}

impl Markers {
    fn see(&mut self, file: &str) {
        match file {
            "package.json" => self.package_json = true,
            "Cargo.toml" => self.cargo = true,
            "pom.xml" => self.maven = true,
            "build.sbt" => self.sbt = true,
            "build.gradle" | "build.gradle.kts" | "settings.gradle" | "settings.gradle.kts" => {
                self.gradle = true;
            }
            "pubspec.yaml" => self.pubspec = true,
            "Podfile" => self.podfile = true,
            "build.zig" => self.zig = true,
            "Package.swift" => self.swift = true,
            "CMakeLists.txt" => self.cmake = true,
            other => {
                if [".csproj", ".fsproj", ".vbproj"]
                    .iter()
                    .any(|ext| other.ends_with(ext))
                {
                    self.dotnet = true;
                }
            }
        }
    }
}

/// What a child folder named `name` of a folder with `markers` is, if an artifact.
/// `dir` is the parent (the project), `child` the folder itself.
fn artifact(name: &str, m: Markers, dir: &Path, child: &Path) -> Option<ItemTag> {
    let tag = match name {
        "node_modules" if m.package_json => ItemTag::NodeModules,
        "target" if m.cargo || m.maven || m.sbt => ItemTag::BuildOutput,
        "build" if m.gradle || m.pubspec => ItemTag::BuildOutput,
        ".gradle" if m.gradle => ItemTag::BuildOutput,
        ".next" | ".nuxt" | ".svelte-kit" | ".turbo" | ".parcel-cache" | ".angular"
            if m.package_json =>
        {
            ItemTag::BuildOutput
        }
        "dist" if m.package_json && has_build_script(dir) => ItemTag::BuildOutput,
        "__pycache__" => ItemTag::PythonEnv,
        ".venv" | "venv" if child.join("pyvenv.cfg").is_file() => ItemTag::PythonEnv,
        "Pods" if m.podfile => ItemTag::BuildOutput,
        ".dart_tool" if m.pubspec => ItemTag::BuildOutput,
        "bin" | "obj" if m.dotnet => ItemTag::BuildOutput,
        "zig-cache" | ".zig-cache" if m.zig => ItemTag::BuildOutput,
        ".build" if m.swift => ItemTag::BuildOutput,
        "DerivedData" => ItemTag::BuildOutput,
        other if m.cmake && other.starts_with("cmake-build-") => ItemTag::BuildOutput,
        _ => return None,
    };
    Some(tag)
}

/// `package.json` has a `build` script (so `dist` is generated).
fn has_build_script(dir: &Path) -> bool {
    fs::read_to_string(dir.join("package.json"))
        .ok()
        .and_then(|text| json::parse(&text))
        .is_some_and(|pkg| pkg.get("scripts").and_then(|s| s.get("build")).is_some())
}

/// Folder names never descended into (besides hidden ones).
fn prune_name(name: &str) -> bool {
    const BUNDLES: [&str; 10] = [
        ".app",
        ".photoslibrary",
        ".musiclibrary",
        ".tvlibrary",
        ".xcarchive",
        ".framework",
        ".bundle",
        ".vmwarevm",
        ".pvm",
        ".utm",
    ];
    name == "node_modules"
        || name == "$Recycle.Bin"
        || BUNDLES.iter().any(|ext| name.ends_with(ext))
}

/// Shared state of the project walk.
struct ProjectWalk<'a> {
    opts: &'a WalkOptions,
    prune: &'a [PathBuf],
    home: &'a Path,
    max_depth: u32,
    /// Projects modified after this (Unix seconds) are skipped; `None` = all.
    cutoff: Option<i64>,
    ctx: &'a JobCtx,
    found: Mutex<Vec<(PathBuf, ItemTag, String)>>,
    denied: Mutex<Vec<Denied>>,
}

impl ProjectWalk<'_> {
    fn visit<'s>(&'s self, scope: &rayon::Scope<'s>, dir: &Path, depth: u32, dev: Option<u64>) {
        if self.ctx.is_cancelled() {
            return;
        }
        self.ctx.entered_dir(dir);
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) => {
                if depth == 0 || err.kind() != std::io::ErrorKind::NotFound {
                    self.denied.lock().push(Denied {
                        path: dir.display().to_string(),
                        reason: errors::classify(&err, dir),
                    });
                }
                return;
            }
        };
        let mut markers = Markers::default();
        let mut subdirs: Vec<(String, PathBuf)> = Vec::new();
        let mut count = 0_u64;
        for entry in entries.flatten() {
            count = count.saturating_add(1);
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if file_type.is_dir() {
                subdirs.push((name, entry.path()));
            } else if file_type.is_file() {
                markers.see(&name);
            }
        }
        self.ctx.add_items(count);
        let mut artifacts: Vec<(PathBuf, ItemTag)> = Vec::new();
        for (name, path) in subdirs {
            if self.opts.is_excluded(&path) {
                continue;
            }
            if let Some(tag) = artifact(&name, markers, dir, &path) {
                artifacts.push((path, tag));
                continue;
            }
            let hidden = name.starts_with('.');
            let at_home = dir == self.home;
            if hidden
                || prune_name(&name)
                || (at_home && (name == "Library" || name == "AppData"))
                || depth >= self.max_depth
                || self.prune.iter().any(|p| paths::is_within(&path, p))
            {
                continue;
            }
            let child_dev = match (dev, device(&path)) {
                (Some(root), Some(here)) if self.opts.one_file_system && root != here => continue,
                (Some(root), _) => Some(root),
                (None, here) => here,
            };
            let child_depth = depth.saturating_add(1);
            scope.spawn(move |scope| self.visit(scope, &path, child_depth, child_dev));
        }
        if artifacts.is_empty() || !self.old_enough(dir, &artifacts) {
            return;
        }
        let project = file_name(dir);
        self.found.lock().extend(
            artifacts
                .into_iter()
                .map(|(path, tag)| (path, tag, project.clone())),
        );
    }

    /// The project folder and its top-level entries (except the artifacts) were not
    /// modified after the cutoff.
    fn old_enough(&self, dir: &Path, artifacts: &[(PathBuf, ItemTag)]) -> bool {
        let Some(cutoff) = self.cutoff else {
            return true;
        };
        let mtime = |p: &Path| {
            fs::symlink_metadata(p)
                .ok()
                .and_then(|m| m.modified().ok())
                .map(paths::unix_secs)
        };
        if mtime(dir).is_some_and(|m| m > cutoff) {
            return false;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return false;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if artifacts.iter().any(|(a, _)| *a == path) {
                continue;
            }
            if entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .map(paths::unix_secs)
                .is_some_and(|m| m > cutoff)
            {
                return false;
            }
        }
        true
    }
}

#[cfg(unix)]
fn device(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt as _;
    fs::symlink_metadata(path).ok().map(|m| m.dev())
}

#[cfg(not(unix))]
fn device(_path: &Path) -> Option<u64> {
    None
}

/// The folders searched for projects: `file_roots` (or home), nested roots removed.
pub(super) fn search_roots(r: &Roots, file_roots: &[String]) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = file_roots
        .iter()
        .filter_map(|p| paths::normalize(&paths::expand(p)))
        .collect();
    if roots.is_empty() {
        roots.push(r.home.clone());
    }
    roots.sort();
    let mut out: Vec<PathBuf> = Vec::with_capacity(roots.len());
    for root in roots {
        if !out.iter().any(|kept| paths::is_within(&root, kept)) {
            out.push(root);
        }
    }
    out
}

fn projects(
    cx: &mut Cx<'_>,
    walker: &Walker,
    ctx: &JobCtx,
    prune: &[PathBuf],
    out: &mut Vec<Candidate>,
) {
    let s = cx.settings;
    let roots = search_roots(cx.roots, &s.file_roots);
    let cutoff = (s.dev_project_min_age_days > 0).then(|| {
        let secs = i64::from(s.dev_project_min_age_days).saturating_mul(86_400);
        paths::now_secs().saturating_sub(secs)
    });
    let walk = ProjectWalk {
        opts: walker.options(),
        prune,
        home: &cx.roots.home,
        max_depth: u32::from(s.dev_project_max_depth),
        cutoff,
        ctx,
        found: Mutex::new(Vec::new()),
        denied: Mutex::new(Vec::new()),
    };
    walker.install(|| {
        rayon::scope(|scope| {
            for root in &roots {
                if walk.opts.is_excluded(root) {
                    continue;
                }
                let dev = device(root);
                let walk = &walk;
                scope.spawn(move |scope| walk.visit(scope, root, 0, dev));
            }
        });
    });
    let ProjectWalk { found, denied, .. } = walk;
    cx.denied.extend(denied.into_inner());
    let mut found = found.into_inner();
    found.sort();
    for (path, tag, project) in found {
        out.push(
            Candidate::new(JunkKind::ProjectArtifacts, project, path)
                .tag(tag)
                .review(),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use omc_proto::junk::Safety;
    use omc_proto::settings::CleanSettings;

    use super::super::Running;
    use super::super::tests::{put, run, temp_dir};
    use super::*;

    fn set_old(path: &Path) {
        let old = SystemTime::now().checked_sub(Duration::from_hours(90 * 24));
        assert!(old.is_some(), "time math");
        let Some(old) = old else { return };
        let file = fs::File::options().read(true).open(path);
        assert!(file.is_ok(), "open {}", path.display());
        let Ok(file) = file else { return };
        assert!(
            file.set_modified(old).is_ok(),
            "set mtime {}",
            path.display()
        );
    }

    /// Makes every top-level entry of `dir` and `dir` itself 90 days old.
    fn age_project(dir: &Path) {
        let entries = fs::read_dir(dir);
        assert!(entries.is_ok(), "list {}", dir.display());
        let Ok(entries) = entries else { return };
        for entry in entries.flatten() {
            set_old(&entry.path());
        }
        set_old(dir);
    }

    fn scan(
        roots: &Roots,
        settings: &CleanSettings,
    ) -> Vec<(String, Option<ItemTag>, JunkKind, Safety)> {
        let mut cx = Cx::new(roots, settings);
        cx.set_running(Running::default());
        let walker = super::super::tests::walker();
        let cands = catalogue(&mut cx, &walker, &JobCtx::new());
        let scanned = run(roots, settings, cands, &[]);
        scanned
            .report
            .groups
            .iter()
            .flat_map(|g| {
                g.items
                    .iter()
                    .map(move |i| (i.name.clone(), i.tag, g.kind, i.safety))
            })
            .collect()
    }

    #[test]
    fn project_artifacts_need_markers_and_age() {
        let base = temp_dir("dev-projects");
        let roots = Roots::fake(&base, Os::Linux);
        let code = roots.home("code");
        // Old JS project with node_modules and a built dist.
        let web = code.join("web");
        put(&web.join("package.json"), 0);
        assert!(
            fs::write(
                web.join("package.json"),
                r#"{"scripts":{"build":"vite build"}}"#
            )
            .is_ok(),
            "write package.json"
        );
        put(&web.join("node_modules/react/index.js"), 4096);
        put(&web.join("dist/index.js"), 4096);
        age_project(&web);
        // Old Rust project.
        let rust = code.join("tool");
        put(&rust.join("Cargo.toml"), 10);
        put(&rust.join("target/debug/tool"), 4096);
        age_project(&rust);
        // Fresh project: skipped by age.
        let fresh = code.join("fresh");
        put(&fresh.join("Cargo.toml"), 10);
        put(&fresh.join("target/debug/x"), 4096);
        // A `target` without a manifest and `dist` without a build script: not artifacts.
        let plain = code.join("plain");
        put(&plain.join("target/x"), 4096);
        put(&plain.join("package.json"), 2);
        put(&plain.join("dist/x"), 4096);
        age_project(&plain);
        // A virtualenv.
        let py = code.join("py");
        put(&py.join(".venv/pyvenv.cfg"), 10);
        put(&py.join(".venv/lib/x.py"), 4096);
        age_project(&py);

        let got = scan(&roots, &CleanSettings::default());
        let has = |name: &str, tag: ItemTag| {
            got.iter().any(|(n, t, k, s)| {
                n == name
                    && *t == Some(tag)
                    && *k == JunkKind::ProjectArtifacts
                    && *s == Safety::Review
            })
        };
        assert!(has("web", ItemTag::NodeModules), "node_modules: {got:?}");
        assert!(
            has("web", ItemTag::BuildOutput),
            "dist with build script: {got:?}"
        );
        assert!(has("tool", ItemTag::BuildOutput), "cargo target: {got:?}");
        assert!(has("py", ItemTag::PythonEnv), "venv: {got:?}");
        assert!(
            !got.iter().any(|(n, ..)| n == "fresh" || n == "plain"),
            "fresh/unmarked skipped: {got:?}"
        );

        let all_ages = CleanSettings {
            dev_project_min_age_days: 0,
            ..CleanSettings::default()
        };
        let got = scan(&roots, &all_ages);
        assert!(
            got.iter().any(|(n, ..)| n == "fresh"),
            "age 0 includes fresh projects: {got:?}"
        );

        let shallow = CleanSettings {
            dev_project_min_age_days: 0,
            dev_project_max_depth: 0,
            ..CleanSettings::default()
        };
        let got = scan(&roots, &shallow);
        assert!(
            !got.iter()
                .any(|(_, _, k, _)| *k == JunkKind::ProjectArtifacts),
            "depth limit stops the search: {got:?}"
        );
    }

    #[test]
    fn package_caches_respect_env_overrides() {
        let base = temp_dir("dev-caches");
        let mut roots = Roots::fake(&base, Os::Mac);
        let cargo_home = base.join("custom-cargo");
        roots.env.push(("CARGO_HOME", cargo_home.clone()));
        put(&cargo_home.join("registry/cache/index/crate.crate"), 4096);
        put(&roots.home(".npm/_cacache/content/a"), 4096);
        put(
            &roots.home("Library/Developer/Xcode/DerivedData/App-abcdefghijklmnopqrstuvwxyzab/x"),
            4096,
        );
        let got = scan(&roots, &CleanSettings::default());
        assert!(
            got.iter()
                .any(|(n, _, k, _)| n == "cargo registry/cache" && *k == JunkKind::DevPackageCache),
            "CARGO_HOME honoured: {got:?}"
        );
        assert!(got.iter().any(|(n, ..)| n == "npm"), "npm cache: {got:?}");
        assert!(
            got.iter()
                .any(|(n, _, k, _)| n == "App" && *k == JunkKind::Xcode),
            "DerivedData named by project: {got:?}"
        );
    }
}
