//! CSV codecs — export and import for the three v1 file shapes in
//! [`CSV_FORMAT.md`](../../../notes/CSV_FORMAT.md): task-data export (§1), task-types
//! export/import (§2/§3), and the insights range export (§5). Delimiter/schema detection
//! is shared (§4).
//!
//! This module never touches the database: exports take already-fetched rows, and import
//! returns parsed rows plus a report for the service layer to insert inside its own
//! transaction (CSV_FORMAT §3 "one transaction" is a service-layer responsibility; the
//! within-file dedup done here is not the same thing as dedup-against-the-DB).

use crate::error::{Error, Result};
use csv::{ReaderBuilder, StringRecord, WriterBuilder};
use std::collections::HashSet;
use std::io::Write;

// ---------------------------------------------------------------------------------------
// Exports
// ---------------------------------------------------------------------------------------

/// One row of a task-data export (CSV_FORMAT §1). Export-only in Phase 1 — there is no
/// importer for this shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryExportRow {
    pub id: i64,
    pub category: String,
    pub task_type: String,
    /// `'YYYY-MM-DD'`.
    pub date: String,
    pub count: i64,
    pub notes: String,
    /// `'YYYY-MM-DD HH:MM:SS.fff'`.
    pub created_at: String,
}

/// Write the task-data export (§1): header
/// `ID,Category,Task Type,Date,Count,Notes,Created At`, CRLF line endings, UTF-8, no BOM.
/// Caller supplies rows already in the documented order (date, created_at, id).
pub fn export_entries<W: Write>(writer: W, rows: &[EntryExportRow]) -> Result<()> {
    let mut wtr = csv_writer(writer);
    wtr.write_record(["ID", "Category", "Task Type", "Date", "Count", "Notes", "Created At"])?;
    for r in rows {
        wtr.write_record([
            r.id.to_string(),
            r.category.clone(),
            r.task_type.clone(),
            r.date.clone(),
            r.count.to_string(),
            r.notes.clone(),
            r.created_at.clone(),
        ])?;
    }
    wtr.flush()?;
    Ok(())
}

/// One row of a task-types export (CSV_FORMAT §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskTypeExportRow {
    pub category: String,
    pub name: String,
    pub description: String,
    /// Derived `SUM(entries.count)`; `0` for unused. Not stored, ignored on re-import.
    pub usage_count: i64,
    pub is_active: bool,
}

/// Write the task-types export (§2): header
/// `Category,Name,Description,Usage Count,Status`. Caller supplies rows already ordered by
/// `categories.sort_order, categories.name, task_types.name`.
pub fn export_task_types<W: Write>(writer: W, rows: &[TaskTypeExportRow]) -> Result<()> {
    let mut wtr = csv_writer(writer);
    wtr.write_record(["Category", "Name", "Description", "Usage Count", "Status"])?;
    for r in rows {
        wtr.write_record([
            r.category.clone(),
            r.name.clone(),
            r.description.clone(),
            r.usage_count.to_string(),
            status_label(r.is_active).to_string(),
        ])?;
    }
    wtr.flush()?;
    Ok(())
}

/// One row of an insights range export (CSV_FORMAT §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsightRow {
    pub category: String,
    pub task_type: String,
    pub count: i64,
}

/// Write the insights range export (§5): header
/// `Category,Task Type,Count,Range Start,Range End`. `range_start`/`range_end` (inclusive,
/// `YYYY-MM-DD`) repeat on every row. Caller supplies rows already ranked `Count DESC,
/// Category, Task Type` with zero-count types excluded; an empty slice yields a
/// header-only file.
pub fn export_insights<W: Write>(
    writer: W,
    rows: &[InsightRow],
    range_start: &str,
    range_end: &str,
) -> Result<()> {
    let mut wtr = csv_writer(writer);
    wtr.write_record(["Category", "Task Type", "Count", "Range Start", "Range End"])?;
    for r in rows {
        wtr.write_record([
            r.category.clone(),
            r.task_type.clone(),
            r.count.to_string(),
            range_start.to_string(),
            range_end.to_string(),
        ])?;
    }
    wtr.flush()?;
    Ok(())
}

