//! Database path resolution and startup open orchestration (PLAN §2 / §1.4 / §1.5).
//!
//! The portable DB lives beside the executable. This module resolves that path, fails loudly
//! if the directory is not writable (PLAN §2 — never silently relocate), and opens the DB
//! *after* finishing any interrupted restore swap (PLAN §1.4 step 6). Deciding *which* file to
//! open on first run (discovery, PLAN §1.5) is the caller's job via [`crate::service::discovery`].

use simpletally_core::{Db, Error, OpenOutcome, Result};
use std::path::{Path, PathBuf};

/// The settled database filename (PLAN decision #1) — kept so a v3.8 file is recognised.
pub const DB_FILENAME: &str = "task_tally.db";

/// The directory the executable lives in — where the portable DB and settings belong.
pub fn exe_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| Error::Invalid("executable path has no parent directory".into()))
}

/// The database path beside the executable.
pub fn database_path() -> Result<PathBuf> {
    Ok(exe_dir()?.join(DB_FILENAME))
}

/// The settings filename (PLAN §5). Portable: it sits beside the executable with the DB,
/// never in AppData or the registry, so the whole app is a directory you can copy.
pub const SETTINGS_FILENAME: &str = "settings.toml";

/// The settings path beside the executable.
pub fn settings_path() -> Result<PathBuf> {
    Ok(exe_dir()?.join(SETTINGS_FILENAME))
}

/// Fail loudly if `dir` is not writable, naming the path (PLAN §2). Used before creating a DB
/// so a read-only location (Program Files, a read-only share) is reported rather than silently
/// worked around.
pub fn ensure_writable(dir: &Path) -> Result<()> {
    let probe = dir.join(".simpletally_write_probe");
    std::fs::write(&probe, b"ok").map_err(|e| {
        Error::Invalid(format!(
            "database directory is not writable: {} ({e})",
            dir.display()
        ))
    })?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

/// Open the database at `path`. First fails loudly if the containing directory is not writable
/// (PLAN §2 — the app needs to write WAL and settings there, so a read-only location is an
/// error, not something to silently work around), then finishes/reverses any interrupted
/// restore swap (PLAN §1.4 step 6) so startup never proceeds on a half-swapped file. Creating
/// or migrating as needed is handled by [`Db::open`]; the returned [`OpenOutcome`] says which.
pub fn open_at(path: &Path) -> Result<(Db, OpenOutcome)> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            ensure_writable(parent)?;
        }
    }
    crate::service::restore::recover_incomplete(path)?;
    Db::open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let d = std::env::temp_dir().join(format!("st_dbsvc_test_{nanos}_{n}"));
            std::fs::create_dir_all(&d).unwrap();
            TempDir(d)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn exe_dir_and_database_path_resolve() {
        let dir = exe_dir().unwrap();
        assert!(dir.is_dir());
        assert!(database_path().unwrap().ends_with(DB_FILENAME));
    }

    #[test]
    fn ensure_writable_ok_on_temp_dir() {
        let dir = TempDir::new();
        ensure_writable(&dir.0).unwrap();
    }

    #[test]
    fn ensure_writable_errors_on_missing_dir() {
        let dir = TempDir::new();
        let missing = dir.0.join("does-not-exist");
        assert!(matches!(ensure_writable(&missing), Err(Error::Invalid(_))));
    }

    #[test]
    fn open_at_creates_then_reopens() {
        let dir = TempDir::new();
        let path = dir.0.join(DB_FILENAME);

        let (db, outcome) = open_at(&path).unwrap();
        assert!(matches!(outcome, OpenOutcome::Created));
        db.create_category("SAKTI").unwrap();
        drop(db);

        let (db2, outcome2) = open_at(&path).unwrap();
        assert!(matches!(outcome2, OpenOutcome::Opened));
        assert_eq!(db2.list_categories().unwrap().len(), 1);
    }

    #[test]
    fn open_at_finishes_interrupted_swap_first() {
        // A DB with content "42" sits in the staging file with a marker but no live DB — the
        // state right after step (a). open_at must recover it before opening.
        let dir = TempDir::new();
        let path = dir.0.join(DB_FILENAME);

        // Build the staging DB via a normal open, then rename it into the staging slot.
        let staged = dir.0.join("seed.db");
        {
            let (db, _) = Db::open(&staged).unwrap();
            let cat = db.create_category("SAKTI").unwrap();
            let ty = db.create_task_type(cat, "Rekon", "").unwrap();
            db.add_tally(ty, "2026-01-05", 42, "").unwrap();
        }
        let _ = std::fs::remove_file(dir.0.join("seed.db-wal"));
        let _ = std::fs::remove_file(dir.0.join("seed.db-shm"));

        let staging = {
            let mut s = path.as_os_str().to_os_string();
            s.push(".restore_staging");
            PathBuf::from(s)
        };
        let marker = {
            let mut s = path.as_os_str().to_os_string();
            s.push(".restore_marker");
            PathBuf::from(s)
        };
        std::fs::rename(&staged, &staging).unwrap();
        std::fs::write(&marker, b"restore-swap").unwrap();

        let (db, _outcome) = open_at(&path).unwrap();
        assert_eq!(db.day_total("2026-01-05").unwrap(), 42, "swap should be finished on open");
        assert!(!marker.exists());
        assert!(!staging.exists());
    }
}
