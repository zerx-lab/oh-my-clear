//! Evidence-based attribution of file and folder names to apps.
//!
//! An app is described by a [`Profile`]: its bundle ids (main and embedded), exact-only ids
//! (app groups), team id, names by source (display name, Electron product name,
//! executable…), vendor prefix and vendor words (bundle id, Sparkle feed host, publisher).
//! [`score`] collects the [`Evidence`] a name gives for one profile at a [`Place`];
//! evidence has a weight and the resulting points map to a [`Confidence`]. [`attribute`]
//! compares the app with every other installed app: a name another app matches at least
//! as strongly belongs to nobody, a weaker rival claim costs points. Direct evidence
//! (open files, installer payload, launchd reference, Homebrew zap) is added by the caller
//! through [`attribute_direct`].
//!
//! Pure: no file-system access, so every rule is unit-tested.

use omc_proto::apps::Confidence;

use super::ident::{self, normalize, strip_suffixes, strip_team};

/// Points needed for [`Confidence::High`] (preselected by default).
pub(super) const HIGH: u32 = 100;
/// Points needed for [`Confidence::Medium`].
pub(super) const MEDIUM: u32 = 50;
/// Points needed to be listed at all ([`Confidence::Low`]).
pub(super) const LOW: u32 = 20;
/// Upper bound of a verdict's points.
const MAX_POINTS: u32 = 400;

/// Sub-id components that name a sibling product or channel rather than a part of the app
/// (`com.google.Chrome.canary`, `com.google.chrome.for.testing`, `com.foo.bar.pro`).
const SIBLINGS: &[&str] = &[
    "alpha",
    "beta",
    "canary",
    "dev",
    "edu",
    "enterprise",
    "for",
    "insider",
    "insiders",
    "lite",
    "nightly",
    "plus",
    "preview",
    "pro",
    "testing",
];

/// Words that qualify an app name without naming another product (`Telegram Desktop`,
/// `cherrystudio-updater`).
const QUALIFIERS: &[&str] = &[
    "agent", "app", "client", "data", "desktop", "files", "helper", "launcher", "mac", "macos",
    "osx", "service", "support", "updater",
];

/// Words that are no vendor (`com.app.x`, publisher `Foo Labs Inc`).
const VENDOR_STOPWORDS: &[&str] = &[
    "app", "apps", "com", "corp", "dev", "inc", "io", "labs", "llc", "ltd", "net", "org",
    "software", "studio", "team", "the",
];

/// Lowercase prefixes of frameworks, SDKs and tools embedded in many apps: their folders
/// are shared and never attributed to one app.
pub(super) const SHARED_PREFIXES: &[&str] = &[
    "org.sparkle-project.",
    "com.crashlytics",
    "io.fabric.",
    "io.sentry",
    "com.getsentry.",
    "sentrycrash",
    "com.plausiblelabs.",
    "com.segment.",
    "com.mixpanel.",
    "com.amplitude.",
    "io.branch.",
    "com.firebase.",
    "com.google.firebase.",
    "com.microsoft.autoupdate",
    "org.llvm.",
    "homebrew.",
    "rollbar",
    "amplitude",
    "realm",
    "parse",
];

/// Lowercase prefixes of names that belong to macOS itself.
pub(super) const APPLE_PREFIXES: &[&str] = &[
    "com.apple.",
    "group.com.apple.",
    "systemgroup.com.apple.",
    "is.workflow.",
];

/// `component` (lowercase) of an id below another app's id names a sibling product.
pub(super) fn is_sibling(component: &str) -> bool {
    SIBLINGS.contains(&component)
}

/// `lower` names a shared framework or SDK folder.
pub(super) fn is_shared(lower: &str) -> bool {
    SHARED_PREFIXES.iter().any(|p| lower.starts_with(p))
}

/// `lower` names something of macOS itself.
pub(super) fn is_apple(lower: &str) -> bool {
    APPLE_PREFIXES.iter().any(|p| lower.starts_with(p)) || lower.contains(".com.apple.")
}

/// Where a name was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum NameKind {
    /// Display name, bundle name or bundle file name.
    Primary,
    /// Electron `productName`/`name`: the app's data folder is named exactly this.
    Product,
    /// electron-updater `updaterCacheDirName` (`Caches/<name>`).
    Updater,
    /// `CFBundleExecutable`.
    Executable,
    /// Last bundle-id component (`com.microsoft.VSCode` → `vscode`).
    IdTail,
}

