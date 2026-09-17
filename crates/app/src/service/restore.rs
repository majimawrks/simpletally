//! Staged restore + interrupted-swap recovery (PLAN §1.4) — the one path that can destroy
//! user data. It proves the candidate backup *before* touching the live file, keeps a safety
//! copy, and guards the file swap with a marker so an interruption at any boundary can be
//! finished or reversed on the next startup.
//!
//! Sidecar files, all beside the live DB `base`:
//! - `<base>.restore_staging` — the candidate, copied and validated before it goes live.
//! - `<base>.restore_safety`  — a `VACUUM INTO` snapshot of the live DB, the fallback.
//! - `<base>.restore_marker`  — present only while the swap is mid-flight.
//!
//! The caller MUST drop its live [`Db`] (close the connection) before calling [`restore`], and
//! re-open `base` with [`simpletally_core::Db::open`] afterwards.

use simpletally_core::{Db, Error, OpenOutcome, Result};
use std::path::{Path, PathBuf};

fn with_suffix(base: &Path, suffix: &str) -> PathBuf {
    let mut s = base.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}
fn staging_path(base: &Path) -> PathBuf {
    with_suffix(base, ".restore_staging")
}
fn safety_path(base: &Path) -> PathBuf {
    with_suffix(base, ".restore_safety")
}
fn marker_path(base: &Path) -> PathBuf {
    with_suffix(base, ".restore_marker")
}
fn wal_path(base: &Path) -> PathBuf {
    with_suffix(base, "-wal")
}
fn shm_path(base: &Path) -> PathBuf {
    with_suffix(base, "-shm")
}

fn remove_if_exists(p: &Path) -> Result<()> {
    match std::fs::remove_file(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Io(e)),
    }
}

/// Remove a SQLite DB file plus its `-wal`/`-shm` companions.
fn remove_db_family(base: &Path) -> Result<()> {
    remove_if_exists(base)?;
    let _ = std::fs::remove_file(wal_path(base));
    let _ = std::fs::remove_file(shm_path(base));
    Ok(())
}

/// Restore `base` from the backup file `backup`, following the staged sequence in PLAN §1.4.
/// On success `base` holds the restored data (re-open it with `Db::open`); on any failure
/// before the swap the live DB is left untouched.
pub fn restore(base: &Path, backup: &Path) -> Result<()> {
    // Start from a clean, known state: finish/reverse any prior interrupted swap and clear
    // stale sidecars.
    recover_incomplete(base)?;

    let staging = staging_path(base);
    let safety = safety_path(base);
    let marker = marker_path(base);

    // 1. Copy the chosen backup to a staging file beside the live DB.
    remove_db_family(&staging)?;
    std::fs::copy(backup, &staging)?;

    // 2. Prove the staged copy — open (identify/migrate) and integrity-check it. A recognizable
    //    backup opens as `Opened` (current) or `Migrated` (legacy); `Created` means the file had
    //    no SimpleTally tables (an empty or unrelated database), which must NOT be allowed to
    //    wipe the live DB. Any failure ends here, with the live DB untouched.
    let validated = (|| -> Result<()> {
        let (db, outcome) = Db::open(&staging)?;
        if matches!(outcome, OpenOutcome::Created) {
            return Err(Error::Invalid(
                "backup file is not a recognizable SimpleTally database".into(),
            ));
        }
        db.check_integrity()
    })();
    if let Err(e) = validated {
        let _ = remove_db_family(&staging);
        return Err(e);
    }
    // Opening left staging `-wal`/`-shm`; drop them so the file that becomes live is clean.
    let _ = std::fs::remove_file(wal_path(&staging));
    let _ = std::fs::remove_file(shm_path(&staging));

    let base_exists = base.exists();

    // 3. Safety copy of the current live DB (if there is one), verified openable, before we
    //    risk the swap. (This also closes/checkpoints the live connection — step 4 — since the
    //    Db is dropped at the end of the block.)
    if base_exists {
        remove_db_family(&safety)?;
        {
            let (db, _o) = Db::open(base)?;
            db.backup_to(&safety)?;
        }
        Db::open(&safety)?; // verify the fallback opens before we depend on it
        let _ = std::fs::remove_file(wal_path(&safety));
        let _ = std::fs::remove_file(shm_path(&safety));
    }

    // 5. Swap, guarded by the marker so an interruption is recoverable (step 6).
    std::fs::write(&marker, b"restore-swap")?;
    if base_exists {
        remove_db_family(base)?; // (a) drop the live DB + its wal/shm
    }
    std::fs::rename(&staging, base)?; // (b) the validated staging becomes the live DB

    // Success: clear the marker first (so a crash here leaves a completed, marker-less state),
    // then the safety copy.
    let _ = std::fs::remove_file(&marker);
    let _ = std::fs::remove_file(&safety);
    Ok(())
}

