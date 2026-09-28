//! Per-user configuration kept outside the platform's app-data folders: children of the
//! XDG base folders (`~/.config/<name>`, `~/.local/share|state/<name>`, `~/.cache/<name>`)
//! and dot entries of the home folder (`~/.<name>`, `~/.<name>rc`, `~/.<name>.json`).
//! Cross-platform and command-line apps keep their settings there (Ghostty, Zed, Electron
//! and Go/Rust tools), but so do hundreds of unrelated tools: a name match alone is weak.
//!
//! Discovery lists each base folder once ([`candidates`]). Only candidates whose name
//! relates to one of the app's names ([`Names::relate`]) are considered further, and
//! shared tool folders ([`is_shared_tool`]) never are. Evidence, strongest first:
//!
//! - the app's own executables (or its Electron `app.asar`) contain the candidate's
//!   relative path as a string (`.config/ghostty`, `$XDG_CONFIG_HOME/ghostty`,
//!   `ghostty/config`, `".claude"`): [`search`] streams the files through
//!   `memchr::memmem` in overlapping chunks, in parallel, under byte caps;
//! - the running app has files open in it (collected by the platform);
//! - exact name equality with one of the app's names;
//! - a partial name match.
//!
//! A command on the search path with the candidate's name that does not resolve into the
//! app ([`commands`]) means the folder may belong to that command-line tool
//! (`~/.claude` of the `claude` CLI, not of Claude desktop): such folders are never more
//! than [`Confidence::Low`]. The platforms add their own rival-app judgement
//! ([`Rival`]) and removal-guard checks, then call [`confidence`].

use std::collections::HashSet;
use std::ffi::OsStr;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use memchr::memmem::Finder;
use omc_proto::apps::{AppFileKind, Confidence};

/// Bytes read per app at most.
pub(crate) const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
/// Files larger than this are skipped.
pub(crate) const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
/// Bytes read per step.
const CHUNK_BYTES: usize = 4 * 1024 * 1024;
/// Chunks per segment: large files are cut into segments searched in parallel.
const SEGMENT_CHUNKS: u64 = 4;

/// Normalised names (see [`normalize`]) of folders and files that belong to shared tools,
/// shells, editors, package managers and the OS: never attributed to an app.
const SHARED_TOOLS: &[&str] = &[
    // The base folders themselves and OS bookkeeping.
    "config",
    "local",
    "cache",
    "dsstore",
    "trash",
    "trashes",
    "cfusertextencoding",
    "localized",
    "spotlightv100",
    "fseventsd",
    "hushlogin",
    "xauthority",
    "iceauthority",
    "xsessionerrors",
    "dbus",
    "pki",
    "var",
    "icons",
    "themes",
    "fonts",
    "fontconfig",
    "mime",
    "applications",
    "autostart",
    "systemd",
    "environmentd",
    "desktop",
    "dconf",
    "pulse",
    "pipewire",
    "gtk20",
    "gtk30",
    "gtk40",
    "qt",
    "kde",
    "gnome",
    "xdgdesktopportal",
    "userdirs",
    "mimeapps",
    "recentlyused",
    "thumbnails",
    "keyrings",
    "wireplumber",
    // Version control, shells and terminal tools.
    "git",
    "gitconfig",
    "gitignore",
    "gitignoreglobal",
    "gitattributes",
    "gitk",
    "gh",
    "hg",
    "svn",
    "ssh",
    "gnupg",
    "gpg",
    "sh",
    "bash",
    "bashprofile",
    "bashlogout",
    "bashsessions",
    "zsh",
    "zshenv",
    "zprofile",
    "zlogin",
    "zlogout",
    "zshsessions",
    "zcompdump",
    "zcompcache",
    "ohmyzsh",
    "p10k",
    "zinit",
    "zplug",
    "antigen",
    "fish",
    "nu",
    "nushell",
    "profile",
    "inputrc",
    "vim",
    "viminfo",
    "nvim",
    "neovim",
    "emacs",
    "emacsd",
    "nano",
    "tmux",
    "screen",
    "starship",
    "bat",
    "less",
    "lesshst",
    "wget",
    "wgetrc",
    "wgethsts",
    "curl",
    "curlrc",
    "htop",
    "btop",
    "yazi",
    "ranger",
    "lf",
    "direnv",
    "zoxide",
    "atuin",
    "fzf",
    "ripgrep",
    "eza",
    "lazygit",
    "netrc",
    "sudoasadminsuccessful",
    "history",
    // Language toolchains and package managers.
    "npm",
    "npmrc",
    "node",
    "nodegyp",
    "nvm",
    "fnm",
    "volta",
    "yarn",
    "yarnrc",
    "pnpm",
    "pnpmstore",
    "bun",
    "deno",
    "configstore",
    "corepack",
    "cargo",
    "rustup",
    "go",
    "gopath",
    "python",
    "pip",
    "pipx",
    "pyenv",
    "conda",
    "condarc",
    "anaconda",
    "miniconda",
    "mamba",
    "uv",
    "poetry",
    "ipython",
    "jupyter",
    "matplotlib",
    "keras",
    "ruby",
    "rbenv",
    "rvm",
    "gem",
    "bundle",
    "irbrc",
    "java",
    "gradle",
    "m2",
    "sdkman",
    "jdks",
    "dotnet",
    "nuget",
    "composer",
    "cpan",
    "cpanm",
    "stack",
    "cabal",
    "ghcup",
    "opam",
    "julia",
    "r",
    "rustc",
    "mix",
    "hex",
    "pub",
    "pubcache",
    "dart",
    "flutter",
    "android",
    "swiftpm",
    "cocoapods",
    "asdf",
    "mise",
    "homebrew",
    "brew",
    "nix",
    "nixprofile",
    "macports",
    "ccache",
    "sccache",
    "cmake",
    "vcpkg",
    "conan",
    // Containers, cloud and infrastructure CLIs.
    "docker",
    "kube",
    "kubectl",
    "helm",
    "minikube",
    "colima",
    "lima",
    "podman",
    "containers",
    "terraform",
    "terraformd",
    "aws",
    "azure",
    "gcloud",
    "gsutil",
    "vagrant",
    "ansible",
    // Shared frameworks and caches.
    "electron",
    "electronbuilder",
    "chromium",
    "puppeteer",
    "playwright",
    "mozilla",
    "wine",
    "steam",
    "pulsecookie",
    "esdauth",
    "nv",
    "vulkan",
    "mesashadercache",
    "gstreamer10",
    "typescript",
    "prisma",
    "huggingface",
    "torch",
    "copilot",
];

