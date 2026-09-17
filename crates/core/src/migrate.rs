//! v3.8 → v1 migration (SCHEMA §7, PLAN §4). The load-bearing, data-loss-risk path.
//!
//! Contract, in order:
//! 1. `VACUUM INTO` a pre-migration copy — before any write (caller supplies the path).
//! 2. **Read-only preflight**: split every legacy `task_types.name` on the first `" - "`,
//!    ASCII-fold, and *abort with a report* if two distinct identities collide — the
//!    user's settled decision (abort, never silently merge). This covers legacy-vs-legacy
//!    collisions *and* an unmatched task whose orphan identity would clash with a migrated
//!    type. Also reject empty split components and invalid legacy `count`/`date` up front.
//! 3. **One transaction**: rename the legacy tables aside, build the v1 schema, copy
//!    categories/types/entries through an explicit `legacy name → new type id` map
//!    (preserving `created_at`), attach unmatched tasks to inactive Uncategorized types,
//!    reconcile, drop the legacy tables, stamp `user_version = 1`.
//!
//! **Validation is independent of the resolution logic it checks** (a Codex review found the
//! earlier accumulator was self-referential): [`reconcile`] recomputes the expected
//! `(identity, date, count, notes)` multiset from the *source* tables and compares it to
//! where entries *actually* landed via a join, so a resolution bug that mis-attributes an
//! entry produces a mismatch instead of two agreeing wrong numbers. Any failure rolls the
//! transaction back; the pre-migration copy is kept.

use crate::dates;
use crate::error::{Error, Result};
use crate::schema::{ALL_DDL, CURRENT_USER_VERSION};
use rusqlite::Connection;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const SEP: &str = " - ";
const UNCATEGORIZED: &str = "Uncategorized";

/// What the migration did, reported to the user (SCHEMA §7 step 7 / PLAN §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    pub categories_created: usize,
    /// Legacy `task_types` rows carried across (the matched types).
    pub types_migrated: usize,
    /// Inactive types created under `Uncategorized` for `tasks` rows whose `task_type`
    /// matched no legacy type — nothing is ever dropped (SCHEMA §7).
    pub types_unmatched: usize,
    pub entries_migrated: usize,
}

/// ASCII case-fold, matching SQLite `COLLATE NOCASE` (ASCII-only) so Rust and the DB agree
/// on identity (SCHEMA §1 note).
fn nocase(s: &str) -> String {
    s.to_ascii_lowercase()
}

/// The lookup key that ties a legacy `tasks.task_type` string to a legacy `task_types`
/// name: trimmed (so stray whitespace in hand-edited data doesn't fragment a type — Codex
/// #5) and ASCII-folded.
fn match_key(s: &str) -> String {
    nocase(s.trim())
}

/// Split a legacy `task_types.name` into `(category, name)` on the **first** `" - "`.
/// No separator → `("Uncategorized", whole string)`. Components are trimmed.
fn split_legacy(full: &str) -> (String, String) {
    match full.find(SEP) {
        Some(i) => (
            full[..i].trim().to_string(),
            full[i + SEP.len()..].trim().to_string(),
        ),
        None => (UNCATEGORIZED.to_string(), full.trim().to_string()),
    }
}

/// A legacy `task_types` row as read during preflight.
struct LegacyType {
    full_name: String,
    description: String,
    is_active: bool,
    category: String,
    name: String,
}

