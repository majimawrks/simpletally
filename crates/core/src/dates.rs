//! Pure date-range boundary functions for the Insights range toolbar (PLAN §4).
//!
//! Everything here is deterministic and side-effect free: no SQL, no I/O. The
//! aggregate layer calls these to compute the `[start, end]` bounds it then
//! passes to queries via [`to_sql`]/[`parse_sql`].

use chrono::{Datelike, Days, NaiveDate, Weekday};

/// An inclusive `[start, end]` span of calendar days. `start <= end` always.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateRange {
    pub start: NaiveDate,
    pub end: NaiveDate,
}

impl DateRange {
    /// Builds a range, or `None` if `start > end`.
    pub fn new(start: NaiveDate, end: NaiveDate) -> Option<Self> {
        if start > end {
            None
        } else {
            Some(DateRange { start, end })
        }
    }

    /// Inclusive day count: `start == end` gives `1`.
    pub fn days(&self) -> i64 {
        (self.end - self.start).num_days() + 1
    }

    /// Whether `d` falls within `[start, end]`, inclusive.
    pub fn contains(&self, d: NaiveDate) -> bool {
        self.start <= d && d <= self.end
    }

    /// Every calendar day in the range, in order, for the zero-filled date spine.
    pub fn iter_days(&self) -> impl Iterator<Item = NaiveDate> {
        let days = self.days();
        let start = self.start;
        (0..days).map(move |i| {
            start
                .checked_add_days(Days::new(i as u64))
                .expect("date arithmetic within an existing DateRange cannot overflow")
        })
    }
}

/// The ISO-8601 week (Monday..Sunday) containing `d`.
pub fn week_of(d: NaiveDate) -> DateRange {
    let monday = d
        .week(Weekday::Mon)
        .first_day();
    let sunday = d.week(Weekday::Mon).last_day();
    DateRange { start: monday, end: sunday }
}

/// The calendar month (1st..last day) containing `d`.
pub fn month_of(d: NaiveDate) -> DateRange {
    let start = NaiveDate::from_ymd_opt(d.year(), d.month(), 1)
        .expect("year/month from an existing date is always valid with day 1");
    let (next_year, next_month) = if d.month() == 12 {
        (d.year() + 1, 1)
    } else {
        (d.year(), d.month() + 1)
    };
    let first_of_next = NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .expect("year/month rollover from an existing date is always valid");
    let end = first_of_next
        .pred_opt()
        .expect("the day before a valid date always exists");
    DateRange { start, end }
}

/// The calendar quarter containing `d`: Q1 Jan1-Mar31, Q2 Apr1-Jun30, Q3 Jul1-Sep30, Q4 Oct1-Dec31.
pub fn quarter_of(d: NaiveDate) -> DateRange {
    let start_month = (d.month0() / 3) * 3 + 1;
    let start = NaiveDate::from_ymd_opt(d.year(), start_month, 1)
        .expect("computed quarter-start month is always 1, 4, 7 or 10");
    let (end_year, end_month_first) = if start_month == 10 {
        (d.year() + 1, 1)
    } else {
        (d.year(), start_month + 3)
    };
    let first_after = NaiveDate::from_ymd_opt(end_year, end_month_first, 1)
        .expect("quarter rollover month is always valid");
    let end = first_after
        .pred_opt()
        .expect("the day before a valid date always exists");
    DateRange { start, end }
}

/// The calendar year (Jan1..Dec31) containing `d`.
pub fn year_of(d: NaiveDate) -> DateRange {
    let start = NaiveDate::from_ymd_opt(d.year(), 1, 1)
        .expect("January 1st of an existing year is always valid");
    let end = NaiveDate::from_ymd_opt(d.year(), 12, 31)
        .expect("December 31st of an existing year is always valid");
    DateRange { start, end }
}