/// Words of display names that do not identify an app.
const GENERIC_WORDS: &[&str] = &[
    "the",
    "app",
    "apps",
    "desktop",
    "studio",
    "helper",
    "manager",
    "launcher",
    "client",
    "editor",
    "player",
    "viewer",
    "pro",
    "free",
    "plus",
    "lite",
    "beta",
    "update",
    "updater",
    "agent",
    "service",
    "tool",
    "tools",
    "mac",
    "macos",
    "for",
    "with",
    "and",
    "setup",
    "installer",
    "uninstaller",
    "edition",
    "preview",
    "insiders",
    "portable",
    "microsoft",
    "google",
    "apple",
    "windows",
    "linux",
    "terminal",
    "browser",
    "config",
    "data",
    "files",
    "user",
    "home",
    "local",
    "cache",
    "share",
    "state",
    "system",
    "server",
];

/// Lowercase, without spaces, dashes, dots and underscores.
pub(crate) fn normalize(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, ' ' | '-' | '.' | '_'))
        .flat_map(char::to_lowercase)
        .collect()
}

/// `name` (any form) names a shared tool, shell, package manager or OS folder.
pub(crate) fn is_shared_tool(name: &str) -> bool {
    let n = normalize(name);
    n.is_empty()
        || SHARED_TOOLS.contains(&n.as_str())
        || n.ends_with("history")
        || n.starts_with("zcompdump")
        || n.starts_with("bash")
        || n.starts_with("zsh")
        || n.starts_with("git")
}

/// Where a candidate lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Base {
    /// `$XDG_CONFIG_HOME` / `~/.config`.
    Config,
    /// `$XDG_DATA_HOME` / `~/.local/share`.
    Data,
    /// `$XDG_STATE_HOME` / `~/.local/state`.
    State,
    /// `$XDG_CACHE_HOME` / `~/.cache`.
    Cache,
    /// A dot entry of the home folder.
    Home,
}

/// The per-user folders candidates are listed from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Bases {
    /// The home folder (its dot entries).
    pub(crate) home: PathBuf,
    pub(crate) config: Option<PathBuf>,
    pub(crate) data: Option<PathBuf>,
    pub(crate) state: Option<PathBuf>,
    pub(crate) cache: Option<PathBuf>,
}

impl Bases {
    /// macOS/Linux: the XDG folders (`XDG_*_HOME` when absolute, else the defaults).
    #[cfg_attr(windows, expect(dead_code, reason = "macOS/Linux layout"))]
    pub(crate) fn xdg(home: &Path) -> Self {
        let xdg = |var: &str, default: &str| {
            std::env::var_os(var)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| home.join(default))
        };
        Self {
            home: home.to_path_buf(),
            config: Some(xdg("XDG_CONFIG_HOME", ".config")),
            data: Some(xdg("XDG_DATA_HOME", ".local/share")),
            state: Some(xdg("XDG_STATE_HOME", ".local/state")),
            cache: Some(xdg("XDG_CACHE_HOME", ".cache")),
        }
    }

    /// Windows: `%USERPROFILE%\.config` and the profile's dot entries.
    #[cfg_attr(not(windows), expect(dead_code, reason = "Windows layout"))]
    pub(crate) fn windows(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            config: Some(home.join(".config")),
            data: None,
            state: None,
            cache: None,
        }
    }

    fn xdg_dirs(&self) -> [(Option<&Path>, Base); 4] {
        [
            (self.config.as_deref(), Base::Config),
            (self.data.as_deref(), Base::Data),
            (self.state.as_deref(), Base::State),
            (self.cache.as_deref(), Base::Cache),
        ]
    }
}

/// A folder or file that may hold an app's per-user configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub(crate) path: PathBuf,
    /// The on-disk name without the leading dot of a home entry (`ghostty`, `claude.json`).
    pub(crate) name: String,
    /// The name matched against the app's names: without `rc` and config-file extensions
    /// of home files (`.ghosttyrc` → `ghostty`).
    pub(crate) stem: String,
    pub(crate) base: Base,
    pub(crate) is_dir: bool,
}

impl Candidate {
    /// The role of the candidate.
    pub(crate) const fn kind(&self) -> AppFileKind {
        match self.base {
            Base::Data | Base::State => AppFileKind::Support,
            Base::Cache => AppFileKind::Cache,
            Base::Home if self.is_dir => AppFileKind::Support,
            Base::Config | Base::Home => AppFileKind::Preferences,
        }
    }
}

/// `.ghosttyrc` / `.claude.json` → `ghostty` / `claude` (files only).
fn file_stem(name: &str) -> &str {
    const EXTS: &[&str] = &[
        ".json", ".jsonc", ".toml", ".yaml", ".yml", ".conf", ".cfg", ".ini", ".config",
    ];
    let lower = name.to_ascii_lowercase();
    let mut end = name.len();
    if let Some(ext) = EXTS.iter().find(|e| lower.ends_with(**e)) {
        end = end.saturating_sub(ext.len());
    }
    let head = name.get(..end).unwrap_or(name);
    let head = match head.strip_suffix("rc") {
        Some(rest) if rest.chars().count() >= 3 => rest,
        _ => head,
    };
    if head.is_empty() { name } else { head }
}