/// Migrate an open connection whose database [`crate::classify`] identified as
/// [`crate::classify::DbState::LegacyV38`]. `premigration_backup` is where the mandatory
/// pre-migration `VACUUM INTO` copy is written (the service layer chooses the path).
pub(crate) fn migrate_v38(conn: &Connection, premigration_backup: &Path) -> Result<MigrationReport> {
    // ---- 1. Pre-migration backup (must precede any write; VACUUM can't run in a tx). ----
    let backup_sql = format!(
        "VACUUM INTO '{}'",
        premigration_backup.to_string_lossy().replace('\'', "''")
    );
    conn.execute_batch(&backup_sql)?;

    // ---- 2. Read-only preflight. -------------------------------------------------------
    let legacy_types = read_legacy_types(conn)?;
    check_split_components(&legacy_types)?;
    check_collisions(&legacy_types)?;
    check_orphan_collisions(conn, &legacy_types)?;
    check_legacy_entries(conn)?;

    // ---- 3. Transform, reconcile, finalize — one transaction. --------------------------
    let tx = conn.unchecked_transaction()?;

    tx.execute_batch(
        "ALTER TABLE task_types RENAME TO _legacy_task_types;\
         ALTER TABLE tasks RENAME TO _legacy_tasks;",
    )?;
    for ddl in ALL_DDL {
        tx.execute_batch(ddl)?;
    }

    // Categories, in first-appearance order (deterministic: legacy task_types.id).
    let mut category_ids: BTreeMap<String, i64> = BTreeMap::new(); // nocase(cat) -> new id
    let mut categories_created = 0usize;
    let mut ensure_category = |tx: &rusqlite::Transaction, name: &str| -> Result<i64> {
        let key = nocase(name);
        if let Some(id) = category_ids.get(&key) {
            return Ok(*id);
        }
        let sort_order = category_ids.len() as i64;
        tx.execute(
            "INSERT INTO categories (name, sort_order) VALUES (?1, ?2)",
            rusqlite::params![name, sort_order],
        )?;
        let id = tx.last_insert_rowid();
        category_ids.insert(key, id);
        categories_created += 1;
        Ok(id)
    };

    // Matched types + the legacy-name -> new type id map (keyed by the trimmed/folded match
    // key, which the collision preflight guarantees is unique).
    let mut type_ids: BTreeMap<String, i64> = BTreeMap::new();
    for lt in &legacy_types {
        let cat_id = ensure_category(&tx, &lt.category)?;
        tx.execute(
            "INSERT INTO task_types (category_id, name, description, is_active) \
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![cat_id, lt.name, lt.description, lt.is_active as i64],
        )?;
        type_ids.insert(match_key(&lt.full_name), tx.last_insert_rowid());
    }
    let types_migrated = legacy_types.len();

    // Entries, resolved through the map. Unmatched task_type strings get an inactive type
    // under Uncategorized (deduped by match key) — nothing is dropped. The orphan preflight
    // has already ruled out an orphan clashing with a migrated identity.
    let mut orphan_ids: BTreeMap<String, i64> = BTreeMap::new(); // match_key(raw) -> new id
    let mut entries_migrated = 0usize;

    let mut stmt =
        tx.prepare("SELECT task_type, date, count, notes, created_at FROM _legacy_tasks")?;
    let rows: Vec<(String, String, i64, Option<String>, Option<String>)> = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    for (task_type, date, count, notes, created_at) in rows {
        let type_id = match type_ids.get(&match_key(&task_type)) {
            Some(id) => *id,
            None => {
                let key = match_key(&task_type);
                if let Some(id) = orphan_ids.get(&key) {
                    *id
                } else {
                    let cat_id = ensure_category(&tx, UNCATEGORIZED)?;
                    tx.execute(
                        "INSERT INTO task_types (category_id, name, description, is_active) \
                         VALUES (?1, ?2, '', 0)",
                        rusqlite::params![cat_id, task_type.trim()],
                    )?;
                    let id = tx.last_insert_rowid();
                    orphan_ids.insert(key, id);
                    id
                }
            }
        };

        // Preserve the legacy timestamp; fall back to the migration clock only when it is
        // genuinely absent (a hand-edited NULL) — SCHEMA §7.
        let created = created_at.filter(|s| !s.trim().is_empty());
        match created {
            Some(ts) => tx.execute(
                "INSERT INTO entries (task_type_id, date, count, notes, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![type_id, date, count, notes.unwrap_or_default(), ts],
            )?,
            None => tx.execute(
                "INSERT INTO entries (task_type_id, date, count, notes) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![type_id, date, count, notes.unwrap_or_default()],
            )?,
        };
        entries_migrated += 1;
    }
    let types_unmatched = orphan_ids.len();

    // ---- Independent validation (must catch mis-attribution, not just gross totals). ----
    reconcile(&tx)?;

    tx.execute_batch("DROP TABLE _legacy_tasks; DROP TABLE _legacy_task_types;")?;
    tx.pragma_update(None, "user_version", CURRENT_USER_VERSION)?;
    tx.commit()?;

    Ok(MigrationReport {
        categories_created,
        types_migrated,
        types_unmatched,
        entries_migrated,
    })
}

