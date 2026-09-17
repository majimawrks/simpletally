//! Category + task-type CRUD (PLAN §6, SCHEMA §1/§2).
//!
//! Categories are real rows now: rename is a single `UPDATE`, and `sort_order` drives the
//! pill row on the Today screen (PLAN §6 "Categories get real management UI"). Task types
//! belong to exactly one category and carry their own active flag; deactivating a type keeps
//! its history but hides it from entry pickers (`list_task_types(active_only)`).
//!
//! Both tables enforce case-insensitive uniqueness in SQLite itself (`COLLATE NOCASE`,
//! SCHEMA §1/§2/§6) — the ops here just translate the resulting UNIQUE constraint failure
//! into [`Error::Duplicate`] via [`Error::is_unique_violation`] rather than re-implementing
//! the case fold in Rust.

use crate::error::{Error, Result};
use crate::model::{Category, TaskType};
use rusqlite::Connection;

/// Reject empty/whitespace-only names up front (`Invalid`) so callers get a clean error
/// instead of a UNIQUE/NOT NULL failure or a silently-stored blank name.
fn require_non_blank(name: &str) -> Result<()> {
    if name.trim().is_empty() {
        return Err(Error::Invalid("name must not be empty".to_string()));
    }
    Ok(())
}

/// Create a category (PLAN §6). `sort_order` is set to one past the current maximum (0 for
/// the first category), so new categories land at the end of the Today pill row. A
/// case-insensitive duplicate name maps SQLite's UNIQUE failure to [`Error::Duplicate`].
pub(crate) fn create_category(conn: &Connection, name: &str) -> Result<i64> {
    require_non_blank(name)?;

    let next_sort_order: i64 = conn.query_row(
        "SELECT COALESCE(MAX(sort_order) + 1, 0) FROM categories",
        [],
        |row| row.get(0),
    )?;

    let result = conn.execute(
        "INSERT INTO categories (name, sort_order) VALUES (?1, ?2)",
        (name, next_sort_order),
    );
    match result {
        Ok(_) => Ok(conn.last_insert_rowid()),
        Err(e) => {
            let e = Error::from(e);
            if e.is_unique_violation() {
                Err(Error::Duplicate(format!("category '{name}' already exists")))
            } else {
                Err(e)
            }
        }
    }
}

/// Rename a category (PLAN §6: "Renaming a category is a single `UPDATE` now that it is a
/// real row"). `NotFound` if `id` does not exist, `Invalid` for an empty name, `Duplicate`
/// if another category already has that name (case-insensitive).
pub(crate) fn rename_category(conn: &Connection, id: i64, new_name: &str) -> Result<()> {
    require_non_blank(new_name)?;

    let result = conn.execute(
        "UPDATE categories SET name = ?1 WHERE id = ?2",
        (new_name, id),
    );
    let rows = match result {
        Ok(rows) => rows,
        Err(e) => {
            let e = Error::from(e);
            return if e.is_unique_violation() {
                Err(Error::Duplicate(format!(
                    "category '{new_name}' already exists"
                )))
            } else {
                Err(e)
            };
        }
    };
    if rows == 0 {
        return Err(Error::NotFound(format!("category {id} not found")));
    }
    Ok(())
}

/// Set `sort_order` from position in `ordered_ids`, in one transaction (PLAN §6: drives the
/// Today pill row).
///
/// Edge-case decision: every id in `ordered_ids` must exist (`NotFound` otherwise), but the
/// slice is **not** required to cover every category — ids left out simply keep their current
/// `sort_order`. This lets a caller reorder a visible subset (e.g. drag-and-drop of two
/// adjacent pills) without having to first fetch and resend the full list. Positions are
/// assigned starting at 0 in slice order; duplicate ids in the slice are applied in order, so
/// the last occurrence wins.
pub(crate) fn reorder_categories(conn: &Connection, ordered_ids: &[i64]) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    for (position, id) in ordered_ids.iter().enumerate() {
        let rows = tx.execute(
            "UPDATE categories SET sort_order = ?1 WHERE id = ?2",
            (position as i64, id),
        )?;
        if rows == 0 {
            return Err(Error::NotFound(format!("category {id} not found")));
        }
    }
    tx.commit()?;
    Ok(())
}