/// A writer configured per CSV_FORMAT §0: CRLF terminator (the crate defaults to LF),
/// UTF-8, no BOM (we never write one).
fn csv_writer<W: Write>(writer: W) -> csv::Writer<W> {
    WriterBuilder::new()
        .terminator(csv::Terminator::CRLF)
        .from_writer(writer)
}

fn status_label(is_active: bool) -> &'static str {
    if is_active {
        "Active"
    } else {
        "Inactive"
    }
}

// ---------------------------------------------------------------------------------------
// Task-types import (§3/§4)
// ---------------------------------------------------------------------------------------

/// A parsed task-types row, ready for the service layer to upsert (category created on
/// demand, matched NOCASE — CSV_FORMAT §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskTypeImportRow {
    pub category: String,
    pub name: String,
    pub description: String,
    pub is_active: bool,
}

/// What the import did, reported to the user (§3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    pub imported: usize,
    /// Duplicate `(category, name)` **within the file** (ASCII-case-insensitive). Dedup
    /// against existing DB rows is the service layer's job, not this module's.
    pub skipped_duplicate: usize,
    pub skipped_malformed: usize,
    pub used_cp1252: bool,
    /// `"utf-8"` or `"windows-1252"`.
    pub encoding: String,
}

/// Explicit override for genuinely ambiguous files (§4), the escape hatch over
/// best-effort detection. `has_header` says whether the first logical record is a header
/// row at all; when it is, `is_legacy` says whether to validate it as a v1 types header
/// (`false`, the normal case) or to simply discard it and read every following row as
/// legacy/headerless (`true`, for a hand-edited file that carries a junk header line over
/// legacy data). With `has_header: false` the whole file is read as legacy/headerless
/// (the only import shape defined for a file with no header row).
pub struct ImportOverride {
    pub delimiter: u8,
    pub has_header: bool,
    pub is_legacy: bool,
}

/// The recognised v1 task-types header column names (§3), lower-cased.
const COL_CATEGORY: &str = "category";
const COL_NAME: &str = "name";
const COL_DESCRIPTION: &str = "description";
const COL_USAGE_COUNT: &str = "usage count";
const COL_STATUS: &str = "status";

/// Every column name that appears in *any* documented v1 header (task-types, task-data,
/// insights). Used only to recognise "this looks like a header row of some kind" so an
/// unrecognised-but-headed file (e.g. an entries export) is rejected instead of being
/// misread as legacy data (§3/§4) — this is a closed vocabulary check, not a prefix/substr
/// sniff, so an ordinary name like "Nameplate" never false-fires.
const KNOWN_HEADER_TOKENS: &[&str] = &[
    "id",
    COL_CATEGORY,
    COL_NAME,
    "task type",
    "date",
    "count",
    "notes",
    "created at",
    COL_DESCRIPTION,
    COL_USAGE_COUNT,
    COL_STATUS,
    "range start",
    "range end",
];

const LEGACY_SEP: &str = " - ";
const UNCATEGORIZED: &str = "Uncategorized";

/// ASCII case-fold, matching SQLite's `COLLATE NOCASE` (ASCII-only), so file-internal
/// dedup agrees with what the DB will later consider a duplicate (SCHEMA §1,
/// `migrate.rs`'s `nocase`).
fn nocase(s: &str) -> String {
    s.to_ascii_lowercase()
}

/// Split on the **first** `" - "`, matching the v3.8 migration split (SCHEMA §7,
/// `migrate.rs`'s `split_legacy`). No separator → `Uncategorized`.
fn split_legacy(full: &str) -> (String, String) {
    match full.find(LEGACY_SEP) {
        Some(i) => (
            full[..i].trim().to_string(),
            full[i + LEGACY_SEP.len()..].trim().to_string(),
        ),
        None => (UNCATEGORIZED.to_string(), full.trim().to_string()),
    }
}