/// The range of equal length immediately preceding `r` (for the "vs previous" delta, PLAN §4).
/// For an N-day range, this is the N days ending the day before `r.start`.
pub fn previous_range(r: DateRange) -> DateRange {
    let len = r.days();
    let end = r
        .start
        .pred_opt()
        .expect("previous_range is only meaningful for ranges not starting at the date minimum");
    let start = end
        .checked_sub_days(Days::new((len - 1) as u64))
        .expect("previous_range span underflowed the representable date range");
    DateRange { start, end }
}

/// Formats a date as the DB's `'YYYY-MM-DD'` TEXT form.
pub fn to_sql(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

/// Strict calendar parse of the DB's `'YYYY-MM-DD'` TEXT form.
pub fn parse_sql(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    // -- DateRange basics --

    #[test]
    fn new_rejects_inverted_range() {
        assert!(DateRange::new(d(2026, 1, 2), d(2026, 1, 1)).is_none());
    }

    #[test]
    fn new_accepts_single_day() {
        let r = DateRange::new(d(2026, 1, 1), d(2026, 1, 1)).unwrap();
        assert_eq!(r.days(), 1);
    }

    #[test]
    fn days_counts_inclusively() {
        let r = DateRange::new(d(2026, 1, 1), d(2026, 1, 7)).unwrap();
        assert_eq!(r.days(), 7);
    }

    #[test]
    fn contains_is_inclusive_on_both_ends() {
        let r = DateRange::new(d(2026, 1, 1), d(2026, 1, 7)).unwrap();
        assert!(r.contains(d(2026, 1, 1)));
        assert!(r.contains(d(2026, 1, 7)));
        assert!(r.contains(d(2026, 1, 4)));
        assert!(!r.contains(d(2025, 12, 31)));
        assert!(!r.contains(d(2026, 1, 8)));
    }

    #[test]
    fn iter_days_yields_exactly_days_count_starting_and_ending_correctly() {
        let r = DateRange::new(d(2026, 1, 1), d(2026, 1, 7)).unwrap();
        let days: Vec<NaiveDate> = r.iter_days().collect();
        assert_eq!(days.len(), r.days() as usize);
        assert_eq!(*days.first().unwrap(), r.start);
        assert_eq!(*days.last().unwrap(), r.end);
    }

    #[test]
    fn iter_days_single_day_yields_one_item() {
        let r = DateRange::new(d(2026, 1, 1), d(2026, 1, 1)).unwrap();
        let days: Vec<NaiveDate> = r.iter_days().collect();
        assert_eq!(days, vec![d(2026, 1, 1)]);
    }

    // -- week_of --

    #[test]
    fn week_of_mid_week_day() {
        // 2026-01-07 is a Wednesday.
        let r = week_of(d(2026, 1, 7));
        assert_eq!(r.start, d(2026, 1, 5)); // Monday
        assert_eq!(r.end, d(2026, 1, 11)); // Sunday
    }

    #[test]
    fn week_of_straddles_iso_year_boundary() {
        // 2026-01-01 is a Thursday; its ISO week spans 2025-12-29 (Mon) .. 2026-01-04 (Sun).
        let r = week_of(d(2026, 1, 1));
        assert_eq!(r.start, d(2025, 12, 29));
        assert_eq!(r.end, d(2026, 1, 4));
    }

    #[test]
    fn week_of_asking_from_either_end_agrees() {
        let from_start = week_of(d(2025, 12, 29));
        let from_end = week_of(d(2026, 1, 4));
        assert_eq!(from_start, from_end);
    }

    // -- month_of --

    #[test]
    fn month_of_leap_february_ends_on_29th() {
        let r = month_of(d(2024, 2, 15));
        assert_eq!(r.start, d(2024, 2, 1));
        assert_eq!(r.end, d(2024, 2, 29));
        assert_eq!(r.days(), 29);
    }

    #[test]
    fn month_of_non_leap_february_ends_on_28th() {
        let r = month_of(d(2025, 2, 10));
        assert_eq!(r.start, d(2025, 2, 1));
        assert_eq!(r.end, d(2025, 2, 28));
        assert_eq!(r.days(), 28);
    }

    #[test]
    fn month_of_december_stays_in_same_year() {
        let r = month_of(d(2025, 12, 25));
        assert_eq!(r.start, d(2025, 12, 1));
        assert_eq!(r.end, d(2025, 12, 31));
    }

    // -- quarter_of --

    #[test]
    fn quarter_of_mar_31_is_q1() {
        let r = quarter_of(d(2026, 3, 31));
        assert_eq!(r.start, d(2026, 1, 1));
        assert_eq!(r.end, d(2026, 3, 31));
    }

    #[test]
    fn quarter_of_apr_1_is_q2() {
        let r = quarter_of(d(2026, 4, 1));
        assert_eq!(r.start, d(2026, 4, 1));
        assert_eq!(r.end, d(2026, 6, 30));
    }

    #[test]
    fn quarter_of_dec_31_is_q4_and_stays_in_year() {
        let r = quarter_of(d(2026, 12, 31));
        assert_eq!(r.start, d(2026, 10, 1));
        assert_eq!(r.end, d(2026, 12, 31));
    }

    #[test]
    fn quarter_of_leap_year_q1_includes_feb_29() {
        let r = quarter_of(d(2024, 1, 15));
        assert_eq!(r.start, d(2024, 1, 1));
        assert_eq!(r.end, d(2024, 3, 31));
        assert!(r.contains(d(2024, 2, 29)));
    }

    // -- year_of --

    #[test]
    fn year_of_leap_year_has_366_days() {
        let r = year_of(d(2024, 6, 1));
        assert_eq!(r.start, d(2024, 1, 1));
        assert_eq!(r.end, d(2024, 12, 31));
        assert_eq!(r.days(), 366);
    }

    #[test]
    fn year_of_non_leap_year_has_365_days() {
        let r = year_of(d(2025, 6, 1));
        assert_eq!(r.start, d(2025, 1, 1));
        assert_eq!(r.end, d(2025, 12, 31));
        assert_eq!(r.days(), 365);
    }

    // -- previous_range --

    #[test]
    fn previous_range_of_a_week_is_the_prior_seven_days() {
        let r = week_of(d(2026, 1, 7)); // 2026-01-05 .. 2026-01-11
        let p = previous_range(r);
        assert_eq!(p.start, d(2025, 12, 29));
        assert_eq!(p.end, d(2026, 1, 4));
        assert_eq!(p.days(), r.days());
    }

    #[test]
    fn previous_range_of_a_single_day_is_the_day_before() {
        let r = DateRange::new(d(2026, 1, 1), d(2026, 1, 1)).unwrap();
        let p = previous_range(r);
        assert_eq!(p.start, d(2025, 12, 31));
        assert_eq!(p.end, d(2025, 12, 31));
    }

    #[test]
    fn previous_range_of_a_ninety_day_quarter_ends_the_day_before_start() {
        let r = quarter_of(d(2026, 2, 1)); // Q1 2026: Jan1-Mar31, 90 days (non-leap)
        assert_eq!(r.days(), 90);
        let p = previous_range(r);
        assert_eq!(p.end, d(2025, 12, 31));
        assert_eq!(p.days(), 90);
    }

    // -- to_sql / parse_sql --

    #[test]
    fn to_sql_formats_as_iso_date() {
        assert_eq!(to_sql(d(2026, 1, 5)), "2026-01-05");
    }

    #[test]
    fn parse_sql_round_trips() {
        let original = d(2024, 2, 29);
        assert_eq!(parse_sql(&to_sql(original)), Some(original));
    }

    #[test]
    fn parse_sql_rejects_non_calendar_date() {
        assert_eq!(parse_sql("2025-02-29"), None); // not a leap year
    }

    #[test]
    fn parse_sql_rejects_malformed_input() {
        assert_eq!(parse_sql("not-a-date"), None);
        assert_eq!(parse_sql("2026/01/05"), None);
    }
}