/// Every candidate below `bases`, each folder listed once; shared tool names dropped.
pub(crate) fn candidates(bases: &Bases) -> Vec<Candidate> {
    let mut out = Vec::new();
    let base_dirs: Vec<&Path> = bases.xdg_dirs().iter().filter_map(|(d, _)| *d).collect();
    for (dir, base) in bases.xdg_dirs() {
        let Some(dir) = dir else { continue };
        for (name, path, is_dir) in list(dir) {
            if is_shared_tool(&name) || base_dirs.iter().any(|b| *b == path) {
                continue;
            }
            out.push(Candidate {
                path,
                stem: name.clone(),
                name,
                base,
                is_dir,
            });
        }
    }
    for (name, path, is_dir) in list(&bases.home) {
        let Some(bare) = name.strip_prefix('.') else {
            continue;
        };
        if bare.is_empty() || bare == "." || base_dirs.iter().any(|b| *b == path) {
            continue;
        }
        let stem = if is_dir { bare } else { file_stem(bare) };
        if is_shared_tool(bare) || is_shared_tool(stem) {
            continue;
        }
        out.push(Candidate {
            path: path.clone(),
            stem: stem.to_owned(),
            name: bare.to_owned(),
            base: Base::Home,
            is_dir,
        });
    }
    out
}

/// Children of `dir`: name, path, is a folder (symlinks followed).
fn list(dir: &Path) -> Vec<(String, PathBuf, bool)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_owned();
            let path = e.path();
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir())
                || (e.file_type().is_ok_and(|t| t.is_symlink()) && path.is_dir());
            Some((name, path, is_dir))
        })
        .collect()
}

/// How a candidate's name relates to the app's names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum NameMatch {
    /// A word of a name, or a name's prefix/extension (`.android` for Android Studio).
    Partial,
    /// Equal to a name after normalisation.
    Exact,
}

/// The app's names, normalised.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Names {
    /// Display, executable, id-tail, product and command names.
    exact: Vec<String>,
    /// Distinctive words of the display names.
    words: Vec<String>,
    /// Command-line tools the app ships.
    commands: Vec<String>,
}

impl Names {
    /// Adds a display name: whole and its distinctive words.
    pub(crate) fn add_display(&mut self, name: &str) {
        self.add(name);
        for word in words(name) {
            if word.chars().count() >= 4
                && !GENERIC_WORDS.contains(&word.as_str())
                && !is_shared_tool(&word)
                && !self.words.contains(&word)
            {
                self.words.push(word);
            }
        }
    }

    /// Adds a name matched whole only (executable, bundle-id tail, product, command).
    pub(crate) fn add(&mut self, name: &str) {
        let n = normalize(name);
        if n.chars().count() >= 3
            && !GENERIC_WORDS.contains(&n.as_str())
            && !is_shared_tool(&n)
            && !self.exact.contains(&n)
        {
            self.exact.push(n);
        }
    }

    /// Adds the name of a command-line tool the app ships (`Contents/Resources/bin/code`).
    #[cfg_attr(
        not(target_os = "macos"),
        expect(dead_code, reason = "bundled tools are a macOS layout")
    )]
    pub(crate) fn add_command(&mut self, name: &str) {
        self.add(name);
        let n = normalize(name);
        if self.exact.contains(&n) && !self.commands.contains(&n) {
            self.commands.push(n);
        }
    }

    /// Adds a name that only hints at the app (bundle-id tail, vendor word): matching it
    /// is a partial match.
    #[cfg_attr(
        windows,
        expect(dead_code, reason = "Windows has no id-tail or vendor hints")
    )]
    pub(crate) fn add_hint(&mut self, name: &str) {
        let n = normalize(name);
        if n.chars().count() >= 3
            && !GENERIC_WORDS.contains(&n.as_str())
            && !is_shared_tool(&n)
            && !self.exact.contains(&n)
            && !self.words.contains(&n)
        {
            self.words.push(n);
        }
    }

    /// Whether no usable name was added.
    pub(crate) fn is_empty(&self) -> bool {
        self.exact.is_empty() && self.words.is_empty()
    }

    /// How `stem` relates to the names.
    pub(crate) fn relate(&self, stem: &str) -> Option<NameMatch> {
        let n = normalize(stem);
        if n.chars().count() < 3 || is_shared_tool(&n) {
            return None;
        }
        if self.exact.contains(&n) {
            return Some(NameMatch::Exact);
        }
        let long = n.chars().count() >= 4;
        let partial = self.words.contains(&n)
            || (long
                && self.exact.iter().any(|e| {
                    e.chars().count() >= 4 && (n.starts_with(e.as_str()) || e.starts_with(&n))
                }));
        partial.then_some(NameMatch::Partial)
    }
}

/// Lowercase words: split at non-alphanumerics and lower→upper case changes.
fn words(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// One byte string searched for, on behalf of candidate `cand`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Needle {
    pub(crate) bytes: Vec<u8>,
    /// UTF-16LE form (context is judged per two-byte unit).
    pub(crate) wide: bool,
    /// Index of the candidate it proves.
    pub(crate) cand: usize,
}

/// The strings an app that uses `c` would contain; for a Windows program (`windows`)
/// backslash and UTF-16LE forms too.
pub(crate) fn needles(c: &Candidate, cand: usize, windows: bool) -> Vec<Needle> {
    let n = c.name.as_str();
    let mut texts: Vec<String> = Vec::new();
    let mut xdg = |rel: &str, var: &str| {
        texts.push(format!("{rel}/{n}"));
        if windows {
            texts.push(format!("{}\\{n}", rel.replace('/', "\\")));
        }
        texts.push(format!("{var}/{n}"));
        texts.push(format!("{var}}}/{n}"));
    };
    match c.base {
        Base::Config => {
            xdg(".config", "XDG_CONFIG_HOME");
            if c.is_dir {
                texts.push(format!("{n}/config"));
            }
        }
        Base::Data => xdg(".local/share", "XDG_DATA_HOME"),
        Base::State => xdg(".local/state", "XDG_STATE_HOME"),
        Base::Cache => xdg(".cache", "XDG_CACHE_HOME"),
        Base::Home => texts.push(format!(".{n}")),
    }
    let mut out = Vec::with_capacity(texts.len().saturating_mul(2));
    for text in texts {
        if windows && text.is_ascii() {
            out.push(Needle {
                bytes: text.bytes().flat_map(|b| [b, 0]).collect(),
                wide: true,
                cand,
            });
        }
        out.push(Needle {
            bytes: text.into_bytes(),
            wide: false,
            cand,
        });
    }
    out
}