/// Finish or reverse a restore swap that a crash interrupted (PLAN §1.4 step 6), and clear
/// stale sidecars. Safe (and cheap) to call on every startup and at the start of [`restore`].
pub fn recover_incomplete(base: &Path) -> Result<()> {
    let staging = staging_path(base);
    let safety = safety_path(base);
    let marker = marker_path(base);

    if !marker.exists() {
        // No swap was in progress. Clear stale sidecars from an aborted pre-swap attempt
        // (e.g. a validation failure) so they neither accumulate nor confuse a later restore.
        let _ = remove_db_family(&staging);
        let _ = remove_db_family(&safety);
        return Ok(());
    }

    // A swap was interrupted. The marker is only written after staging is validated and the
    // safety copy is made, so "finish forward" is always safe.
    match (base.exists(), staging.exists()) {
        // (b) completed: the new file is live, staging consumed — just finalize.
        (true, false) => {}
        // (a) done, (b) not: live removed, staging present — finish forward.
        (false, true) => {
            std::fs::rename(&staging, base)?;
        }
        // Crashed after the marker but before (a): staging validated, not yet applied —
        // finish forward.
        (true, true) => {
            remove_db_family(base)?;
            std::fs::rename(&staging, base)?;
        }
        // Both gone (a rare mid-rename loss): fall back to the safety copy if we have one.
        (false, false) => {
            if safety.exists() {
                std::fs::rename(&safety, base)?;
            }
            // else: nothing recoverable; leave base absent so startup discovery/create runs.
        }
    }

    let _ = std::fs::remove_file(&marker);
    let _ = std::fs::remove_file(&safety);
    // Any `-wal`/`-shm` next to a freshly renamed file belong to the old file and are invalid.
    let _ = std::fs::remove_file(wal_path(base));
    let _ = std::fs::remove_file(shm_path(base));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use simpletally_core::Db;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A unique temp directory, recursively removed on drop.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let d = std::env::temp_dir().join(format!("st_restore_test_{nanos}_{n}"));
            std::fs::create_dir_all(&d).unwrap();
            TempDir(d)
        }
        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Create a DB at `path` with one category/type and `count` tallied on 2026-01-05.
    fn make_db(path: &Path, count: i64) {
        let (db, _) = Db::open(path).unwrap();
        let cat = db.create_category("SAKTI").unwrap();
        let ty = db.create_task_type(cat, "Rekon", "").unwrap();
        db.add_tally(ty, "2026-01-05", count, "").unwrap();
        // drop closes the connection
    }

    /// A `.tally` backup (VACUUM INTO) of the DB currently at `src`.
    fn make_backup(src: &Path, dest: &Path) {
        let (db, _) = Db::open(src).unwrap();
        db.backup_to(dest).unwrap();
        let _ = std::fs::remove_file(wal_path(src));
        let _ = std::fs::remove_file(shm_path(src));
    }

    fn day_total(path: &Path) -> i64 {
        let (db, _) = Db::open(path).unwrap();
        db.day_total("2026-01-05").unwrap()
    }

    fn no_sidecars(base: &Path) -> bool {
        // No staging/safety/marker, AND none of their WAL companions left orphaned.
        for p in [staging_path(base), safety_path(base)] {
            if p.exists() || wal_path(&p).exists() || shm_path(&p).exists() {
                return false;
            }
        }
        !marker_path(base).exists()
    }

    #[test]
    fn restore_replaces_live_with_backup_content() {
        let dir = TempDir::new();
        let base = dir.path("task_tally.db");
        let backup = dir.path("backup.tally");

        make_db(&base, 3);
        make_backup(&base, &backup);
        // The live DB moves on after the backup was taken: add 7 more (total 10).
        {
            let (db, _) = Db::open(&base).unwrap();
            let ty: i64 = {
                let types = db.list_task_types(false).unwrap();
                types[0].id
            };
            db.add_tally(ty, "2026-01-05", 7, "").unwrap();
        }
        assert_eq!(day_total(&base), 10);
        // strip wal/shm left by the reads above
        let _ = std::fs::remove_file(wal_path(&base));
        let _ = std::fs::remove_file(shm_path(&base));

        restore(&base, &backup).unwrap();

        assert_eq!(day_total(&base), 3, "live DB should now hold the backup's content");
        let _ = std::fs::remove_file(wal_path(&base));
        let _ = std::fs::remove_file(shm_path(&base));
        assert!(no_sidecars(&base), "sidecars must be cleaned up on success");
    }

    #[test]
    fn restore_validation_failure_leaves_live_untouched() {
        let dir = TempDir::new();
        let base = dir.path("task_tally.db");
        let bogus = dir.path("bogus.tally");

        make_db(&base, 3);
        let _ = std::fs::remove_file(wal_path(&base));
        let _ = std::fs::remove_file(shm_path(&base));
        std::fs::write(&bogus, b"this is not a sqlite database").unwrap();

        let err = restore(&base, &bogus);
        assert!(err.is_err(), "restoring from a non-DB must fail");

        assert_eq!(day_total(&base), 3, "live DB must be untouched on validation failure");
        let _ = std::fs::remove_file(wal_path(&base));
        let _ = std::fs::remove_file(shm_path(&base));
        assert!(no_sidecars(&base), "failed staging must be cleaned up");
    }

    /// A workspace-relative path (from this crate's manifest dir up to the repo root).
    fn workspace_file(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join(rel)
    }

    /// Exit-test: restoring from the shipped v3.8 fixture migrates it (the "supplied fixture"
    /// + "restore from a supported older schema version" items, PLAN §4). The fixture lives
    /// under a gitignored `_*/` dir, so this skips cleanly when it isn't present.
    #[test]
    fn restore_from_shipped_v38_fixture_migrates_if_present() {
        let fixture = workspace_file("_rustrefactor/task_tally.db");
        if !fixture.exists() {
            eprintln!("skipping: shipped v3.8 fixture not present at {}", fixture.display());
            return;
        }
        let dir = TempDir::new();
        let base = dir.path("task_tally.db");
        make_db(&base, 3); // a current live DB that the restore will replace
        let _ = std::fs::remove_file(wal_path(&base));
        let _ = std::fs::remove_file(shm_path(&base));

        // Restore from a temp copy of the legacy fixture (never from the repo file itself).
        let backup = dir.path("legacy_backup.db");
        std::fs::copy(&fixture, &backup).unwrap();

        restore(&base, &backup).unwrap();

        // The live DB is now the migrated legacy content: task types present, opens as current.
        let (db, _) = Db::open(&base).unwrap();
        assert!(
            !db.list_task_types(false).unwrap().is_empty(),
            "migrated legacy fixture should have task types"
        );
        drop(db);
        let _ = std::fs::remove_file(wal_path(&base));
        let _ = std::fs::remove_file(shm_path(&base));
        assert!(no_sidecars(&base));
    }

    #[test]
    fn restore_from_empty_or_unrelated_file_is_rejected_and_live_untouched() {
        let dir = TempDir::new();
        let base = dir.path("task_tally.db");
        let empty = dir.path("empty.tally");

        make_db(&base, 3);
        let _ = std::fs::remove_file(wal_path(&base));
        let _ = std::fs::remove_file(shm_path(&base));
        // A zero-byte file opens as a valid-but-empty SQLite DB (classify -> Empty -> Created).
        std::fs::write(&empty, b"").unwrap();

        let err = restore(&base, &empty);
        assert!(err.is_err(), "an empty/unrelated DB must not be accepted as a backup");

        assert_eq!(day_total(&base), 3, "live DB must be untouched");
        let _ = std::fs::remove_file(wal_path(&base));
        let _ = std::fs::remove_file(shm_path(&base));
        assert!(no_sidecars(&base));
    }

    #[test]
    fn recover_no_marker_clears_stale_sidecars() {
        let dir = TempDir::new();
        let base = dir.path("task_tally.db");
        make_db(&base, 3);
        let _ = std::fs::remove_file(wal_path(&base));
        let _ = std::fs::remove_file(shm_path(&base));
        // Stale sidecars with no marker (a pre-swap abort).
        std::fs::write(staging_path(&base), b"stale").unwrap();
        std::fs::write(safety_path(&base), b"stale").unwrap();

        recover_incomplete(&base).unwrap();

        assert_eq!(day_total(&base), 3, "base untouched");
        let _ = std::fs::remove_file(wal_path(&base));
        let _ = std::fs::remove_file(shm_path(&base));
        assert!(no_sidecars(&base));
    }

    #[test]
    fn recover_finishes_swap_when_base_removed_and_staging_present() {
        // State after step (a): base gone, staging = validated new content, marker present.
        let dir = TempDir::new();
        let base = dir.path("task_tally.db");
        make_db(&staging_path(&base), 42); // staging holds the "new" content
        let _ = std::fs::remove_file(wal_path(&staging_path(&base)));
        let _ = std::fs::remove_file(shm_path(&staging_path(&base)));
        std::fs::write(marker_path(&base), b"restore-swap").unwrap();
        // base does not exist.

        recover_incomplete(&base).unwrap();

        assert_eq!(day_total(&base), 42, "staging should have been promoted to base");
        let _ = std::fs::remove_file(wal_path(&base));
        let _ = std::fs::remove_file(shm_path(&base));
        assert!(no_sidecars(&base));
    }

    #[test]
    fn recover_finishes_swap_when_both_base_and_staging_present() {
        // Crashed after the marker, before (a): base = original, staging = validated new.
        let dir = TempDir::new();
        let base = dir.path("task_tally.db");
        make_db(&base, 1); // original
        make_db(&staging_path(&base), 99); // validated new
        for p in [&base, &staging_path(&base)] {
            let _ = std::fs::remove_file(wal_path(p));
            let _ = std::fs::remove_file(shm_path(p));
        }
        std::fs::write(marker_path(&base), b"restore-swap").unwrap();

        recover_incomplete(&base).unwrap();

        assert_eq!(day_total(&base), 99, "should finish forward to the validated staging");
        let _ = std::fs::remove_file(wal_path(&base));
        let _ = std::fs::remove_file(shm_path(&base));
        assert!(no_sidecars(&base));
    }

    #[test]
    fn recover_finalizes_completed_swap() {
        // State after (b): base = new content, staging gone, marker + safety linger.
        let dir = TempDir::new();
        let base = dir.path("task_tally.db");
        make_db(&base, 7);
        make_db(&safety_path(&base), 1); // leftover safety
        for p in [&base, &safety_path(&base)] {
            let _ = std::fs::remove_file(wal_path(p));
            let _ = std::fs::remove_file(shm_path(p));
        }
        std::fs::write(marker_path(&base), b"restore-swap").unwrap();

        recover_incomplete(&base).unwrap();

        assert_eq!(day_total(&base), 7, "completed swap left intact");
        let _ = std::fs::remove_file(wal_path(&base));
        let _ = std::fs::remove_file(shm_path(&base));
        assert!(no_sidecars(&base));
    }

    #[test]
    fn recover_falls_back_to_safety_when_both_gone() {
        // Rare mid-rename loss: base and staging both gone, marker + safety present.
        let dir = TempDir::new();
        let base = dir.path("task_tally.db");
        make_db(&safety_path(&base), 5); // safety = the original data
        let _ = std::fs::remove_file(wal_path(&safety_path(&base)));
        let _ = std::fs::remove_file(shm_path(&safety_path(&base)));
        std::fs::write(marker_path(&base), b"restore-swap").unwrap();
        // base and staging do not exist.

        recover_incomplete(&base).unwrap();

        assert_eq!(day_total(&base), 5, "should recover the original from the safety copy");
        let _ = std::fs::remove_file(wal_path(&base));
        let _ = std::fs::remove_file(shm_path(&base));
        assert!(no_sidecars(&base));
    }
}