/// Import task-types rows from raw bytes (§3/§4). `override_` forces delimiter/header
/// interpretation for an ambiguous file; `None` runs full detection.
pub fn import_task_types(
    bytes: &[u8],
    override_: Option<ImportOverride>,
) -> Result<(Vec<TaskTypeImportRow>, ImportReport)> {
    let (text, used_cp1252) = decode(bytes);

    let (records, data_start, header): (Vec<StringRecord>, usize, Option<Vec<String>>) =
        match override_ {
            Some(ov) => {
                let records = parse_records(&text, ov.delimiter)?;
                if ov.has_header {
                    match first_meaningful_index(&records) {
                        None => (records, 0, None),
                        Some(idx) => {
                            if ov.is_legacy {
                                (records, idx + 1, None)
                            } else {
                                let norm = normalize_header(&records[idx]);
                                if !is_recognized_types_header(&norm) {
                                    return Err(unrecognized_header_error(&records[idx]));
                                }
                                (records, idx + 1, Some(norm))
                            }
                        }
                    }
                } else {
                    let start = first_meaningful_index(&records).unwrap_or(records.len());
                    (records, start, None)
                }
            }
            None => {
                let (delimiter, shape) = detect(&text)?;
                let records = parse_records(&text, delimiter)?;
                match shape {
                    Shape::Recognized(idx, norm) => (records, idx + 1, Some(norm)),
                    Shape::UnrecognizedHeaded(idx) => {
                        return Err(unrecognized_header_error(&records[idx]))
                    }
                    Shape::Legacy(start) => (records, start, None),
                }
            }
        };

    let (imported, skipped_duplicate, skipped_malformed) = match header {
        Some(norm) => process_headed(&records, data_start, &norm),
        None => process_legacy(&records, data_start),
    };

    let report = ImportReport {
        imported: imported.len(),
        skipped_duplicate,
        skipped_malformed,
        used_cp1252,
        encoding: if used_cp1252 { "windows-1252" } else { "utf-8" }.to_string(),
    };
    Ok((imported, report))
}

/// `(rows, duplicates skipped, malformed skipped)` for the v1-header path (§3): validate
/// row width against the header, skip blank/whitespace names, parse `Status` if present,
/// dedup `(category, name)` case-insensitively within the file. A blank `Category` column
/// defaults to `Uncategorized` (not spelled out in CSV_FORMAT for the headed path; treated
/// the same as the legacy no-separator default for consistency — flagged for review).
fn process_headed(
    records: &[StringRecord],
    data_start: usize,
    norm: &[String],
) -> (Vec<TaskTypeImportRow>, usize, usize) {
    let expected_len = norm.len();
    let cat_idx = norm.iter().position(|f| f == COL_CATEGORY);
    let name_idx = norm.iter().position(|f| f == COL_NAME);
    let desc_idx = norm.iter().position(|f| f == COL_DESCRIPTION);
    let status_idx = norm.iter().position(|f| f == COL_STATUS);

    let mut imported = Vec::new();
    let mut malformed = 0usize;
    let mut duplicate = 0usize;
    let mut seen: HashSet<(String, String)> = HashSet::new();

    for rec in records.iter().skip(data_start) {
        if is_blank_line(rec) {
            continue;
        }
        if rec.len() != expected_len {
            malformed += 1;
            continue;
        }
        let name = name_idx.and_then(|i| rec.get(i)).unwrap_or("").trim().to_string();
        if name.is_empty() {
            malformed += 1;
            continue;
        }
        let mut category = cat_idx.and_then(|i| rec.get(i)).unwrap_or("").trim().to_string();
        if category.is_empty() {
            category = UNCATEGORIZED.to_string();
        }
        let description = desc_idx.and_then(|i| rec.get(i)).unwrap_or("").to_string();
        let is_active = match status_idx.and_then(|i| rec.get(i)).map(str::trim) {
            None | Some("") => true,
            Some(s) if s.eq_ignore_ascii_case("active") => true,
            Some(s) if s.eq_ignore_ascii_case("inactive") => false,
            Some(_) => {
                malformed += 1;
                continue;
            }
        };

        let key = (nocase(&category), nocase(&name));
        if !seen.insert(key) {
            duplicate += 1;
            continue;
        }
        imported.push(TaskTypeImportRow { category, name, description, is_active });
    }

    (imported, duplicate, malformed)
}

