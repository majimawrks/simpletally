//! DDL, schema creation, connection-pragma setup, and the drift-guard test.
//! See [`SCHEMA.md`](../../../notes/SCHEMA.md) §0–§5 — this module is the DDL with no
//! ellipses; SCHEMA.md and this file must always agree.

use crate::error::{Error, Result};
use rusqlite::Connection;

/// `categories` — SCHEMA §1.
pub const CREATE_CATEGORIES: &str = "
CREATE TABLE categories (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT    NOT NULL COLLATE NOCASE,
    sort_order  INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%f', 'now')),
    UNIQUE (name)
) STRICT;
";

/// `task_types` — SCHEMA §2.
pub const CREATE_TASK_TYPES: &str = "
CREATE TABLE task_types (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    category_id  INTEGER NOT NULL
                   REFERENCES categories(id) ON UPDATE CASCADE ON DELETE RESTRICT,
    name         TEXT    NOT NULL COLLATE NOCASE,
    description  TEXT    NOT NULL DEFAULT '',
    is_active    INTEGER NOT NULL DEFAULT 1 CHECK (is_active IN (0, 1)),
    created_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%f', 'now')),
    UNIQUE (category_id, name)
) STRICT;
";

/// `entries` + its two indexes — SCHEMA §3.
pub const CREATE_ENTRIES: &str = "
CREATE TABLE entries (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    task_type_id INTEGER NOT NULL
                   REFERENCES task_types(id) ON UPDATE CASCADE ON DELETE RESTRICT,
    date         TEXT    NOT NULL,
    count        INTEGER NOT NULL DEFAULT 1 CHECK (count > 0),
    notes        TEXT    NOT NULL DEFAULT '',
    created_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%f', 'now'))
) STRICT;
";
pub const CREATE_IDX_ENTRIES_TYPE_DATE: &str =
    "CREATE INDEX idx_entries_type_date ON entries(task_type_id, date);";
pub const CREATE_IDX_ENTRIES_DATE: &str = "CREATE INDEX idx_entries_date ON entries(date);";

/// `deletions` — SCHEMA §4.
pub const CREATE_DELETIONS: &str = "
CREATE TABLE deletions (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    deleted_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%f', 'now')),
    kind        TEXT    NOT NULL CHECK (kind IN ('task_type')),
    summary     TEXT    NOT NULL
) STRICT;
";

/// `deleted_types` (mirror of `task_types` + `deletion_id` + `category_name` snapshot) —
/// SCHEMA §4.
pub const CREATE_DELETED_TYPES: &str = "
CREATE TABLE deleted_types (
    id             INTEGER NOT NULL,
    category_id    INTEGER NOT NULL,
    category_name  TEXT    NOT NULL,
    name           TEXT    NOT NULL,
    description    TEXT    NOT NULL DEFAULT '',
    is_active      INTEGER NOT NULL DEFAULT 1,
    created_at     TEXT    NOT NULL,
    deletion_id    INTEGER NOT NULL
                     REFERENCES deletions(id) ON UPDATE CASCADE ON DELETE CASCADE
) STRICT;
";
pub const CREATE_IDX_DELETED_TYPES_DELETION: &str =
    "CREATE INDEX idx_deleted_types_deletion ON deleted_types(deletion_id);";

/// `deleted_entries` (mirror of `entries` + `deletion_id`) — SCHEMA §4.
pub const CREATE_DELETED_ENTRIES: &str = "
CREATE TABLE deleted_entries (
    id           INTEGER NOT NULL,
    task_type_id INTEGER NOT NULL,
    date         TEXT    NOT NULL,
    count        INTEGER NOT NULL,
    notes        TEXT    NOT NULL DEFAULT '',
    created_at   TEXT    NOT NULL,
    deletion_id  INTEGER NOT NULL
                   REFERENCES deletions(id) ON UPDATE CASCADE ON DELETE CASCADE
) STRICT;
";
pub const CREATE_IDX_DELETED_ENTRIES_DELETION: &str =
    "CREATE INDEX idx_deleted_entries_deletion ON deleted_entries(deletion_id);";

