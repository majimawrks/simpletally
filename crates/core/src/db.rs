//! `Db` — the single public handle to the data layer (PLAN §2, boundary rule 1).
//!
//! `Db` owns the `rusqlite::Connection`; no caller can obtain a bare `Connection` or run raw
//! SQL through this crate's public API. Every domain operation is exposed as a `&self` method
//! that delegates to the (now `pub(crate)`) module functions, unchanged in behavior.

use crate::aggregate::{
    DayLogRow, DayTypeTotal, RangeSummary, TypeLifetimeTotal, TypeRangeTotal, TypeUsage,
};
use crate::catalog;
use crate::dates::DateRange;
use crate::entries::{RemoveOutcome, RemoveReport};
use crate::error::{Error, Result};
use crate::model::{Category, TaskType};
use crate::trash::{DeletionSummary, RestoreReport};
use crate::{aggregate, entries, trash};
use chrono::NaiveDate;
use rusqlite::Connection;
use std::path::{Path, PathBuf};

/// What [`Db::open`] did to the file it opened.
pub enum OpenOutcome {
    /// The file was empty/new; a fresh v1 schema was created.
    Created,
    /// The file was already at the current schema; opened as-is.
    Opened,
    /// The file was a legacy v3.8 database; it was migrated to v1. Carries the report.
    Migrated(crate::migrate::MigrationReport),
}

/// Owns the connection; the only public handle to the data layer.
pub struct Db {
    conn: Connection,
}

impl Db {
    /// Open (or create) the database at `path`, classify it, and bring it to the current
    /// schema. Empty/new -> create schema. Current -> open as-is. Legacy v3.8 -> migrate
    /// (writing a pre-migration VACUUM INTO copy beside `path` first). Malformed/unsupported
    /// -> Err. Connection pragmas are applied on every non-error path.
    pub fn open(path: &Path) -> Result<(Db, OpenOutcome)> {
        let conn = Connection::open(path)?;
        let outcome = match crate::classify::classify(&conn)? {
            crate::classify::DbState::Empty => {
                crate::schema::create_schema(&conn)?;
                OpenOutcome::Created
            }
            crate::classify::DbState::Current => OpenOutcome::Opened,
            crate::classify::DbState::LegacyV38 => {
                let backup = premigration_path(path);
                let report = crate::migrate::migrate_v38(&conn, &backup)?;
                OpenOutcome::Migrated(report)
            }
            crate::classify::DbState::Unsupported(msg) => {
                return Err(Error::UnsupportedDatabase(msg));
            }
        };
        crate::schema::configure_connection(&conn)?;
        Ok((Db { conn }, outcome))
    }

    /// In-memory database with the current schema — for tools and tests.
    pub fn open_in_memory() -> Result<Db> {
        let conn = Connection::open_in_memory()?;
        crate::schema::create_schema(&conn)?;
        crate::schema::configure_connection(&conn)?;
        Ok(Db { conn })
    }

    /// VACUUM INTO a standalone copy at `dest` (used by the service layer's backup/restore).
    pub fn backup_to(&self, dest: &Path) -> Result<()> {
        let sql = format!("VACUUM INTO '{}'", dest.to_string_lossy().replace('\'', "''"));
        self.conn.execute_batch(&sql)?;
        Ok(())
    }