/// Byte caps of one [`search`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    /// Bytes read in total at most.
    pub(crate) max_total: u64,
    /// Larger files are skipped.
    pub(crate) max_file: u64,
    /// Bytes read per step (raised to fit the longest needle).
    pub(crate) chunk: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_total: MAX_TOTAL_BYTES,
            max_file: MAX_FILE_BYTES,
            chunk: CHUNK_BYTES,
        }
    }
}

/// What one [`search`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SearchStats {
    /// Files read (fully or until done).
    pub(crate) files: usize,
    /// Files skipped as too large or unreadable.
    pub(crate) skipped: usize,
    /// Bytes read.
    pub(crate) bytes: u64,
    pub(crate) elapsed: Duration,
}

/// Bytes still allowed.
struct Budget(AtomicU64);

impl Budget {
    /// Takes up to `want` bytes; 0 when spent.
    fn take(&self, want: u64) -> u64 {
        match self
            .0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |left| {
                Some(left.saturating_sub(want))
            }) {
            Ok(left) | Err(left) => left.min(want),
        }
    }
}

/// Byte in the identifier set around a needle (a longer name, another word).
const fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')
}

/// Shared state of one search.
struct Search<'a> {
    finders: Vec<(Finder<'a>, &'a Needle)>,
    /// Per candidate: proven.
    hits: Vec<AtomicBool>,
    remaining: AtomicUsize,
    budget: Budget,
    limits: Limits,
    /// Bytes kept between chunks.
    overlap: usize,
    bytes: AtomicU64,
    files: AtomicUsize,
    skipped: AtomicUsize,
}

/// Where the context of a match can be judged.
enum Context {
    /// It is a separator: the match counts.
    Boundary,
    /// It continues a word: not this name.
    Word,
    /// Not in this buffer: judged in the neighbouring one.
    Unknown,
}

impl Search<'_> {
    fn done(&self) -> bool {
        self.remaining.load(Ordering::Acquire) == 0
    }

    fn hit(&self, cand: usize) {
        if let Some(flag) = self.hits.get(cand)
            && !flag.swap(true, Ordering::AcqRel)
        {
            self.remaining.fetch_sub(1, Ordering::AcqRel);
        }
    }

    fn is_hit(&self, cand: usize) -> bool {
        self.hits
            .get(cand)
            .is_some_and(|f| f.load(Ordering::Acquire))
    }

    /// The character before `at` (`None` at a non-ASCII wide unit: a boundary).
    fn before(buf: &[u8], at: usize, wide: bool, first: bool) -> Context {
        let unit = if wide { 2 } else { 1 };
        let Some(start) = at.checked_sub(unit) else {
            return if first && at == 0 {
                Context::Boundary
            } else {
                Context::Unknown
            };
        };
        let (Some(&b), Some(&hi)) = (buf.get(start), buf.get(start.saturating_add(1))) else {
            return Context::Unknown;
        };
        let word = if wide {
            hi == 0 && is_ident(b)
        } else {
            is_ident(b)
        };
        if word {
            Context::Word
        } else {
            Context::Boundary
        }
    }

    /// The character at `end`.
    fn after(buf: &[u8], end: usize, wide: bool, eof: bool) -> Context {
        let unit = if wide { 2 } else { 1 };
        if end.saturating_add(unit) > buf.len() {
            // The file's end (or a lone trailing byte of a wide string) is a boundary.
            return if eof {
                Context::Boundary
            } else {
                Context::Unknown
            };
        }
        let b = buf.get(end).copied().unwrap_or(0);
        let hi = buf.get(end.saturating_add(1)).copied().unwrap_or(0);
        let word = if wide {
            hi == 0 && is_ident(b)
        } else {
            // A dot followed by a separator ends a sentence (`~/.config/foo.`).
            let sentence_end =
                b == b'.' && !buf.get(end.saturating_add(1)).is_some_and(|n| is_ident(*n));
            is_ident(b) && !sentence_end
        };
        if word {
            Context::Word
        } else {
            Context::Boundary
        }
    }

    /// Searches one buffer; `first`: it starts the file, `eof`: it ends it.
    fn scan(&self, buf: &[u8], first: bool, eof: bool) {
        for (finder, needle) in &self.finders {
            if self.is_hit(needle.cand) {
                continue;
            }
            for at in finder.find_iter(buf) {
                let end = at.saturating_add(needle.bytes.len());
                let before = Self::before(buf, at, needle.wide, first);
                let after = Self::after(buf, end, needle.wide, eof);
                if matches!(before, Context::Boundary) && matches!(after, Context::Boundary) {
                    self.hit(needle.cand);
                    break;
                }
            }
        }
    }

    /// Streams bytes `start..end` of `path` (`len` bytes long) through [`Search::scan`],
    /// starting `overlap` bytes early so that matches across `start` are seen whole.
    fn segment(&self, path: &Path, start: u64, end: u64, len: u64) {
        let Ok(mut file) = std::fs::File::open(path) else {
            return;
        };
        let overlap = u64::try_from(self.overlap).unwrap_or(u64::MAX);
        let from = start.saturating_sub(overlap);
        if from > 0 && file.seek(SeekFrom::Start(from)).is_err() {
            return;
        }
        let chunk = self.limits.chunk.max(self.overlap.saturating_mul(2));
        let mut buf = vec![0_u8; chunk.saturating_add(self.overlap)];
        let mut carry = 0_usize;
        let mut first = from == 0;
        let mut pos = from;
        while pos < end && !self.done() {
            let want = end
                .saturating_sub(pos)
                .min(u64::try_from(chunk).unwrap_or(u64::MAX));
            let granted = usize::try_from(self.budget.take(want)).unwrap_or(0);
            if granted == 0 {
                return;
            }
            let mut filled = carry;
            let limit = carry.saturating_add(granted).min(buf.len());
            while filled < limit {
                let Some(slot) = buf.get_mut(filled..limit) else {
                    break;
                };
                match file.read(slot) {
                    Ok(0) => break,
                    Ok(n) => filled = filled.saturating_add(n),
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            let read = filled.saturating_sub(carry);
            let read64 = u64::try_from(read).unwrap_or(0);
            self.bytes.fetch_add(read64, Ordering::Relaxed);
            if let Some(unused) = granted.checked_sub(read)
                && unused > 0
            {
                // Unused budget goes back.
                self.budget
                    .0
                    .fetch_add(u64::try_from(unused).unwrap_or(0), Ordering::AcqRel);
            }
            pos = pos.saturating_add(read64);
            let Some(window) = buf.get(..filled) else {
                return;
            };
            let eof = read == 0 || pos >= len;
            self.scan(window, first, eof);
            if read == 0 {
                return;
            }
            let keep = self.overlap.min(filled);
            buf.copy_within(filled.saturating_sub(keep)..filled, 0);
            carry = keep;
            first = false;
        }
    }
}

/// Which candidates `files` prove: a needle occurs with a separator (or the file's edge)
/// on both sides. Files are cut into segments read in parallel on up to `threads` threads
/// (0 = one per CPU), in chunks with overlap, within `limits`; reading stops once every
/// candidate with a needle is proven.
pub(crate) fn search(
    files: &[PathBuf],
    needles: &[Needle],
    limits: Limits,
    threads: usize,
) -> (HashSet<usize>, SearchStats) {
    let started = Instant::now();
    let cands = needles
        .iter()
        .map(|n| n.cand.saturating_add(1))
        .max()
        .unwrap_or(0);
    let distinct: HashSet<usize> = needles.iter().map(|n| n.cand).collect();
    let overlap = needles
        .iter()
        .map(|n| n.bytes.len())
        .max()
        .unwrap_or(0)
        .saturating_add(8);
    let search = Search {
        finders: needles
            .iter()
            .filter(|n| !n.bytes.is_empty())
            .map(|n| (Finder::new(&n.bytes), n))
            .collect(),
        hits: (0..cands).map(|_| AtomicBool::new(false)).collect(),
        remaining: AtomicUsize::new(distinct.len()),
        budget: Budget(AtomicU64::new(limits.max_total)),
        limits,
        overlap,
        bytes: AtomicU64::new(0),
        files: AtomicUsize::new(0),
        skipped: AtomicUsize::new(0),
    };
    // Segments in file order: the most telling files get the budget first.
    let segment = u64::try_from(limits.chunk.max(overlap.saturating_mul(2)))
        .unwrap_or(u64::MAX)
        .saturating_mul(SEGMENT_CHUNKS);
    let mut work: Vec<(&Path, u64, u64, u64)> = Vec::new();
    if !search.finders.is_empty() {
        for path in files {
            let len = match std::fs::metadata(path) {
                Ok(meta) if meta.is_file() && meta.len() <= limits.max_file => meta.len(),
                _ => {
                    search.skipped.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            };
            search.files.fetch_add(1, Ordering::Relaxed);
            let mut start = 0;
            while start < len {
                let end = start.saturating_add(segment).min(len);
                work.push((path, start, end, len));
                start = end;
            }
        }
    }
    if !work.is_empty() {
        let threads = match threads {
            0 => std::thread::available_parallelism().map_or(4, usize::from),
            n => n,
        }
        .min(work.len())
        .max(1);
        let next = AtomicUsize::new(0);
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| {
                    while let Some(&(path, start, end, len)) =
                        work.get(next.fetch_add(1, Ordering::AcqRel))
                    {
                        if search.done() {
                            return;
                        }
                        search.segment(path, start, end, len);
                    }
                });
            }
        });
    }
    let hits = search
        .hits
        .iter()
        .enumerate()
        .filter(|(_, f)| f.load(Ordering::Acquire))
        .map(|(i, _)| i)
        .collect();
    let stats = SearchStats {
        files: search.files.load(Ordering::Acquire),
        skipped: search.skipped.load(Ordering::Acquire),
        bytes: search.bytes.load(Ordering::Acquire),
        elapsed: started.elapsed(),
    };
    (hits, stats)
}