/// Everything that identifies one app.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Profile {
    /// Lowercase ids matched exactly and below (`id.helper`): the app's and its embedded
    /// bundles'.
    pub(super) ids: Vec<String>,
    /// Lowercase ids matched exactly only: app and keychain groups, two-component ids.
    pub(super) exact: Vec<String>,
    /// Code-signing team id (uppercase).
    pub(super) team: Option<String>,
    /// Normalised names with their source.
    pub(super) names: Vec<(String, NameKind)>,
    /// Lowercase words of each primary name (for vendor/product folders).
    primary_tokens: Vec<Vec<String>>,
    /// Lowercase vendor prefix of the bundle id (`com.google`).
    pub(super) vendor: Option<String>,
    /// Normalised vendor words (`google`), from the bundle id, update feed and publisher.
    pub(super) vendor_words: Vec<String>,
    /// The app is an Apple app (`com.apple.*` names may match).
    pub(super) apple: bool,
    /// No other installed app shares the vendor prefix or a vendor word.
    pub(super) sole_vendor: bool,
}

impl Profile {
    /// A profile from a bundle id, display name and executable name.
    pub(super) fn basic(id: Option<&str>, name: &str, executable: Option<&str>) -> Self {
        let mut out = Self::default();
        if let Some(id) = id {
            out.set_main_id(id);
        }
        out.add_name(name, NameKind::Primary);
        if let Some(exe) = executable {
            out.add_name(exe, NameKind::Executable);
        }
        out
    }

    /// Sets the app's own bundle id: vendor, vendor word, id tail.
    pub(super) fn set_main_id(&mut self, id: &str) {
        let lower = id.trim().to_lowercase();
        self.apple = lower.starts_with("com.apple.");
        self.vendor = ident::vendor(&lower);
        let word = self
            .vendor
            .as_deref()
            .and_then(|v| v.split('.').nth(1))
            .map(str::to_owned);
        if let Some(word) = word {
            self.add_vendor_word(&word);
        }
        if lower.split('.').count() >= 3
            && let Some(tail) = id.trim().rsplit('.').next()
        {
            self.add_name(tail, NameKind::IdTail);
        }
        self.add_id(&lower);
    }

    /// Adds a bundle id: three or more components match below it too, shorter ones only
    /// exactly.
    pub(super) fn add_id(&mut self, id: &str) {
        let id = id.trim().to_lowercase();
        let parts = id.split('.').count();
        if parts >= 3 {
            if !self.ids.contains(&id) {
                self.ids.push(id);
            }
        } else if parts == 2 {
            self.add_exact(&id);
        }
    }

    /// Adds an id matched exactly only (`TEAM.group` app groups), with and without its team
    /// prefix and `group.` prefix.
    pub(super) fn add_exact(&mut self, id: &str) {
        let trimmed = id.trim();
        let rest = strip_team(trimmed).map_or(trimmed, |(_, rest)| rest);
        let bare = rest.strip_prefix("group.").unwrap_or(rest);
        for form in [trimmed, bare] {
            let lower = form.to_lowercase();
            if lower.contains('.') && !self.exact.contains(&lower) {
                self.exact.push(lower);
            }
        }
    }

    /// Adds a name when it is specific enough.
    pub(super) fn add_name(&mut self, name: &str, kind: NameKind) {
        let Some(n) = ident::usable_name(name) else {
            return;
        };
        if !self.names.iter().any(|(m, k)| *m == n && *k == kind) {
            self.names.push((n, kind));
        }
        if kind == NameKind::Primary {
            let words = ident::tokens(name);
            if !words.is_empty() && !self.primary_tokens.contains(&words) {
                self.primary_tokens.push(words);
            }
        }
    }

    /// Adds a vendor word (`google`, `kingsoft`).
    pub(super) fn add_vendor_word(&mut self, word: &str) {
        let n = normalize(word);
        if n.chars().count() >= 3
            && !VENDOR_STOPWORDS.contains(&n.as_str())
            && !self.vendor_words.contains(&n)
        {
            self.vendor_words.push(n);
        }
    }

    /// Product names: each primary name without its vendor words (`Google Chrome` →
    /// `chrome`), normalised.
    fn products(&self) -> Vec<String> {
        let mut out = Vec::new();
        for words in &self.primary_tokens {
            let rest: String = words
                .iter()
                .filter(|w| !self.vendor_words.contains(w))
                .map(String::as_str)
                .collect();
            if rest.chars().count() >= 3
                && !ident::is_generic(&rest)
                && rest.len() < words.iter().map(String::len).sum()
                && !out.contains(&rest)
            {
                out.push(rest);
            }
        }
        out
    }

