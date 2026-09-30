//! Which items of a scan a rule acts on. Pure: a report in, the kept items out.
//!
//! Unattended runs are stricter than a manual clean: items that need administrator rights
//! (the elevation prompt must not appear out of nowhere) and items of a running app are
//! never taken, whatever the rule says.

use omc_proto::jobs::ItemId;
use omc_proto::junk::{JunkKind, JunkReport, Safety};
use omc_proto::rules::{RuleFilter, RunItem, Timestamp};

const DAY_SECS: i64 = 86_400;

/// One item that passed the filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Kept {
    /// Id inside the scan job (what the clean job takes).
    pub(crate) id: ItemId,
    /// The group kind of the item.
    pub(crate) kind: JunkKind,
    /// Display form for the run.
    pub(crate) item: RunItem,
    /// The path, for `Location::Path` items.
    pub(crate) path: Option<String>,
}

/// The items of `report` in `kinds` (empty = all) that pass `filter` at time `now`, largest
/// first. An item of unknown age never passes an `idle_days` bound.
pub(crate) fn apply(
    report: &JunkReport,
    kinds: &[JunkKind],
    filter: &RuleFilter,
    now: Timestamp,
) -> Vec<Kept> {
    let newest_allowed = (filter.idle_days > 0)
        .then(|| now.saturating_sub(i64::from(filter.idle_days).saturating_mul(DAY_SECS)));
    let mut kept: Vec<Kept> = report
        .groups
        .iter()
        .filter(|group| kinds.is_empty() || kinds.contains(&group.kind))
        .flat_map(|group| group.items.iter().map(move |item| (group.kind, item)))
        .filter(|(_, item)| {
            (item.safety == Safety::Safe || filter.include_review)
                && !item.needs_admin
                && !item.app_running
                && item.bytes >= filter.min_bytes
                && newest_allowed.is_none_or(|limit| item.modified.is_some_and(|m| m <= limit))
        })
        .map(|(kind, item)| Kept {
            id: item.id,
            kind,
            path: item.location.as_path().map(str::to_owned),
            item: RunItem {
                name: item.name.clone(),
                location: item.location.display(),
                bytes: item.bytes,
                modified: item.modified,
            },
        })
        .collect();
    kept.sort_by(|a, b| b.item.bytes.cmp(&a.item.bytes).then(a.id.cmp(&b.id)));
    kept
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "fixture timestamps are small constants"
)]
mod tests {
    use omc_proto::jobs::Location;
    use omc_proto::junk::{JunkGroup, JunkItem};

    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn item(id: ItemId, bytes: u64, age_days: Option<i64>) -> JunkItem {
        JunkItem {
            id,
            name: format!("item {id}"),
            location: Location::Path {
                path: format!("/work/p{id}/target"),
            },
            tag: None,
            bytes,
            files: 1,
            modified: age_days.map(|days| NOW - days * DAY_SECS),
            safety: Safety::Safe,
            needs_admin: false,
            app_running: false,
            ident: None,
            icon: None,
        }
    }

    fn report(items: Vec<JunkItem>) -> JunkReport {
        JunkReport {
            groups: vec![
                JunkGroup {
                    kind: JunkKind::ProjectArtifacts,
                    items,
                },
                JunkGroup {
                    kind: JunkKind::IdeCache,
                    items: vec![item(99, 5_000, Some(100))],
                },
            ],
            denied: Vec::new(),
        }
    }

    fn ids(kept: &[Kept]) -> Vec<ItemId> {
        kept.iter().map(|k| k.id).collect()
    }

    #[test]
    fn kinds_narrow_the_scan_and_empty_means_all() {
        let report = report(vec![item(1, 10, Some(30))]);
        let only = apply(
            &report,
            &[JunkKind::ProjectArtifacts],
            &RuleFilter::default(),
            NOW,
        );
        assert_eq!(ids(&only), vec![1], "only the project artifacts");
        let all = apply(&report, &[], &RuleFilter::default(), NOW);
        assert_eq!(ids(&all), vec![99, 1], "every kind, largest first");
    }

    #[test]
    fn idle_days_bound_is_inclusive_and_unknown_age_never_passes() {
        let report = report(vec![
            item(1, 10, Some(14)),
            item(2, 20, Some(13)),
            item(3, 30, None),
            item(4, 40, Some(400)),
        ]);
        let filter = RuleFilter {
            idle_days: 14,
            ..RuleFilter::default()
        };
        let kept = apply(&report, &[JunkKind::ProjectArtifacts], &filter, NOW);
        assert_eq!(
            ids(&kept),
            vec![4, 1],
            "exactly 14 days old passes, 13 and unknown do not"
        );
        let any_age = apply(
            &report,
            &[JunkKind::ProjectArtifacts],
            &RuleFilter::default(),
            NOW,
        );
        assert_eq!(any_age.len(), 4, "idle_days 0 keeps every age");
    }

    #[test]
    fn size_safety_admin_and_running_apps_are_enforced() {
        let mut review = item(1, 100, Some(30));
        review.safety = Safety::Review;
        let mut admin = item(2, 100, Some(30));
        admin.needs_admin = true;
        let mut running = item(3, 100, Some(30));
        running.app_running = true;
        let small = item(4, 5, Some(30));
        let fine = item(5, 100, Some(30));
        let report = report(vec![review, admin, running, small, fine]);
        let strict = RuleFilter {
            min_bytes: 50,
            ..RuleFilter::default()
        };
        let kept = apply(&report, &[JunkKind::ProjectArtifacts], &strict, NOW);
        assert_eq!(
            ids(&kept),
            vec![5],
            "review, admin, running and small dropped"
        );
        let with_review = RuleFilter {
            include_review: true,
            ..strict
        };
        let kept = apply(&report, &[JunkKind::ProjectArtifacts], &with_review, NOW);
        assert_eq!(
            ids(&kept),
            vec![1, 5],
            "include_review adds review items but never admin or running ones"
        );
    }
}
