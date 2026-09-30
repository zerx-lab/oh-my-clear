//! The scheduling math, pure so it can be tested with fixed offsets and fake clocks.
//!
//! A rule is due when the wall clock reached its *next run*:
//!
//! - it ran before (`last_run >= anchor`): `days` calendar days after the **local date** of
//!   the last run, at local `hour`. Dates, not instants: a run that fired at 10:00:30 for an
//!   `hour = 10` rule is due again exactly `days` days later at 10:00, not a day later.
//! - it never ran, or was created/resumed after its last run (`anchor > last_run`): the first
//!   local `hour` o'clock at or after `anchor`.
//!
//! Only wall-clock time enters, so a machine that slept through the due time fires once at
//! wake (the last run moves to "now", the next run is computed from that). Local time
//! follows DST: an hour that does not exist that day (spring forward) is taken one hour
//! later, an hour that exists twice (fall back) is taken at its first occurrence.

use chrono::{DateTime, Days, Local, NaiveDate, NaiveDateTime, TimeDelta, TimeZone};
use omc_proto::rules::{Timestamp, Trigger};

/// How far a nonexistent local time (a DST gap) may be moved forward to find one that exists.
const MAX_GAP_HOURS: i64 = 3;

/// The zone rules are scheduled in: the machine's local time.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Zone {
    /// The OS time zone.
    Local,
    /// A fixed UTC offset in seconds (tests).
    #[cfg(test)]
    Fixed(i32),
}

impl Zone {
    /// When `trigger` fires next, given the start of the last run and the schedule anchor
    /// (creation or resume time). `None` when the calendar overflows.
    pub(crate) fn next_run(
        self,
        trigger: &Trigger,
        last_run: Option<Timestamp>,
        anchor: Timestamp,
    ) -> Option<Timestamp> {
        match self {
            Self::Local => next_run(&Local, trigger, last_run, anchor),
            #[cfg(test)]
            Self::Fixed(secs) => {
                let offset = chrono::FixedOffset::east_opt(secs)?;
                next_run(&offset, trigger, last_run, anchor)
            }
        }
    }
}

/// See the module docs.
pub(crate) fn next_run<Tz: TimeZone>(
    tz: &Tz,
    trigger: &Trigger,
    last_run: Option<Timestamp>,
    anchor: Timestamp,
) -> Option<Timestamp> {
    let Trigger::Every { days, hour } = trigger;
    if let Some(last) = last_run.filter(|last| *last >= anchor) {
        let date = local_date(tz, last)?.checked_add_days(Days::new(u64::from(*days)))?;
        at_hour(tz, date, *hour)
    } else {
        let date = local_date(tz, anchor)?;
        let today = at_hour(tz, date, *hour)?;
        if today >= anchor {
            Some(today)
        } else {
            at_hour(tz, date.checked_add_days(Days::new(1))?, *hour)
        }
    }
}

fn local_date<Tz: TimeZone>(tz: &Tz, ts: Timestamp) -> Option<NaiveDate> {
    Some(
        DateTime::from_timestamp(ts, 0)?
            .with_timezone(tz)
            .date_naive(),
    )
}

/// The instant of `hour`:00 local time on `date`.
fn at_hour<Tz: TimeZone>(tz: &Tz, date: NaiveDate, hour: u8) -> Option<Timestamp> {
    let naive = date.and_hms_opt(u32::from(hour), 0, 0)?;
    (0..=MAX_GAP_HOURS).find_map(|gap| resolve(tz, naive, gap))
}