    /// Whether `word` (normalised) is one of this app's vendor words.
    pub(super) fn has_vendor_word(&self, word: &str) -> bool {
        self.vendor_words.iter().any(|w| w == word)
    }
}

/// The context of a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Place<'a> {
    /// A child of a Library folder; `user_data`: Electron and Chromium apps keep their
    /// profile here under their product name (`Application Support`, `Logs`, `Caches`).
    Library {
        /// Folder of per-app user data.
        user_data: bool,
    },
    /// A child of a per-user folder under `/var/folders`: `<id>.<anything>` is the app's
    /// own temp or cache entry.
    Temp,
    /// A child of the vendor folder named by this normalised vendor word
    /// (`Application Support/Google/Chrome`).
    Vendor(&'a str),
    /// A dot entry of the home folder (dot removed) or a child of `~/.config`.
    Home,
}

/// One piece of evidence that a name belongs to an app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum Evidence {
    /// The app's bundle id or an embedded bundle's (`TEAM.id`, `group.id` included).
    Id,
    /// One component below one of the app's ids (`id.helper`).
    SubId,
    /// `<id>.<anything>` in a temp or cache folder under `/var/folders`.
    TempId,
    /// An app-group or keychain-group id, exactly.
    GroupId,
    /// Two or more components below one of the app's ids (`id.installer.sdk`).
    DeepSubId,
    /// `TEAMID.anything` with the app's team id.
    Team,
    /// The Electron product name in a user-data folder.
    UserData,
    /// The product name inside the app's vendor folder (`Google/Chrome`).
    VendorProduct,
    /// A display, bundle or product name.
    Name,
    /// A name plus a qualifier word (`Telegram Desktop`).
    QualifiedName,
    /// The executable name.
    Executable,
    /// The last bundle-id component.
    IdTail,
    /// A whole vendor folder when no other installed app has that vendor.
    VendorFolder,
    /// The bundle-id vendor prefix (`com.google.other`).
    Vendor,
    /// Bonus for [`Evidence::Vendor`]: no other installed app has that vendor.
    SoleVendor,
    /// Bonus for a name match: the folder holds files of the app (its ids, or a Chromium
    /// profile for Chromium-based apps).
    Content,
    /// The running app has files open below it.
    OpenFile,
    /// A launchd job referencing the app.
    Launchd,
}

/// How evidence combines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Name-based: the strongest one counts.
    Identity,
    /// Added to the strongest identity evidence it corroborates.
    Bonus,
    /// Independent of names: always added.
    Direct,
}

impl Evidence {
    /// Points of this evidence.
    pub(super) const fn weight(self) -> u32 {
        match self {
            Self::Id
            | Self::SubId
            | Self::TempId
            | Self::GroupId
            | Self::UserData
            | Self::VendorProduct
            | Self::OpenFile
            | Self::Launchd => 100,
            Self::Name => 60,
            Self::DeepSubId | Self::Team | Self::QualifiedName | Self::VendorFolder => 50,
            Self::Executable | Self::IdTail | Self::Content => 40,
            Self::SoleVendor => 30,
            Self::Vendor => 20,
        }
    }

    const fn class(self) -> Class {
        match self {
            Self::SoleVendor | Self::Content => Class::Bonus,
            Self::OpenFile | Self::Launchd => Class::Direct,
            _ => Class::Identity,
        }
    }

    /// Whether this bonus corroborates `best` identity evidence.
    const fn corroborates(self, best: Self) -> bool {
        match self {
            Self::SoleVendor => matches!(best, Self::Vendor),
            Self::Content => matches!(
                best,
                Self::Name | Self::QualifiedName | Self::Executable | Self::IdTail
            ),
            _ => false,
        }
    }

    /// Name-based evidence that only says "a word of the app's name" (not an id).
    pub(super) const fn is_name(self) -> bool {
        matches!(
            self,
            Self::Name | Self::QualifiedName | Self::Executable | Self::IdTail
        )
    }
}

/// The evidence one name gives for one app.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Verdict {
    /// Distinct evidence found.
    pub(super) evidence: Vec<Evidence>,
    /// Points lost to a weaker rival claim.
    pub(super) penalty: u32,
}

impl Verdict {
    /// Adds evidence (once).
    pub(super) fn add(&mut self, e: Evidence) {
        if !self.evidence.contains(&e) {
            self.evidence.push(e);
        }
    }

