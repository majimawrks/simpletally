//! Core error type. Every core operation returns [`Result`]; the app/service layer
//! maps these onto user-facing modals.

use std::fmt;

/// Errors surfaced by the data layer.
#[derive(Debug)]
pub enum Error {
    /// A `rusqlite` / SQLite error.
    Sqlite(rusqlite::Error),
    /// A filesystem error (e.g. writing the pre-migration backup).
    Io(std::io::Error),
    /// The bootstrap classifier found a file we refuse to touch
    /// (malformed, half-migrated, or a newer `user_version`). SCHEMA §6.
    UnsupportedDatabase(String),
    /// Migration preflight found distinct legacy identities that collapse to the
    /// same `(category, name)`. Nothing was written; the message lists the groups.
    /// SCHEMA §7, user decision: abort with a report.
    MigrationCollision(Vec<String>),
    /// Migration post-transform validation failed (row count, per-identity totals,
    /// `foreign_key_check`, or `integrity_check`). The pre-migration copy is kept.
    MigrationValidation(String),
    /// Invalid domain input: a non-calendar date, a non-positive count, an empty name.
    Invalid(String),
    /// A uniqueness constraint was violated — a duplicate category or task-type name
    /// (case-insensitive). Mutation ops map SQLite's UNIQUE failure onto this.
    Duplicate(String),
    /// A referenced row does not exist (bad id).
    NotFound(String),
    /// The operation conflicts with current state: deleting a non-empty category, merging a
    /// type into itself, restoring onto a name that is in use, etc.
    Conflict(String),
    /// A CSV read/write error.
    Csv(csv::Error),
}

impl Error {
    /// True if this wraps a SQLite UNIQUE (or PRIMARY KEY) constraint violation — used by the
    /// mutation ops to turn a raw insert failure into [`Error::Duplicate`].
    pub fn is_unique_violation(&self) -> bool {
        matches!(
            self,
            Error::Sqlite(rusqlite::Error::SqliteFailure(e, _))
                if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
                    || e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
        )
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Sqlite(e) => write!(f, "database error: {e}"),
            Error::Io(e) => write!(f, "filesystem error: {e}"),
            Error::UnsupportedDatabase(m) => write!(f, "unsupported database: {m}"),
            Error::MigrationCollision(groups) => {
                write!(f, "migration aborted — colliding legacy names: {}", groups.join("; "))
            }
            Error::MigrationValidation(m) => write!(f, "migration validation failed: {m}"),
            Error::Invalid(m) => write!(f, "invalid input: {m}"),
            Error::Duplicate(m) => write!(f, "already exists: {m}"),
            Error::NotFound(m) => write!(f, "not found: {m}"),
            Error::Conflict(m) => write!(f, "conflict: {m}"),
            Error::Csv(e) => write!(f, "CSV error: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Sqlite(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<csv::Error> for Error {
    fn from(e: csv::Error) -> Self {
        Error::Csv(e)
    }
}

/// The crate-wide result alias.
pub type Result<T> = std::result::Result<T, Error>;
