//! Read-only `GROUP BY` aggregate queries (PLAN §4, "Every aggregate is a `GROUP BY`, none
//! of it in Rust"). No mutations, no schema changes — every function here takes `&Connection`
//! and only ever issues `SELECT`s (plus, for the zero-filled spine, cheap in-memory fill-in
//! over [`dates::DateRange::iter_days`], which is not aggregation, just presentation of an
//! aggregate that would otherwise have gaps for empty days).
//!
//! Every query is scoped to the live `entries`/`task_types`/`categories` tables, so a
//! trashed type (PLAN §4 trash design) never leaks into a total — it simply is not in the
//! table being aggregated.

use crate::dates::{self, DateRange};
use crate::error::Result;
use chrono::NaiveDate;
use rusqlite::{Connection, OptionalExtension};

/// One row of "day, per-type totals" (PLAN §4 bullet 1) — the Today grid + pills.
///
/// Only types with at least one entry on the day are included (an `INNER JOIN`, so
/// `count` is always `> 0`, matching the `CHECK (count > 0)` invariant). Types with zero
/// entries that day are **not** returned here — the UI is expected to merge this against
/// its full list of active types and treat any type missing from this result as zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayTypeTotal {
    pub task_type_id: i64,
    pub category_id: i64,
    pub name: String,
    pub count: i64,
}

/// One row of the day's log (PLAN §4 bullet 1), newest first, joined to type + category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayLogRow {
    pub entry_id: i64,
    pub task_type_id: i64,
    pub category_name: String,
    pub type_name: String,
    pub count: i64,
    pub notes: String,
    pub created_at: String,
}

/// One row of a per-type ranked total over a range (PLAN §4 bullet 5, the quarterly
/// workload / "money" query, PLAN §0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeRangeTotal {
    pub category_name: String,
    pub type_name: String,
    pub total: i64,
}

/// One row of a per-type lifetime total (PLAN §4 bullet 6) — the Task-types `Used` column.
/// Every task type is included, even with `total == 0` (a `LEFT JOIN`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeLifetimeTotal {
    pub task_type_id: i64,
    pub total: i64,
}

/// A range's total plus the total of [`dates::previous_range`] of equal length, for the
/// "vs previous" delta (PLAN §4 bullet 3). Also carries active days, the busiest day, and
/// distinct types used, all computed for `range` (not the previous one).
///
/// Busiest-day tie-break: when two or more days in the range share the same total, the
/// **earliest date wins** (`ORDER BY total DESC, date ASC LIMIT 1`), so the result is
/// deterministic regardless of `GROUP BY` row order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeSummary {
    pub total: i64,
    pub active_days: i64,
    pub busiest_day: Option<(NaiveDate, i64)>,
    pub distinct_types: i64,
    pub previous_total: i64,
}

