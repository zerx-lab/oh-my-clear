//! The main window's navigation: every cleaning area, grouped the way established cleaners
//! group them (CleanMyMac: Cleanup / My Clutter / Space Lens / Applications; czkawka:
//! duplicates, big files, temporary files; Mole: clean / purge / analyze / uninstall /
//! installer; Stacer and Windows Storage Sense: caches, logs, recycle bin, startup apps).
//!
//! Pure data: labels resolve through rust-i18n, icons through [`crate::assets::Assets`].

use gpui_kit::SharedString;
use gpui_kit::assets::IconName;

/// One destination in the sidebar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Category {
    /// Start page: what the app does and the commands that exist. Has a title only.
    #[default]
    Overview,
    /// Caches, logs, crash reports and temporary files of the OS and apps.
    SystemJunk,
    /// Browser caches, cookies, history and site data.
    BrowserData,
    /// Build output and package-manager caches (`node_modules`, `target`, `DerivedData`…).
    DeveloperJunk,
    /// Deleted items that still hold space (Trash / Recycle Bin).
    Trash,
    /// Visual map of what fills a disk.
    SpaceLens,
    /// Files that are very large or untouched for a long time.
    LargeOldFiles,
    /// Files with identical content.
    Duplicates,
    /// Removes an app together with its support files.
    Uninstaller,
    /// Support files of apps that are no longer installed.
    Leftovers,
    /// Installer packages that were already used.
    Installers,
    /// Programs and services launched at login.
    StartupItems,
    /// Long-lived automation rules the daemon runs on its own (ADR 0024).
    Rules,
    /// Runs of those rules: pending decisions and the history.
    Activity,
}

/// A titled run of sidebar entries.
#[derive(Debug)]
pub struct NavGroup {
    /// i18n key of the group heading; `None` for the ungrouped top entry.
    pub label: Option<&'static str>,
    /// Entries in display order.
    pub items: &'static [Category],
}

impl NavGroup {
    /// Stable identifier (the label key without its prefix): what the sidebar persists
    /// for a collapsed group. `None` for the ungrouped top entry, which never collapses.
    pub fn id(&self) -> Option<&'static str> {
        self.label
            .map(|label| label.strip_prefix("nav.group.").unwrap_or(label))
    }
}

/// Sidebar layout, top to bottom. Every [`Category`] appears exactly once.
pub const NAV: &[NavGroup] = &[
    NavGroup {
        label: None,
        items: &[Category::Overview],
    },
    NavGroup {
        label: Some("nav.group.cleanup"),
        items: &[
            Category::SystemJunk,
            Category::BrowserData,
            Category::DeveloperJunk,
            Category::Trash,
        ],
    },
    NavGroup {
        label: Some("nav.group.storage"),
        items: &[
            Category::SpaceLens,
            Category::LargeOldFiles,
            Category::Duplicates,
        ],
    },
    NavGroup {
        label: Some("nav.group.applications"),
        items: &[
            Category::Uninstaller,
            Category::Leftovers,
            Category::Installers,
            Category::StartupItems,
        ],
    },
    NavGroup {
        label: Some("nav.group.automation"),
        items: &[Category::Rules, Category::Activity],
    },
];

/// The OS's name for deleted-but-kept items.
const TRASH_TITLE: &str = if cfg!(target_os = "macos") {
    "nav.trash.title_macos"
} else if cfg!(target_os = "windows") {
    "nav.trash.title_windows"
} else {
    "nav.trash.title_linux"
};