    /// Whether `e` was found.
    pub(super) fn has(&self, e: Evidence) -> bool {
        self.evidence.contains(&e)
    }

    /// The strongest name-based evidence.
    pub(super) fn identity(&self) -> Option<Evidence> {
        self.evidence
            .iter()
            .copied()
            .filter(|e| e.class() == Class::Identity)
            .max_by_key(|e| e.weight())
    }

    /// Strongest identity evidence + corroborating bonuses + direct evidence − penalty.
    pub(super) fn points(&self) -> u32 {
        let best = self.identity();
        let mut points = best.map_or(0, Evidence::weight);
        for e in &self.evidence {
            let counts = match e.class() {
                Class::Identity => false,
                Class::Bonus => best.is_some_and(|b| e.corroborates(b)),
                Class::Direct => true,
            };
            if counts {
                points = points.saturating_add(e.weight());
            }
        }
        points.saturating_sub(self.penalty).min(MAX_POINTS)
    }

    /// The confidence class of the points, `None` below [`LOW`].
    pub(super) fn confidence(&self) -> Option<Confidence> {
        confidence(self.points())
    }
}

/// The confidence class of `points`.
pub(super) const fn confidence(points: u32) -> Option<Confidence> {
    if points >= HIGH {
        Some(Confidence::High)
    } else if points >= MEDIUM {
        Some(Confidence::Medium)
    } else if points >= LOW {
        Some(Confidence::Low)
    } else {
        None
    }
}

/// How a lowercase name relates to one of the app's ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Relation {
    /// The id itself, or ending in it (`TEAM.id`, `group.id`).
    Same,
    /// Below it; `deep` when more than one component below.
    Child { deep: bool },
    /// Below it, but naming a sibling product (`id.canary`, `id.for.testing`).
    Sibling,
}

fn relation(lower: &str, id: &str) -> Option<Relation> {
    if lower == id || lower.strip_suffix(id).is_some_and(|r| r.ends_with('.')) {
        return Some(Relation::Same);
    }
    let rest = lower.strip_prefix(id)?.strip_prefix('.')?;
    let first = rest.split('.').next().unwrap_or_default();
    if SIBLINGS.contains(&first) {
        return Some(Relation::Sibling);
    }
    Some(Relation::Child {
        deep: rest.contains('.'),
    })
}

/// The evidence `entry` (a file or folder name) at `place` gives for `p`.
pub(super) fn score(p: &Profile, entry: &str, place: Place<'_>) -> Verdict {
    let mut v = Verdict::default();
    let stem = strip_suffixes(entry);
    let stem = match place {
        Place::Home | Place::Temp => stem.trim_start_matches('.'),
        Place::Library { .. } | Place::Vendor(_) => stem,
    };
    let lower = stem.to_lowercase();
    if (!p.apple && is_apple(&lower)) || is_shared(&lower) {
        return v;
    }
    let mut sibling = false;
    for id in &p.ids {
        match relation(&lower, id) {
            Some(Relation::Same) => v.add(Evidence::Id),
            Some(Relation::Child { deep }) => v.add(if place == Place::Temp {
                Evidence::TempId
            } else if deep {
                Evidence::DeepSubId
            } else {
                Evidence::SubId
            }),
            Some(Relation::Sibling) => sibling = true,
            None => {}
        }
    }
    if sibling && v.identity().is_none() {
        // Another product of the same family: not this app's.
        return v;
    }
    // `TEAMID.rest` / `group.rest`: judge the rest like a plain name.
    let unprefixed = match strip_team(stem) {
        Some((team, rest)) => {
            if p.team.as_deref() == Some(team) {
                v.add(Evidence::Team);
            }
            rest
        }
        None => stem,
    };
    let unprefixed = unprefixed.strip_prefix("group.").unwrap_or(unprefixed);
    let unprefixed_lower = unprefixed.to_lowercase();
    if p.exact
        .iter()
        .any(|g| *g == lower || *g == unprefixed_lower)
    {
        v.add(Evidence::GroupId);
    }
    let normalized = normalize(unprefixed);
    // `AndroidStudio2026.1`, `Bartender 6`: a versioned folder of the app.
    let unversioned = normalized.trim_end_matches(|c: char| c.is_ascii_digit());
    let unversioned = (unversioned.len() != normalized.len() && unversioned.chars().count() >= 4)
        .then_some(unversioned);
    if let Place::Vendor(word) = place
        && !p.has_vendor_word(word)
    {
        // Another vendor's product folder: only ids can make it this app's.
        return v;
    }
    name_evidence(p, &normalized, place, &mut v);
    if let Some(bare) = unversioned
        && p.names
            .iter()
            .any(|(n, k)| n == bare && matches!(k, NameKind::Primary | NameKind::Product))
    {
        v.add(Evidence::QualifiedName);
    }
    if let Place::Vendor(_) = place
        && is_product(p, &normalized, unversioned)
    {
        v.add(Evidence::VendorProduct);
    }
    if matches!(place, Place::Library { .. } | Place::Home)
        && p.sole_vendor
        && p.has_vendor_word(&normalized)
    {
        v.add(Evidence::VendorFolder);
    }
    if let Some(vendor) = &p.vendor
        && unprefixed_lower.len() > vendor.len()
        && unprefixed_lower.starts_with(vendor.as_str())
        && matches!(
            unprefixed_lower.as_bytes().get(vendor.len()),
            Some(b'.' | b'-')
        )
    {
        v.add(Evidence::Vendor);
        if p.sole_vendor {
            v.add(Evidence::SoleVendor);
        }
    }
    v
}

