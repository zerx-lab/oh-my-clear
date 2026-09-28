//! [`Target`]: how one reported item is removed. Every scan returns its report together
//! with one target per item id ([`Scanned`]); the engine keeps both and resolves a clean
//! request's item ids to targets, so removal never trusts a path from the client.

use omc_proto::jobs::{DeleteMethod, ItemId, Location};

/// Removal instructions for one item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// What to remove.
    pub location: Location,
    /// Bytes expected to be freed (reported size).
    pub bytes: u64,
    /// Remove the directory's children but keep the directory (caches, temp, trash).
    pub contents_only: bool,
    /// Only entries not modified for this many seconds are removed (temp files, logs);
    /// 0 = everything.
    pub min_age_secs: u64,
    /// Permanent or to the Trash.
    pub method: DeleteMethod,
    /// Needs administrator rights (goes to the elevated helper directly).
    pub needs_admin: bool,
}

impl Target {
    /// A whole file or directory.
    pub fn path(path: impl Into<String>, bytes: u64, method: DeleteMethod) -> Self {
        Self {
            location: Location::Path { path: path.into() },
            bytes,
            contents_only: false,
            min_age_secs: 0,
            method,
            needs_admin: false,
        }
    }

    /// The children of a directory.
    pub fn contents(path: impl Into<String>, bytes: u64, method: DeleteMethod) -> Self {
        Self {
            contents_only: true,
            ..Self::path(path, bytes, method)
        }
    }

    /// A registry key, special action or other non-path location.
    pub fn location(location: Location, bytes: u64) -> Self {
        Self {
            location,
            bytes,
            contents_only: false,
            min_age_secs: 0,
            method: DeleteMethod::Permanent,
            needs_admin: false,
        }
    }

    /// Sets [`Self::min_age_secs`].
    #[must_use]
    pub fn older_than(mut self, secs: u64) -> Self {
        self.min_age_secs = secs;
        self
    }

    /// Sets [`Self::needs_admin`].
    #[must_use]
    pub fn admin(mut self, needs_admin: bool) -> Self {
        self.needs_admin = needs_admin;
        self
    }
}

/// A report plus the removal target of each of its items: `targets[id]` belongs to the
/// item with that [`ItemId`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scanned<R> {
    /// What the UI sees.
    pub report: R,
    /// Indexed by item id.
    pub targets: Vec<Target>,
}

impl<R> Scanned<R> {
    /// The targets of `items`, skipping unknown ids.
    pub fn select(&self, items: &[ItemId]) -> Vec<Target> {
        items
            .iter()
            .filter_map(|id| self.targets.get(usize::try_from(*id).ok()?).cloned())
            .collect()
    }
}

/// Assigns sequential ids: returns the id for the next target and pushes it.
pub fn push_target(targets: &mut Vec<Target>, target: Target) -> ItemId {
    let id = ItemId::try_from(targets.len()).unwrap_or(ItemId::MAX);
    targets.push(target);
    id
}