/// Folders commands are searched in: `PATH` plus the usual per-user and package-manager
/// folders (a GUI or daemon often runs with a minimal `PATH`).
pub(crate) fn command_dirs(path_var: Option<&OsStr>, home: Option<&Path>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = path_var
        .map(|p| std::env::split_paths(p).collect())
        .unwrap_or_default();
    let fixed: &[&str] = if cfg!(windows) {
        &[]
    } else {
        &[
            "/usr/local/bin",
            "/opt/homebrew/bin",
            "/opt/local/bin",
            "/usr/bin",
            "/bin",
            "/snap/bin",
        ]
    };
    out.extend(fixed.iter().map(PathBuf::from));
    if let Some(home) = home {
        for rel in [
            ".local/bin",
            "bin",
            ".cargo/bin",
            ".bun/bin",
            ".deno/bin",
            ".npm-global/bin",
            ".volta/bin",
        ] {
            out.push(home.join(rel));
        }
    }
    let mut seen = HashSet::new();
    out.retain(|d| d.is_absolute() && seen.insert(d.clone()));
    out
}

/// Largest command script read to see whether it starts the app.
const MAX_SHIM_BYTES: u64 = 64 * 1024;

/// Commands on the search path named like candidates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Commands {
    /// Normalised names of commands that are another program.
    pub(crate) foreign: HashSet<String>,
    /// Normalised names of commands that are the app's: links into `own`, or small
    /// scripts that start a program in `own` (Homebrew's `exec '/Applications/X.app/…'`).
    pub(crate) own: HashSet<String>,
}

/// `path` is a small `#!` script naming one of `own`.
fn is_shim(path: &Path, own: &[PathBuf]) -> bool {
    let small = std::fs::metadata(path).is_ok_and(|m| m.len() <= MAX_SHIM_BYTES);
    if !small {
        return false;
    }
    let Ok(text) = std::fs::read(path) else {
        return false;
    };
    text.starts_with(b"#!")
        && own.iter().any(|root| {
            let root = root.to_string_lossy();
            !root.is_empty() && memchr::memmem::find(&text, root.as_bytes()).is_some()
        })
}