/// All DDL statements in FK-dependency order (SCHEMA §5), used by both [`create_schema`]
/// and the drift-guard test so there is exactly one source of truth for "the full schema".
pub const ALL_DDL: &[&str] = &[
    CREATE_CATEGORIES,
    CREATE_TASK_TYPES,
    CREATE_ENTRIES,
    CREATE_IDX_ENTRIES_TYPE_DATE,
    CREATE_IDX_ENTRIES_DATE,
    CREATE_DELETIONS,
    CREATE_DELETED_TYPES,
    CREATE_IDX_DELETED_TYPES_DELETION,
    CREATE_DELETED_ENTRIES,
    CREATE_IDX_DELETED_ENTRIES_DELETION,
];

/// The `user_version` stamped by a freshly created schema (SCHEMA §5 step 6, §6).
pub const CURRENT_USER_VERSION: i64 = 1;

/// Create the full v1 schema on a fresh connection: all tables and indexes in
/// FK-dependency order, inside one transaction, then stamp `PRAGMA user_version = 1`
/// (SCHEMA §5). No seed data.
pub(crate) fn create_schema(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    for ddl in ALL_DDL {
        tx.execute_batch(ddl)?;
    }
    tx.commit()?;
    // PRAGMA user_version cannot be parameterized and does not persist reliably when
    // issued inside some transaction contexts on all SQLite builds; issue it directly.
    conn.pragma_update(None, "user_version", CURRENT_USER_VERSION)?;
    Ok(())
}

