//! Entry (tally) mutation ops — add, remove-most-recent, edit, delete (PLAN §1.1/§4).
//!
//! These are the only sanctioned ways to mutate `entries`; callers never see a bare
//! `&Connection` (PLAN §2, boundary rule 1). Each op validates its own inputs (a real
//! calendar date via [`crate::dates::parse_sql`], a positive `count`, an existing
//! `task_type_id`/`entry_id`) and maps failures onto [`Error::Invalid`] /
//! [`Error::NotFound`] rather than leaking raw SQLite errors.

use crate::dates;
use crate::error::{Error, Result};
use rusqlite::{Connection, OptionalExtension};

/// The outcome of [`remove_most_recent`] — which of the two behaviors fired, or neither.
#[derive(Debug, PartialEq, Eq)]
pub enum RemoveOutcome {
    /// The most recent entry's `count` was decremented by 1 (it was > 1).
    Decremented,
    /// The most recent entry was deleted outright (its `count` was 1).
    Deleted,
    /// No entry existed for the given `(task_type_id, date)`; nothing happened.
    NoOp,
}

/// What [`remove_tallies`] actually did — the quick-add popup reports this back, because it
/// is routinely less than what was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RemoveReport {
    /// Tallies actually removed.
    pub removed: i64,
    /// Rows deleted outright (their whole count was taken).
    pub rows_deleted: usize,
    /// Rows left holding a tally **only** because they carry a note — see [`remove_tallies`].
    pub notes_protected: usize,
}

/// How many tallies [`remove_tallies`] could take from a day's rows, given `(count, has_note)`
/// for each. A note-carrying row can be reduced but never emptied, so it contributes
/// `count - 1`.
///
/// Pure, and public, so the quick-add preview shows the number the write will really produce
/// instead of the number the user asked for. A preview that silently over-promises is the
/// same defect as a silent clamp.
pub fn removable(rows: impl IntoIterator<Item = (i64, bool)>) -> i64 {
    rows.into_iter().map(|(count, has_note)| if has_note { count - 1 } else { count }).sum()
}

/// Validates that `date` is a real calendar day, returning [`Error::Invalid`] otherwise.
fn require_valid_date(date: &str) -> Result<()> {
    if dates::parse_sql(date).is_none() {
        return Err(Error::Invalid(format!("not a calendar date: {date}")));
    }
    Ok(())
}

/// Validates that `count` is positive, returning [`Error::Invalid`] otherwise.
fn require_positive_count(count: i64) -> Result<()> {
    if count <= 0 {
        return Err(Error::Invalid(format!("count must be positive, got {count}")));
    }
    Ok(())
}

/// Validates that `task_type_id` exists, returning [`Error::NotFound`] otherwise.
fn require_task_type_exists(conn: &Connection, task_type_id: i64) -> Result<()> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM task_types WHERE id = ?1)",
        [task_type_id],
        |row| row.get(0),
    )?;
    if !exists {
        return Err(Error::NotFound(format!("task_type {task_type_id}")));
    }
    Ok(())
}

/// Adds a new tally (PLAN §1.1): validates `date` (real calendar day) and `count`
/// (> 0), confirms `task_type_id` exists, then inserts a new `entries` row with
/// `created_at` defaulted by the schema. Returns the new row's id. `notes` is stored
/// exactly as given, untrimmed.
pub(crate) fn add_tally(conn: &Connection, task_type_id: i64, date: &str, count: i64, notes: &str) -> Result<i64> {
    require_valid_date(date)?;
    require_positive_count(count)?;
    require_task_type_exists(conn, task_type_id)?;

    conn.execute(
        "INSERT INTO entries (task_type_id, date, count, notes) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![task_type_id, date, count, notes],
    )?;
    Ok(conn.last_insert_rowid())
}