/// The commands in `dirs` whose normalised names are in `wanted`, split into the app's
/// own (resolving into `own`, or a script starting it) and other programs.
pub(crate) fn commands(dirs: &[PathBuf], wanted: &HashSet<String>, own: &[PathBuf]) -> Commands {
    const EXTS: &[&str] = &[".exe", ".cmd", ".bat", ".ps1", ".com"];
    let mut out = Commands::default();
    if wanted.is_empty() {
        return out;
    }
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let lower = name.to_ascii_lowercase();
            let stem = EXTS
                .iter()
                .find_map(|e| lower.strip_suffix(e))
                .unwrap_or(&lower);
            let n = normalize(stem);
            if !wanted.contains(&n) {
                continue;
            }
            let path = entry.path();
            let Ok(real) = std::fs::canonicalize(&path) else {
                continue;
            };
            if !real.is_file() {
                continue;
            }
            let inside = own.iter().any(|root| {
                omc_scan::paths::is_within(&real, root) || omc_scan::paths::is_within(&path, root)
            });
            if inside || is_shim(&real, own) {
                out.own.insert(n);
            } else {
                out.foreign.insert(n);
            }
        }
    }
    out
}

/// What another installed app says about a candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rival {
    /// No other app matches the name.
    None,
    /// Another app matches it, less strongly.
    Weaker,
    /// Another app matches it at least as strongly.
    Equal,
}

/// Everything known about one candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Signals {
    pub(crate) name: Option<NameMatch>,
    /// The app's binaries contain its path.
    pub(crate) binary: bool,
    /// The running app has files open below it.
    pub(crate) open: bool,
    pub(crate) command: Command,
    pub(crate) rival: Rival,
}

/// A command-line tool named like a candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Command {
    /// None.
    None,
    /// The app's own tool (XDG: a tool owns `~/.config/<tool>`).
    Own,
    /// Another program on the search path (it may own the folder); wins over [`Command::Own`].
    Foreign,
}

/// The confidence `s` supports; `None`: not the app's.
///
/// Direct evidence (binary, open files) makes a name-related candidate [`Confidence::High`],
/// an exact name [`Confidence::Medium`], a partial one [`Confidence::Low`]. A rival app
/// matching at least as strongly leaves direct evidence at Medium and drops name-only
/// matches; a weaker rival costs name-only matches one level. An exact name of the
/// app's own command-line tool counts like direct evidence. A same-named command outside
/// the app caps everything at Low.
pub(crate) fn confidence(s: Signals) -> Option<Confidence> {
    let name = s.name?;
    let own_tool = s.command == Command::Own && name == NameMatch::Exact;
    let level = if s.binary || s.open || own_tool {
        if s.rival == Rival::Equal {
            Confidence::Medium
        } else {
            Confidence::High
        }
    } else {
        match (name, s.rival) {
            (NameMatch::Exact, Rival::None) => Confidence::Medium,
            (NameMatch::Exact, Rival::Weaker) | (NameMatch::Partial, Rival::None) => {
                Confidence::Low
            }
            (_, Rival::Equal) | (NameMatch::Partial, Rival::Weaker) => return None,
        }
    };
    Some(if s.command == Command::Foreign {
        level.min(Confidence::Low)
    } else {
        level
    })
}

/// A candidate with the name and binary evidence found for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Lead {
    pub(crate) candidate: Candidate,
    pub(crate) name: NameMatch,
    pub(crate) binary: bool,
    pub(crate) foreign_command: bool,
    /// The app's own command-line tool has the candidate's name (bundled, or on the
    /// search path resolving into the app).
    pub(crate) own_command: bool,
}

impl Lead {
    /// The command evidence: a foreign command wins over the app's own.
    pub(crate) const fn command(&self) -> Command {
        if self.foreign_command {
            Command::Foreign
        } else if self.own_command {
            Command::Own
        } else {
            Command::None
        }
    }
}

/// What the app offers to [`leads`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct AppSide<'a> {
    pub(crate) names: &'a Names,
    /// Executables, libraries and archives to search, most telling first.
    pub(crate) files: &'a [PathBuf],
    /// The bundle or install folder(s): commands resolving into them are the app's.
    pub(crate) own: &'a [PathBuf],
    /// A Windows program: search backslash and UTF-16LE forms too.
    pub(crate) wide: bool,
}