    /// Run `PRAGMA integrity_check` and `foreign_key_check`; return an error if either reports
    /// a problem. Used by the restore path to prove a staged candidate before it replaces the
    /// live database (PLAN §1.4 step 2).
    pub fn check_integrity(&self) -> Result<()> {
        let integrity: String =
            self.conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        if integrity != "ok" {
            return Err(Error::UnsupportedDatabase(format!(
                "integrity_check: {integrity}"
            )));
        }
        let fk_problems: i64 =
            self.conn.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r.get(0))?;
        if fk_problems > 0 {
            return Err(Error::UnsupportedDatabase(format!(
                "foreign_key_check found {fk_problems} problem(s)"
            )));
        }
        Ok(())
    }

    // ---- aggregates ----------------------------------------------------------------------

    pub fn day_type_totals(&self, date: &str) -> Result<Vec<DayTypeTotal>> {
        aggregate::day_type_totals(&self.conn, date)
    }

    pub fn day_total(&self, date: &str) -> Result<i64> {
        aggregate::day_total(&self.conn, date)
    }

    pub fn day_distinct_types(&self, date: &str) -> Result<i64> {
        aggregate::day_distinct_types(&self.conn, date)
    }

    pub fn day_log_rows(&self, date: &str) -> Result<Vec<DayLogRow>> {
        aggregate::day_log_rows(&self.conn, date)
    }

    pub fn range_summary(&self, range: DateRange) -> Result<RangeSummary> {
        aggregate::range_summary(&self.conn, range)
    }

    pub fn range_daily_totals(&self, range: DateRange) -> Result<Vec<(NaiveDate, i64)>> {
        aggregate::range_daily_totals(&self.conn, range)
    }

    pub fn range_type_totals_ranked(&self, range: DateRange) -> Result<Vec<TypeRangeTotal>> {
        aggregate::range_type_totals_ranked(&self.conn, range)
    }

    pub fn type_lifetime_totals(&self) -> Result<Vec<TypeLifetimeTotal>> {
        aggregate::type_lifetime_totals(&self.conn)
    }

    pub fn type_usage(&self, task_type_id: i64) -> Result<TypeUsage> {
        aggregate::type_usage(&self.conn, task_type_id)
    }

    // ---- entries ---------------------------------------------------------------------------

    pub fn add_tally(&self, task_type_id: i64, date: &str, count: i64, notes: &str) -> Result<i64> {
        entries::add_tally(&self.conn, task_type_id, date, count, notes)
    }

    pub fn remove_most_recent(&self, task_type_id: i64, date: &str) -> Result<RemoveOutcome> {
        entries::remove_most_recent(&self.conn, task_type_id, date)
    }

    /// Remove up to `n` tallies across the whole day (quick add's `-3`). Never empties a
    /// note-carrying row — see [`crate::entries::remove_tallies`].
    pub fn remove_tallies(&self, task_type_id: i64, date: &str, n: i64) -> Result<RemoveReport> {
        entries::remove_tallies(&self.conn, task_type_id, date, n)
    }

    /// Per task type, how many of `date`'s tallies a removal could take. The number the
    /// quick-add preview must show — do NOT derive this from [`Db::day_log_rows`], which is
    /// capped at five rows.
    pub fn removable_by_type(&self, date: &str) -> Result<std::collections::BTreeMap<i64, i64>> {
        entries::removable_by_type(&self.conn, date)
    }

    pub fn edit_entry(
        &self,
        entry_id: i64,
        task_type_id: i64,
        date: &str,
        count: i64,
        notes: &str,
    ) -> Result<()> {
        entries::edit_entry(&self.conn, entry_id, task_type_id, date, count, notes)
    }

    pub fn delete_entry(&self, entry_id: i64) -> Result<()> {
        entries::delete_entry(&self.conn, entry_id)
    }

    // ---- catalog ---------------------------------------------------------------------------

    pub fn create_category(&self, name: &str) -> Result<i64> {
        catalog::create_category(&self.conn, name)
    }

    pub fn rename_category(&self, id: i64, new_name: &str) -> Result<()> {
        catalog::rename_category(&self.conn, id, new_name)
    }

    pub fn reorder_categories(&self, ordered_ids: &[i64]) -> Result<()> {
        catalog::reorder_categories(&self.conn, ordered_ids)
    }

    pub fn delete_category(&self, id: i64) -> Result<()> {
        catalog::delete_category(&self.conn, id)
    }

    pub fn list_categories(&self) -> Result<Vec<Category>> {
        catalog::list_categories(&self.conn)
    }

    pub fn create_task_type(&self, category_id: i64, name: &str, description: &str) -> Result<i64> {
        catalog::create_task_type(&self.conn, category_id, name, description)
    }

    pub fn edit_task_type(
        &self,
        id: i64,
        category_id: i64,
        name: &str,
        description: &str,
        is_active: bool,
    ) -> Result<()> {
        catalog::edit_task_type(&self.conn, id, category_id, name, description, is_active)
    }

    pub fn list_task_types(&self, active_only: bool) -> Result<Vec<TaskType>> {
        catalog::list_task_types(&self.conn, active_only)
    }

    // ---- trash -----------------------------------------------------------------------------

    pub fn merge_task_type(&self, from_id: i64, into_id: i64) -> Result<()> {
        trash::merge_task_type(&self.conn, from_id, into_id)
    }

    pub fn trash_task_type(&self, task_type_id: i64) -> Result<i64> {
        trash::trash_task_type(&self.conn, task_type_id)
    }

    pub fn restore_from_trash(&self, deletion_id: i64) -> Result<RestoreReport> {
        trash::restore_from_trash(&self.conn, deletion_id)
    }

    pub fn purge(&self, deletion_id: i64) -> Result<i64> {
        trash::purge(&self.conn, deletion_id)
    }

    pub fn list_trash(&self) -> Result<Vec<DeletionSummary>> {
        trash::list_trash(&self.conn)
    }

    /// Purge the whole trash in one transaction; returns `(units, tallies)` destroyed.
    pub fn purge_all(&self) -> Result<(i64, i64)> {
        trash::purge_all(&self.conn)
    }

    /// The `YYYY-MM` months the trash holds tallies in — the months a purge would change.
    pub fn trash_months(&self) -> Result<Vec<String>> {
        trash::trash_months(&self.conn)
    }
}

