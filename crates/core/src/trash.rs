//! Deleting a task type — three non-destructive outcomes plus the trash view (PLAN §4,
//! SCHEMA §4). Nothing is destroyed by default.
//!
//! - [`merge_task_type`] — move a type's entries onto another type, then drop the type.
//! - [`trash_task_type`] — move the type and its entries into the mirror tables as one
//!   restorable unit (the default delete).
//! - [`restore_from_trash`] — put a trashed unit back, recreating its category from the
//!   snapshot if needed and renaming on a live name clash (never attaching history to a
//!   live namesake).
//! - [`purge`] — permanent removal, reachable only from the trash view.
//! - [`list_trash`] — the trash view.
//!
//! Every mutation runs in one transaction. Deletes honor the live FKs' `ON DELETE RESTRICT`
//! by removing children before parents; a failure anywhere rolls the whole unit back.

use crate::error::{Error, Result};
use rusqlite::{Connection, OptionalExtension};

/// One row in the trash view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletionSummary {
    pub deletion_id: i64,
    pub deleted_at: String,
    pub kind: String,
    pub summary: String,
    pub type_count: i64,
    pub entry_count: i64,
}

/// What a restore did (PLAN §4 — restore reports what it put back and any rename).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreReport {
    pub restored_types: usize,
    pub restored_entries: usize,
    /// `(original name, name it was restored under)` for any type renamed to avoid a clash.
    pub renamed: Vec<(String, String)>,
}

/// Merge `from_id` into `into_id`: reassign every entry, then drop the emptied type. History
/// is preserved under the target (PLAN §4). One transaction.
pub(crate) fn merge_task_type(conn: &Connection, from_id: i64, into_id: i64) -> Result<()> {
    if from_id == into_id {
        return Err(Error::Conflict("cannot merge a task type into itself".into()));
    }
    if !type_exists(conn, from_id)? {
        return Err(Error::NotFound(format!("task type {from_id}")));
    }
    if !type_exists(conn, into_id)? {
        return Err(Error::NotFound(format!("task type {into_id}")));
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE entries SET task_type_id = ?1 WHERE task_type_id = ?2",
        [into_id, from_id],
    )?;
    tx.execute("DELETE FROM task_types WHERE id = ?1", [from_id])?;
    tx.commit()?;
    Ok(())
}

/// Move a task type and all its entries into the mirror tables as one restorable unit
/// (SCHEMA §4). Returns the new `deletion_id`. One transaction; children before parent.
pub(crate) fn trash_task_type(conn: &Connection, task_type_id: i64) -> Result<i64> {
    // Read the type + its category name (the snapshot) before touching anything.
    let row = conn
        .query_row(
            "SELECT t.category_id, c.name, t.name, t.description, t.is_active, t.created_at \
             FROM task_types t JOIN categories c ON c.id = t.category_id WHERE t.id = ?1",
            [task_type_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,    // category_id
                    r.get::<_, String>(1)?, // category_name (snapshot)
                    r.get::<_, String>(2)?, // name
                    r.get::<_, String>(3)?, // description
                    r.get::<_, i64>(4)?,    // is_active
                    r.get::<_, String>(5)?, // created_at
                ))
            },
        )
        .optional()?;
    let (category_id, category_name, name, description, is_active, created_at) =
        row.ok_or_else(|| Error::NotFound(format!("task type {task_type_id}")))?;

    let entry_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM entries WHERE task_type_id = ?1",
        [task_type_id],
        |r| r.get(0),
    )?;
    let summary = format!("{category_name} - {name} ({entry_count} entries)");

    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO deletions (kind, summary) VALUES ('task_type', ?1)",
        [&summary],
    )?;
    let deletion_id = tx.last_insert_rowid();

    tx.execute(
        "INSERT INTO deleted_types \
         (id, category_id, category_name, name, description, is_active, created_at, deletion_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            task_type_id,
            category_id,
            category_name,
            name,
            description,
            is_active,
            created_at,
            deletion_id
        ],
    )?;
    tx.execute(
        "INSERT INTO deleted_entries \
         (id, task_type_id, date, count, notes, created_at, deletion_id) \
         SELECT id, task_type_id, date, count, notes, created_at, ?1 \
         FROM entries WHERE task_type_id = ?2",
        [deletion_id, task_type_id],
    )?;

    // Children before parent (the live FK is ON DELETE RESTRICT).
    tx.execute("DELETE FROM entries WHERE task_type_id = ?1", [task_type_id])?;
    tx.execute("DELETE FROM task_types WHERE id = ?1", [task_type_id])?;

    tx.commit()?;
    Ok(deletion_id)
}