/// The name-related candidates of `bases` with their evidence (no rival judgement), and
/// what the binary search did.
pub(crate) fn leads(
    bases: &Bases,
    app: AppSide<'_>,
    command_dirs: &[PathBuf],
    limits: Limits,
    threads: usize,
) -> (Vec<Lead>, SearchStats) {
    if app.names.is_empty() {
        return (Vec::new(), SearchStats::default());
    }
    let related: Vec<(Candidate, NameMatch)> = candidates(bases)
        .into_iter()
        .filter_map(|c| app.names.relate(&c.stem).map(|m| (c, m)))
        .collect();
    let needles: Vec<Needle> = related
        .iter()
        .enumerate()
        .flat_map(|(i, (c, _))| needles(c, i, app.wide))
        .collect();
    let (hits, stats) = search(app.files, &needles, limits, threads);
    let wanted: HashSet<String> = related.iter().map(|(c, _)| normalize(&c.stem)).collect();
    let found = commands(command_dirs, &wanted, app.own);
    let out = related
        .into_iter()
        .enumerate()
        .map(|(i, (candidate, name))| Lead {
            foreign_command: found.foreign.contains(&normalize(&candidate.stem)),
            own_command: {
                let n = normalize(&candidate.stem);
                found.own.contains(&n) || app.names.commands.contains(&n)
            },
            binary: hits.contains(&i),
            name,
            candidate,
        })
        .collect();
    (out, stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh temp folder, removed on drop.
    struct Temp(PathBuf);

    impl Temp {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "omc-userconf-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ignored = std::fs::remove_dir_all(&dir);
            assert!(std::fs::create_dir_all(&dir).is_ok(), "temp dir created");
            Self(dir)
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ignored = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            assert!(std::fs::create_dir_all(parent).is_ok(), "parent created");
        }
        assert!(std::fs::write(path, bytes).is_ok(), "file written");
    }

    fn mkdir(path: &Path) {
        assert!(std::fs::create_dir_all(path).is_ok(), "dir created");
    }

    fn needle(text: &str, cand: usize) -> Needle {
        Needle {
            bytes: text.as_bytes().to_vec(),
            wide: false,
            cand,
        }
    }

    fn found(file: &Path, needles: &[Needle], limits: Limits) -> (HashSet<usize>, SearchStats) {
        search(&[file.to_path_buf()], needles, limits, 1)
    }

    #[test]
    fn needle_straddling_chunks_is_found() {
        let tmp = Temp::new("straddle");
        let file = tmp.0.join("bin");
        let limits = Limits {
            chunk: 64,
            ..Limits::default()
        };
        // Every split position of the needle across the 64-byte chunk edges and the
        // 256-byte segment edge (segments are searched on separate threads).
        for offset in 40..300 {
            let mut bytes = vec![b'#'; offset];
            bytes.extend_from_slice(b"\0.config/foo\0");
            bytes.extend(std::iter::repeat_n(b'#', 200));
            write(&file, &bytes);
            let (hits, _) = search(
                std::slice::from_ref(&file),
                &[needle(".config/foo", 0)],
                limits,
                4,
            );
            assert!(
                hits.contains(&0),
                "needle at offset {offset} found across chunks"
            );
        }
    }

    #[test]
    fn word_context_rejects_longer_names_even_across_chunks() {
        let tmp = Temp::new("context");
        let file = tmp.0.join("bin");
        let limits = Limits {
            chunk: 32,
            ..Limits::default()
        };
        for offset in 10..60 {
            let mut bytes = vec![b'#'; offset];
            bytes.extend_from_slice(b".config/foobar x.config/foo2 com.foo.claude");
            bytes.extend(std::iter::repeat_n(b'#', 100));
            write(&file, &bytes);
            let (hits, _) = found(
                &file,
                &[needle(".config/foo", 0), needle(".claude", 1)],
                limits,
            );
            assert!(
                hits.is_empty(),
                "longer names at offset {offset} are not a match"
            );
        }
        write(&file, b".claude");
        let (hits, _) = found(&file, &[needle(".claude", 1)], limits);
        assert!(hits.contains(&1), "file edges are boundaries");
        write(&file, b"see ~/.config/foo.");
        let (hits, _) = found(&file, &[needle(".config/foo", 0)], limits);
        assert!(hits.contains(&0), "a sentence-ending dot is a boundary");
    }

    #[test]
    fn wide_needles_match_utf16() {
        let tmp = Temp::new("wide");
        let file = tmp.0.join("app.exe");
        let text: Vec<u8> = "C:\\x\\.config\\foo\\settings"
            .bytes()
            .flat_map(|b| [b, 0])
            .collect();
        write(&file, &text);
        let c = Candidate {
            path: tmp.0.join(".config/foo"),
            name: "foo".into(),
            stem: "foo".into(),
            base: Base::Config,
            is_dir: true,
        };
        let (hits, _) = search(
            std::slice::from_ref(&file),
            &needles(&c, 0, true),
            Limits::default(),
            1,
        );
        assert!(hits.contains(&0), "UTF-16LE path found");
        let (hits, _) = search(&[file], &needles(&c, 0, false), Limits::default(), 1);
        assert!(hits.is_empty(), "narrow needles do not match wide text");
    }

    #[test]
    fn byte_caps_are_respected() {
        let tmp = Temp::new("caps");
        let early = tmp.0.join("early");
        let late = tmp.0.join("late");
        let mut bytes = vec![b'#'; 10_000];
        bytes.extend_from_slice(b" .config/foo ");
        write(&late, &bytes);
        write(&early, &[b'#'; 100]);
        let limits = Limits {
            max_total: 5_000,
            max_file: 1 << 20,
            chunk: 1_000,
        };
        let (hits, stats) = search(
            &[early.clone(), late.clone()],
            &[needle(".config/foo", 0)],
            limits,
            1,
        );
        assert!(hits.is_empty(), "the match lies beyond the total cap");
        assert!(
            stats.bytes <= 5_000,
            "at most max_total bytes read: {}",
            stats.bytes
        );
        let small_files = Limits {
            max_file: 1_000,
            ..Limits::default()
        };
        let (hits, stats) = search(&[late], &[needle(".config/foo", 0)], small_files, 1);
        assert!(hits.is_empty(), "files above max_file are skipped");
        assert_eq!((stats.skipped, stats.bytes), (1, 0), "skipped unread");
    }

    #[test]
    fn candidates_skip_shared_tools_and_strip_dotfile_suffixes() {
        let tmp = Temp::new("cands");
        let home = &tmp.0;
        for dir in [
            ".config/foo",
            ".config/git",
            ".config/nvim",
            ".local/share/foo",
            ".cache/foo",
            ".ssh",
            ".npm",
            ".foo",
            ".cargo",
        ] {
            mkdir(&home.join(dir));
        }
        for file in [
            ".foorc",
            ".foo.json",
            ".zshrc",
            ".gitconfig",
            ".bash_history",
            "visible",
        ] {
            write(&home.join(file), b"x");
        }
        let bases = Bases {
            home: home.clone(),
            config: Some(home.join(".config")),
            data: Some(home.join(".local/share")),
            state: Some(home.join(".local/state")),
            cache: Some(home.join(".cache")),
        };
        let mut got: Vec<(Base, String, String)> = candidates(&bases)
            .into_iter()
            .map(|c| (c.base, c.name, c.stem))
            .collect();
        got.sort_by(|a, b| (format!("{:?}", a.0), &a.1).cmp(&(format!("{:?}", b.0), &b.1)));
        let want = |b, n: &str, s: &str| (b, n.to_owned(), s.to_owned());
        assert_eq!(
            got,
            vec![
                want(Base::Cache, "foo", "foo"),
                want(Base::Config, "foo", "foo"),
                want(Base::Data, "foo", "foo"),
                want(Base::Home, "foo", "foo"),
                want(Base::Home, "foo.json", "foo"),
                want(Base::Home, "foorc", "foo"),
            ],
            "shared tools, histories, base folders and non-dot entries dropped"
        );
    }

    #[test]
    fn names_relate_exactly_or_partially() {
        let mut names = Names::default();
        names.add_display("CherryStudio");
        names.add(
            "com.mitchellh.ghostty"
                .rsplit('.')
                .next()
                .unwrap_or_default(),
        );
        names.add("studio");
        assert_eq!(
            names.relate("Ghostty"),
            Some(NameMatch::Exact),
            "normalised equality"
        );
        assert_eq!(
            names.relate("cherry"),
            Some(NameMatch::Partial),
            "distinctive word"
        );
        assert_eq!(
            names.relate("ghostty-nightly"),
            Some(NameMatch::Partial),
            "extension of a name"
        );
        assert_eq!(names.relate("studio"), None, "generic words never match");
        assert_eq!(names.relate("git"), None, "shared tools never match");
        assert_eq!(names.relate("gho"), None, "short prefixes never match");
    }

    #[test]
    fn confidence_rules() {
        let base = Signals {
            name: Some(NameMatch::Exact),
            binary: false,
            open: false,
            command: Command::None,
            rival: Rival::None,
        };
        let c = confidence;
        assert_eq!(
            c(Signals {
                binary: true,
                ..base
            }),
            Some(Confidence::High),
            "binary proof"
        );
        assert_eq!(c(base), Some(Confidence::Medium), "unique exact name");
        assert_eq!(
            c(Signals {
                name: Some(NameMatch::Partial),
                ..base
            }),
            Some(Confidence::Low),
            "partial name"
        );
        assert_eq!(
            c(Signals {
                command: Command::Foreign,
                binary: true,
                ..base
            }),
            Some(Confidence::Low),
            "a same-named command elsewhere caps at low"
        );
        assert_eq!(
            c(Signals {
                rival: Rival::Equal,
                ..base
            }),
            None,
            "rival owns name-only match"
        );
        assert_eq!(
            c(Signals {
                rival: Rival::Equal,
                open: true,
                ..base
            }),
            Some(Confidence::Medium),
            "direct evidence against an equal rival"
        );
        assert_eq!(
            c(Signals {
                name: None,
                binary: true,
                ..base
            }),
            None,
            "name required"
        );
        assert_eq!(
            c(Signals {
                command: Command::Own,
                ..base
            }),
            Some(Confidence::High),
            "~/.config/<tool> of the app's own command-line tool"
        );
        assert_eq!(
            c(Signals {
                command: Command::Own,
                name: Some(NameMatch::Partial),
                ..base
            }),
            Some(Confidence::Low),
            "an own tool proves only its exact name"
        );
    }

    #[test]
    fn leads_combine_binary_name_and_command_evidence() {
        let tmp = Temp::new("leads");
        let home = tmp.0.join("home");
        let app = tmp.0.join("Foo.app");
        let bin = tmp.0.join("bin");
        for dir in [".config/foo", ".config/bar", ".config/baz", ".config/git"] {
            mkdir(&home.join(dir));
        }
        write(&app.join("foo"), b"\0\0~/.config/foo/config\0.config/git\0");
        // `bar`: a command outside the app; `baz`: the app's own command link target.
        write(&bin.join("bar"), b"#!/bin/sh");
        let mut names = Names::default();
        for n in ["Foo", "bar", "baz", "git"] {
            names.add(n);
        }
        let files = [app.join("foo")];
        let own = [app.clone()];
        let side = AppSide {
            names: &names,
            files: &files,
            own: &own,
            wide: false,
        };
        let bases = Bases {
            home: home.clone(),
            config: Some(home.join(".config")),
            data: None,
            state: None,
            cache: None,
        };
        let (leads, stats) = leads(
            &bases,
            side,
            std::slice::from_ref(&bin),
            Limits::default(),
            2,
        );
        assert_eq!(stats.files, 1, "the app binary was read");
        let get = |n: &str| leads.iter().find(|l| l.candidate.name == n);
        let foo = get("foo").map(|l| (l.binary, l.foreign_command));
        assert_eq!(foo, Some((true, false)), "path compiled into the binary");
        let bar = get("bar").map(|l| (l.binary, l.foreign_command));
        assert_eq!(
            bar,
            Some((false, true)),
            "same-named command outside the app"
        );
        let baz = get("baz").map(|l| (l.binary, l.foreign_command, l.name));
        assert_eq!(
            baz,
            Some((false, false, NameMatch::Exact)),
            "unique name only"
        );
        assert!(
            get("git").is_none(),
            "shared tools excluded even when referenced"
        );
        assert_eq!(
            get("foo").map(|l| l.candidate.kind()),
            Some(AppFileKind::Preferences),
            "config kind"
        );
    }

    #[test]
    fn command_scripts_starting_the_app_are_its_own() {
        let tmp = Temp::new("shims");
        let app = tmp.0.join("Apps/Foo.app");
        let bin = tmp.0.join("bin");
        write(&app.join("Contents/Resources/CLI/foo"), b"\x7fELF");
        let script = format!(
            "#!/bin/bash\nexec '{}/Contents/Resources/CLI/foo' \"$@\"\n",
            app.display()
        );
        write(&bin.join("foo"), script.as_bytes());
        write(
            &bin.join("bar"),
            b"#!/bin/bash\nexec /usr/local/lib/bar/run \"$@\"\n",
        );
        let wanted: HashSet<String> = ["foo", "bar"].iter().map(|s| (*s).to_owned()).collect();
        let found = commands(
            std::slice::from_ref(&bin),
            &wanted,
            std::slice::from_ref(&app),
        );
        assert!(
            found.own.contains("foo") && !found.foreign.contains("foo"),
            "a script exec-ing into the bundle is the app's own tool"
        );
        assert!(
            found.foreign.contains("bar") && !found.own.contains("bar"),
            "a script starting something else is another program"
        );
    }
}