/// The right-click removal (PLAN §1.1): in one transaction, finds the most recent
/// entry for `(task_type_id, date)` — `ORDER BY created_at DESC, id DESC LIMIT 1`,
/// so ties on `created_at` are broken by the highest `id` — and either decrements its
/// `count` by 1 (if `count > 1`) or deletes the row outright (if `count == 1`). If no
/// entry exists for that day, this is a silent no-op.
pub(crate) fn remove_most_recent(conn: &Connection, task_type_id: i64, date: &str) -> Result<RemoveOutcome> {
    let tx = conn.unchecked_transaction()?;

    let found: Option<(i64, i64)> = tx
        .query_row(
            "SELECT id, count FROM entries
             WHERE task_type_id = ?1 AND date = ?2
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
            rusqlite::params![task_type_id, date],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    let outcome = match found {
        None => RemoveOutcome::NoOp,
        Some((id, count)) if count > 1 => {
            tx.execute("UPDATE entries SET count = count - 1 WHERE id = ?1", [id])?;
            RemoveOutcome::Decremented
        }
        Some((id, _)) => {
            tx.execute("DELETE FROM entries WHERE id = ?1", [id])?;
            RemoveOutcome::Deleted
        }
    };

    tx.commit()?;
    Ok(outcome)
}

/// Remove up to `n` tallies for `(task_type_id, date)`, walking the day's entries newest
/// first (PLAN §1.1's ordering: `created_at DESC, id DESC`, the `id` breaking ties because
/// `created_at` has limited resolution). The quick-add popup's `-3`.
///
/// This spans **every** entry for the day, not just the newest one like
/// [`remove_most_recent`]: whether five tallies sit in one row of five or five rows of one is
/// an implementation detail the user never sees, so `-5` must not depend on it.
///
/// **A row carrying a note is never deleted.** It is decremented to a floor of 1 and then
/// skipped. A count is re-entered in seconds; the free text in `notes` is not, and quick add
/// dismisses itself immediately, so the user would never see what went. `notes_protected`
/// reports how many rows this spared so the caller can say so.
///
/// Removing more than the day holds removes what it holds. One transaction; `n < 1` is
/// [`Error::Invalid`].
pub(crate) fn remove_tallies(
    conn: &Connection,
    task_type_id: i64,
    date: &str,
    n: i64,
) -> Result<RemoveReport> {
    if n < 1 {
        return Err(Error::Invalid(format!("remove count must be at least 1, got {n}")));
    }
    require_valid_date(date)?;

    let tx = conn.unchecked_transaction()?;
    let mut stmt = tx.prepare(
        "SELECT id, count, notes FROM entries
         WHERE task_type_id = ?1 AND date = ?2
         ORDER BY created_at DESC, id DESC",
    )?;
    let rows: Vec<(i64, i64, String)> = stmt
        .query_map(rusqlite::params![task_type_id, date], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut report = RemoveReport::default();
    let mut remaining = n;
    for (id, count, notes) in rows {
        if remaining == 0 {
            break;
        }
        let has_note = !notes.trim().is_empty();
        // A noted row keeps its last tally, so at most `count - 1` is available from it.
        let available = if has_note { count - 1 } else { count };
        let take = available.min(remaining);

        if take > 0 {
            if take == count {
                tx.execute("DELETE FROM entries WHERE id = ?1", [id])?;
                report.rows_deleted += 1;
            } else {
                tx.execute("UPDATE entries SET count = count - ?1 WHERE id = ?2", [take, id])?;
            }
            report.removed += take;
            remaining -= take;
        }
        // Wanted more from this row and a note is what stopped us.
        if has_note && remaining > 0 {
            report.notes_protected += 1;
        }
    }

    tx.commit()?;
    Ok(report)
}

/// Updates every field of an existing entry (the day-log edit dialog). Validates
/// `date`/`count` as in [`add_tally`]; `entry_id` and `task_type_id` must both exist
/// or this returns [`Error::NotFound`].
pub(crate) fn edit_entry(
    conn: &Connection,
    entry_id: i64,
    task_type_id: i64,
    date: &str,
    count: i64,
    notes: &str,
) -> Result<()> {
    require_valid_date(date)?;
    require_positive_count(count)?;
    require_task_type_exists(conn, task_type_id)?;

    let rows = conn.execute(
        "UPDATE entries SET task_type_id = ?1, date = ?2, count = ?3, notes = ?4 WHERE id = ?5",
        rusqlite::params![task_type_id, date, count, notes, entry_id],
    )?;
    if rows == 0 {
        return Err(Error::NotFound(format!("entry {entry_id}")));
    }
    Ok(())
}

/// Deletes a single entry row (the day-log row delete). [`Error::NotFound`] if it
/// doesn't exist.
pub(crate) fn delete_entry(conn: &Connection, entry_id: i64) -> Result<()> {
    let rows = conn.execute("DELETE FROM entries WHERE id = ?1", [entry_id])?;
    if rows == 0 {
        return Err(Error::NotFound(format!("entry {entry_id}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{configure_connection, create_schema};

    /// Sets up an in-memory DB with the schema, one category, and one task type
    /// (id 1) to hang entries on.
    fn setup() -> Connection {
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
        conn
    }

    fn get_entry(conn: &Connection, id: i64) -> Option<(i64, String, i64, String)> {
        conn.query_row(
            "SELECT task_type_id, date, count, notes FROM entries WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .unwrap()
    }

    // -- remove_tallies (quick add's `-N`) --

    /// Adds an entry with an explicit `created_at` so the newest-first walk is deterministic
    /// rather than at the mercy of same-millisecond inserts.
    fn seed(conn: &Connection, date: &str, count: i64, notes: &str, ts: &str) -> i64 {
        conn.execute(
            "INSERT INTO entries (task_type_id, date, count, notes, created_at) \
             VALUES (1, ?1, ?2, ?3, ?4)",
            rusqlite::params![date, count, notes, ts],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn day_total(conn: &Connection, date: &str) -> i64 {
        conn.query_row(
            "SELECT COALESCE(SUM(count), 0) FROM entries WHERE date = ?1",
            [date],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn remove_tallies_spans_every_row_of_the_day() {
        let conn = setup();
        // 2 + 1 + 2 = 5 tallies across three rows; `-4` must not stop at the newest row.
        let a = seed(&conn, "2026-01-05", 2, "", "2026-01-05 09:00:00.000");
        let b = seed(&conn, "2026-01-05", 1, "", "2026-01-05 10:00:00.000");
        let c = seed(&conn, "2026-01-05", 2, "", "2026-01-05 11:00:00.000");

        let report = remove_tallies(&conn, 1, "2026-01-05", 4).unwrap();
        assert_eq!(report.removed, 4);
        assert_eq!(report.rows_deleted, 2, "newest two rows fully consumed");
        assert_eq!(day_total(&conn, "2026-01-05"), 1);
        // Newest first: c and b go, a is decremented to 1.
        assert!(get_entry(&conn, c).is_none());
        assert!(get_entry(&conn, b).is_none());
        assert_eq!(get_entry(&conn, a).unwrap().2, 1);
    }

    #[test]
    fn remove_tallies_never_empties_a_noted_row() {
        let conn = setup();
        let noted = seed(&conn, "2026-01-05", 3, "asked twice, same ticket", "2026-01-05 09:00:00.000");

        // Asking for all 3 takes 2 and leaves the row — and its note — standing.
        let report = remove_tallies(&conn, 1, "2026-01-05", 3).unwrap();
        assert_eq!(report.removed, 2);
        assert_eq!(report.rows_deleted, 0);
        assert_eq!(report.notes_protected, 1);
        let (_, _, count, notes) = get_entry(&conn, noted).unwrap();
        assert_eq!(count, 1);
        assert_eq!(notes, "asked twice, same ticket");
    }

    #[test]
    fn remove_tallies_takes_from_plain_rows_before_a_note_blocks() {
        let conn = setup();
        let noted = seed(&conn, "2026-01-05", 2, "context", "2026-01-05 09:00:00.000");
        let plain = seed(&conn, "2026-01-05", 2, "", "2026-01-05 10:00:00.000");

        // 4 present, 3 removable (the noted row keeps one).
        let report = remove_tallies(&conn, 1, "2026-01-05", 9).unwrap();
        assert_eq!(report.removed, 3);
        assert_eq!(report.rows_deleted, 1);
        assert_eq!(report.notes_protected, 1);
        assert!(get_entry(&conn, plain).is_none());
        assert_eq!(get_entry(&conn, noted).unwrap().2, 1);
        assert_eq!(day_total(&conn, "2026-01-05"), 1);
    }

    #[test]
    fn remove_tallies_clamps_and_is_a_no_op_on_an_empty_day() {
        let conn = setup();
        seed(&conn, "2026-01-05", 2, "", "2026-01-05 09:00:00.000");

        let report = remove_tallies(&conn, 1, "2026-01-05", 10).unwrap();
        assert_eq!(report.removed, 2);
        assert_eq!(day_total(&conn, "2026-01-05"), 0);

        let again = remove_tallies(&conn, 1, "2026-01-05", 3).unwrap();
        assert_eq!(again, RemoveReport::default(), "nothing left to take");
    }

    #[test]
    fn remove_tallies_leaves_other_days_and_types_alone() {
        let conn = setup();
        conn.execute("INSERT INTO task_types (category_id, name) VALUES (1, 'Lain')", []).unwrap();
        let other_day = seed(&conn, "2026-01-04", 5, "", "2026-01-04 09:00:00.000");
        conn.execute(
            "INSERT INTO entries (task_type_id, date, count, notes, created_at) \
             VALUES (2, '2026-01-05', 4, '', '2026-01-05 09:00:00.000')",
            [],
        )
        .unwrap();
        seed(&conn, "2026-01-05", 1, "", "2026-01-05 10:00:00.000");

        remove_tallies(&conn, 1, "2026-01-05", 99).unwrap();
        assert_eq!(get_entry(&conn, other_day).unwrap().2, 5);
        assert_eq!(day_total(&conn, "2026-01-05"), 4, "the other type's row is untouched");
    }

    #[test]
    fn remove_tallies_rejects_a_non_positive_count_and_a_bad_date() {
        let conn = setup();
        assert!(matches!(remove_tallies(&conn, 1, "2026-01-05", 0), Err(Error::Invalid(_))));
        assert!(matches!(remove_tallies(&conn, 1, "2026-01-05", -2), Err(Error::Invalid(_))));
        assert!(matches!(remove_tallies(&conn, 1, "2026-02-30", 1), Err(Error::Invalid(_))));
    }

    #[test]
    fn removable_matches_what_remove_tallies_will_take() {
        // The preview's number and the write's number come from the same rule.
        assert_eq!(removable([(2, false), (1, false), (2, false)]), 5);
        assert_eq!(removable([(3, true)]), 2);
        assert_eq!(removable([(2, true), (2, false)]), 3);
        assert_eq!(removable([(1, true)]), 0, "a noted row of 1 gives nothing");
        assert_eq!(removable([]), 0);

        let conn = setup();
        seed(&conn, "2026-01-05", 2, "note", "2026-01-05 09:00:00.000");
        seed(&conn, "2026-01-05", 2, "", "2026-01-05 10:00:00.000");
        let predicted = removable([(2, true), (2, false)]);
        let report = remove_tallies(&conn, 1, "2026-01-05", 100).unwrap();
        assert_eq!(report.removed, predicted);
    }

    // -- add_tally --

    #[test]
    fn add_tally_happy_path() {
        let conn = setup();
        let id = add_tally(&conn, 1, "2026-01-05", 3, "some notes").unwrap();
        let (task_type_id, date, count, notes) = get_entry(&conn, id).unwrap();
        assert_eq!(task_type_id, 1);
        assert_eq!(date, "2026-01-05");
        assert_eq!(count, 3);
        assert_eq!(notes, "some notes");
    }

    #[test]
    fn add_tally_rejects_invalid_date() {
        let conn = setup();
        let err = add_tally(&conn, 1, "2026-02-30", 1, "").unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err:?}");
    }

    #[test]
    fn add_tally_rejects_malformed_date() {
        let conn = setup();
        let err = add_tally(&conn, 1, "not-a-date", 1, "").unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err:?}");
    }

    #[test]
    fn add_tally_rejects_zero_count() {
        let conn = setup();
        let err = add_tally(&conn, 1, "2026-01-05", 0, "").unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err:?}");
    }

    #[test]
    fn add_tally_rejects_negative_count() {
        let conn = setup();
        let err = add_tally(&conn, 1, "2026-01-05", -1, "").unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err:?}");
    }

    #[test]
    fn add_tally_rejects_unknown_task_type() {
        let conn = setup();
        let err = add_tally(&conn, 999, "2026-01-05", 1, "").unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err:?}");
    }

    // -- remove_most_recent --

    #[test]
    fn remove_most_recent_decrements_when_count_above_one() {
        let conn = setup();
        let id = add_tally(&conn, 1, "2026-01-05", 3, "").unwrap();
        let outcome = remove_most_recent(&conn, 1, "2026-01-05").unwrap();
        assert_eq!(outcome, RemoveOutcome::Decremented);
        let (_, _, count, _) = get_entry(&conn, id).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn remove_most_recent_deletes_when_count_is_one() {
        let conn = setup();
        let id = add_tally(&conn, 1, "2026-01-05", 1, "").unwrap();
        let outcome = remove_most_recent(&conn, 1, "2026-01-05").unwrap();
        assert_eq!(outcome, RemoveOutcome::Deleted);
        assert!(get_entry(&conn, id).is_none());
    }

    #[test]
    fn remove_most_recent_is_noop_when_nothing_that_day() {
        let conn = setup();
        let outcome = remove_most_recent(&conn, 1, "2026-01-05").unwrap();
        assert_eq!(outcome, RemoveOutcome::NoOp);
    }

    #[test]
    fn remove_most_recent_respects_date_scoping() {
        let conn = setup();
        let other_day = add_tally(&conn, 1, "2026-01-04", 5, "other day").unwrap();
        add_tally(&conn, 1, "2026-01-05", 1, "today").unwrap();

        remove_most_recent(&conn, 1, "2026-01-05").unwrap();

        // The other day's entry is untouched.
        let (_, _, count, notes) = get_entry(&conn, other_day).unwrap();
        assert_eq!(count, 5);
        assert_eq!(notes, "other day");
    }

    /// The load-bearing tie-break test (PLAN §1.1): two entries on the same day with
    /// an identical explicit `created_at`, differing only by id. The one with the
    /// HIGHEST id must be the one touched, never an arbitrary one.
    #[test]
    fn remove_most_recent_breaks_created_at_ties_by_highest_id() {
        let conn = setup();
        // Insert explicitly so both rows share the same created_at literal.
        conn.execute(
            "INSERT INTO entries (id, task_type_id, date, count, notes, created_at)
             VALUES (1, 1, '2026-01-05', 5, 'lower-id', '2026-01-05 10:00:00.000')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO entries (id, task_type_id, date, count, notes, created_at)
             VALUES (2, 1, '2026-01-05', 9, 'higher-id', '2026-01-05 10:00:00.000')",
            [],
        )
        .unwrap();

        let outcome = remove_most_recent(&conn, 1, "2026-01-05").unwrap();
        assert_eq!(outcome, RemoveOutcome::Decremented);

        // The higher-id row (id 2) was decremented; the lower-id row is untouched.
        let (_, _, count_lower, _) = get_entry(&conn, 1).unwrap();
        let (_, _, count_higher, _) = get_entry(&conn, 2).unwrap();
        assert_eq!(count_lower, 5, "lower-id row must be untouched");
        assert_eq!(count_higher, 8, "higher-id row must be the one decremented");
    }

    // -- edit_entry --

    #[test]
    fn edit_entry_updates_fields() {
        let conn = setup();
        conn.execute(
            "INSERT INTO task_types (category_id, name) VALUES (1, 'Other')",
            [],
        )
        .unwrap();
        let id = add_tally(&conn, 1, "2026-01-05", 1, "orig").unwrap();

        edit_entry(&conn, id, 2, "2026-01-06", 4, "updated").unwrap();

        let (task_type_id, date, count, notes) = get_entry(&conn, id).unwrap();
        assert_eq!(task_type_id, 2);
        assert_eq!(date, "2026-01-06");
        assert_eq!(count, 4);
        assert_eq!(notes, "updated");
    }

    #[test]
    fn edit_entry_rejects_invalid_date() {
        let conn = setup();
        let id = add_tally(&conn, 1, "2026-01-05", 1, "").unwrap();
        let err = edit_entry(&conn, id, 1, "2026-02-30", 1, "").unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err:?}");
    }

    #[test]
    fn edit_entry_rejects_non_positive_count() {
        let conn = setup();
        let id = add_tally(&conn, 1, "2026-01-05", 1, "").unwrap();
        let err = edit_entry(&conn, id, 1, "2026-01-05", 0, "").unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err:?}");
    }

    #[test]
    fn edit_entry_rejects_unknown_task_type() {
        let conn = setup();
        let id = add_tally(&conn, 1, "2026-01-05", 1, "").unwrap();
        let err = edit_entry(&conn, id, 999, "2026-01-05", 1, "").unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err:?}");
    }

    #[test]
    fn edit_entry_rejects_unknown_entry_id() {
        let conn = setup();
        let err = edit_entry(&conn, 999, 1, "2026-01-05", 1, "").unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err:?}");
    }

    // -- delete_entry --

    #[test]
    fn delete_entry_removes_row() {
        let conn = setup();
        let id = add_tally(&conn, 1, "2026-01-05", 1, "").unwrap();
        delete_entry(&conn, id).unwrap();
        assert!(get_entry(&conn, id).is_none());
    }

    #[test]
    fn delete_entry_rejects_unknown_id() {
        let conn = setup();
        let err = delete_entry(&conn, 999).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err:?}");
    }
}
