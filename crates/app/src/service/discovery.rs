//! Legacy-database discovery (PLAN §1.5).
//!
//! The legacy v3.8 app opened `task_tally.db` via a relative path, so old data may sit in
//! whatever the process working directory was rather than beside the new executable. This
//! module only locates plausible files by path; it never opens or classifies them (that is
//! the caller's job, later, via the core `Db::open`). It must never silently collapse
//! multiple candidates into one — starting fresh next to a database the user thought was
//! loaded is the worst available outcome, so every candidate found is reported and the
//! caller (the UI) is responsible for asking the user.

use std::path::{Path, PathBuf};

/// The filename the settled DB uses, both new and legacy (PLAN decision #1).
const DB_FILENAME: &str = "task_tally.db";

/// The result of scanning for databases at startup.
pub struct DiscoveryResult {
    /// `task_tally.db` sitting beside the executable, if it exists (the normal case).
    pub beside_exe: Option<PathBuf>,
    /// Other plausible legacy `task_tally.db` files found in the search dirs, excluding the
    /// beside-exe one. If `beside_exe` is None and this is non-empty, the UI must ask the
    /// user which to load (or start fresh) — never auto-pick.
    pub other_candidates: Vec<PathBuf>,
}

/// Scan `exe_dir` plus each of `extra_dirs` for a file named `task_tally.db`.
///
/// `extra_dirs` is supplied by the caller (typically the current working directory and any
/// other plausible spots) so this stays testable and side-effect-free about how dirs are
/// chosen. The exe dir itself is found by the caller via
/// `std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf))`.
///
/// `beside_exe` is `exe_dir/task_tally.db` if it exists as a file, else `None`.
/// `other_candidates` is built by checking `dir/task_tally.db` for each `dir` in `extra_dirs`,
/// in order, keeping the first occurrence of each canonicalized path and dropping the
/// beside-exe path if it reappears there. A dir that does not exist, or has no matching file,
/// is silently skipped — never an error.
pub fn discover(exe_dir: &Path, extra_dirs: &[PathBuf]) -> DiscoveryResult {
    let beside_exe = existing_db_file(exe_dir);
    let beside_exe_canonical = beside_exe.as_deref().and_then(canonical_or_none);

    let mut other_candidates = Vec::new();
    let mut seen: Vec<PathBuf> = beside_exe_canonical.into_iter().collect();

    for dir in extra_dirs {
        let Some(candidate) = existing_db_file(dir) else {
            continue;
        };
        let key = canonical_or_none(&candidate).unwrap_or_else(|| candidate.clone());
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        other_candidates.push(candidate);
    }

    DiscoveryResult { beside_exe, other_candidates }
}

/// The current working directory, as a ready-made single-element search list for callers that
/// want the common case. `discover` itself stays explicit about its dirs so it is testable.
pub fn default_search_dirs() -> Vec<PathBuf> {
    std::env::current_dir().into_iter().collect()
}

/// `dir/task_tally.db` if it exists as a file, else `None`.
fn existing_db_file(dir: &Path) -> Option<PathBuf> {
    let candidate = dir.join(DB_FILENAME);
    candidate.is_file().then_some(candidate)
}

/// Best-effort canonicalization for de-dup comparisons; `None` if it fails (e.g. the file
/// vanished between the existence check and here — treat it as its own key by falling back to
/// the raw path at the call site).
fn canonical_or_none(path: &Path) -> Option<PathBuf> {
    path.canonicalize().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A scratch directory under the OS temp dir, removed on drop.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "simpletally-discovery-test-{}-{}-{:?}",
                tag,
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock")
                    .as_nanos()
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn path(&self) -> PathBuf {
            self.path.clone()
        }

        fn touch_db(&self) {
            fs::write(self.path.join(DB_FILENAME), b"").expect("write db file");
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn no_files_anywhere_finds_nothing() {
        let exe_dir = TempDir::new("no-files-exe");
        let extra = TempDir::new("no-files-extra");

        let result = discover(&exe_dir.path(), &[extra.path()]);

        assert!(result.beside_exe.is_none());
        assert!(result.other_candidates.is_empty());
    }

    #[test]
    fn db_beside_exe_is_found() {
        let exe_dir = TempDir::new("beside-exe");
        exe_dir.touch_db();

        let result = discover(&exe_dir.path(), &[]);

        assert_eq!(result.beside_exe, Some(exe_dir.path().join(DB_FILENAME)));
        assert!(result.other_candidates.is_empty());
    }

    #[test]
    fn db_only_in_extra_dir_is_a_candidate_not_beside_exe() {
        let exe_dir = TempDir::new("only-extra-exe");
        let extra = TempDir::new("only-extra-extra");
        extra.touch_db();

        let result = discover(&exe_dir.path(), &[extra.path()]);

        assert!(result.beside_exe.is_none());
        assert_eq!(result.other_candidates, vec![extra.path().join(DB_FILENAME)]);
    }

    #[test]
    fn multiple_candidates_across_extra_dirs_are_all_listed() {
        // Load-bearing: the "never silently pick" guarantee. If discover ever collapsed
        // this to one candidate, or picked a winner, that would be exactly the failure
        // mode PLAN §1.5 calls out as the worst available outcome.
        let exe_dir = TempDir::new("multi-exe");
        let extra_a = TempDir::new("multi-extra-a");
        let extra_b = TempDir::new("multi-extra-b");
        extra_a.touch_db();
        extra_b.touch_db();

        let result = discover(&exe_dir.path(), &[extra_a.path(), extra_b.path()]);

        assert!(result.beside_exe.is_none());
        assert_eq!(
            result.other_candidates,
            vec![extra_a.path().join(DB_FILENAME), extra_b.path().join(DB_FILENAME)]
        );
    }

    #[test]
    fn beside_exe_path_not_duplicated_when_exe_dir_also_passed_as_extra() {
        let exe_dir = TempDir::new("dup-exe");
        exe_dir.touch_db();

        let result = discover(&exe_dir.path(), &[exe_dir.path()]);

        assert_eq!(result.beside_exe, Some(exe_dir.path().join(DB_FILENAME)));
        assert!(result.other_candidates.is_empty());
    }

    #[test]
    fn missing_dir_is_skipped_without_error() {
        let exe_dir = TempDir::new("missing-dir-exe");
        let missing = exe_dir.path().join("does-not-exist");

        let result = discover(&exe_dir.path(), &[missing]);

        assert!(result.beside_exe.is_none());
        assert!(result.other_candidates.is_empty());
    }
}