/// Read legacy `task_types`, computing each row's split target.
fn read_legacy_types(conn: &Connection) -> Result<Vec<LegacyType>> {
    let mut stmt = conn.prepare(
        "SELECT name, COALESCE(description, ''), COALESCE(is_active, 1) \
         FROM task_types ORDER BY id",
    )?;
    let out = stmt
        .query_map([], |r| {
            let full_name: String = r.get(0)?;
            let description: String = r.get(1)?;
            let is_active: i64 = r.get(2)?;
            let (category, name) = split_legacy(&full_name);
            Ok(LegacyType {
                full_name,
                description,
                is_active: is_active != 0,
                category,
                name,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(out)
}

/// Abort if any split produced an empty category or name (e.g. `" - x"`, `"x - "`).
fn check_split_components(types: &[LegacyType]) -> Result<()> {
    let bad: Vec<String> = types
        .iter()
        .filter(|t| t.category.is_empty() || t.name.is_empty())
        .map(|t| format!("'{}'", t.full_name))
        .collect();
    if bad.is_empty() {
        Ok(())
    } else {
        Err(Error::MigrationValidation(format!(
            "legacy type name(s) split into an empty category or name: {}",
            bad.join(", ")
        )))
    }
}

/// The `(nocase category, nocase name)` identity a legacy type maps to.
fn identity(t: &LegacyType) -> (String, String) {
    (nocase(&t.category), nocase(&t.name))
}

/// Abort with a report if two *distinct* legacy names collapse to the same identity — the
/// settled "abort, don't merge" decision (SCHEMA §7).
fn check_collisions(types: &[LegacyType]) -> Result<()> {
    let mut by_target: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for t in types {
        by_target.entry(identity(t)).or_default().push(t.full_name.clone());
    }
    let groups: Vec<String> = by_target
        .into_iter()
        .filter(|(_, names)| names.len() > 1)
        .map(|((cat, name), names)| format!("({cat}, {name}) <- [{}]", names.join(", ")))
        .collect();
    if groups.is_empty() {
        Ok(())
    } else {
        Err(Error::MigrationCollision(groups))
    }
}

/// Abort with a report if an *unmatched* `tasks.task_type` (one that will become an orphan
/// type under `Uncategorized`) would collide with a migrated type's identity — otherwise the
/// orphan `INSERT` would hit the `UNIQUE` constraint mid-transaction and surface as a raw SQL
/// error rather than the designed collision report (Codex #3). An orphan whose trimmed value
/// is empty is also rejected here.
fn check_orphan_collisions(conn: &Connection, legacy_types: &[LegacyType]) -> Result<()> {
    let legacy_match_keys: BTreeSet<String> =
        legacy_types.iter().map(|t| match_key(&t.full_name)).collect();
    let legacy_identities: BTreeSet<(String, String)> =
        legacy_types.iter().map(identity).collect();

    let mut stmt = conn.prepare("SELECT DISTINCT task_type FROM tasks")?;
    let raws: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut empties = Vec::new();
    let mut collisions = Vec::new();
    for raw in raws {
        if legacy_match_keys.contains(&match_key(&raw)) {
            continue; // matches a real type — not an orphan
        }
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            empties.push(format!("'{raw}'"));
            continue;
        }
        let orphan_ident = (nocase(UNCATEGORIZED), nocase(trimmed));
        if legacy_identities.contains(&orphan_ident) {
            collisions.push(format!(
                "orphan '{raw}' collides with migrated ({}, {})",
                orphan_ident.0, orphan_ident.1
            ));
        }
    }

    if !empties.is_empty() {
        return Err(Error::MigrationValidation(format!(
            "legacy task(s) have an empty task_type: {}",
            empties.join(", ")
        )));
    }
    if !collisions.is_empty() {
        return Err(Error::MigrationCollision(collisions));
    }
    Ok(())
}

/// Reject invalid legacy `tasks` data before writing anything (SCHEMA §7 preflight):
/// non-integer or non-positive counts, and non-calendar dates.
fn check_legacy_entries(conn: &Connection) -> Result<()> {
    let bad_counts: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE typeof(count) != 'integer' OR count <= 0",
        [],
        |r| r.get(0),
    )?;
    if bad_counts > 0 {
        return Err(Error::MigrationValidation(format!(
            "{bad_counts} legacy task row(s) have a non-positive or non-integer count"
        )));
    }

    let mut stmt = conn.prepare("SELECT DISTINCT date FROM tasks")?;
    let dates_iter = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut bad_dates = Vec::new();
    for d in dates_iter {
        let d = d?;
        if dates::parse_sql(&d).is_none() {
            bad_dates.push(d);
        }
    }
    if !bad_dates.is_empty() {
        return Err(Error::MigrationValidation(format!(
            "legacy task row(s) have invalid date(s): {}",
            bad_dates.join(", ")
        )));
    }
    Ok(())
}

/// A reconciliation row key: `(nocase category, nocase name, date, count, notes)`. Identity
/// is folded (uniqueness is case-insensitive); `date`/`count`/`notes` compare exactly.
/// `created_at` is deliberately excluded — its NULL→migration-clock fallback makes exact
/// multiset comparison impossible; verbatim preservation has its own dedicated test.
type ReconKey = (String, String, String, i64, String);

/// Independent post-transform validation, inside the transaction. Recomputes the expected
/// `(identity, date, count, notes)` multiset from the **source** tables (`_legacy_tasks`
/// resolved against `_legacy_task_types`) and compares it to the multiset that actually
/// landed (`entries` joined to its stored type/category). A resolution bug that put an entry
/// on the wrong type makes the "actual" identity differ from the "expected" one, so it is
/// caught — the check does not depend on the resolution logic it validates. Also verifies
/// row count, global total, `foreign_key_check` and `integrity_check`. Fails → rollback.
fn reconcile(tx: &rusqlite::Transaction) -> Result<()> {
    // Row count: every legacy task became exactly one entry.
    let legacy_count: i64 = tx.query_row("SELECT COUNT(*) FROM _legacy_tasks", [], |r| r.get(0))?;
    let new_count: i64 = tx.query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))?;
    if legacy_count != new_count {
        return Err(Error::MigrationValidation(format!(
            "row count mismatch: legacy={legacy_count}, new={new_count}"
        )));
    }

    // Global total (necessary but not sufficient — the multiset below is the real check).
    let legacy_total: i64 =
        tx.query_row("SELECT COALESCE(SUM(count), 0) FROM _legacy_tasks", [], |r| r.get(0))?;
    let new_total: i64 =
        tx.query_row("SELECT COALESCE(SUM(count), 0) FROM entries", [], |r| r.get(0))?;
    if legacy_total != new_total {
        return Err(Error::MigrationValidation(format!(
            "total count mismatch: legacy={legacy_total}, new={new_total}"
        )));
    }

    // Expected multiset, derived independently from the source tables.
    let mut legacy_ident: BTreeMap<String, (String, String)> = BTreeMap::new();
    {
        let mut stmt = tx.prepare("SELECT name FROM _legacy_task_types")?;
        let names: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?;
        for full in names {
            let (cat, name) = split_legacy(&full);
            legacy_ident.insert(match_key(&full), (cat, name));
        }
    }
    let mut expected: BTreeMap<ReconKey, i64> = BTreeMap::new();
    {
        let mut stmt =
            tx.prepare("SELECT task_type, date, count, notes FROM _legacy_tasks")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?;
        for row in rows {
            let (task_type, date, count, notes) = row?;
            let (cat, name) = legacy_ident.get(&match_key(&task_type)).cloned().unwrap_or_else(
                || (UNCATEGORIZED.to_string(), task_type.trim().to_string()),
            );
            let key = (nocase(&cat), nocase(&name), date, count, notes.unwrap_or_default());
            *expected.entry(key).or_insert(0) += 1;
        }
    }

    // Actual multiset, read from where entries really landed.
    let mut actual: BTreeMap<ReconKey, i64> = BTreeMap::new();
    {
        let mut stmt = tx.prepare(
            "SELECT c.name, t.name, e.date, e.count, e.notes \
             FROM entries e JOIN task_types t ON t.id = e.task_type_id \
             JOIN categories c ON c.id = t.category_id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        for row in rows {
            let (cat, name, date, count, notes) = row?;
            let key = (nocase(&cat), nocase(&name), date, count, notes);
            *actual.entry(key).or_insert(0) += 1;
        }
    }

    if expected != actual {
        return Err(Error::MigrationValidation(
            "row-level reconciliation mismatch: an entry landed on the wrong type, date, \
             count or note relative to the legacy source"
                .to_string(),
        ));
    }

    // Referential + structural integrity.
    let fk_problems: i64 =
        tx.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r.get(0))?;
    if fk_problems > 0 {
        return Err(Error::MigrationValidation(format!(
            "foreign_key_check found {fk_problems} problem(s)"
        )));
    }
    let integrity: String = tx.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if integrity != "ok" {
        return Err(Error::MigrationValidation(format!(
            "integrity_check failed: {integrity}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::{classify, DbState};
    use rusqlite::Connection;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A unique temp path for the pre-migration `VACUUM INTO` copy, removed on drop.
    struct TempPath(std::path::PathBuf);
    impl TempPath {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            TempPath(std::env::temp_dir().join(format!("st_migrate_test_{nanos}_{n}.db")))
        }
    }
    impl Drop for TempPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
            let _ = std::fs::remove_file(self.0.with_extension("db-wal"));
            let _ = std::fs::remove_file(self.0.with_extension("db-shm"));
        }
    }

    /// Build an in-memory v3.8-shaped database. `types` is `(name, description, is_active)`;
    /// `tasks` is `(task_type, date, count, notes, created_at)` — `count` is written as a
    /// literal so a non-integer can be planted; empty `created_at` becomes NULL.
    fn legacy_db(types: &[(&str, &str, i64)], tasks: &[(&str, &str, &str, &str, &str)]) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE task_types (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT UNIQUE NOT NULL,
                description TEXT,
                is_active INTEGER DEFAULT 1,
                usage_count INTEGER DEFAULT 0,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
             );
             CREATE TABLE tasks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                task_type TEXT NOT NULL,
                date TEXT NOT NULL,
                count INTEGER DEFAULT 1,
                notes TEXT,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
             );",
        )
        .unwrap();
        for (name, desc, active) in types {
            conn.execute(
                "INSERT INTO task_types (name, description, is_active) VALUES (?1, ?2, ?3)",
                rusqlite::params![name, desc, active],
            )
            .unwrap();
        }
        for (tt, date, count, notes, created) in tasks {
            let created_sql = if created.is_empty() {
                "NULL".to_string()
            } else {
                format!("'{created}'")
            };
            conn.execute_batch(&format!(
                "INSERT INTO tasks (task_type, date, count, notes, created_at) \
                 VALUES ('{}', '{}', {}, '{}', {})",
                tt.replace('\'', "''"),
                date,
                count,
                notes.replace('\'', "''"),
                created_sql
            ))
            .unwrap();
        }
        conn
    }

    fn per_type_totals(conn: &Connection) -> Vec<(String, String, i64)> {
        let mut stmt = conn
            .prepare(
                "SELECT c.name, t.name, SUM(e.count) \
                 FROM entries e JOIN task_types t ON t.id = e.task_type_id \
                 JOIN categories c ON c.id = t.category_id \
                 GROUP BY t.id ORDER BY c.name, t.name",
            )
            .unwrap();
        stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    }

    #[test]
    fn clean_split_migrates_categories_types_entries() {
        let conn = legacy_db(
            &[
                ("SAKTI - Rekon", "reconcile", 1),
                ("SAKTI - Posting", "", 1),
                ("DIGIPAY - Bayar", "", 1),
            ],
            &[
                ("SAKTI - Rekon", "2026-01-05", "3", "", "2026-01-05 09:00:00.100"),
                ("SAKTI - Rekon", "2026-01-05", "2", "note", "2026-01-05 09:05:00.200"),
                ("SAKTI - Posting", "2026-01-06", "1", "", "2026-01-06 10:00:00.000"),
                ("DIGIPAY - Bayar", "2026-01-07", "4", "", "2026-01-07 11:00:00.000"),
            ],
        );
        assert_eq!(classify(&conn).unwrap(), DbState::LegacyV38);
        let backup = TempPath::new();

        let report = migrate_v38(&conn, &backup.0).unwrap();
        assert_eq!(report.categories_created, 2);
        assert_eq!(report.types_migrated, 3);
        assert_eq!(report.types_unmatched, 0);
        assert_eq!(report.entries_migrated, 4);

        assert_eq!(
            per_type_totals(&conn),
            vec![
                ("DIGIPAY".into(), "Bayar".into(), 4),
                ("SAKTI".into(), "Posting".into(), 1),
                ("SAKTI".into(), "Rekon".into(), 5),
            ]
        );
        assert_eq!(classify(&conn).unwrap(), DbState::Current);
        assert!(backup.0.exists(), "pre-migration backup was not written");
    }

    #[test]
    fn no_separator_goes_to_uncategorized() {
        let conn = legacy_db(
            &[("Standalone", "", 1)],
            &[("Standalone", "2026-02-01", "2", "", "2026-02-01 08:00:00.000")],
        );
        let backup = TempPath::new();
        let report = migrate_v38(&conn, &backup.0).unwrap();
        assert_eq!(report.categories_created, 1);
        assert_eq!(per_type_totals(&conn), vec![("Uncategorized".into(), "Standalone".into(), 2)]);
    }

    #[test]
    fn unmatched_task_type_becomes_inactive_orphan_nothing_dropped() {
        let conn = legacy_db(
            &[("SAKTI - Rekon", "", 1)],
            &[
                ("SAKTI - Rekon", "2026-01-05", "1", "", "2026-01-05 09:00:00.000"),
                ("Ghost Type", "2026-01-05", "7", "orphan", "2026-01-05 09:00:00.000"),
            ],
        );
        let backup = TempPath::new();
        let report = migrate_v38(&conn, &backup.0).unwrap();
        assert_eq!(report.types_unmatched, 1);
        assert_eq!(report.entries_migrated, 2);

        let (cat, name, active, cnt): (String, String, i64, i64) = conn
            .query_row(
                "SELECT c.name, t.name, t.is_active, SUM(e.count) \
                 FROM entries e JOIN task_types t ON t.id = e.task_type_id \
                 JOIN categories c ON c.id = t.category_id \
                 WHERE t.name = 'Ghost Type' GROUP BY t.id",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!((cat.as_str(), name.as_str(), active, cnt), ("Uncategorized", "Ghost Type", 0, 7));
    }

    #[test]
    fn stray_whitespace_still_matches_real_type() {
        // Legacy type name has no stray space; a task references it with a trailing space.
        // The trimmed match key must still tie the task to the real type (Codex #5), not
        // fragment it into an orphan.
        let conn = legacy_db(
            &[("SAKTI - Rekon", "", 1)],
            &[("SAKTI - Rekon ", "2026-01-05", "3", "", "2026-01-05 09:00:00.000")],
        );
        let backup = TempPath::new();
        let report = migrate_v38(&conn, &backup.0).unwrap();
        assert_eq!(report.types_unmatched, 0, "trailing space should not create an orphan");
        assert_eq!(per_type_totals(&conn), vec![("SAKTI".into(), "Rekon".into(), 3)]);
    }

    #[test]
    fn created_at_is_preserved_verbatim() {
        let conn = legacy_db(
            &[("SAKTI - Rekon", "", 1)],
            &[("SAKTI - Rekon", "2020-05-01", "1", "", "2020-05-01 08:30:15.123")],
        );
        let backup = TempPath::new();
        migrate_v38(&conn, &backup.0).unwrap();
        let ts: String = conn
            .query_row("SELECT created_at FROM entries", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ts, "2020-05-01 08:30:15.123");
    }

    #[test]
    fn collision_aborts_with_report_and_leaves_db_untouched() {
        let conn = legacy_db(
            &[("Uncategorized - Rekon", "", 1), ("Rekon", "", 1)],
            &[("Rekon", "2026-01-05", "1", "", "2026-01-05 09:00:00.000")],
        );
        let backup = TempPath::new();
        match migrate_v38(&conn, &backup.0).unwrap_err() {
            Error::MigrationCollision(groups) => assert!(!groups.is_empty()),
            other => panic!("expected MigrationCollision, got {other:?}"),
        }
        assert_eq!(classify(&conn).unwrap(), DbState::LegacyV38);
        let has_entries: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='entries'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_entries, 0);
    }

    #[test]
    fn case_variant_collision_aborts() {
        let conn = legacy_db(&[("SAKTI - Rekon", "", 1), ("SAKTI - rekon", "", 1)], &[]);
        let backup = TempPath::new();
        assert!(matches!(
            migrate_v38(&conn, &backup.0),
            Err(Error::MigrationCollision(_))
        ));
    }

    #[test]
    fn orphan_colliding_with_migrated_type_aborts_cleanly() {
        // 'Uncategorized - Foo' migrates to (Uncategorized, Foo). A task 'Foo' (no
        // separator) is unmatched and would orphan to the same identity. Preflight must
        // report a clean collision, not let the UNIQUE insert blow up mid-transaction.
        let conn = legacy_db(
            &[("Uncategorized - Foo", "", 1)],
            &[("Foo", "2026-01-05", "1", "", "2026-01-05 09:00:00.000")],
        );
        let backup = TempPath::new();
        match migrate_v38(&conn, &backup.0).unwrap_err() {
            Error::MigrationCollision(groups) => assert!(!groups.is_empty()),
            other => panic!("expected MigrationCollision, got {other:?}"),
        }
        assert_eq!(classify(&conn).unwrap(), DbState::LegacyV38);
    }

    #[test]
    fn empty_task_type_aborts() {
        let conn = legacy_db(
            &[("SAKTI - Rekon", "", 1)],
            &[("   ", "2026-01-05", "1", "", "2026-01-05 09:00:00.000")],
        );
        let backup = TempPath::new();
        assert!(matches!(
            migrate_v38(&conn, &backup.0),
            Err(Error::MigrationValidation(_))
        ));
    }

    #[test]
    fn empty_split_component_aborts() {
        let conn = legacy_db(&[("SAKTI - ", "", 1)], &[]);
        let backup = TempPath::new();
        assert!(matches!(
            migrate_v38(&conn, &backup.0),
            Err(Error::MigrationValidation(_))
        ));
    }

    #[test]
    fn non_positive_count_aborts() {
        let conn = legacy_db(
            &[("SAKTI - Rekon", "", 1)],
            &[("SAKTI - Rekon", "2026-01-05", "0", "", "2026-01-05 09:00:00.000")],
        );
        let backup = TempPath::new();
        assert!(matches!(
            migrate_v38(&conn, &backup.0),
            Err(Error::MigrationValidation(_))
        ));
    }

    #[test]
    fn non_integer_count_aborts() {
        let conn = legacy_db(
            &[("SAKTI - Rekon", "", 1)],
            &[("SAKTI - Rekon", "2026-01-05", "'lots'", "", "2026-01-05 09:00:00.000")],
        );
        let backup = TempPath::new();
        assert!(matches!(
            migrate_v38(&conn, &backup.0),
            Err(Error::MigrationValidation(_))
        ));
    }

    #[test]
    fn invalid_date_aborts() {
        let conn = legacy_db(
            &[("SAKTI - Rekon", "", 1)],
            &[("SAKTI - Rekon", "2025-02-30", "1", "", "2025-02-30 09:00:00.000")],
        );
        let backup = TempPath::new();
        assert!(matches!(
            migrate_v38(&conn, &backup.0),
            Err(Error::MigrationValidation(_))
        ));
    }

    #[test]
    fn backup_is_a_valid_legacy_copy() {
        let conn = legacy_db(
            &[("SAKTI - Rekon", "", 1)],
            &[("SAKTI - Rekon", "2026-01-05", "1", "", "2026-01-05 09:00:00.000")],
        );
        let backup = TempPath::new();
        migrate_v38(&conn, &backup.0).unwrap();
        let restored = Connection::open(&backup.0).unwrap();
        assert_eq!(classify(&restored).unwrap(), DbState::LegacyV38);
    }

    // ---- Independent-validation teeth: reconcile() must catch mis-attribution. ----------

    /// Build a post-transform state by hand: the v1 schema plus the renamed legacy source
    /// tables, populated so we control exactly how entries were attributed.
    fn recon_fixture(
        wrong_attribution: bool,
    ) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        for ddl in ALL_DDL {
            conn.execute_batch(ddl).unwrap();
        }
        // Source tables as migration would have renamed them.
        conn.execute_batch(
            "CREATE TABLE _legacy_task_types (id INTEGER PRIMARY KEY, name TEXT, description TEXT, is_active INTEGER, usage_count INTEGER, created_at TEXT);
             CREATE TABLE _legacy_tasks (id INTEGER PRIMARY KEY, task_type TEXT, date TEXT, count INTEGER, notes TEXT, created_at TEXT);
             INSERT INTO _legacy_task_types (name) VALUES ('SAKTI - Rekon'), ('SAKTI - Posting');
             INSERT INTO _legacy_tasks (task_type, date, count, notes) VALUES ('SAKTI - Rekon','2026-01-05',5,''), ('SAKTI - Posting','2026-01-06',3,'');",
        )
        .unwrap();
        conn.execute_batch(
            "INSERT INTO categories (id, name, sort_order) VALUES (1,'SAKTI',0);
             INSERT INTO task_types (id, category_id, name) VALUES (1,1,'Rekon'), (2,1,'Posting');",
        )
        .unwrap();
        if wrong_attribution {
            // Both entries attributed to Rekon (id 1) — a resolution bug. Global sum (8) and
            // row count (2) still match the legacy source.
            conn.execute_batch(
                "INSERT INTO entries (task_type_id, date, count, notes) VALUES (1,'2026-01-05',5,''), (1,'2026-01-06',3,'');",
            )
            .unwrap();
        } else {
            conn.execute_batch(
                "INSERT INTO entries (task_type_id, date, count, notes) VALUES (1,'2026-01-05',5,''), (2,'2026-01-06',3,'');",
            )
            .unwrap();
        }
        conn
    }

    #[test]
    fn reconcile_passes_on_correct_attribution() {
        let conn = recon_fixture(false);
        let tx = conn.unchecked_transaction().unwrap();
        reconcile(&tx).unwrap();
    }

    #[test]
    fn reconcile_catches_wrong_type_attribution() {
        // The teeth test: same row count AND same global sum as the source, but one entry is
        // on the wrong type. A self-referential check would miss this; the independent
        // multiset must catch it.
        let conn = recon_fixture(true);
        let tx = conn.unchecked_transaction().unwrap();
        assert!(matches!(reconcile(&tx), Err(Error::MigrationValidation(_))));
    }

    #[test]
    fn transaction_rolls_back_on_drop_without_commit() {
        // Proves the rollback mechanism migrate_v38 relies on: a rename + writes inside a
        // transaction that is dropped (as happens when reconcile returns Err) leaves the
        // database untouched.
        let conn = legacy_db(
            &[("SAKTI - Rekon", "", 1)],
            &[("SAKTI - Rekon", "2026-01-05", "1", "", "2026-01-05 09:00:00.000")],
        );
        {
            let tx = conn.unchecked_transaction().unwrap();
            tx.execute_batch("ALTER TABLE tasks RENAME TO _legacy_tasks;").unwrap();
            tx.execute_batch("CREATE TABLE entries (id INTEGER PRIMARY KEY);").unwrap();
            // drop tx without commit -> ROLLBACK
        }
        // Original schema intact: 'tasks' still exists, no 'entries', no '_legacy_tasks'.
        let tables: BTreeSet<String> = {
            let mut stmt = conn
                .prepare("SELECT name FROM sqlite_master WHERE type='table'")
                .unwrap();
            stmt.query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        assert!(tables.contains("tasks"));
        assert!(!tables.contains("entries"));
        assert!(!tables.contains("_legacy_tasks"));
    }
}