/// Day, per-type totals for `date` (`'YYYY-MM-DD'`) — PLAN §4 bullet 1, first half.
///
/// See [`DayTypeTotal`] for which types are included. Ordered by category then name so
/// the UI can render pills in a stable order without a second sort.
pub(crate) fn day_type_totals(conn: &Connection, date: &str) -> Result<Vec<DayTypeTotal>> {
    let mut stmt = conn.prepare(
        "SELECT tt.id, tt.category_id, tt.name, SUM(e.count)
         FROM entries e
         JOIN task_types tt ON tt.id = e.task_type_id
         WHERE e.date = ?1
         GROUP BY tt.id, tt.category_id, tt.name
         ORDER BY tt.category_id, tt.name",
    )?;
    let rows = stmt
        .query_map([date], |row| {
            Ok(DayTypeTotal {
                task_type_id: row.get(0)?,
                category_id: row.get(1)?,
                name: row.get(2)?,
                count: row.get(3)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The day's total tally count for `date` — PLAN §4 bullet 1, second half. `0` on a day
/// with no entries (`COALESCE`, not a bare `SUM` that would return `NULL`).
pub(crate) fn day_total(conn: &Connection, date: &str) -> Result<i64> {
    conn.query_row(
        "SELECT COALESCE(SUM(count), 0) FROM entries WHERE date = ?1",
        [date],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

/// The number of distinct task types used on `date` — PLAN §4 bullet 1, second half.
/// `0` on a day with no entries.
pub(crate) fn day_distinct_types(conn: &Connection, date: &str) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(DISTINCT task_type_id) FROM entries WHERE date = ?1",
        [date],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

/// The day's log rows for `date`, newest first, capped at 6 — PLAN §4 bullet 2. Order
/// matches the §1.1 most-recent-tally rule: `created_at DESC, id DESC`, so the tiebreaker
/// for a same-timestamp burst is deterministic here too.
pub(crate) fn day_log_rows(conn: &Connection, date: &str) -> Result<Vec<DayLogRow>> {
    let mut stmt = conn.prepare(
        "SELECT e.id, e.task_type_id, c.name, tt.name, e.count, e.notes, e.created_at
         FROM entries e
         JOIN task_types tt ON tt.id = e.task_type_id
         JOIN categories c ON c.id = tt.category_id
         WHERE e.date = ?1
         ORDER BY e.created_at DESC, e.id DESC
         LIMIT 5",
    )?;
    let rows = stmt
        .query_map([date], |row| {
            Ok(DayLogRow {
                entry_id: row.get(0)?,
                task_type_id: row.get(1)?,
                category_name: row.get(2)?,
                type_name: row.get(3)?,
                count: row.get(4)?,
                notes: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The total tally count over `range` — shared by [`range_summary`] and reused for the
/// "previous" side of the delta.
fn range_total(conn: &Connection, range: DateRange) -> Result<i64> {
    conn.query_row(
        "SELECT COALESCE(SUM(count), 0) FROM entries WHERE date BETWEEN ?1 AND ?2",
        [dates::to_sql(range.start), dates::to_sql(range.end)],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

/// Range total, active days, busiest day, distinct types used, and the equal-length
/// [`dates::previous_range`] total for the vs-previous delta — PLAN §4 bullet 3. Never
/// divides by zero: an empty range yields `total = 0`, `active_days = 0`,
/// `busiest_day = None`, `distinct_types = 0`, computed via `COALESCE`/`COUNT`/`Option`
/// rather than any division.
pub(crate) fn range_summary(conn: &Connection, range: DateRange) -> Result<RangeSummary> {
    let start = dates::to_sql(range.start);
    let end = dates::to_sql(range.end);

    let total = range_total(conn, range)?;

    let active_days: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT date) FROM entries WHERE date BETWEEN ?1 AND ?2",
        [&start, &end],
        |row| row.get(0),
    )?;

    let distinct_types: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT task_type_id) FROM entries WHERE date BETWEEN ?1 AND ?2",
        [&start, &end],
        |row| row.get(0),
    )?;

    // Tie-break documented on RangeSummary: earliest date wins among equal totals.
    let busiest_day: Option<(String, i64)> = conn
        .query_row(
            "SELECT date, SUM(count) AS total
             FROM entries
             WHERE date BETWEEN ?1 AND ?2
             GROUP BY date
             ORDER BY total DESC, date ASC
             LIMIT 1",
            [&start, &end],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let busiest_day = busiest_day.map(|(d, total)| {
        let parsed = dates::parse_sql(&d)
            .expect("dates read back from the entries.date column are always valid calendar days");
        (parsed, total)
    });

    let previous_total = range_total(conn, dates::previous_range(range))?;

    Ok(RangeSummary {
        total,
        active_days,
        busiest_day,
        distinct_types,
        previous_total,
    })
}

/// Per-day totals across `range`, zero-filled — PLAN §4 bullet 4. The `GROUP BY` only
/// returns days with entries; the date spine from [`DateRange::iter_days`] fills in every
/// other day as `0` so weekends and other gaps read as gaps, not as missing rows. Always
/// `range.days()` entries long, in date order.
pub(crate) fn range_daily_totals(conn: &Connection, range: DateRange) -> Result<Vec<(NaiveDate, i64)>> {
    let start = dates::to_sql(range.start);
    let end = dates::to_sql(range.end);

    let mut stmt = conn.prepare(
        "SELECT date, SUM(count) FROM entries WHERE date BETWEEN ?1 AND ?2 GROUP BY date",
    )?;
    let mut totals: std::collections::HashMap<NaiveDate, i64> = std::collections::HashMap::new();
    let mapped = stmt.query_map([&start, &end], |row| {
        let date_str: String = row.get(0)?;
        let total: i64 = row.get(1)?;
        Ok((date_str, total))
    })?;
    for row in mapped {
        let (date_str, total) = row?;
        let date = dates::parse_sql(&date_str)
            .expect("dates read back from the entries.date column are always valid calendar days");
        totals.insert(date, total);
    }

    Ok(range
        .iter_days()
        .map(|d| (d, totals.get(&d).copied().unwrap_or(0)))
        .collect())
}

/// Per-type totals across `range`, ranked and **complete** — PLAN §4 bullet 5, the
/// quarterly workload ("money") query from PLAN §0. Only types with at least one entry in
/// the range appear (an `INNER JOIN`, so every returned `total` is `> 0`). Ordered by
/// total descending, then category, then name. **No `LIMIT` here** — capping to a top-N is
/// strictly a display concern (PLAN §4).
pub(crate) fn range_type_totals_ranked(conn: &Connection, range: DateRange) -> Result<Vec<TypeRangeTotal>> {
    let mut stmt = conn.prepare(
        "SELECT c.name, tt.name, SUM(e.count) AS total
         FROM entries e
         JOIN task_types tt ON tt.id = e.task_type_id
         JOIN categories c ON c.id = tt.category_id
         WHERE e.date BETWEEN ?1 AND ?2
         GROUP BY tt.id, c.name, tt.name
         HAVING SUM(e.count) > 0
         ORDER BY total DESC, c.name ASC, tt.name ASC",
    )?;
    let rows = stmt
        .query_map(
            [dates::to_sql(range.start), dates::to_sql(range.end)],
            |row| {
                Ok(TypeRangeTotal {
                    category_name: row.get(0)?,
                    type_name: row.get(1)?,
                    total: row.get(2)?,
                })
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// One task type's lifetime usage — the edit/delete dialogs' "N tallies · last used ..."
/// line (Phase 3B screen 09). `active_days` is `COUNT(DISTINCT date)`, not `COUNT(*)`, so a
/// type tallied twice on the same day still counts that day once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypeUsage {
    pub total: i64,
    pub active_days: i64,
    pub last_used: Option<NaiveDate>,
}

/// A single task type's lifetime usage over `entries` — total tallies, distinct days tallied,
/// and the most recent date. A type with no entries yields `{0, 0, None}`, not an error.
pub(crate) fn type_usage(conn: &Connection, task_type_id: i64) -> Result<TypeUsage> {
    let (total, active_days, last_used): (i64, i64, Option<String>) = conn.query_row(
        "SELECT COALESCE(SUM(count), 0), COUNT(DISTINCT date), MAX(date)
         FROM entries WHERE task_type_id = ?1",
        [task_type_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let last_used = last_used.map(|d| {
        dates::parse_sql(&d)
            .expect("dates read back from the entries.date column are always valid calendar days")
    });
    Ok(TypeUsage { total, active_days, last_used })
}

/// Per-type lifetime `SUM(count)` across all time — PLAN §4 bullet 6, the Task-types
/// `Used` column. Every task type is included via `LEFT JOIN`, with `total = 0` for a
/// type that has never been tallied.
pub(crate) fn type_lifetime_totals(conn: &Connection) -> Result<Vec<TypeLifetimeTotal>> {
    let mut stmt = conn.prepare(
        "SELECT tt.id, COALESCE(SUM(e.count), 0)
         FROM task_types tt
         LEFT JOIN entries e ON e.task_type_id = tt.id
         GROUP BY tt.id
         ORDER BY tt.id",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(TypeLifetimeTotal {
                task_type_id: row.get(0)?,
                total: row.get(1)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dates::{month_of, quarter_of, week_of};
    use crate::schema::{configure_connection, create_schema};

    /// Shared fixture: two categories (one with two types sharing a short name under a
    /// third category to test the "same short name, two categories" case), entries across
    /// several dates including a weekend gap.
    ///
    /// Layout (all dates 2026, a week Mon 2026-01-05..Sun 2026-01-11):
    /// - SAKTI / Rekon      (id 1): Mon 3, Tue 2, Fri 1   -> total 6
    /// - SAKTI / Approval   (id 2): Tue 1                 -> total 1
    /// - DIGIPAY / Rekon    (id 3): Wed 4                 -> total 4   (shares name "Rekon")
    /// - HAICSO / Unused    (id 4): no entries at all      -> total 0, lifetime 0
    /// Saturday/Sunday (01-10, 01-11) have no entries at all — the weekend gap.
    struct Fixture {
        conn: Connection,
    }

    impl Fixture {
        fn build() -> Self {
            let conn = Connection::open_in_memory().unwrap();
            create_schema(&conn).unwrap();
            configure_connection(&conn).unwrap();

            conn.execute("INSERT INTO categories (name) VALUES ('SAKTI')", [])
                .unwrap();
            conn.execute("INSERT INTO categories (name) VALUES ('DIGIPAY')", [])
                .unwrap();
            conn.execute("INSERT INTO categories (name) VALUES ('HAICSO')", [])
                .unwrap();

            conn.execute(
                "INSERT INTO task_types (category_id, name) VALUES (1, 'Rekon')",
                [],
            )
            .unwrap(); // id 1: SAKTI - Rekon
            conn.execute(
                "INSERT INTO task_types (category_id, name) VALUES (1, 'Approval')",
                [],
            )
            .unwrap(); // id 2: SAKTI - Approval
            conn.execute(
                "INSERT INTO task_types (category_id, name) VALUES (2, 'Rekon')",
                [],
            )
            .unwrap(); // id 3: DIGIPAY - Rekon (same short name as id 1)
            conn.execute(
                "INSERT INTO task_types (category_id, name) VALUES (3, 'Unused')",
                [],
            )
            .unwrap(); // id 4: HAICSO - Unused, no entries ever

            let mut seq = 0i64;
            let mut insert = |type_id: i64, date: &str, count: i64| {
                seq += 1;
                conn.execute(
                    "INSERT INTO entries (task_type_id, date, count, created_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![
                        type_id,
                        date,
                        count,
                        format!("2026-01-01 00:00:{:02}.000", seq)
                    ],
                )
                .unwrap();
            };

            insert(1, "2026-01-05", 3); // Mon
            insert(1, "2026-01-06", 2); // Tue
            insert(2, "2026-01-06", 1); // Tue
            insert(3, "2026-01-07", 4); // Wed
            insert(1, "2026-01-09", 1); // Fri

            Fixture { conn }
        }

        fn week(&self) -> DateRange {
            week_of(dates::parse_sql("2026-01-07").unwrap())
        }
    }

    // -- day_type_totals / day_total / day_distinct_types --

    #[test]
    fn day_type_totals_returns_only_types_with_entries_that_day() {
        let f = Fixture::build();
        let rows = day_type_totals(&f.conn, "2026-01-06").unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|r| r.task_type_id == 1 && r.count == 2));
        assert!(rows.iter().any(|r| r.task_type_id == 2 && r.count == 1));
    }

    #[test]
    fn day_type_totals_empty_day_is_empty_vec() {
        let f = Fixture::build();
        let rows = day_type_totals(&f.conn, "2026-01-10").unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn day_total_sums_all_types_that_day() {
        let f = Fixture::build();
        assert_eq!(day_total(&f.conn, "2026-01-06").unwrap(), 3);
    }

    #[test]
    fn day_total_empty_day_is_zero() {
        let f = Fixture::build();
        assert_eq!(day_total(&f.conn, "2026-01-10").unwrap(), 0);
    }

    #[test]
    fn day_distinct_types_counts_types_not_entries() {
        let f = Fixture::build();
        // Mon 2026-01-05 has one entry for type 1 only.
        assert_eq!(day_distinct_types(&f.conn, "2026-01-05").unwrap(), 1);
        // Tue 2026-01-06 has entries for types 1 and 2.
        assert_eq!(day_distinct_types(&f.conn, "2026-01-06").unwrap(), 2);
    }

    #[test]
    fn day_distinct_types_empty_day_is_zero() {
        let f = Fixture::build();
        assert_eq!(day_distinct_types(&f.conn, "2026-01-10").unwrap(), 0);
    }

    // -- day_log_rows --

    #[test]
    fn day_log_rows_orders_newest_first_and_joins_names() {
        let f = Fixture::build();
        let rows = day_log_rows(&f.conn, "2026-01-06").unwrap();
        assert_eq!(rows.len(), 2);
        // type 2 (Approval) was inserted after type 1's Tuesday row, so it is newest.
        assert_eq!(rows[0].type_name, "Approval");
        assert_eq!(rows[0].category_name, "SAKTI");
        assert_eq!(rows[1].type_name, "Rekon");
    }

    #[test]
    fn day_log_rows_caps_at_five() {
        let f = Fixture::build();
        for i in 0..8 {
            f.conn
                .execute(
                    "INSERT INTO entries (task_type_id, date, count, created_at) \
                     VALUES (1, '2026-01-20', 1, ?1)",
                    [format!("2026-01-01 00:01:{:02}.000", i)],
                )
                .unwrap();
        }
        let rows = day_log_rows(&f.conn, "2026-01-20").unwrap();
        assert_eq!(rows.len(), 5);
    }

    #[test]
    fn day_log_rows_empty_day_is_empty_vec() {
        let f = Fixture::build();
        let rows = day_log_rows(&f.conn, "2026-01-10").unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn day_log_rows_tiebreak_uses_id_when_created_at_ties() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        configure_connection(&conn).unwrap();
        conn.execute("INSERT INTO categories (name) VALUES ('SAKTI')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO task_types (category_id, name) VALUES (1, 'Rekon')",
            [],
        )
        .unwrap();
        // Two entries with identical created_at: id must break the tie (§1.1).
        conn.execute(
            "INSERT INTO entries (task_type_id, date, count, notes, created_at) \
             VALUES (1, '2026-02-01', 1, 'first', '2026-02-01 00:00:00.000')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO entries (task_type_id, date, count, notes, created_at) \
             VALUES (1, '2026-02-01', 1, 'second', '2026-02-01 00:00:00.000')",
            [],
        )
        .unwrap();

        let rows = day_log_rows(&conn, "2026-02-01").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].notes, "second"); // higher id wins the tie
        assert_eq!(rows[1].notes, "first");
    }

    // -- range_summary --

    #[test]
    fn range_summary_totals_active_days_and_distinct_types() {
        let f = Fixture::build();
        let summary = range_summary(&f.conn, f.week()).unwrap();
        // 3 + 2 + 1 + 4 + 1 = 11 across the week.
        assert_eq!(summary.total, 11);
        // Active days: 01-05, 01-06, 01-07, 01-09 = 4.
        assert_eq!(summary.active_days, 4);
        // Distinct types used: 1, 2, 3 = 3.
        assert_eq!(summary.distinct_types, 3);
    }

    #[test]
    fn range_summary_busiest_day_is_the_highest_total() {
        let f = Fixture::build();
        let summary = range_summary(&f.conn, f.week()).unwrap();
        // 2026-01-07 (Wed) has 4, the single highest day.
        assert_eq!(
            summary.busiest_day,
            Some((dates::parse_sql("2026-01-07").unwrap(), 4))
        );
    }

    #[test]
    fn range_summary_busiest_day_tie_break_is_earliest_date() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        configure_connection(&conn).unwrap();
        conn.execute("INSERT INTO categories (name) VALUES ('SAKTI')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO task_types (category_id, name) VALUES (1, 'Rekon')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO entries (task_type_id, date, count) VALUES (1, '2026-03-02', 5)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO entries (task_type_id, date, count) VALUES (1, '2026-03-01', 5)",
            [],
        )
        .unwrap();

        let range = DateRange::new(
            dates::parse_sql("2026-03-01").unwrap(),
            dates::parse_sql("2026-03-02").unwrap(),
        )
        .unwrap();
        let summary = range_summary(&conn, range).unwrap();
        assert_eq!(
            summary.busiest_day,
            Some((dates::parse_sql("2026-03-01").unwrap(), 5))
        );
    }

    #[test]
    fn range_summary_previous_range_delta() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        configure_connection(&conn).unwrap();
        conn.execute("INSERT INTO categories (name) VALUES ('SAKTI')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO task_types (category_id, name) VALUES (1, 'Rekon')",
            [],
        )
        .unwrap();

        // Current week: 2026-01-05..2026-01-11, total 10.
        conn.execute(
            "INSERT INTO entries (task_type_id, date, count) VALUES (1, '2026-01-06', 10)",
            [],
        )
        .unwrap();
        // Previous week: 2025-12-29..2026-01-04, total 3.
        conn.execute(
            "INSERT INTO entries (task_type_id, date, count) VALUES (1, '2026-01-01', 3)",
            [],
        )
        .unwrap();

        let range = week_of(dates::parse_sql("2026-01-07").unwrap());
        let summary = range_summary(&conn, range).unwrap();
        assert_eq!(summary.total, 10);
        assert_eq!(summary.previous_total, 3);
    }

    #[test]
    fn range_summary_empty_range_has_zero_total_and_no_busiest_day() {
        let f = Fixture::build();
        let empty_range = DateRange::new(
            dates::parse_sql("2030-01-01").unwrap(),
            dates::parse_sql("2030-01-07").unwrap(),
        )
        .unwrap();
        let summary = range_summary(&f.conn, empty_range).unwrap();
        assert_eq!(summary.total, 0);
        assert_eq!(summary.active_days, 0);
        assert_eq!(summary.distinct_types, 0);
        assert_eq!(summary.busiest_day, None);
        assert_eq!(summary.previous_total, 0);
    }

    // -- range_daily_totals --

    #[test]
    fn range_daily_totals_is_zero_filled_and_full_length() {
        let f = Fixture::build();
        let range = f.week();
        let totals = range_daily_totals(&f.conn, range).unwrap();
        assert_eq!(totals.len(), range.days() as usize);
        assert_eq!(totals.first().unwrap().0, range.start);
        assert_eq!(totals.last().unwrap().0, range.end);

        // Saturday 2026-01-10 has no entries: must be present as zero, not missing.
        let saturday = totals
            .iter()
            .find(|(d, _)| *d == dates::parse_sql("2026-01-10").unwrap())
            .expect("saturday must be present in the zero-filled spine");
        assert_eq!(saturday.1, 0);

        // Wednesday 2026-01-07 has entries totalling 4.
        let wednesday = totals
            .iter()
            .find(|(d, _)| *d == dates::parse_sql("2026-01-07").unwrap())
            .unwrap();
        assert_eq!(wednesday.1, 4);
    }

    #[test]
    fn range_daily_totals_empty_range_is_all_zero_but_full_length() {
        let f = Fixture::build();
        let empty_range = DateRange::new(
            dates::parse_sql("2030-01-01").unwrap(),
            dates::parse_sql("2030-01-05").unwrap(),
        )
        .unwrap();
        let totals = range_daily_totals(&f.conn, empty_range).unwrap();
        assert_eq!(totals.len(), 5);
        assert!(totals.iter().all(|(_, c)| *c == 0));
    }

    // -- range_type_totals_ranked --

    #[test]
    fn range_type_totals_ranked_is_complete_and_ordered_by_total_desc() {
        let f = Fixture::build();
        let ranked = range_type_totals_ranked(&f.conn, f.week()).unwrap();
        // Three types have entries in the week: SAKTI/Rekon=6, DIGIPAY/Rekon=4, SAKTI/Approval=1.
        assert_eq!(ranked.len(), 3);
        assert_eq!(ranked[0].total, 6);
        assert_eq!(ranked[0].category_name, "SAKTI");
        assert_eq!(ranked[0].type_name, "Rekon");
        assert_eq!(ranked[1].total, 4);
        assert_eq!(ranked[1].category_name, "DIGIPAY");
        assert_eq!(ranked[2].total, 1);
    }

    #[test]
    fn range_type_totals_ranked_not_capped() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        configure_connection(&conn).unwrap();
        conn.execute("INSERT INTO categories (name) VALUES ('SAKTI')", [])
            .unwrap();
        for i in 0..15 {
            conn.execute(
                &format!(
                    "INSERT INTO task_types (category_id, name) VALUES (1, 'Type{i}')",
                    i = i
                ),
                [],
            )
            .unwrap();
            conn.execute(
                &format!(
                    "INSERT INTO entries (task_type_id, date, count) VALUES ({id}, '2026-04-01', 1)",
                    id = i + 1
                ),
                [],
            )
            .unwrap();
        }
        let range = DateRange::new(
            dates::parse_sql("2026-04-01").unwrap(),
            dates::parse_sql("2026-04-01").unwrap(),
        )
        .unwrap();
        let ranked = range_type_totals_ranked(&conn, range).unwrap();
        assert_eq!(ranked.len(), 15, "must not be capped at 8 in the data layer");
    }

    #[test]
    fn range_type_totals_ranked_empty_range_is_empty_vec() {
        let f = Fixture::build();
        let empty_range = DateRange::new(
            dates::parse_sql("2030-01-01").unwrap(),
            dates::parse_sql("2030-01-07").unwrap(),
        )
        .unwrap();
        let ranked = range_type_totals_ranked(&f.conn, empty_range).unwrap();
        assert!(ranked.is_empty());
    }

    // -- type_lifetime_totals --

    #[test]
    fn type_lifetime_totals_includes_zero_usage_types() {
        let f = Fixture::build();
        let totals = type_lifetime_totals(&f.conn).unwrap();
        assert_eq!(totals.len(), 4);
        let by_id = |id: i64| totals.iter().find(|t| t.task_type_id == id).unwrap().total;
        assert_eq!(by_id(1), 6); // SAKTI/Rekon: 3+2+1
        assert_eq!(by_id(2), 1); // SAKTI/Approval
        assert_eq!(by_id(3), 4); // DIGIPAY/Rekon
        assert_eq!(by_id(4), 0); // HAICSO/Unused — never tallied
    }

    // -- type_usage --

    #[test]
    fn type_usage_no_entries_is_zeroed() {
        let f = Fixture::build();
        let usage = type_usage(&f.conn, 4).unwrap(); // HAICSO/Unused — never tallied
        assert_eq!(usage, TypeUsage { total: 0, active_days: 0, last_used: None });
    }

    #[test]
    fn type_usage_populated_type_sums_and_tracks_last_used() {
        let f = Fixture::build();
        // Type 1 (SAKTI/Rekon): Mon 3, Tue 2, Fri 1 -> total 6, 3 distinct days, last 01-09.
        let usage = type_usage(&f.conn, 1).unwrap();
        assert_eq!(usage.total, 6);
        assert_eq!(usage.active_days, 3);
        assert_eq!(usage.last_used, Some(dates::parse_sql("2026-01-09").unwrap()));
    }

    #[test]
    fn type_usage_two_entries_same_date_count_as_one_active_day() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        configure_connection(&conn).unwrap();
        conn.execute("INSERT INTO categories (name) VALUES ('SAKTI')", []).unwrap();
        conn.execute("INSERT INTO task_types (category_id, name) VALUES (1, 'Rekon')", []).unwrap();
        conn.execute(
            "INSERT INTO entries (task_type_id, date, count) VALUES (1, '2026-02-01', 2)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO entries (task_type_id, date, count) VALUES (1, '2026-02-01', 5)",
            [],
        )
        .unwrap();

        let usage = type_usage(&conn, 1).unwrap();
        assert_eq!(usage.total, 7);
        assert_eq!(usage.active_days, 1);
        assert_eq!(usage.last_used, Some(dates::parse_sql("2026-02-01").unwrap()));
    }

    #[test]
    fn type_lifetime_totals_empty_schema_is_empty_vec() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        configure_connection(&conn).unwrap();
        let totals = type_lifetime_totals(&conn).unwrap();
        assert!(totals.is_empty());
    }

    // -- sanity check that month/quarter boundary helpers compose with these queries --

    #[test]
    fn range_summary_works_over_a_month_range() {
        let f = Fixture::build();
        let month = month_of(dates::parse_sql("2026-01-15").unwrap());
        let summary = range_summary(&f.conn, month).unwrap();
        assert_eq!(summary.total, 11);
    }

    #[test]
    fn range_summary_works_over_a_quarter_range() {
        let f = Fixture::build();
        let quarter = quarter_of(dates::parse_sql("2026-01-15").unwrap());
        let summary = range_summary(&f.conn, quarter).unwrap();
        assert_eq!(summary.total, 11);
    }
}