/// `(rows, duplicates skipped, malformed skipped)` for the legacy/headerless path (§3):
/// column 1 is the name (required, split on the first `" - "`), column 2 is an optional
/// description; extra columns are ignored (the format does not fix a column count).
fn process_legacy(
    records: &[StringRecord],
    data_start: usize,
) -> (Vec<TaskTypeImportRow>, usize, usize) {
    let mut imported = Vec::new();
    let mut malformed = 0usize;
    let mut duplicate = 0usize;
    let mut seen: HashSet<(String, String)> = HashSet::new();

    for rec in records.iter().skip(data_start) {
        if is_blank_line(rec) {
            continue;
        }
        let raw_name = rec.get(0).unwrap_or("").trim();
        if raw_name.is_empty() {
            malformed += 1;
            continue;
        }
        let description = rec.get(1).unwrap_or("").trim().to_string();
        let (category, name) = split_legacy(raw_name);

        let key = (nocase(&category), nocase(&name));
        if !seen.insert(key) {
            duplicate += 1;
            continue;
        }
        imported.push(TaskTypeImportRow { category, name, description, is_active: true });
    }

    (imported, duplicate, malformed)
}

fn is_blank_line(rec: &StringRecord) -> bool {
    rec.len() == 1 && rec.get(0) == Some("")
}

fn unrecognized_header_error(rec: &StringRecord) -> Error {
    let fields: Vec<&str> = rec.iter().collect();
    Error::Invalid(format!(
        "unrecognised task-types header {:?}; expected one of {{Category,Name}}, \
         {{Category,Name,Description}}, or \
         {{Category,Name,Description,Usage Count,Status}} (case-insensitive, order-tolerant)",
        fields
    ))
}

// ---------------------------------------------------------------------------------------
// Delimiter & header-schema detection (§4)
// ---------------------------------------------------------------------------------------

enum Shape {
    /// Recognised v1 types header at this record index.
    Recognized(usize, Vec<String>),
    /// A header-shaped row (matches known column vocabulary) that isn't one of the
    /// recognised types schemas — reject, don't reinterpret.
    UnrecognizedHeaded(usize),
    /// No header: data starts at this record index.
    Legacy(usize),
}

/// Try `,` `;` `\t` in that order, parsing the first non-empty **logical** record under
/// each (quoting respected, so a quoted field containing the candidate delimiter never
/// miscounts columns). Pick the delimiter that yields a recognised header, else one that
/// yields a header-shaped-but-unrecognised row, else one with a consistent column count
/// across all rows. Ties (including "none of the above") default to `,` because it is
/// tried first and only a strictly better candidate replaces it.
fn detect(text: &str) -> Result<(u8, Shape)> {
    let candidates: [u8; 3] = [b',', b';', b'\t'];
    let mut best: Option<(u8, Shape, i32)> = None;

    for &d in &candidates {
        let records = parse_records(text, d)?;
        let first_idx = first_meaningful_index(&records);
        let (shape, header_score) = match first_idx {
            None => (Shape::Legacy(records.len()), 0),
            Some(idx) => {
                let norm = normalize_header(&records[idx]);
                if is_recognized_types_header(&norm) {
                    (Shape::Recognized(idx, norm), 2)
                } else if looks_like_header(&norm) {
                    (Shape::UnrecognizedHeaded(idx), 1)
                } else {
                    (Shape::Legacy(idx), 0)
                }
            }
        };
        let expected_len = first_idx.map(|i| records[i].len()).unwrap_or(0);
        let consistent = records
            .iter()
            .filter(|r| !is_blank_line(r))
            .all(|r| r.len() == expected_len);
        // A candidate that never actually splits anything (the delimiter character never
        // appears) is trivially "consistent" at one column each — that must not outscore
        // a candidate that genuinely splits the data, or an unused delimiter would win
        // over the real one whenever the real one has legitimately ragged rows (e.g. the
        // legacy format's optional second column, CSV_FORMAT §3).
        let column_signal = if expected_len > 1 { if consistent { 2 } else { 1 } } else { 0 };
        let score = header_score * 100 + column_signal * 10 + i32::from(consistent);

        let better = match &best {
            None => true,
            Some((_, _, best_score)) => score > *best_score,
        };
        if better {
            best = Some((d, shape, score));
        }
    }

    let (d, shape, _) = best.expect("candidates is non-empty");
    Ok((d, shape))
}

