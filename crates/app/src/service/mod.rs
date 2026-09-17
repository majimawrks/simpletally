//! Service layer: DB file lifecycle around the core `Db` — path resolution + open
//! orchestration, backup, staged restore + interrupted-swap recovery, legacy-database
//! discovery, and the settings file. Filesystem lifecycle lives here, not in core
//! (PLAN §2 boundary rule 2).

pub mod db_service;
pub mod discovery;
pub mod import;
pub mod restore;
pub mod settings;