/// `naive` moved `gap` hours forward, as an instant; `None` while it falls in a DST gap.
fn resolve<Tz: TimeZone>(tz: &Tz, naive: NaiveDateTime, gap: i64) -> Option<Timestamp> {
    let shifted = naive.checked_add_signed(TimeDelta::try_hours(gap)?)?;
    tz.from_local_datetime(&shifted)
        .earliest()
        .map(|time| time.timestamp())
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "fixture timestamps are small constants"
)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;
    /// UTC+2 (no DST): local 10:00 is 08:00 UTC.
    const PLUS2: Zone = Zone::Fixed(2 * 3600);
    /// 2026-01-01 00:00 UTC.
    const JAN1: i64 = 1_767_225_600;

    fn every(days: u32, hour: u8) -> Trigger {
        Trigger::Every { days, hour }
    }

    fn next(zone: Zone, trigger: &Trigger, last: Option<i64>, anchor: i64) -> Option<i64> {
        zone.next_run(trigger, last, anchor)
    }

    #[test]
    fn a_new_rule_fires_at_the_next_occurrence_of_its_hour() {
        // Local 09:00 on Jan 1 (= 07:00 UTC): 10:00 is an hour away.
        let anchor = JAN1 + 7 * 3600;
        assert_eq!(
            next(PLUS2, &every(14, 10), None, anchor),
            Some(JAN1 + 8 * 3600),
            "later today"
        );
        // Exactly at 10:00 counts as "at or after".
        assert_eq!(
            next(PLUS2, &every(14, 10), None, JAN1 + 8 * 3600),
            Some(JAN1 + 8 * 3600),
            "an anchor exactly on the hour fires at once"
        );
        // One second past 10:00: tomorrow.
        assert_eq!(
            next(PLUS2, &every(14, 10), None, JAN1 + 8 * 3600 + 1),
            Some(JAN1 + DAY + 8 * 3600),
            "past the hour waits for tomorrow"
        );
    }

    #[test]
    fn the_local_date_not_the_utc_date_decides_the_day() {
        // 23:30 UTC on Jan 1 is 01:30 local on Jan 2; hour 1 (local 01:00 Jan 2) already
        // passed, so the next one is Jan 3 01:00 local = Jan 2 23:00 UTC.
        let anchor = JAN1 + 23 * 3600 + 1800;
        assert_eq!(
            next(PLUS2, &every(3, 1), None, anchor),
            Some(JAN1 + DAY + 23 * 3600),
            "the day boundary follows the local clock"
        );
    }

    #[test]
    fn an_interval_counts_calendar_days_from_the_last_run() {
        // Fired at 10:00:30 local: due exactly 14 days later at 10:00, not a day after.
        let fired = JAN1 + 8 * 3600 + 30;
        assert_eq!(
            next(PLUS2, &every(14, 10), Some(fired), JAN1),
            Some(JAN1 + 14 * DAY + 8 * 3600),
            "the next run lands on the hour, `days` days later"
        );
        // A run at 15:00 (manual, or a late wake) keeps the hour of day too.
        let late = JAN1 + 13 * 3600;
        assert_eq!(
            next(PLUS2, &every(2, 10), Some(late), JAN1),
            Some(JAN1 + 2 * DAY + 8 * 3600),
            "a late run still schedules by date"
        );
    }

    #[test]
    fn several_missed_intervals_fire_once() {
        // Due long ago: any `now` past it is due; after the (single) run at `now`, the
        // next run is a whole interval away from that run, never in the past.
        let trigger = every(1, 10);
        let last = JAN1 + 8 * 3600;
        let due = next(PLUS2, &trigger, Some(last), JAN1);
        assert_eq!(due, Some(last + DAY), "due a day after");
        let wake = last + 30 * DAY + 5 * 3600;
        assert!(
            due.is_some_and(|due| wake >= due),
            "the machine slept past it"
        );
        let after = next(PLUS2, &trigger, Some(wake), JAN1);
        assert!(
            after.is_some_and(|after| after > wake),
            "the run at wake schedules the future, not 30 catch-up runs"
        );
    }

    #[test]
    fn resuming_after_the_last_run_restarts_the_schedule() {
        // Paused for weeks, resumed at anchor: the old last run must not make it due.
        let last = JAN1;
        let anchor = JAN1 + 40 * DAY;
        let due = next(PLUS2, &every(14, 10), Some(last), anchor);
        assert!(
            due.is_some_and(|due| due >= anchor && due < anchor + DAY),
            "resume schedules the next `hour`, got {due:?}"
        );
    }

    #[test]
    fn hours_outside_the_day_have_no_time() {
        assert_eq!(
            next(PLUS2, &every(1, 24), None, JAN1),
            None,
            "hour 24 does not exist"
        );
    }

    /// A zone that skips 02:00-03:00 local on one date (spring forward) and repeats
    /// 01:00-02:00 on another (fall back), offsets in seconds east of UTC.
    #[derive(Debug, Clone, Copy)]
    struct Dst;

    /// Instants (UTC) where the offset changes: +0 → +1h at `GAP_AT`, +1h → +0 at `FOLD_AT`.
    const GAP_AT: i64 = JAN1 + 10 * DAY + 2 * 3600;
    const FOLD_AT: i64 = JAN1 + 20 * DAY + 3600;

    impl Dst {
        fn offset_at(utc: i64) -> chrono::FixedOffset {
            let secs = if (GAP_AT..FOLD_AT).contains(&utc) {
                3600
            } else {
                0
            };
            chrono::FixedOffset::east_opt(secs).unwrap_or_else(|| chrono::Offset::fix(&chrono::Utc))
        }
    }

    impl TimeZone for Dst {
        type Offset = chrono::FixedOffset;

        fn from_offset(_: &Self::Offset) -> Self {
            Self
        }

        fn offset_from_local_date(&self, local: &NaiveDate) -> chrono::LocalResult<Self::Offset> {
            match local.and_hms_opt(12, 0, 0) {
                Some(noon) => self.offset_from_local_datetime(&noon),
                None => chrono::LocalResult::None,
            }
        }

        fn offset_from_local_datetime(
            &self,
            local: &NaiveDateTime,
        ) -> chrono::LocalResult<Self::Offset> {
            // Try both offsets, larger (earlier instant) first, as `Ambiguous` wants; keep those that
            // map back to themselves.
            let as_utc = local.and_utc().timestamp();
            let candidates = [3600_i64, 0]
                .into_iter()
                .filter(|off| {
                    let utc = as_utc - off;
                    Self::offset_at(utc).local_minus_utc() == i32::try_from(*off).unwrap_or(0)
                })
                .map(|off| Self::offset_at(as_utc - off))
                .collect::<Vec<_>>();
            match candidates.as_slice() {
                [] => chrono::LocalResult::None,
                [one] => chrono::LocalResult::Single(*one),
                [a, b, ..] => chrono::LocalResult::Ambiguous(*a, *b),
            }
        }

        fn offset_from_utc_date(&self, utc: &NaiveDate) -> Self::Offset {
            Self::offset_at(
                utc.and_hms_opt(12, 0, 0)
                    .map_or(0, |t| t.and_utc().timestamp()),
            )
        }

        fn offset_from_utc_datetime(&self, utc: &NaiveDateTime) -> Self::Offset {
            Self::offset_at(utc.and_utc().timestamp())
        }
    }

    #[test]
    fn a_nonexistent_local_hour_moves_forward_and_a_repeated_one_takes_the_first() {
        // Gap day (Jan 11): local 02:00-03:00 does not exist; a 02:00 rule fires at 03:00
        // local = 02:00 UTC.
        let gap_day = JAN1 + 10 * DAY;
        assert_eq!(
            next_run(&Dst, &every(1, 2), None, gap_day),
            Some(GAP_AT),
            "02:00 in the gap runs at the first hour that exists"
        );
        // Fold day (Jan 21): 01:00 local happens twice (00:00 UTC and 01:00 UTC).
        let fold_day = JAN1 + 20 * DAY;
        assert_eq!(
            next_run(&Dst, &every(1, 1), None, fold_day - 1),
            Some(fold_day),
            "the first 01:00 wins"
        );
    }
}