/// Parse the whole text as logical CSV records under `delimiter` (`flexible` so a ragged
/// row doesn't hard-error here — width mismatches are counted as malformed rows instead).
/// Accepts either `\r\n` or `\n` line endings (the `csv` crate handles both).
fn parse_records(text: &str, delimiter: u8) -> Result<Vec<StringRecord>> {
    let mut rdr = ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());
    let mut out = Vec::new();
    for rec in rdr.records() {
        out.push(rec?);
    }
    Ok(out)
}

fn first_meaningful_index(records: &[StringRecord]) -> Option<usize> {
    records.iter().position(|r| !is_blank_line(r))
}

fn normalize_header(rec: &StringRecord) -> Vec<String> {
    rec.iter().map(|f| f.trim().to_ascii_lowercase()).collect()
}

/// Exact-set match (order-tolerant) against the three recognised v1 types header shapes
/// (§3): the full set, or the `{Category, Name}` / `{Category, Name, Description}`
/// subsets.
fn is_recognized_types_header(norm: &[String]) -> bool {
    let set: HashSet<&str> = norm.iter().map(String::as_str).collect();
    if set.len() != norm.len() {
        return false; // duplicate column names never form a valid header
    }
    let full: HashSet<&str> =
        [COL_CATEGORY, COL_NAME, COL_DESCRIPTION, COL_USAGE_COUNT, COL_STATUS].into_iter().collect();
    let ab: HashSet<&str> = [COL_CATEGORY, COL_NAME].into_iter().collect();
    let abc: HashSet<&str> = [COL_CATEGORY, COL_NAME, COL_DESCRIPTION].into_iter().collect();
    set == full || set == ab || set == abc
}

/// Whole-field membership in the known header vocabulary — never a prefix/substring test,
/// so an ordinary data value like "Nameplate" cannot false-fire (§4).
fn looks_like_header(norm: &[String]) -> bool {
    norm.iter().any(|f| KNOWN_HEADER_TOKENS.contains(&f.as_str()))
}

// ---------------------------------------------------------------------------------------
// Encoding (§0)
// ---------------------------------------------------------------------------------------

/// Decode the whole buffer as UTF-8; on failure, fall back to Windows-1252. Strips a
/// leading UTF-8 BOM first. Returns `(text, used_cp1252)`.
fn decode(bytes: &[u8]) -> (String, bool) {
    let bytes = strip_bom(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), false),
        Err(_) => (decode_cp1252(bytes), true),
    }
}

fn strip_bom(bytes: &[u8]) -> &[u8] {
    const BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];
    if bytes.starts_with(&BOM) {
        &bytes[3..]
    } else {
        bytes
    }
}