/// A folder in the app's vendor folder is named after the app or its product
/// (`Google/Chrome`, `Google/AndroidStudio2026.1`).
fn is_product(p: &Profile, normalized: &str, unversioned: Option<&str>) -> bool {
    let named = |x: &str| {
        p.names
            .iter()
            .any(|(n, k)| n == x && matches!(k, NameKind::Primary | NameKind::Product))
    };
    let products = p.products();
    named(normalized)
        || unversioned.is_some_and(named)
        || products.iter().any(|x| x == normalized)
        || unversioned.is_some_and(|bare| products.iter().any(|x| x == bare))
}

/// Evidence of the app's names on a normalised name.
fn name_evidence(p: &Profile, normalized: &str, place: Place<'_>, v: &mut Verdict) {
    let user_data = matches!(place, Place::Library { user_data: true });
    for (name, kind) in &p.names {
        if normalized == name {
            v.add(match (kind, user_data) {
                (NameKind::Product | NameKind::Updater, true) => Evidence::UserData,
                (NameKind::Primary | NameKind::Product, _) => Evidence::Name,
                (NameKind::Updater, false) => Evidence::QualifiedName,
                (NameKind::Executable, _) => Evidence::Executable,
                (NameKind::IdTail, _) => Evidence::IdTail,
            });
        } else if matches!(kind, NameKind::Primary | NameKind::Product)
            && QUALIFIERS.iter().any(|q| {
                normalized
                    .strip_prefix(name.as_str())
                    .is_some_and(|rest| rest == *q)
                    || normalized
                        .strip_suffix(name.as_str())
                        .is_some_and(|rest| rest == *q)
            })
        {
            v.add(Evidence::QualifiedName);
        }
    }
}

/// `entry` is another app's id or lies below it (`com.google.chrome.for.testing` below
/// Chrome's id), and none of `me`'s ids is a more specific prefix: the name is that app's
/// family, not `me`'s.
fn in_foreign_namespace(me: &Profile, others: &[Profile], entry: &str) -> bool {
    let stem = strip_suffixes(entry).trim_start_matches('.');
    let stem = strip_team(stem).map_or(stem, |(_, rest)| rest);
    let lower = stem.to_lowercase();
    let depth = |p: &Profile| {
        p.ids
            .iter()
            .filter(|id| {
                lower == **id
                    || lower
                        .strip_prefix(id.as_str())
                        .is_some_and(|r| r.starts_with('.'))
            })
            .map(String::len)
            .max()
    };
    let theirs = others.iter().filter_map(depth).max();
    theirs.is_some_and(|t| depth(me).is_none_or(|m| m < t))
}

/// The strongest claim of any app in `others` on `entry`.
fn rival_points(others: &[Profile], entry: &str, place: Place<'_>) -> u32 {
    others
        .iter()
        .map(|o| score(o, entry, place).points())
        .max()
        .unwrap_or(0)
}

/// The verdict for `me` when no other installed app matches `entry` at least as strongly;
/// a weaker rival claim of at least [`MEDIUM`] costs half its points.
pub(super) fn attribute(
    me: &Profile,
    others: &[Profile],
    entry: &str,
    place: Place<'_>,
) -> Option<Verdict> {
    let mut v = score(me, entry, place);
    let mine = v.points();
    if mine < LOW || in_foreign_namespace(me, others, entry) {
        return None;
    }
    let theirs = rival_points(others, entry, place);
    if theirs >= mine {
        return None;
    }
    if theirs >= MEDIUM {
        v.penalty = theirs / 2;
    }
    (v.points() >= LOW).then_some(v)
}