/// Restore a trashed unit (SCHEMA §4). Recreates the category from the snapshot name if it is
/// gone, renames a restored type when its `(category, name)` is taken (rather than failing on
/// the `UNIQUE` constraint), and re-wires entries onto the restored type by deletion grouping,
/// not by their stale original ids. One transaction.
pub(crate) fn restore_from_trash(conn: &Connection, deletion_id: i64) -> Result<RestoreReport> {
    if !deletion_exists(conn, deletion_id)? {
        return Err(Error::NotFound(format!("deletion {deletion_id}")));
    }

    let tx = conn.unchecked_transaction()?;

    // Read the trashed types for this unit.
    struct TrashedType {
        original_id: i64,
        original_category_id: i64,
        category_name: String,
        name: String,
        description: String,
        is_active: i64,
        created_at: String,
    }
    let trashed: Vec<TrashedType> = {
        let mut stmt = tx.prepare(
            "SELECT id, category_id, category_name, name, description, is_active, created_at \
             FROM deleted_types WHERE deletion_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([deletion_id], |r| {
            Ok(TrashedType {
                original_id: r.get(0)?,
                original_category_id: r.get(1)?,
                category_name: r.get(2)?,
                name: r.get(3)?,
                description: r.get(4)?,
                is_active: r.get(5)?,
                created_at: r.get(6)?,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };

    let mut renamed = Vec::new();
    // Map each original type id to the id it was restored under, to re-wire entries.
    let mut id_map: std::collections::BTreeMap<i64, i64> = std::collections::BTreeMap::new();

    for t in &trashed {
        let category_id = ensure_category(&tx, t.original_category_id, &t.category_name)?;
        let (final_name, was_renamed) = free_type_name(&tx, category_id, &t.name)?;
        if was_renamed {
            renamed.push((t.name.clone(), final_name.clone()));
        }
        tx.execute(
            "INSERT INTO task_types (category_id, name, description, is_active, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![category_id, final_name, t.description, t.is_active, t.created_at],
        )?;
        id_map.insert(t.original_id, tx.last_insert_rowid());
    }

    // Restore entries, re-wired onto the restored type ids.
    let mut restored_entries = 0usize;
    {
        let mut stmt = tx.prepare(
            "SELECT task_type_id, date, count, notes, created_at \
             FROM deleted_entries WHERE deletion_id = ?1",
        )?;
        let rows: Vec<(i64, String, i64, String, String)> = stmt
            .query_map([deletion_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        for (orig_type_id, date, count, notes, created_at) in rows {
            let new_type_id = id_map.get(&orig_type_id).copied().ok_or_else(|| {
                Error::Conflict(format!(
                    "trashed entry references type {orig_type_id} not in this deletion unit"
                ))
            })?;
            tx.execute(
                "INSERT INTO entries (task_type_id, date, count, notes, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![new_type_id, date, count, notes, created_at],
            )?;
            restored_entries += 1;
        }
    }

    // Remove the trash unit (cascades the mirror rows).
    tx.execute("DELETE FROM deletions WHERE id = ?1", [deletion_id])?;

    tx.commit()?;
    Ok(RestoreReport {
        restored_types: trashed.len(),
        restored_entries,
        renamed,
    })
}

/// Permanently destroy a trashed unit (PLAN §4 — reachable only from the trash view, behind a
/// loud confirmation the UI owns). Returns the number of entries destroyed, for that
/// confirmation. Cascades the mirror rows.
pub(crate) fn purge(conn: &Connection, deletion_id: i64) -> Result<i64> {
    let entry_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM deleted_entries WHERE deletion_id = ?1",
            [deletion_id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let affected = conn.execute("DELETE FROM deletions WHERE id = ?1", [deletion_id])?;
    if affected == 0 {
        return Err(Error::NotFound(format!("deletion {deletion_id}")));
    }
    Ok(entry_count)
}

/// The trash view: every deletion unit, newest first, with its type/entry counts (PLAN §4).
pub(crate) fn list_trash(conn: &Connection) -> Result<Vec<DeletionSummary>> {
    let mut stmt = conn.prepare(
        "SELECT d.id, d.deleted_at, d.kind, d.summary, \
                (SELECT COUNT(*) FROM deleted_types dt WHERE dt.deletion_id = d.id), \
                (SELECT COUNT(*) FROM deleted_entries de WHERE de.deletion_id = d.id) \
         FROM deletions d ORDER BY d.deleted_at DESC, d.id DESC",
    )?;
    let out = stmt
        .query_map([], |r| {
            Ok(DeletionSummary {
                deletion_id: r.get(0)?,
                deleted_at: r.get(1)?,
                kind: r.get(2)?,
                summary: r.get(3)?,
                type_count: r.get(4)?,
                entry_count: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(out)
}

// ---- helpers ----------------------------------------------------------------------------

fn type_exists(conn: &Connection, id: i64) -> Result<bool> {
    Ok(conn
        .query_row("SELECT 1 FROM task_types WHERE id = ?1", [id], |_| Ok(()))
        .optional()?
        .is_some())
}

fn deletion_exists(conn: &Connection, id: i64) -> Result<bool> {
    Ok(conn
        .query_row("SELECT 1 FROM deletions WHERE id = ?1", [id], |_| Ok(()))
        .optional()?
        .is_some())
}

/// Resolve the category a restored type should attach to (SCHEMA §4). Best-effort **exact
/// reattach** first: if the original category row still exists (ids are never reused —
/// `AUTOINCREMENT`), use it, so a category that was merely *renamed* while the type sat in
/// trash keeps the type rather than spawning a duplicate. Otherwise fall back to the snapshot
/// name (reuse a live category with that name, else recreate it).
fn ensure_category(
    tx: &rusqlite::Transaction,
    original_id: i64,
    snapshot_name: &str,
) -> Result<i64> {
    let original_exists = tx
        .query_row("SELECT 1 FROM categories WHERE id = ?1", [original_id], |_| Ok(()))
        .optional()?
        .is_some();
    if original_exists {
        return Ok(original_id);
    }
    ensure_category_by_name(tx, snapshot_name)
}

/// Find a live category by name (case-insensitive via the column's `COLLATE NOCASE`), or
/// create it with the next `sort_order`. Returns its id.
fn ensure_category_by_name(tx: &rusqlite::Transaction, name: &str) -> Result<i64> {
    if let Some(id) = tx
        .query_row("SELECT id FROM categories WHERE name = ?1", [name], |r| {
            r.get::<_, i64>(0)
        })
        .optional()?
    {
        return Ok(id);
    }
    let next_order: i64 =
        tx.query_row("SELECT COALESCE(MAX(sort_order) + 1, 0) FROM categories", [], |r| {
            r.get(0)
        })?;
    tx.execute(
        "INSERT INTO categories (name, sort_order) VALUES (?1, ?2)",
        rusqlite::params![name, next_order],
    )?;
    Ok(tx.last_insert_rowid())
}

/// Whether a `(category_id, name)` type name is taken (case-insensitive via the column).
fn type_name_taken(tx: &rusqlite::Transaction, category_id: i64, name: &str) -> Result<bool> {
    Ok(tx
        .query_row(
            "SELECT 1 FROM task_types WHERE category_id = ?1 AND name = ?2",
            rusqlite::params![category_id, name],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// The name to restore a type under: `base` if free, else `base (restored)`, `base
/// (restored 2)`, … Returns `(name, was_renamed)`.
fn free_type_name(
    tx: &rusqlite::Transaction,
    category_id: i64,
    base: &str,
) -> Result<(String, bool)> {
    if !type_name_taken(tx, category_id, base)? {
        return Ok((base.to_string(), false));
    }
    let mut n = 1;
    loop {
        let candidate = if n == 1 {
            format!("{base} (restored)")
        } else {
            format!("{base} (restored {n})")
        };
        if !type_name_taken(tx, category_id, &candidate)? {
            return Ok((candidate, true));
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{configure_connection, create_schema};

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        configure_connection(&conn).unwrap();
        conn
    }

    fn add_category(conn: &Connection, name: &str) -> i64 {
        conn.execute("INSERT INTO categories (name, sort_order) VALUES (?1, 0)", [name])
            .unwrap();
        conn.last_insert_rowid()
    }
    fn add_type(conn: &Connection, cat: i64, name: &str) -> i64 {
        conn.execute(
            "INSERT INTO task_types (category_id, name) VALUES (?1, ?2)",
            rusqlite::params![cat, name],
        )
        .unwrap();
        conn.last_insert_rowid()
    }
    fn add_entry(conn: &Connection, type_id: i64, date: &str, count: i64, notes: &str, ts: &str) {
        conn.execute(
            "INSERT INTO entries (task_type_id, date, count, notes, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![type_id, date, count, notes, ts],
        )
        .unwrap();
    }
    fn type_total(conn: &Connection, type_id: i64) -> i64 {
        conn.query_row(
            "SELECT COALESCE(SUM(count), 0) FROM entries WHERE task_type_id = ?1",
            [type_id],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn merge_moves_entries_and_drops_source() {
        let conn = db();
        let cat = add_category(&conn, "SAKTI");
        let a = add_type(&conn, cat, "Rekon");
        let b = add_type(&conn, cat, "Posting");
        add_entry(&conn, a, "2026-01-05", 3, "", "2026-01-05 09:00:00.000");
        add_entry(&conn, b, "2026-01-05", 2, "", "2026-01-05 09:01:00.000");

        merge_task_type(&conn, a, b).unwrap();

        assert!(!type_exists(&conn, a).unwrap());
        assert_eq!(type_total(&conn, b), 5);
    }

    #[test]
    fn merge_into_self_is_conflict() {
        let conn = db();
        let cat = add_category(&conn, "SAKTI");
        let a = add_type(&conn, cat, "Rekon");
        assert!(matches!(merge_task_type(&conn, a, a), Err(Error::Conflict(_))));
    }

    #[test]
    fn merge_unknown_is_not_found() {
        let conn = db();
        let cat = add_category(&conn, "SAKTI");
        let a = add_type(&conn, cat, "Rekon");
        assert!(matches!(merge_task_type(&conn, a, 999), Err(Error::NotFound(_))));
        assert!(matches!(merge_task_type(&conn, 999, a), Err(Error::NotFound(_))));
    }

    #[test]
    fn trash_moves_type_and_entries_to_mirrors() {
        let conn = db();
        let cat = add_category(&conn, "SAKTI");
        let t = add_type(&conn, cat, "Rekon");
        add_entry(&conn, t, "2026-01-05", 3, "note", "2026-01-05 09:00:00.000");
        add_entry(&conn, t, "2026-01-06", 2, "", "2026-01-06 09:00:00.000");

        let did = trash_task_type(&conn, t).unwrap();

        // Gone from live.
        assert!(!type_exists(&conn, t).unwrap());
        assert_eq!(type_total(&conn, t), 0);
        // Present in mirrors with the category-name snapshot.
        let snap: String = conn
            .query_row(
                "SELECT category_name FROM deleted_types WHERE deletion_id = ?1",
                [did],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(snap, "SAKTI");
        let mirror_entries: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM deleted_entries WHERE deletion_id = ?1",
                [did],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(mirror_entries, 2);
    }

    #[test]
    fn trash_then_restore_round_trips() {
        let conn = db();
        let cat = add_category(&conn, "SAKTI");
        let t = add_type(&conn, cat, "Rekon");
        add_entry(&conn, t, "2026-01-05", 3, "note", "2026-01-05 09:00:00.123");

        let did = trash_task_type(&conn, t).unwrap();
        let report = restore_from_trash(&conn, did).unwrap();
        assert_eq!(report.restored_types, 1);
        assert_eq!(report.restored_entries, 1);
        assert!(report.renamed.is_empty());

        // The type is back under SAKTI/Rekon with its entry intact (created_at preserved).
        let (cat_name, name, date, count, notes, ts): (String, String, String, i64, String, String) =
            conn.query_row(
                "SELECT c.name, t.name, e.date, e.count, e.notes, e.created_at \
                 FROM entries e JOIN task_types t ON t.id = e.task_type_id \
                 JOIN categories c ON c.id = t.category_id",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .unwrap();
        assert_eq!(
            (cat_name.as_str(), name.as_str(), date.as_str(), count, notes.as_str(), ts.as_str()),
            ("SAKTI", "Rekon", "2026-01-05", 3, "note", "2026-01-05 09:00:00.123")
        );
        // Trash is emptied.
        assert!(list_trash(&conn).unwrap().is_empty());
    }

    #[test]
    fn restore_renames_on_live_name_clash() {
        let conn = db();
        let cat = add_category(&conn, "SAKTI");
        let t = add_type(&conn, cat, "Rekon");
        add_entry(&conn, t, "2026-01-05", 1, "", "2026-01-05 09:00:00.000");
        let did = trash_task_type(&conn, t).unwrap();

        // The name is reused before restoring.
        let live = add_type(&conn, cat, "Rekon");

        let report = restore_from_trash(&conn, did).unwrap();
        assert_eq!(report.renamed, vec![("Rekon".to_string(), "Rekon (restored)".to_string())]);
        // Live namesake untouched; restored type is the renamed one and carries the entry.
        assert_eq!(type_total(&conn, live), 0);
        let restored_total: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(count),0) FROM entries e JOIN task_types t ON t.id=e.task_type_id \
                 WHERE t.name = 'Rekon (restored)'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(restored_total, 1);
    }

    #[test]
    fn restore_recreates_category_deleted_meanwhile() {
        let conn = db();
        let cat = add_category(&conn, "SAKTI");
        let t = add_type(&conn, cat, "Rekon");
        add_entry(&conn, t, "2026-01-05", 4, "", "2026-01-05 09:00:00.000");
        let did = trash_task_type(&conn, t).unwrap();

        // The now-empty category is deleted while the type sits in trash.
        conn.execute("DELETE FROM categories WHERE id = ?1", [cat]).unwrap();

        let report = restore_from_trash(&conn, did).unwrap();
        assert_eq!(report.restored_types, 1);
        // A category named SAKTI exists again and carries the restored type + entry.
        let total: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(e.count),0) FROM entries e \
                 JOIN task_types t ON t.id = e.task_type_id \
                 JOIN categories c ON c.id = t.category_id WHERE c.name = 'SAKTI'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(total, 4);
    }

    #[test]
    fn purge_destroys_unit_and_reports_count() {
        let conn = db();
        let cat = add_category(&conn, "SAKTI");
        let t = add_type(&conn, cat, "Rekon");
        add_entry(&conn, t, "2026-01-05", 1, "", "2026-01-05 09:00:00.000");
        add_entry(&conn, t, "2026-01-06", 1, "", "2026-01-06 09:00:00.000");
        let did = trash_task_type(&conn, t).unwrap();

        let destroyed = purge(&conn, did).unwrap();
        assert_eq!(destroyed, 2);
        assert!(list_trash(&conn).unwrap().is_empty());
        // Mirror rows are gone (cascade), and restore now fails.
        assert!(matches!(restore_from_trash(&conn, did), Err(Error::NotFound(_))));
    }

    #[test]
    fn purge_unknown_is_not_found() {
        let conn = db();
        assert!(matches!(purge(&conn, 999), Err(Error::NotFound(_))));
    }

    #[test]
    fn list_trash_reports_counts_newest_first() {
        let conn = db();
        let cat = add_category(&conn, "SAKTI");
        let t1 = add_type(&conn, cat, "Rekon");
        add_entry(&conn, t1, "2026-01-05", 1, "", "2026-01-05 09:00:00.000");
        let d1 = trash_task_type(&conn, t1).unwrap();

        let list = list_trash(&conn).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].deletion_id, d1);
        assert_eq!(list[0].type_count, 1);
        assert_eq!(list[0].entry_count, 1);
        assert_eq!(list[0].kind, "task_type");
    }

    #[test]
    fn trash_and_restore_type_with_no_entries() {
        let conn = db();
        let cat = add_category(&conn, "SAKTI");
        let t = add_type(&conn, cat, "Rekon");
        let did = trash_task_type(&conn, t).unwrap();
        assert!(!type_exists(&conn, t).unwrap());
        let report = restore_from_trash(&conn, did).unwrap();
        assert_eq!(report.restored_types, 1);
        assert_eq!(report.restored_entries, 0);
    }

    #[test]
    fn restore_twice_is_not_found() {
        let conn = db();
        let cat = add_category(&conn, "SAKTI");
        let t = add_type(&conn, cat, "Rekon");
        add_entry(&conn, t, "2026-01-05", 1, "", "2026-01-05 09:00:00.000");
        let did = trash_task_type(&conn, t).unwrap();
        restore_from_trash(&conn, did).unwrap();
        assert!(matches!(restore_from_trash(&conn, did), Err(Error::NotFound(_))));
    }

    #[test]
    fn restore_reattaches_to_renamed_category_by_id() {
        // SCHEMA §4: category_id is a best-effort exact reattach. If the category was merely
        // renamed (same id) while the type sat in trash, restore must reuse it, not create a
        // duplicate under the snapshot name.
        let conn = db();
        let cat = add_category(&conn, "SAKTI");
        let t = add_type(&conn, cat, "Rekon");
        add_entry(&conn, t, "2026-01-05", 1, "", "2026-01-05 09:00:00.000");
        let did = trash_task_type(&conn, t).unwrap();

        conn.execute("UPDATE categories SET name = 'SAKTI-NEW' WHERE id = ?1", [cat])
            .unwrap();
        restore_from_trash(&conn, did).unwrap();

        let cat_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM categories", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cat_count, 1, "should reattach to the existing id, not duplicate");
        let restored_cat_id: i64 = conn
            .query_row("SELECT category_id FROM task_types", [], |r| r.get(0))
            .unwrap();
        assert_eq!(restored_cat_id, cat);
    }

    /// SCHEMA §4 mandates a test that an injected failure mid-operation rolls back the whole
    /// unit. Here a `BEFORE DELETE` trigger aborts `trash_task_type` at its final step (after
    /// the mirror rows are written and the live entries deleted, in the same transaction).
    #[test]
    fn trash_rolls_back_on_mid_operation_failure() {
        let conn = db();
        let cat = add_category(&conn, "SAKTI");
        let t = add_type(&conn, cat, "Rekon");
        add_entry(&conn, t, "2026-01-05", 3, "note", "2026-01-05 09:00:00.000");

        conn.execute_batch(
            "CREATE TRIGGER boom BEFORE DELETE ON task_types \
             BEGIN SELECT RAISE(ABORT, 'boom'); END;",
        )
        .unwrap();
        assert!(trash_task_type(&conn, t).is_err());
        conn.execute_batch("DROP TRIGGER boom;").unwrap();

        // Everything is exactly as before: live type + entry intact, trash empty.
        assert!(type_exists(&conn, t).unwrap());
        assert_eq!(type_total(&conn, t), 3);
        for table in ["deletions", "deleted_types", "deleted_entries"] {
            let n: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "{table} should be empty after rollback");
        }
    }

    /// The restore counterpart: a corrupt trash unit (a deleted entry pointing at a type not
    /// in the unit) makes restore fail *after* the type row is inserted; the transaction must
    /// roll back, leaving no restored rows and the trash unit intact for inspection.
    #[test]
    fn restore_rolls_back_on_corrupt_unit() {
        let conn = db();
        let cat = add_category(&conn, "SAKTI");
        let t = add_type(&conn, cat, "Rekon");
        add_entry(&conn, t, "2026-01-05", 2, "", "2026-01-05 09:00:00.000");
        let did = trash_task_type(&conn, t).unwrap();

        conn.execute(
            "UPDATE deleted_entries SET task_type_id = 999999 WHERE deletion_id = ?1",
            [did],
        )
        .unwrap();

        assert!(matches!(restore_from_trash(&conn, did), Err(Error::Conflict(_))));

        // Nothing was restored (the inserted type was rolled back)...
        let live_types: i64 = conn
            .query_row("SELECT COUNT(*) FROM task_types", [], |r| r.get(0))
            .unwrap();
        let live_entries: i64 = conn
            .query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))
            .unwrap();
        assert_eq!((live_types, live_entries), (0, 0));
        // ...and the trash unit is still present.
        assert_eq!(list_trash(&conn).unwrap().len(), 1);
    }
}