/// Issue the per-connection pragmas from SCHEMA §0 and verify the values that actually
/// took effect, rather than assuming success. `foreign_keys` is always verified; WAL is
/// only verified for file-backed connections since an in-memory database reports
/// `journal_mode = 'memory'` regardless of what is requested.
pub(crate) fn configure_connection(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", "ON")?;
    let foreign_keys: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    if foreign_keys != 1 {
        return Err(Error::UnsupportedDatabase(format!(
            "foreign_keys did not enable (read back {foreign_keys})"
        )));
    }

    let journal_mode: String =
        conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    let is_memory = journal_mode.eq_ignore_ascii_case("memory");
    if !is_memory && !journal_mode.eq_ignore_ascii_case("wal") {
        return Err(Error::UnsupportedDatabase(format!(
            "journal_mode did not switch to wal (read back {journal_mode})"
        )));
    }

    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn table_names(conn: &Connection) -> BTreeSet<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
            .unwrap();
        stmt.query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    fn column_names(conn: &Connection, table: &str) -> BTreeSet<String> {
        let mut stmt = conn
            .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
            .unwrap();
        stmt.query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    #[test]
    fn create_schema_succeeds_and_stamps_version() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();

        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);

        let tables = table_names(&conn);
        for expected in [
            "categories",
            "task_types",
            "entries",
            "deletions",
            "deleted_types",
            "deleted_entries",
        ] {
            assert!(tables.contains(expected), "missing table {expected}");
        }
    }

    #[test]
    fn configure_connection_verifies_pragmas_in_memory() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        configure_connection(&conn).unwrap();

        let fk: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(fk, 1);
    }

    #[test]
    fn strict_rejects_non_integer_count() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        configure_connection(&conn).unwrap();
        conn.execute(
            "INSERT INTO categories (name) VALUES ('SAKTI')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO task_types (category_id, name) VALUES (1, 'Rekon')",
            [],
        )
        .unwrap();

        let err = conn
            .execute(
                "INSERT INTO entries (task_type_id, date, count) VALUES (1, '2026-01-01', 'not-a-number')",
                [],
            )
            .unwrap_err();
        assert!(format!("{err}").len() > 0, "{err:?}");
    }

    #[test]
    fn check_rejects_zero_and_negative_count() {
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

        assert!(conn
            .execute(
                "INSERT INTO entries (task_type_id, date, count) VALUES (1, '2026-01-01', 0)",
                [],
            )
            .is_err());
        assert!(conn
            .execute(
                "INSERT INTO entries (task_type_id, date, count) VALUES (1, '2026-01-01', -1)",
                [],
            )
            .is_err());
    }

    #[test]
    fn collate_nocase_uniqueness_on_categories() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        configure_connection(&conn).unwrap();
        conn.execute("INSERT INTO categories (name) VALUES ('SAKTI')", [])
            .unwrap();
        let err = conn
            .execute("INSERT INTO categories (name) VALUES ('sakti')", [])
            .unwrap_err();
        assert!(format!("{err}").len() > 0);
    }

    #[test]
    fn same_short_name_allowed_under_two_categories() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        configure_connection(&conn).unwrap();
        conn.execute("INSERT INTO categories (name) VALUES ('SAKTI')", [])
            .unwrap();
        conn.execute("INSERT INTO categories (name) VALUES ('DIGIPAY')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO task_types (category_id, name) VALUES (1, 'Rekon')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO task_types (category_id, name) VALUES (2, 'Rekon')",
            [],
        )
        .unwrap();
    }

    #[test]
    fn fk_restrict_blocks_deleting_task_type_with_entries() {
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
            "INSERT INTO entries (task_type_id, date, count) VALUES (1, '2026-01-01', 1)",
            [],
        )
        .unwrap();

        let err = conn
            .execute("DELETE FROM task_types WHERE id = 1", [])
            .unwrap_err();
        assert!(format!("{err}").len() > 0);
    }

    #[test]
    fn fk_restrict_blocks_deleting_category_with_task_types() {
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

        let err = conn
            .execute("DELETE FROM categories WHERE id = 1", [])
            .unwrap_err();
        assert!(format!("{err}").len() > 0);
    }

    #[test]
    fn fk_cascade_deleting_deletion_removes_mirror_children() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        configure_connection(&conn).unwrap();
        conn.execute(
            "INSERT INTO deletions (kind, summary) VALUES ('task_type', 'SAKTI - Rekon (1 entry)')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO deleted_types
                (id, category_id, category_name, name, description, is_active, created_at, deletion_id)
             VALUES (1, 1, 'SAKTI', 'Rekon', '', 1, '2026-01-01 00:00:00.000', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO deleted_entries
                (id, task_type_id, date, count, notes, created_at, deletion_id)
             VALUES (1, 1, '2026-01-01', 1, '', '2026-01-01 00:00:00.000', 1)",
            [],
        )
        .unwrap();

        conn.execute("DELETE FROM deletions WHERE id = 1", []).unwrap();

        let types_left: i64 = conn
            .query_row("SELECT COUNT(*) FROM deleted_types", [], |row| row.get(0))
            .unwrap();
        let entries_left: i64 = conn
            .query_row("SELECT COUNT(*) FROM deleted_entries", [], |row| row.get(0))
            .unwrap();
        assert_eq!(types_left, 0);
        assert_eq!(entries_left, 0);
    }

    /// Drift guard (SCHEMA §4, mandatory): the mirror tables must track their live
    /// counterparts column-for-column, modulo the documented extra columns. This must
    /// fail the build if a future edit changes one table but not its mirror.
    #[test]
    fn drift_guard_mirror_tables_match_live_tables() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();

        let task_types_cols = column_names(&conn, "task_types");
        let mut deleted_types_cols = column_names(&conn, "deleted_types");
        deleted_types_cols.remove("deletion_id");
        deleted_types_cols.remove("category_name");
        assert_eq!(
            deleted_types_cols, task_types_cols,
            "deleted_types drifted from task_types"
        );

        let entries_cols = column_names(&conn, "entries");
        let mut deleted_entries_cols = column_names(&conn, "deleted_entries");
        deleted_entries_cols.remove("deletion_id");
        assert_eq!(
            deleted_entries_cols, entries_cols,
            "deleted_entries drifted from entries"
        );
    }
}
