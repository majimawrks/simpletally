//! CSV task-types import glue (Phase 3B). `simpletally_core::csvio::import_task_types` only
//! dedups *within the file* — dedup against what's already in the DB, and creating categories
//! on demand, is this layer's job (service boundary rule: filesystem/DB-orchestration logic
//! lives here, not in core).

use simpletally_core::csvio::{ImportReport, TaskTypeImportRow};
use simpletally_core::{Category, Db, Error, Result};

/// The combined report shown to the user: the core crate's within-file numbers plus what this
/// layer did against the DB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CombinedReport {
    pub imported: usize,
    /// Rows that parsed fine but already existed in the DB (case-insensitive name match
    /// within the same category) — counted as skipped, not a failure.
    pub skipped_existing: usize,
    /// Duplicates the core parser already collapsed within the file itself.
    pub skipped_in_file: usize,
    pub skipped_malformed: usize,
    pub categories_created: usize,
    /// `"utf-8"` or `"windows-1252"`, carried over from the core report.
    pub encoding: String,
}

/// Import already-parsed rows into `db`. For each row: find its category case-insensitively
/// or create it, then create the task type. A DB-side duplicate (`Error::Duplicate`) is
/// counted as `skipped_existing` rather than failing the run; any other DB error aborts and
/// is returned (rows already written stay written — there is no whole-import transaction,
/// matching the create/create nature of the rest of the catalog API).
pub fn import_rows(
    db: &Db,
    rows: &[TaskTypeImportRow],
    core_report: &ImportReport,
) -> Result<CombinedReport> {
    let mut categories = db.list_categories()?;
    let mut imported = 0usize;
    let mut skipped_existing = 0usize;
    let mut categories_created = 0usize;

    for row in rows {
        let cat_id = match categories.iter().find(|c| c.name.eq_ignore_ascii_case(&row.category)) {
            Some(c) => c.id,
            None => {
                let id = db.create_category(&row.category)?;
                categories_created += 1;
                categories.push(Category {
                    id,
                    name: row.category.clone(),
                    sort_order: 0,
                    created_at: String::new(),
                });
                id
            }
        };

        match db.create_task_type(cat_id, &row.name, &row.description) {
            Ok(new_id) => {
                imported += 1;
                if !row.is_active {
                    db.edit_task_type(new_id, cat_id, &row.name, &row.description, false)?;
                }
            }
            Err(Error::Duplicate(_)) => skipped_existing += 1,
            Err(e) => return Err(e),
        }
    }

    Ok(CombinedReport {
        imported,
        skipped_existing,
        skipped_in_file: core_report.skipped_duplicate,
        skipped_malformed: core_report.skipped_malformed,
        categories_created,
        encoding: core_report.encoding.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use simpletally_core::csvio::import_task_types;

    fn rows_of(csv: &str) -> (Vec<TaskTypeImportRow>, ImportReport) {
        import_task_types(csv.as_bytes(), None).unwrap()
    }

    #[test]
    fn clean_import_creates_category_and_type() {
        let db = Db::open_in_memory().unwrap();
        let (rows, core_report) = rows_of("Category,Name,Description\nSAKTI,Rekon,reconciliation\n");

        let report = import_rows(&db, &rows, &core_report).unwrap();

        assert_eq!(report.imported, 1);
        assert_eq!(report.categories_created, 1);
        assert_eq!(report.skipped_existing, 0);

        let types = db.list_task_types(false).unwrap();
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].name, "Rekon");
    }

    #[test]
    fn row_already_in_db_is_skipped_existing() {
        let db = Db::open_in_memory().unwrap();
        let cat = db.create_category("SAKTI").unwrap();
        db.create_task_type(cat, "Rekon", "").unwrap();

        let (rows, core_report) = rows_of("Category,Name,Description\nSAKTI,Rekon,reconciliation\n");
        let report = import_rows(&db, &rows, &core_report).unwrap();

        assert_eq!(report.imported, 0);
        assert_eq!(report.skipped_existing, 1);
        assert_eq!(report.categories_created, 0);
        assert_eq!(db.list_task_types(false).unwrap().len(), 1);
    }

    #[test]
    fn category_is_created_when_missing() {
        let db = Db::open_in_memory().unwrap();
        let (rows, core_report) = rows_of("Category,Name,Description\nDIGIPAY,Setor,\n");

        let report = import_rows(&db, &rows, &core_report).unwrap();

        assert_eq!(report.categories_created, 1);
        let cats = db.list_categories().unwrap();
        assert_eq!(cats.len(), 1);
        assert_eq!(cats[0].name, "DIGIPAY");
    }

    #[test]
    fn category_match_is_case_insensitive_and_does_not_duplicate() {
        let db = Db::open_in_memory().unwrap();
        db.create_category("sakti").unwrap();

        let (rows, core_report) = rows_of("Category,Name,Description\nSAKTI,Rekon,\n");
        let report = import_rows(&db, &rows, &core_report).unwrap();

        assert_eq!(report.categories_created, 0);
        assert_eq!(db.list_categories().unwrap().len(), 1);
        assert_eq!(report.imported, 1);
    }

    #[test]
    fn inactive_row_creates_an_inactive_type() {
        let db = Db::open_in_memory().unwrap();
        let (rows, core_report) =
            rows_of("Category,Name,Description,Usage Count,Status\nSAKTI,Rekon,,0,Inactive\n");

        let report = import_rows(&db, &rows, &core_report).unwrap();
        assert_eq!(report.imported, 1);

        let types = db.list_task_types(false).unwrap();
        assert_eq!(types.len(), 1);
        assert!(!types[0].is_active);
    }
}