fn premigration_path(path: &Path) -> PathBuf {
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs()).unwrap_or(0);
    let mut s = path.as_os_str().to_os_string();
    s.push(format!(".premigration_{ts}"));
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A unique temp file path, removed (plus -wal/-shm) on drop.
    struct TempDbPath(PathBuf);
    impl TempDbPath {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            TempDbPath(std::env::temp_dir().join(format!("st_db_test_{nanos}_{n}.db")))
        }
    }
    impl Drop for TempDbPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
            let _ = std::fs::remove_file(self.0.with_extension("db-wal"));
            let _ = std::fs::remove_file(self.0.with_extension("db-shm"));
        }
    }

    #[test]
    fn facade_round_trip_through_db_methods_only() {
        let db = Db::open_in_memory().unwrap();

        let cat_id = db.create_category("SAKTI").unwrap();
        let type_id = db.create_task_type(cat_id, "Rekon", "").unwrap();
        db.add_tally(type_id, "2026-01-05", 3, "").unwrap();

        assert_eq!(db.day_total("2026-01-05").unwrap(), 3);

        let types = db.list_task_types(false).unwrap();
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].id, type_id);

        let deletion_id = db.trash_task_type(type_id).unwrap();
        let trash = db.list_trash().unwrap();
        assert_eq!(trash.len(), 1);
        assert_eq!(trash[0].deletion_id, deletion_id);
        assert!(db.list_task_types(false).unwrap().is_empty());

        let report = db.restore_from_trash(deletion_id).unwrap();
        assert_eq!(report.restored_types, 1);
        assert_eq!(report.restored_entries, 1);
        let types = db.list_task_types(false).unwrap();
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].name, "Rekon");
    }

    #[test]
    fn open_on_fresh_path_creates_then_reopens_as_current() {
        let path = TempDbPath::new();

        let (db1, outcome1) = Db::open(&path.0).unwrap();
        assert!(matches!(outcome1, OpenOutcome::Created));
        // Usable immediately.
        db1.create_category("SAKTI").unwrap();
        drop(db1);

        let (db2, outcome2) = Db::open(&path.0).unwrap();
        assert!(matches!(outcome2, OpenOutcome::Opened));
        assert_eq!(db2.list_categories().unwrap().len(), 1);
    }
}
