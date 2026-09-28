//! [`SettingsStore`]: the daemon's persisted [`Settings`] (`settings.toml` in the config
//! dir). Loaded once at startup; every `put` is validated, written atomically (temp file in
//! the same directory + rename) and only then becomes visible to `get`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use omc_proto::settings::{CleanSettings, Settings};
use parking_lot::Mutex;

use crate::{Error, Result};

/// File name inside the config dir.
const FILE_NAME: &str = "settings.toml";
/// Upper bound for `scan_threads` (0 still means "one per logical CPU").
const MAX_SCAN_THREADS: u16 = 256;
/// Allowed range of `dev_project_max_depth`.
const PROJECT_DEPTH: std::ops::RangeInclusive<u16> = 1..=32;
/// 1 MiB, the smallest "large file" threshold.
const MIB: u64 = 1 << 20;

/// Where the daemon keeps `settings.toml`: macOS `~/Library/Application Support/
/// dev.zerx.oh-my-clear`, Windows `%APPDATA%\oh-my-clear`, other Unix
/// `$XDG_CONFIG_HOME/oh-my-clear` (default `~/.config/oh-my-clear`). `None` when the
/// environment names no usable base directory.
pub fn default_settings_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join(FILE_NAME))
}

#[cfg(target_os = "macos")]
fn config_dir() -> Option<PathBuf> {
    omc_scan::paths::home().map(|home| {
        home.join("Library")
            .join("Application Support")
            .join("dev.zerx.oh-my-clear")
    })
}

#[cfg(windows)]
fn config_dir() -> Option<PathBuf> {
    omc_scan::paths::env_dir("APPDATA")
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("oh-my-clear"))
}

#[cfg(not(any(target_os = "macos", windows)))]
fn config_dir() -> Option<PathBuf> {
    omc_scan::paths::env_dir("XDG_CONFIG_HOME")
        .filter(|dir| dir.is_absolute())
        .or_else(|| omc_scan::paths::home().map(|home| home.join(".config")))
        .map(|dir| dir.join("oh-my-clear"))
}

/// The current settings plus where they are persisted.
#[derive(Debug)]
pub(crate) struct SettingsStore {
    /// `None`: in memory only (tests).
    path: Option<PathBuf>,
    current: Mutex<Settings>,
    /// Serializes writers so the file and `current` always agree.
    writing: Mutex<()>,
}

impl SettingsStore {
    /// In-memory store with default settings.
    pub(crate) fn in_memory() -> Self {
        Self {
            path: None,
            current: Mutex::new(Settings::default()),
            writing: Mutex::new(()),
        }
    }

    /// Loads `path`: a missing file gives defaults; an unparsable one is logged, renamed to
    /// `<path>.bak` and replaced by defaults.
    pub(crate) fn load(path: PathBuf) -> Self {
        let settings = read_file(&path);
        Self {
            path: Some(path),
            current: Mutex::new(settings),
            writing: Mutex::new(()),
        }
    }

    /// A copy of the current settings.
    pub(crate) fn get(&self) -> Settings {
        self.current.lock().clone()
    }

    /// Validates, persists and publishes `settings`. Blocking (file I/O).
    pub(crate) fn put(&self, mut settings: Settings) -> Result<()> {
        sanitize(&mut settings.clean);
        let _writer = self.writing.lock();
        if let Some(path) = &self.path {
            write_atomic(path, &toml::to_string_pretty(&settings)?)?;
        }
        *self.current.lock() = settings;
        Ok(())
    }
}

/// Reads and validates the settings file; any problem yields defaults.
fn read_file(path: &Path) -> Settings {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!(path = %path.display(), "no settings file; using defaults");
            return Settings::default();
        }
        Err(err) if err.kind() == std::io::ErrorKind::InvalidData => {
            tracing::warn!(path = %path.display(), %err, "settings file is not UTF-8");
            keep_backup(path);
            return Settings::default();
        }
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "cannot read settings; using defaults");
            return Settings::default();
        }
    };
    match toml::from_str::<Settings>(&text) {
        Ok(mut settings) => {
            sanitize(&mut settings.clean);
            settings
        }
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "corrupt settings file; using defaults");
            keep_backup(path);
            Settings::default()
        }
    }
}

/// Moves an unreadable settings file aside to `<name>.bak` so the user can recover it.
fn keep_backup(path: &Path) {
    let mut name = path.as_os_str().to_owned();
    name.push(".bak");
    let backup = PathBuf::from(name);
    match std::fs::rename(path, &backup) {
        Ok(()) => tracing::warn!(backup = %backup.display(), "kept the corrupt settings file"),
        Err(err) => tracing::warn!(backup = %backup.display(), %err, "cannot back up settings"),
    }
}

/// Clamps values to what the scanners support and drops empty paths.
fn sanitize(clean: &mut CleanSettings) {
    clean.scan_threads = clean.scan_threads.min(MAX_SCAN_THREADS);
    clean.dev_project_max_depth = clean
        .dev_project_max_depth
        .clamp(*PROJECT_DEPTH.start(), *PROJECT_DEPTH.end());
    clean.large_min_bytes = clean.large_min_bytes.max(MIB);
    clean.dup_min_bytes = clean.dup_min_bytes.max(1);
    for list in [&mut clean.exclude, &mut clean.file_roots] {
        list.retain_mut(|path| {
            let trimmed = path.trim();
            if trimmed.len() != path.len() {
                *path = trimmed.to_owned();
            }
            !path.is_empty()
        });
    }
}

/// Writes `text` to a temp file next to `path`, flushes it and renames it over `path`, so
/// readers only ever see the old or the new file.
fn write_atomic(path: &Path, text: &str) -> Result<()> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let io_err = |source| Error::SettingsIo {
        path: path.to_path_buf(),
        source,
    };
    let dir = path.parent().ok_or_else(|| {
        io_err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "settings path has no parent directory",
        ))
    })?;
    std::fs::create_dir_all(dir).map_err(io_err)?;
    let mut tmp_name = path.file_name().unwrap_or_default().to_owned();
    tmp_name.push(format!(
        ".tmp-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let tmp = dir.join(tmp_name);
    let written = write_synced(&tmp, text).and_then(|()| std::fs::rename(&tmp, path));
    if let Err(err) = written {
        if let Err(cleanup) = std::fs::remove_file(&tmp)
            && cleanup.kind() != std::io::ErrorKind::NotFound
        {
            tracing::debug!(tmp = %tmp.display(), %cleanup, "temp settings file left behind");
        }
        return Err(io_err(err));
    }
    Ok(())
}

fn write_synced(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut file = std::fs::File::create(path)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_clamps_and_trims() {
        let mut clean = CleanSettings {
            scan_threads: 9_999,
            dev_project_max_depth: 0,
            large_min_bytes: 5,
            dup_min_bytes: 0,
            exclude: vec!["  ".to_owned(), " /tmp/x ".to_owned()],
            ..CleanSettings::default()
        };
        sanitize(&mut clean);
        assert_eq!(clean.scan_threads, MAX_SCAN_THREADS, "threads capped");
        assert_eq!(clean.dev_project_max_depth, 1, "depth raised to 1");
        assert_eq!(clean.large_min_bytes, MIB, "large threshold at least 1 MiB");
        assert_eq!(clean.dup_min_bytes, 1, "dup threshold at least 1 byte");
        assert_eq!(
            clean.exclude,
            vec!["/tmp/x".to_owned()],
            "blank dropped, rest trimmed"
        );

        clean.dev_project_max_depth = 500;
        sanitize(&mut clean);
        assert_eq!(clean.dev_project_max_depth, 32, "depth capped at 32");
    }
}
