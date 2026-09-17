//! Domain structs shared across the data layer. These mirror the tables in
//! [`SCHEMA.md`](../../../notes/SCHEMA.md) but are plain owned values — the core API
//! hands these back, never a `Row` or a `&Connection`.

/// A `categories` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Category {
    pub id: i64,
    pub name: String,
    pub sort_order: i64,
    /// `'YYYY-MM-DD HH:MM:SS.fff'`.
    pub created_at: String,
}

/// A `task_types` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskType {
    pub id: i64,
    pub category_id: i64,
    pub name: String,
    pub description: String,
    pub is_active: bool,
    pub created_at: String,
}

/// An `entries` row (a tally).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: i64,
    pub task_type_id: i64,
    /// `'YYYY-MM-DD'`, a validated calendar day.
    pub date: String,
    pub count: i64,
    pub notes: String,
    /// `'YYYY-MM-DD HH:MM:SS.fff'`.
    pub created_at: String,
}
