//! Reports about individual files: large/old files, duplicates, and the space-lens tree.

use serde::{Deserialize, Serialize};

use crate::jobs::{Denied, ItemId};

/// Output of `large_old_files`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileReport {
    /// Matches, largest first.
    pub files: Vec<FileEntry>,
    /// More files matched than the report holds (the smallest were dropped).
    #[serde(default)]
    pub truncated: bool,
    /// Places the scan could not read.
    #[serde(default)]
    pub denied: Vec<Denied>,
}

/// One file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Id for `clean`.
    pub id: ItemId,
    /// Absolute path.
    pub path: String,
    /// Size on disk in bytes.
    pub bytes: u64,
    /// Last modification (Unix seconds).
    #[serde(default)]
    pub modified: Option<i64>,
    /// Last access (Unix seconds), when the file system records it.
    #[serde(default)]
    pub accessed: Option<i64>,
    /// Coarse type from the extension.
    pub kind: FileKind,
    /// Matched the size threshold.
    #[serde(default)]
    pub large: bool,
    /// Matched the age threshold.
    #[serde(default)]
    pub old: bool,
}

/// Coarse file types (UI key `files.kind.<snake_case>`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    /// Video.
    Video,
    /// Audio.
    Audio,
    /// Images and RAW photos.
    Image,
    /// Archives (zip, tar, 7z, rar…).
    Archive,
    /// Disk images and installers (dmg, iso, pkg, msi…).
    DiskImage,
    /// Documents (pdf, office, text…).
    Document,
    /// Virtual machine disks.
    VirtualMachine,
    /// Anything else.
    #[default]
    Other,
}

/// Output of `duplicates`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DupReport {
    /// Groups of identical files, most wasted space first.
    pub groups: Vec<DupGroup>,
    /// More groups existed than the report holds.
    #[serde(default)]
    pub truncated: bool,
    /// Places the scan could not read.
    #[serde(default)]
    pub denied: Vec<Denied>,
}

/// Files with byte-identical content (same size, same XXH3-128 of the whole content).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DupGroup {
    /// Size of each file.
    pub bytes: u64,
    /// The copies, oldest modification first (the usual "original").
    pub files: Vec<FileEntry>,
}

impl DupGroup {
    /// Bytes freed by keeping one copy.
    pub fn wasted(&self) -> u64 {
        let copies = u64::try_from(self.files.len()).unwrap_or(u64::MAX);
        self.bytes.saturating_mul(copies.saturating_sub(1))
    }
}

/// One directory level of a space-lens tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceListing {
    /// The listed node (its id is the `node` of `space_children`; the root is 0).
    pub node: ItemId,
    /// Its absolute path.
    pub path: String,
    /// Its total size.
    pub bytes: u64,
    /// Files below it.
    pub files: u64,
    /// Children, largest first.
    pub children: Vec<SpaceNode>,
    /// Places the scan could not read (root listing only).
    #[serde(default)]
    pub denied: Vec<Denied>,
}

/// A child in a [`SpaceListing`]. Its id is valid for `space_children` (directories) and
/// for `clean` (directories and files; not `rest`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceNode {
    /// Node id inside the job.
    pub id: ItemId,
    /// File or directory name.
    pub name: String,
    /// Total size.
    pub bytes: u64,
    /// Files below it (1 for a file).
    pub files: u64,
    /// What it is.
    pub kind: SpaceKind,
}

/// Kinds of space-lens nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpaceKind {
    /// A directory (can be opened).
    Dir,
    /// A file.
    File,
    /// Many small files of one directory, aggregated to keep the tree small; `files` says
    /// how many. Not removable as a unit.
    Rest,
}