impl Category {
    /// Stable identifier: element ids and the i18n key segment.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::SystemJunk => "system_junk",
            Self::BrowserData => "browser_data",
            Self::DeveloperJunk => "developer_junk",
            Self::Trash => "trash",
            Self::SpaceLens => "space_lens",
            Self::LargeOldFiles => "large_old_files",
            Self::Duplicates => "duplicates",
            Self::Uninstaller => "uninstaller",
            Self::Leftovers => "leftovers",
            Self::Installers => "installers",
            Self::StartupItems => "startup_items",
            Self::Rules => "rules",
            Self::Activity => "activity",
        }
    }

    /// Lucide icon shown in the sidebar and the page header.
    pub const fn icon(self) -> IconName {
        match self {
            Self::Overview => IconName::LayoutDashboard,
            Self::SystemJunk => IconName::Broom,
            Self::BrowserData => IconName::Cookie,
            Self::DeveloperJunk => IconName::CodeXml,
            Self::Trash => IconName::Trash,
            Self::SpaceLens => IconName::ChartPie,
            Self::LargeOldFiles => IconName::FileClock,
            Self::Duplicates => IconName::Copy,
            Self::Uninstaller => IconName::PackageX,
            Self::Leftovers => IconName::FolderX,
            Self::Installers => IconName::Disc3,
            Self::StartupItems => IconName::Rocket,
            Self::Rules => IconName::CalendarClock,
            Self::Activity => IconName::Activity,
        }
    }

    fn title_key(self) -> String {
        match self {
            Self::Trash => TRASH_TITLE.to_owned(),
            _ => format!("nav.{}.title", self.key()),
        }
    }

    fn description_key(self) -> String {
        format!("nav.{}.description", self.key())
    }

    fn covers_key(self) -> String {
        format!("nav.{}.covers", self.key())
    }

    /// Sidebar label and page title.
    pub fn title(self) -> SharedString {
        tr(&self.title_key())
    }

    /// One sentence on what the area is for; `None` for the overview.
    pub fn description(self) -> Option<SharedString> {
        (self != Self::Overview).then(|| tr(&self.description_key()))
    }

    /// Concrete examples of what the area looks at; `None` for the overview.
    pub fn covers(self) -> Option<SharedString> {
        (self != Self::Overview).then(|| tr(&self.covers_key()))
    }
}

fn tr(key: &str) -> SharedString {
    rust_i18n::t!(key).to_string().into()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    const ALL: [Category; 14] = [
        Category::Overview,
        Category::SystemJunk,
        Category::BrowserData,
        Category::DeveloperJunk,
        Category::Trash,
        Category::SpaceLens,
        Category::LargeOldFiles,
        Category::Duplicates,
        Category::Uninstaller,
        Category::Leftovers,
        Category::Installers,
        Category::StartupItems,
        Category::Rules,
        Category::Activity,
    ];

    #[test]
    fn every_category_is_listed_exactly_once() {
        let listed: Vec<Category> = NAV.iter().flat_map(|g| g.items.iter().copied()).collect();
        let unique: HashSet<Category> = listed.iter().copied().collect();
        assert_eq!(listed.len(), unique.len(), "no category is listed twice");
        assert_eq!(unique, HashSet::from(ALL), "every category is reachable");
        assert_eq!(
            listed.first(),
            Some(&Category::default()),
            "the default category is the first entry"
        );
    }

    /// A missing key renders as the raw key; English and Chinese parity is checked in
    /// `i18n`, so English suffices here.
    #[test]
    fn every_label_is_translated() {
        let mut keys: Vec<String> = NAV
            .iter()
            .filter_map(|g| g.label.map(str::to_owned))
            .collect();
        for category in ALL {
            keys.push(category.title_key());
            if category != Category::Overview {
                keys.extend([category.description_key(), category.covers_key()]);
            }
        }
        keys.extend(
            [
                "nav.trash.title_macos",
                "nav.trash.title_windows",
                "nav.trash.title_linux",
            ]
            .map(str::to_owned),
        );
        let defined: HashSet<String> = crate::_rust_i18n_backend()
            .messages_for_locale("en")
            .unwrap_or_default()
            .into_iter()
            .map(|(key, _)| key.into_owned())
            .collect();
        let missing: Vec<&String> = keys.iter().filter(|k| !defined.contains(*k)).collect();
        assert!(missing.is_empty(), "untranslated nav keys: {missing:?}");
    }
}