/// Delete a category, only if it has no task types (PLAN §6: "A category with types cannot be
/// deleted; it offers to move them first"). The FK is `ON DELETE RESTRICT` (SCHEMA §2) as a
/// backstop, but this checks explicitly first so the caller gets a clean [`Error::Conflict`]
/// instead of a raw SQLite constraint error. `NotFound` if `id` does not exist.
pub(crate) fn delete_category(conn: &Connection, id: i64) -> Result<()> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM categories WHERE id = ?1)",
        [id],
        |row| row.get(0),
    )?;
    if !exists {
        return Err(Error::NotFound(format!("category {id} not found")));
    }

    let type_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM task_types WHERE category_id = ?1",
        [id],
        |row| row.get(0),
    )?;
    if type_count > 0 {
        return Err(Error::Conflict(format!(
            "category {id} has {type_count} task type(s); move or delete them first"
        )));
    }

    conn.execute("DELETE FROM categories WHERE id = ?1", [id])?;
    Ok(())
}

/// List all categories ordered by `sort_order, name` — the Today pill row order (PLAN §6).
pub(crate) fn list_categories(conn: &Connection) -> Result<Vec<Category>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, sort_order, created_at FROM categories ORDER BY sort_order, name",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(Category {
                id: row.get(0)?,
                name: row.get(1)?,
                sort_order: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Create a task type under `category_id` (PLAN §6). New types default `is_active = 1`
/// (SCHEMA §2). `NotFound` if the category does not exist, `Invalid` for an empty name,
/// `Duplicate` for a case-insensitive name collision within the same category — the same
/// name is allowed under a different category (SCHEMA §6).
pub(crate) fn create_task_type(
    conn: &Connection,
    category_id: i64,
    name: &str,
    description: &str,
) -> Result<i64> {
    require_non_blank(name)?;

    let category_exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM categories WHERE id = ?1)",
        [category_id],
        |row| row.get(0),
    )?;
    if !category_exists {
        return Err(Error::NotFound(format!("category {category_id} not found")));
    }

    let result = conn.execute(
        "INSERT INTO task_types (category_id, name, description) VALUES (?1, ?2, ?3)",
        (category_id, name, description),
    );
    match result {
        Ok(_) => Ok(conn.last_insert_rowid()),
        Err(e) => {
            let e = Error::from(e);
            if e.is_unique_violation() {
                Err(Error::Duplicate(format!(
                    "task type '{name}' already exists in category {category_id}"
                )))
            } else {
                Err(e)
            }
        }
    }
}

/// Rename / move / (de)activate a task type in one call (PLAN §6). `NotFound` if `id` or the
/// target `category_id` does not exist, `Invalid` for an empty name, `Duplicate` for a
/// case-insensitive `(category_id, name)` collision.
pub(crate) fn edit_task_type(
    conn: &Connection,
    id: i64,
    category_id: i64,
    name: &str,
    description: &str,
    is_active: bool,
) -> Result<()> {
    require_non_blank(name)?;

    let category_exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM categories WHERE id = ?1)",
        [category_id],
        |row| row.get(0),
    )?;
    if !category_exists {
        return Err(Error::NotFound(format!("category {category_id} not found")));
    }

    let result = conn.execute(
        "UPDATE task_types
         SET category_id = ?1, name = ?2, description = ?3, is_active = ?4
         WHERE id = ?5",
        (category_id, name, description, is_active, id),
    );
    let rows = match result {
        Ok(rows) => rows,
        Err(e) => {
            let e = Error::from(e);
            return if e.is_unique_violation() {
                Err(Error::Duplicate(format!(
                    "task type '{name}' already exists in category {category_id}"
                )))
            } else {
                Err(e)
            };
        }
    };
    if rows == 0 {
        return Err(Error::NotFound(format!("task type {id} not found")));
    }
    Ok(())
}

