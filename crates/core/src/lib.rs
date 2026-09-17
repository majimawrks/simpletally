//! simpletally-core — the data layer for SimpleTally.
//!
//! Schema, v3.8 migration, queries, aggregates and CSV codecs live here (PLAN §4).
//! This crate exposes a domain API that owns its own transactions; it deliberately
//! does not hand out a bare `&Connection` to callers (PLAN §2, boundary rule 1).
//!
//! Contract documents: [`SCHEMA.md`](../../../notes/SCHEMA.md) and
//! [`CSV_FORMAT.md`](../../../notes/CSV_FORMAT.md) under `notes/`.

pub mod error;
pub mod model;

// Phase-1 modules, filled in by the build (see notes/STATUS.md "Next actions"):
pub mod schema; // DDL + create + drift guard (SCHEMA §0/§4/§5)
pub mod classify; // read-only bootstrap classifier (SCHEMA §6)
pub mod dates; // pure week/month/quarter/year boundary functions (PLAN §4)
pub mod migrate; // v3.8 -> v1 migration (SCHEMA §7)
pub mod aggregate; // GROUP BY aggregates (PLAN §4)
pub mod csvio; // CSV codecs (CSV_FORMAT.md)
pub mod entries; // add/remove/edit/delete tallies (PLAN §1.1/§4)
pub mod catalog; // category + task-type CRUD (PLAN §6)
pub mod trash; // delete via merge/trash, restore-from-trash, purge (PLAN §4)
pub mod db; // the Db facade — the only public handle to the data layer (PLAN §2)

pub use error::{Error, Result};
pub use model::{Category, Entry, TaskType};
pub use db::{Db, OpenOutcome};