/// Decode a byte stream as Windows-1252. Identical to ISO-8859-1 for `0x00..=0x7F` and
/// `0xA0..=0xFF`; `0x80..=0x9F` uses the Windows-1252 table (curly quotes, dashes, etc.);
/// the five undefined slots (`0x81`, `0x8D`, `0x8F`, `0x90`, `0x9D`) map to U+FFFD. This
/// decodes almost any byte stream, so a "successful" fallback proves nothing about
/// correctness beyond "it produced text" (CSV_FORMAT §0).
fn decode_cp1252(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len());
    for &b in bytes {
        let cp: u32 = match b {
            0x80 => 0x20AC,
            0x81 => 0xFFFD,
            0x82 => 0x201A,
            0x83 => 0x0192,
            0x84 => 0x201E,
            0x85 => 0x2026,
            0x86 => 0x2020,
            0x87 => 0x2021,
            0x88 => 0x02C6,
            0x89 => 0x2030,
            0x8A => 0x0160,
            0x8B => 0x2039,
            0x8C => 0x0152,
            0x8D => 0xFFFD,
            0x8E => 0x017D,
            0x8F => 0xFFFD,
            0x90 => 0xFFFD,
            0x91 => 0x2018,
            0x92 => 0x2019,
            0x93 => 0x201C,
            0x94 => 0x201D,
            0x95 => 0x2022,
            0x96 => 0x2013,
            0x97 => 0x2014,
            0x98 => 0x02DC,
            0x99 => 0x2122,
            0x9A => 0x0161,
            0x9B => 0x203A,
            0x9C => 0x0153,
            0x9D => 0xFFFD,
            0x9E => 0x017E,
            0x9F => 0x0178,
            other => other as u32, // 0x00-0x7F and 0xA0-0xFF == Latin-1 == the byte value
        };
        s.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crlf(s: &str) -> String {
        // Write test fixtures with plain \n for readability, convert to the on-disk CRLF.
        s.replace('\n', "\r\n")
    }

    // -- Exports: quoting round-trip (§7) ------------------------------------------------

    #[test]
    fn entries_export_quotes_comma_quote_and_newline_and_round_trips() {
        let rows = vec![EntryExportRow {
            id: 1,
            category: "SAKTI".into(),
            task_type: "Rekon".into(),
            date: "2026-01-05".into(),
            count: 3,
            notes: "has, a comma, a \"quote\", and a\nnewline".into(),
            created_at: "2026-01-05 09:00:00.100".into(),
        }];
        let mut buf = Vec::new();
        export_entries(&mut buf, &rows).unwrap();

        let text = String::from_utf8(buf.clone()).unwrap();
        assert!(text.starts_with("ID,Category,Task Type,Date,Count,Notes,Created At\r\n"));
        assert!(!text.contains("\r\n\r\n")); // no stray blank line from the embedded \n

        // Byte-faithful round trip through the csv reader.
        let mut rdr = ReaderBuilder::new().has_headers(true).from_reader(buf.as_slice());
        let rec = rdr.records().next().unwrap().unwrap();
        assert_eq!(rec.get(5).unwrap(), rows[0].notes);
    }

    // -- Delimiter detection (§4/§7) ------------------------------------------------------

    fn types_header_body(sep: &str) -> String {
        format!(
            "Category{sep}Name{sep}Description{sep}Usage Count{sep}Status\nSAKTI{sep}Rekon{sep}reconcile{sep}0{sep}Active\n"
        )
    }

    #[test]
    fn delimiter_detection_agrees_across_comma_semicolon_tab() {
        let comma = types_header_body(",");
        let semi = types_header_body(";");
        let tab = types_header_body("\t");

        let (rows_c, _) = import_task_types(comma.as_bytes(), None).unwrap();
        let (rows_s, _) = import_task_types(semi.as_bytes(), None).unwrap();
        let (rows_t, _) = import_task_types(tab.as_bytes(), None).unwrap();

        assert_eq!(rows_c, rows_s);
        assert_eq!(rows_c, rows_t);
        assert_eq!(rows_c[0].category, "SAKTI");
        assert_eq!(rows_c[0].name, "Rekon");
    }

    #[test]
    fn quoted_semicolon_field_does_not_fool_comma_detection() {
        let text = "Category,Name,Description,Usage Count,Status\n\
                     SAKTI,Rekon,\"Review; urgent\",0,Active\n";
        let (rows, report) = import_task_types(text.as_bytes(), None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].description, "Review; urgent");
        assert_eq!(report.imported, 1);
    }

    // -- Header recognised vs headerless-legacy vs rejected (§3/§4/§7) -------------------

    #[test]
    fn recognized_header_imports() {
        let text = "Category,Name,Description,Usage Count,Status\nSAKTI,Rekon,,0,Active\n";
        let (rows, report) = import_task_types(text.as_bytes(), None).unwrap();
        assert_eq!(rows, vec![TaskTypeImportRow {
            category: "SAKTI".into(),
            name: "Rekon".into(),
            description: "".into(),
            is_active: true,
        }]);
        assert_eq!(report.imported, 1);
    }

    #[test]
    fn headerless_legacy_imports() {
        let text = "SAKTI - Rekon,reconcile\nStandalone\n";
        let (rows, report) = import_task_types(text.as_bytes(), None).unwrap();
        assert_eq!(
            rows,
            vec![
                TaskTypeImportRow {
                    category: "SAKTI".into(),
                    name: "Rekon".into(),
                    description: "reconcile".into(),
                    is_active: true,
                },
                TaskTypeImportRow {
                    category: "Uncategorized".into(),
                    name: "Standalone".into(),
                    description: "".into(),
                    is_active: true,
                },
            ]
        );
        assert_eq!(report.imported, 2);
    }

    #[test]
    fn entries_export_header_fed_to_types_importer_is_rejected() {
        let text = "ID,Category,Task Type,Date,Count,Notes,Created At\n\
                     1,SAKTI,Rekon,2026-01-05,3,,2026-01-05 09:00:00.100\n";
        let err = import_task_types(text.as_bytes(), None).unwrap_err();
        match err {
            Error::Invalid(msg) => assert!(msg.contains("unrecognised"), "{msg}"),
            other => panic!("expected Error::Invalid, got {other:?}"),
        }
    }

    // -- Encoding (§0/§7) ------------------------------------------------------------------

    #[test]
    fn cp1252_buffer_imports_with_flag_and_correct_unicode() {
        // "SAKTI - Rekon\x92s" i.e. curly apostrophe (0x92) and an em dash (0x97) in the
        // description, encoded as raw cp1252 bytes (not valid UTF-8 on their own).
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"SAKTI - Rekon,note ");
        bytes.push(0x91); // U+2018
        bytes.extend_from_slice("x".as_bytes());
        bytes.push(0x92); // U+2019
        bytes.extend_from_slice(b" ");
        bytes.push(0x97); // U+2014 em dash
        bytes.extend_from_slice(b"end\n");

        let (rows, report) = import_task_types(&bytes, None).unwrap();
        assert!(report.used_cp1252);
        assert_eq!(report.encoding, "windows-1252");
        assert_eq!(rows[0].description, "note \u{2018}x\u{2019} \u{2014}end");
    }

    #[test]
    fn bom_prefixed_utf8_buffer_imports() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"Category,Name\nSAKTI,Rekon\n");
        let (rows, report) = import_task_types(&bytes, None).unwrap();
        assert!(!report.used_cp1252);
        assert_eq!(rows[0].category, "SAKTI");
    }

    #[test]
    fn csv_syntax_error_does_not_trigger_encoding_fallback() {
        // Malformed/irregular CSV content (a stray character right after a closing quote,
        // an unterminated quote) is still valid UTF-8, and the `csv` crate — run with
        // `flexible(true)` so ragged rows are reported as malformed rows rather than a
        // hard abort (CSV_FORMAT §3) — parses it leniently rather than raising a hard
        // `csv::Error` (verified empirically: no textual input reaches that path with
        // `flexible(true)`; only an I/O error would). What §0 actually requires is that
        // decoding is decided once, before any CSV parsing, and never retried based on
        // how the CSV parse goes — `decode()` runs first and its result is never revisited
        // regardless of downstream parse outcome. These buffers, despite being odd CSV,
        // must therefore always report the encoding as UTF-8, never fall back to cp1252.
        for text in [
            "Category,Name\n\"SAKTI\"x,Rekon\n",
            "Category,Name\n\"SAKTI,Rekon\n",
        ] {
            let (_, report) = import_task_types(text.as_bytes(), None).unwrap();
            assert!(!report.used_cp1252, "{text:?} should decode as utf-8, got {report:?}");
            assert_eq!(report.encoding, "utf-8");
        }
    }

    // -- Round trip incl. two categories sharing a short name, field equality (§7) -------

    #[test]
    fn round_trip_two_categories_sharing_a_short_name() {
        let rows = vec![
            TaskTypeExportRow {
                category: "SAKTI".into(),
                name: "Rekon".into(),
                description: "reconcile SAKTI".into(),
                usage_count: 5,
                is_active: true,
            },
            TaskTypeExportRow {
                category: "DIGIPAY".into(),
                name: "Rekon".into(),
                description: "reconcile DIGIPAY".into(),
                usage_count: 0,
                is_active: false,
            },
        ];
        let mut buf = Vec::new();
        export_task_types(&mut buf, &rows).unwrap();

        let (imported, report) = import_task_types(&buf, None).unwrap();
        assert_eq!(report.imported, 2);
        assert_eq!(report.skipped_duplicate, 0);
        assert_eq!(report.skipped_malformed, 0);

        assert_eq!(imported[0].category, "SAKTI");
        assert_eq!(imported[0].name, "Rekon");
        assert_eq!(imported[0].description, "reconcile SAKTI");
        assert!(imported[0].is_active);

        assert_eq!(imported[1].category, "DIGIPAY");
        assert_eq!(imported[1].name, "Rekon");
        assert_eq!(imported[1].description, "reconcile DIGIPAY");
        assert!(!imported[1].is_active);
    }

    // -- Duplicates & malformed (§3/§7) ---------------------------------------------------

    #[test]
    fn duplicates_within_file_ascii_case_insensitive_skipped_and_counted() {
        let text = "Category,Name\nSAKTI,Rekon\nsakti,rekon\nSAKTI,Posting\n";
        let (rows, report) = import_task_types(text.as_bytes(), None).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(report.imported, 2);
        assert_eq!(report.skipped_duplicate, 1);
    }

    #[test]
    fn blank_name_skipped_as_malformed() {
        let text = "Category,Name\nSAKTI,\nSAKTI,Rekon\n";
        let (rows, report) = import_task_types(text.as_bytes(), None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(report.skipped_malformed, 1);
    }

    // -- Legacy split matches the migration rule (§6/§7) ----------------------------------

    #[test]
    fn legacy_split_matches_migration_rule() {
        assert_eq!(split_legacy("SAKTI - Rekon"), ("SAKTI".into(), "Rekon".into()));
        assert_eq!(split_legacy("Standalone"), ("Uncategorized".into(), "Standalone".into()));
        // First " - " only: a name containing a second separator keeps the rest in `name`.
        assert_eq!(split_legacy("A - B - C"), ("A".into(), "B - C".into()));
    }

    // -- Insights export: ranked rows, repeated range, header-only on empty (§5/§7) ------

    #[test]
    fn insights_export_repeats_range_and_empty_input_is_header_only() {
        let rows = vec![
            InsightRow { category: "SAKTI".into(), task_type: "Rekon".into(), count: 10 },
            InsightRow { category: "DIGIPAY".into(), task_type: "Bayar".into(), count: 4 },
        ];
        let mut buf = Vec::new();
        export_insights(&mut buf, &rows, "2026-01-01", "2026-01-31").unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert_eq!(
            text,
            crlf(
                "Category,Task Type,Count,Range Start,Range End\n\
                 SAKTI,Rekon,10,2026-01-01,2026-01-31\n\
                 DIGIPAY,Bayar,4,2026-01-01,2026-01-31\n"
            )
        );

        let mut empty_buf = Vec::new();
        export_insights(&mut empty_buf, &[], "2026-01-01", "2026-01-31").unwrap();
        let empty_text = String::from_utf8(empty_buf).unwrap();
        assert_eq!(empty_text, crlf("Category,Task Type,Count,Range Start,Range End\n"));
        assert_eq!(empty_text.lines().count(), 1);
    }
}