/// The verdict for `me` with `direct` evidence (open files, installer payload…) added,
/// unless another installed app clearly owns the name. With `need_name`, the name itself
/// must also point at the app (for places full of unrelated tool folders, like `~/.x`).
pub(super) fn attribute_direct(
    me: &Profile,
    others: &[Profile],
    entry: &str,
    place: Place<'_>,
    direct: Evidence,
    need_name: bool,
) -> Option<Verdict> {
    let stem = strip_suffixes(entry).trim_start_matches('.');
    let lower = stem.to_lowercase();
    if (!me.apple && is_apple(&lower)) || is_shared(&lower) || ident::is_generic(&normalize(stem)) {
        return None;
    }
    let mut v = score(me, entry, place);
    let mine = v.points();
    if need_name && mine < LOW {
        return None;
    }
    let theirs = rival_points(others, entry, place);
    if theirs >= HIGH && theirs >= mine {
        return None;
    }
    v.add(direct);
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIB: Place<'static> = Place::Library { user_data: false };
    const DATA: Place<'static> = Place::Library { user_data: true };

    fn chrome() -> Profile {
        let mut p = Profile::basic(
            Some("com.google.Chrome"),
            "Google Chrome",
            Some("Google Chrome"),
        );
        p.team = Some("EQHXZ8M8AV".to_owned());
        p.add_id("com.google.Chrome.helper");
        p
    }

    fn android_studio() -> Profile {
        Profile::basic(
            Some("com.google.android.studio"),
            "Android Studio",
            Some("studio"),
        )
    }

    fn conf(p: &Profile, entry: &str, place: Place<'_>) -> Option<Confidence> {
        score(p, entry, place).confidence()
    }

    #[test]
    fn ids_rank_above_names_above_vendor() {
        let app = chrome();
        assert_eq!(
            conf(&app, "com.google.Chrome.plist", LIB),
            Some(Confidence::High),
            "bundle id"
        );
        assert_eq!(
            conf(&app, "EQHXZ8M8AV.com.google.Chrome", LIB),
            Some(Confidence::High),
            "team + id"
        );
        assert_eq!(
            conf(&app, "EQHXZ8M8AV.shared", LIB),
            Some(Confidence::Medium),
            "team only"
        );
        assert_eq!(
            conf(&app, "Google Chrome", LIB),
            Some(Confidence::Medium),
            "app name"
        );
        assert_eq!(
            conf(&app, "com.google.Keystone.Agent.plist", LIB),
            Some(Confidence::Low),
            "vendor prefix"
        );
        assert_eq!(
            conf(&app, "com.google.Chrome.helper.Renderer", LIB),
            Some(Confidence::High),
            "below an embedded id"
        );
        assert_eq!(
            conf(&app, "com.google.Chrome.something.deeper", LIB),
            Some(Confidence::Medium),
            "deep below the main id"
        );
        assert_eq!(
            conf(&app, "com.googlex.foo", LIB),
            None,
            "prefix ends at a dot"
        );
        assert_eq!(conf(&app, "com.apple.Safari", LIB), None, "apple");
        assert_eq!(conf(&app, "Chromeless", LIB), None, "unrelated");
        assert_eq!(
            conf(&app, "org.sparkle-project.Sparkle", LIB),
            None,
            "shared framework"
        );
    }

    #[test]
    fn sibling_products_are_not_the_app() {
        let app = chrome();
        assert_eq!(
            conf(&app, "com.google.chrome.for.testing.plist", LIB),
            None,
            "Chrome for Testing"
        );
        assert_eq!(
            conf(&app, "com.google.Chrome.canary", LIB),
            None,
            "Canary channel"
        );
        assert_eq!(
            conf(&app, "com.google.chrome.for.testing.4MiDrg", Place::Temp),
            None,
            "sibling temp folders too"
        );
    }

    #[test]
    fn temp_entries_below_an_id_are_the_apps() {
        let app = chrome();
        assert_eq!(
            conf(
                &app,
                "com.google.Chrome.chrome_chrome_url_fetcher_.4a5xv0",
                Place::Temp
            ),
            Some(Confidence::High),
            "mkdtemp folder"
        );
        assert_eq!(
            conf(&app, ".com.google.Chrome.Tml0QA", Place::Temp),
            Some(Confidence::High),
            "hidden temp"
        );
    }

    #[test]
    fn vendor_product_folders_are_high() {
        let app = chrome();
        assert_eq!(
            conf(&app, "Chrome", Place::Vendor("google")),
            Some(Confidence::High),
            "Application Support/Google/Chrome"
        );
        assert_eq!(
            conf(&app, "Chrome for Testing", Place::Vendor("google")),
            None,
            "another product in the vendor folder"
        );
        assert_eq!(
            conf(&app, "Chrome", Place::Vendor("microsoft")),
            None,
            "someone else's vendor folder"
        );
        assert_eq!(
            conf(&app, "GoogleUpdater", Place::Vendor("google")),
            None,
            "the vendor's updater"
        );
        assert_eq!(
            conf(
                &android_studio(),
                "AndroidStudio2026.1",
                Place::Vendor("google")
            ),
            Some(Confidence::High),
            "versioned product folder"
        );
        assert_eq!(
            conf(&android_studio(), "Android Studio 2", LIB),
            Some(Confidence::Medium),
            "versioned name"
        );
    }

    #[test]
    fn electron_product_names_own_user_data() {
        let mut code = Profile::basic(
            Some("com.microsoft.VSCode"),
            "Visual Studio Code",
            Some("Electron"),
        );
        code.add_name("Code", NameKind::Product);
        code.add_name("code-updater", NameKind::Updater);
        assert_eq!(
            conf(&code, "Code", DATA),
            Some(Confidence::High),
            "Application Support/Code"
        );
        assert_eq!(
            conf(&code, "Code", LIB),
            Some(Confidence::Medium),
            "elsewhere the product name is only a name"
        );
        assert_eq!(
            conf(&code, "code-updater", DATA),
            Some(Confidence::High),
            "electron-updater cache"
        );
        assert_eq!(
            conf(&code, "vscode", Place::Home),
            Some(Confidence::Low),
            "~/.vscode by id tail"
        );
    }

    #[test]
    fn qualified_names_are_medium() {
        let tg = Profile::basic(Some("com.tdesktop.Telegram"), "Telegram", Some("Telegram"));
        assert_eq!(
            conf(&tg, "Telegram Desktop", DATA),
            Some(Confidence::Medium),
            "name + qualifier"
        );
        assert_eq!(conf(&tg, "Telegram Beta X", DATA), None, "other words");
    }

    #[test]
    fn bonuses_corroborate_only_their_evidence() {
        let mut v = Verdict::default();
        v.add(Evidence::Name);
        v.add(Evidence::Content);
        assert_eq!(v.confidence(), Some(Confidence::High), "name + content");
        let mut v = Verdict::default();
        v.add(Evidence::Vendor);
        v.add(Evidence::Content);
        assert_eq!(
            v.points(),
            20,
            "content does not corroborate a vendor match"
        );
        v.add(Evidence::SoleVendor);
        assert_eq!(v.confidence(), Some(Confidence::Medium), "sole vendor");
        let mut v = Verdict::default();
        v.add(Evidence::OpenFile);
        assert_eq!(v.confidence(), Some(Confidence::High), "direct alone");
    }

    #[test]
    fn sole_vendor_raises_vendor_matches() {
        let mut wps = Profile::basic(
            Some("com.kingsoft.wpsoffice.mac"),
            "wpsoffice",
            Some("wpsoffice"),
        );
        assert_eq!(
            conf(&wps, "com.kingsoft.Office.plist", LIB),
            Some(Confidence::Low),
            "shared vendor"
        );
        assert_eq!(
            conf(&wps, "Kingsoft", DATA),
            None,
            "vendor folder needs sole vendor"
        );
        wps.sole_vendor = true;
        assert_eq!(
            conf(&wps, "com.kingsoft-office.plist", LIB),
            Some(Confidence::Medium),
            "dash form, sole vendor"
        );
        assert_eq!(
            conf(&wps, "Kingsoft", DATA),
            Some(Confidence::Medium),
            "whole vendor folder"
        );
        assert_eq!(
            conf(&wps, "kingsoft", Place::Home),
            Some(Confidence::Medium),
            "~/.kingsoft"
        );
    }

    #[test]
    fn app_groups_match_exactly_only() {
        let mut qq = Profile::basic(Some("com.tencent.qq"), "QQ", Some("QQ"));
        qq.add_exact("FN2V63AD2J.com.tencent");
        assert_eq!(
            conf(&qq, "FN2V63AD2J.com.tencent", LIB),
            Some(Confidence::High),
            "group container"
        );
        assert_eq!(
            conf(&qq, "com.tencent.meeting", LIB),
            Some(Confidence::Low),
            "a two-component group is not a prefix"
        );
    }

    #[test]
    fn rivals_take_contested_names() {
        let base = Profile::basic(Some("com.foo.bar"), "Bar", None);
        let pro = Profile::basic(Some("com.foo.bar.pro"), "Bar Pro", None);
        assert!(
            attribute(
                &base,
                std::slice::from_ref(&pro),
                "com.foo.bar.pro.plist",
                LIB
            )
            .is_none(),
            "the sibling owns its files"
        );
        assert!(
            attribute(
                &pro,
                std::slice::from_ref(&base),
                "com.foo.bar.pro.plist",
                LIB
            )
            .is_some(),
            "and keeps them"
        );
        assert!(
            attribute(&base, std::slice::from_ref(&pro), "com.foo.shared", LIB).is_none(),
            "vendor-only ties go to nobody"
        );
        assert!(
            attribute(&base, std::slice::from_ref(&pro), "com.foo.bar.helper", LIB).is_some(),
            "own sub-ids stay"
        );
        let other = Profile::basic(Some("com.foo.other"), "Other", None);
        assert!(
            attribute(
                &other,
                std::slice::from_ref(&base),
                "com.foo.bar.beta.plist",
                LIB
            )
            .is_none(),
            "a sibling of another app's id is not a vendor match"
        );
    }

    #[test]
    fn weak_rivals_cost_points_strong_ones_win() {
        let mut claude = Profile::basic(
            Some("com.anthropic.claudefordesktop"),
            "Claude",
            Some("Claude"),
        );
        claude.add_name("Claude", NameKind::Product);
        let handler = Profile::basic(
            Some("com.anthropic.claude-code-url-handler"),
            "Claude Code URL Handler",
            Some("claude"),
        );
        let v = attribute(&claude, std::slice::from_ref(&handler), "Claude", DATA);
        assert_eq!(
            v.and_then(|v| v.confidence()),
            Some(Confidence::High),
            "product name beats another app's executable name"
        );
        let chrome = chrome();
        let studio = android_studio();
        assert_eq!(
            attribute(
                &chrome,
                std::slice::from_ref(&studio),
                "Chrome",
                Place::Vendor("google")
            )
            .and_then(|v| v.confidence()),
            Some(Confidence::High),
            "vendor product survives a same-vendor app"
        );
        assert!(
            attribute(
                &chrome,
                std::slice::from_ref(&studio),
                "com.google.Keystone.Agent.plist",
                LIB
            )
            .is_none(),
            "vendor-wide files go to nobody"
        );
        let mut rival = Profile::basic(Some("com.x.y"), "Chrome Tools", None);
        rival.add_name("Google Chrome", NameKind::Primary);
        let v = attribute(&chrome, std::slice::from_ref(&rival), "Google Chrome", LIB);
        assert!(v.is_none(), "an equal rival name claim makes it nobody's");
    }

    #[test]
    fn direct_evidence_needs_an_uncontested_name() {
        let chrome = chrome();
        let studio = android_studio();
        let v = attribute_direct(
            &chrome,
            std::slice::from_ref(&studio),
            "com.google.Chrome",
            LIB,
            Evidence::OpenFile,
            false,
        );
        assert_eq!(v.map(|v| v.points()), Some(200), "id + open file");
        assert!(
            attribute_direct(
                &chrome,
                std::slice::from_ref(&studio),
                "com.google.android.studio",
                LIB,
                Evidence::OpenFile,
                false
            )
            .is_none(),
            "another app's own folder"
        );
        assert!(
            attribute_direct(&chrome, &[], "cargo", Place::Home, Evidence::OpenFile, true)
                .is_none(),
            "home tool folders need a name match"
        );
    }

    #[test]
    fn products_strip_vendor_words() {
        let chrome = chrome();
        assert_eq!(
            chrome.products(),
            vec!["chrome".to_owned()],
            "Google Chrome → chrome"
        );
        let mut teams = Profile::basic(Some("com.microsoft.teams2"), "Microsoft Teams", None);
        teams.add_vendor_word("Microsoft");
        assert_eq!(
            teams.products(),
            vec!["teams".to_owned()],
            "Microsoft Teams → teams"
        );
        let rect = Profile::basic(Some("com.knollsoft.Rectangle"), "Rectangle", None);
        assert!(rect.products().is_empty(), "no vendor word in the name");
    }
}
