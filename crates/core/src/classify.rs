//! Read-only bootstrap classifier (SCHEMA §6). Runs before any pragma that persists
//! state to the file, so it issues no writes — not even a pragma with a side effect.
//! Reading `user_version` is fine; it is inherently read-only.

use crate::error::Result;
use rusqlite::Connection;
use std::collections::BTreeSet;

/// The four mutually exclusive states a database file can be in on open (SCHEMA §6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbState {
    /// 0 tables, or none of `entries`/`task_types`/`tasks` present. Create schema §5.
    Empty,
    /// `tasks` and `task_types` present, no `entries`, `user_version = 0`. Back up and
    /// migrate (SCHEMA §7).
    LegacyV38,
    /// The full v1 set present, `user_version = 1`, no `tasks`. Open as-is.
    Current,
    /// Anything else — a half-migrated, malformed, or newer-than-supported file. The
    /// caller must refuse and leave the file untouched. Carries a human-readable reason.
    Unsupported(String),
}

const V1_TABLES: &[&str] = &[
    "categories",
    "task_types",
    "entries",
    "deletions",
    "deleted_types",
    "deleted_entries",
];

/// Classify a connection's database by table presence and `user_version`, per the
/// signature table in SCHEMA §6. Issues no writes.
pub(crate) fn classify(conn: &Connection) -> Result<DbState> {
    let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?;
    let tables: BTreeSet<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let user_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;

    let has_tasks = tables.contains("tasks");
    let has_task_types = tables.contains("task_types");
    let has_entries = tables.contains("entries");

    // Empty / brand-new: 0 tables, or none of entries/task_types/tasks present.
    if tables.is_empty() || (!has_entries && !has_task_types && !has_tasks) {
        return Ok(DbState::Empty);
    }

    // v3.8 legacy: tasks AND task_types present, NO entries, user_version = 0.
    if has_tasks && has_task_types && !has_entries && user_version == 0 {
        return Ok(DbState::LegacyV38);
    }

    // Current: the FULL v1 set present, user_version = 1, NO tasks.
    let has_full_v1_set = V1_TABLES.iter().all(|t| tables.contains(*t));
    if has_full_v1_set && !has_tasks && user_version == 1 {
        return Ok(DbState::Current);
    }

    // Anything else is unsupported: entries with user_version=0; tasks AND entries
    // together; a partial v1 set; user_version > 1.
    Ok(DbState::Unsupported(format!(
        "unrecognized database signature: tables={:?}, user_version={}",
        tables, user_version
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::create_schema;

    #[test]
    fn fresh_empty_db_is_empty() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(classify(&conn).unwrap(), DbState::Empty);
    }

    #[test]
    fn legacy_v38_fixture_is_legacy() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE task_types (
                id INTEGER PRIMARY KEY,
                name TEXT UNIQUE,
                description TEXT,
                is_active INTEGER,
                usage_count INTEGER,
                created_at TEXT
            );
            CREATE TABLE tasks (
                id INTEGER PRIMARY KEY,
                task_type TEXT,
                date TEXT,
                count INTEGER,
                notes TEXT,
                created_at TEXT
            );
            ",
        )
        .unwrap();

        assert_eq!(classify(&conn).unwrap(), DbState::LegacyV38);
    }

    #[test]
    fn after_create_schema_is_current() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        assert_eq!(classify(&conn).unwrap(), DbState::Current);
    }

    #[test]
    fn partial_malformed_db_is_unsupported() {
        let conn = Connection::open_in_memory().unwrap();
        // entries present with user_version 0 and no task_types/tasks: not Empty, not
        // Legacy, not the full Current set.
        conn.execute_batch(
            "CREATE TABLE entries (
                id INTEGER PRIMARY KEY,
                task_type_id INTEGER,
                date TEXT,
                count INTEGER,
                notes TEXT,
                created_at TEXT
            );",
        )
        .unwrap();

        match classify(&conn).unwrap() {
            DbState::Unsupported(_) => {}
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn newer_user_version_is_unsupported() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
        match classify(&conn).unwrap() {
            DbState::Unsupported(_) => {}
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn tasks_and_entries_together_is_unsupported() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        conn.execute_batch(
            "CREATE TABLE tasks (
                id INTEGER PRIMARY KEY,
                task_type TEXT,
                date TEXT,
                count INTEGER,
                notes TEXT,
                created_at TEXT
            );",
        )
        .unwrap();

        match classify(&conn).unwrap() {
            DbState::Unsupported(_) => {}
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }
}