/// List task types, ordered by category `sort_order` then task-type `name` (PLAN §6).
/// `active_only` filters out `is_active = 0` rows (e.g. for entry pickers).
pub(crate) fn list_task_types(conn: &Connection, active_only: bool) -> Result<Vec<TaskType>> {
    let sql = "
        SELECT t.id, t.category_id, t.name, t.description, t.is_active, t.created_at
        FROM task_types t
        JOIN categories c ON c.id = t.category_id
        WHERE (?1 = 0 OR t.is_active = 1)
        ORDER BY c.sort_order, c.name, t.name
    ";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map([active_only as i64], |row| {
            Ok(TaskType {
                id: row.get(0)?,
                category_id: row.get(1)?,
                name: row.get(2)?,
                description: row.get(3)?,
                is_active: row.get::<_, i64>(4)? != 0,
                created_at: row.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{configure_connection, create_schema};

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        configure_connection(&conn).unwrap();
        conn
    }

    #[test]
    fn create_category_ok_and_duplicate_case_insensitive() {
        let conn = setup();
        let id1 = create_category(&conn, "SAKTI").unwrap();
        assert!(id1 > 0);

        let err = create_category(&conn, "sakti").unwrap_err();
        assert!(matches!(err, Error::Duplicate(_)), "{err:?}");
    }

    #[test]
    fn create_category_rejects_empty_name() {
        let conn = setup();
        let err = create_category(&conn, "   ").unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err:?}");
    }

    #[test]
    fn create_category_increments_sort_order() {
        let conn = setup();
        create_category(&conn, "SAKTI").unwrap();
        create_category(&conn, "DIGIPAY").unwrap();
        create_category(&conn, "OM SPAN").unwrap();

        let cats = list_categories(&conn).unwrap();
        assert_eq!(cats.len(), 3);
        assert_eq!(cats[0].sort_order, 0);
        assert_eq!(cats[1].sort_order, 1);
        assert_eq!(cats[2].sort_order, 2);
    }

    #[test]
    fn rename_category_ok() {
        let conn = setup();
        let id = create_category(&conn, "SAKTI").unwrap();
        rename_category(&conn, id, "SAKTI Web").unwrap();

        let cats = list_categories(&conn).unwrap();
        assert_eq!(cats[0].name, "SAKTI Web");
    }

    #[test]
    fn rename_category_to_existing_name_is_duplicate() {
        let conn = setup();
        create_category(&conn, "SAKTI").unwrap();
        let id2 = create_category(&conn, "DIGIPAY").unwrap();

        let err = rename_category(&conn, id2, "sakti").unwrap_err();
        assert!(matches!(err, Error::Duplicate(_)), "{err:?}");
    }

    #[test]
    fn rename_category_missing_id_is_not_found() {
        let conn = setup();
        let err = rename_category(&conn, 999, "New Name").unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err:?}");
    }

    #[test]
    fn reorder_categories_persists_and_list_reflects_it() {
        let conn = setup();
        let a = create_category(&conn, "A").unwrap();
        let b = create_category(&conn, "B").unwrap();
        let c = create_category(&conn, "C").unwrap();

        // Reverse the order.
        reorder_categories(&conn, &[c, b, a]).unwrap();

        let cats = list_categories(&conn).unwrap();
        assert_eq!(cats.iter().map(|c| c.id).collect::<Vec<_>>(), vec![c, b, a]);
    }

    #[test]
    fn reorder_categories_missing_id_is_not_found() {
        let conn = setup();
        create_category(&conn, "A").unwrap();
        let err = reorder_categories(&conn, &[999]).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err:?}");
    }

    #[test]
    fn delete_category_empty_ok() {
        let conn = setup();
        let id = create_category(&conn, "SAKTI").unwrap();
        delete_category(&conn, id).unwrap();
        assert!(list_categories(&conn).unwrap().is_empty());
    }

    #[test]
    fn delete_category_with_task_type_is_conflict() {
        let conn = setup();
        let id = create_category(&conn, "SAKTI").unwrap();
        create_task_type(&conn, id, "Rekon", "").unwrap();

        let err = delete_category(&conn, id).unwrap_err();
        assert!(matches!(err, Error::Conflict(_)), "{err:?}");
    }

    #[test]
    fn delete_category_missing_id_is_not_found() {
        let conn = setup();
        let err = delete_category(&conn, 999).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err:?}");
    }

    #[test]
    fn create_task_type_ok() {
        let conn = setup();
        let cat = create_category(&conn, "SAKTI").unwrap();
        let id = create_task_type(&conn, cat, "Rekon", "reconciliation").unwrap();
        assert!(id > 0);
    }

    #[test]
    fn create_task_type_duplicate_same_category_case_insensitive() {
        let conn = setup();
        let cat = create_category(&conn, "SAKTI").unwrap();
        create_task_type(&conn, cat, "Rekon", "").unwrap();

        let err = create_task_type(&conn, cat, "rekon", "").unwrap_err();
        assert!(matches!(err, Error::Duplicate(_)), "{err:?}");
    }

    #[test]
    fn create_task_type_same_name_two_categories_allowed() {
        let conn = setup();
        let cat1 = create_category(&conn, "SAKTI").unwrap();
        let cat2 = create_category(&conn, "DIGIPAY").unwrap();
        create_task_type(&conn, cat1, "Rekon", "").unwrap();
        create_task_type(&conn, cat2, "Rekon", "").unwrap();

        let types = list_task_types(&conn, false).unwrap();
        assert_eq!(types.len(), 2);
    }

    #[test]
    fn create_task_type_unknown_category_is_not_found() {
        let conn = setup();
        let err = create_task_type(&conn, 999, "Rekon", "").unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err:?}");
    }

    #[test]
    fn create_task_type_rejects_empty_name() {
        let conn = setup();
        let cat = create_category(&conn, "SAKTI").unwrap();
        let err = create_task_type(&conn, cat, "  ", "").unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err:?}");
    }

    #[test]
    fn edit_task_type_rename_move_deactivate() {
        let conn = setup();
        let cat1 = create_category(&conn, "SAKTI").unwrap();
        let cat2 = create_category(&conn, "DIGIPAY").unwrap();
        let id = create_task_type(&conn, cat1, "Rekon", "old").unwrap();

        edit_task_type(&conn, id, cat2, "Rekon Baru", "new", false).unwrap();

        let types = list_task_types(&conn, false).unwrap();
        let updated = types.iter().find(|t| t.id == id).unwrap();
        assert_eq!(updated.category_id, cat2);
        assert_eq!(updated.name, "Rekon Baru");
        assert_eq!(updated.description, "new");
        assert!(!updated.is_active);
    }

    #[test]
    fn edit_task_type_duplicate_on_rename_is_duplicate() {
        let conn = setup();
        let cat = create_category(&conn, "SAKTI").unwrap();
        create_task_type(&conn, cat, "Rekon", "").unwrap();
        let id2 = create_task_type(&conn, cat, "Setor", "").unwrap();

        let err = edit_task_type(&conn, id2, cat, "rekon", "", true).unwrap_err();
        assert!(matches!(err, Error::Duplicate(_)), "{err:?}");
    }

    #[test]
    fn edit_task_type_unknown_id_is_not_found() {
        let conn = setup();
        let cat = create_category(&conn, "SAKTI").unwrap();
        let err = edit_task_type(&conn, 999, cat, "Rekon", "", true).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err:?}");
    }

    #[test]
    fn list_task_types_active_only_filters_and_orders() {
        let conn = setup();
        let cat_b = create_category(&conn, "BBB").unwrap();
        let cat_a = create_category(&conn, "AAA").unwrap();
        // cat_b was created first, so it has the lower sort_order (0) despite the name.
        let t1 = create_task_type(&conn, cat_b, "Zed", "").unwrap();
        create_task_type(&conn, cat_a, "Alpha", "").unwrap();

        edit_task_type(&conn, t1, cat_b, "Zed", "", false).unwrap();

        let active = list_task_types(&conn, true).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].name, "Alpha");

        let all = list_task_types(&conn, false).unwrap();
        assert_eq!(all.len(), 2);
        // Ordered by category sort_order first: cat_b (sort_order 0) before cat_a (1).
        assert_eq!(all[0].category_id, cat_b);
        assert_eq!(all[1].category_id, cat_a);
    }
}
