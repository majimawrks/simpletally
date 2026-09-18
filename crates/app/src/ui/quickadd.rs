//! Quick-add popup: a single-line query box that filters/ranks active task types and logs a
//! tally with one keystroke (Enter). Summoned by the platform shell (a global hotkey lives
//! under `crates/app/src/platform/`, out of scope here) — this module only paints one frame
//! and reports what happened via [`Action`].
//!
//! Always dark (`theme::popup()`), independent of the app's light/dark [`super::theme::Mode`]
//! — it's a floating overlay, not a screen. Layout uses the explicit-rect banding pattern
//! from `today.rs`/`types.rs`'s LAYOUT TRAP note: each row is one allocated rect painted
//! directly, never `allocate_ui_with_layout` inside a `horizontal` or a nested
//! `right_to_left`, both of which staircase/overdraw sibling controls.

use std::collections::BTreeMap;

use chrono::{Datelike, NaiveDate};
use nucleo::{Config, Matcher, Utf32Str};

use crate::ui::theme::{self as t};
use crate::ui::widgets::glyph;
use simpletally_core::{Db, TaskType};

/// Popup window width (logical px) — the platform shell sizes its window to this.
pub const WIDTH: f32 = 560.0;

const INPUT_ROW_H: f32 = 64.0;
const LIST_PAD: f32 = 8.0;
const ROW_H: f32 = 44.0;
const FOOTER_H: f32 = 38.0;
const MAX_ROWS: usize = 6;

/// What one painted frame concluded; the caller (platform shell) acts on it after `show`
/// returns.
pub enum Action {
    None,
    /// Esc, or a click outside a row — the caller hides the popup.
    Dismiss,
    /// A tally was written. Carries a one-line confirmation, e.g. "+3 Reset Password".
    Logged(String),
}

/// One ranked result row, resolved from the DB snapshot for painting.
struct Row {
    task_type_id: i64,
    name: String,
    category_name: String,
    today_count: i64,
}

/// Query results, rebuilt from the DB only when `dirty` — mirrors `today.rs`/`types.rs`'s
/// `View` pattern so `state` stays free to mutate (selection, query text) between reloads.
struct View {
    types: Vec<TaskType>,
    today_counts: BTreeMap<i64, i64>,
    categories: BTreeMap<i64, String>,
}

pub struct QuickAddState {
    query: String,
    selected: usize,
    dirty: bool,
    view: Option<View>,
    error: Option<String>,
}

impl QuickAddState {
    pub fn new() -> Self {
        Self { query: String::new(), selected: 0, dirty: true, view: None, error: None }
    }

    /// Clear query + selection and force a data reload. Called when the popup is summoned.
    pub fn reset(&mut self) {
        self.query.clear();
        self.selected = 0;
        self.error = None;
        self.dirty = true;
    }

    /// Force a data reload on the next frame (e.g. after the database is swapped).
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }
}

impl Default for QuickAddState {
    fn default() -> Self {
        Self::new()
    }
}

/// `YYYY-MM-DD` for the DB — same approach as `today.rs`'s `date_sql`.
fn date_sql(d: NaiveDate) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day())
}

/// Splits `raw` into (search text, count). A trailing integer in `1..=9999` is the count
/// only when there's non-empty text before it — otherwise the whole trimmed string is
/// search text and the count defaults to 1.
pub fn parse_query(raw: &str) -> (String, i64) {
    let trimmed = raw.trim();
    if let Some(idx) = trimmed.rfind(char::is_whitespace) {
        let head = trimmed[..idx].trim_end();
        let tail = trimmed[idx..].trim_start();
        if !head.is_empty() {
            if let Ok(n) = tail.parse::<i64>() {
                if (1..=9999).contains(&n) {
                    return (head.to_string(), n);
                }
            }
        }
    }
    (trimmed.to_string(), 1)
}

/// Ranks active types by `text` against name + description (name hits outrank description
/// hits — `max(name_score * 2, description_score)`), or by today's tally count when `text`
/// is empty. Returns task-type ids in display order, capped to `limit`. Pure/data-only so
/// it's unit-testable without a database or egui.
pub fn rank(
    types: &[TaskType],
    today_counts: &BTreeMap<i64, i64>,
    text: &str,
    limit: usize,
) -> Vec<i64> {
    let active: Vec<&TaskType> = types.iter().filter(|t| t.is_active).collect();

    if text.trim().is_empty() {
        let mut rows = active;
        rows.sort_by(|a, b| {
            let ca = today_counts.get(&a.id).copied().unwrap_or(0);
            let cb = today_counts.get(&b.id).copied().unwrap_or(0);
            cb.cmp(&ca).then_with(|| a.name.cmp(&b.name))
        });
        return rows.into_iter().take(limit).map(|t| t.id).collect();
    }

    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut needle_buf = Vec::new();
    let needle = Utf32Str::new(text, &mut needle_buf);

    let mut scored: Vec<(&TaskType, u32)> = Vec::new();
    for ty in active {
        let mut name_buf = Vec::new();
        let name_score =
            matcher.fuzzy_match(Utf32Str::new(&ty.name, &mut name_buf), needle).unwrap_or(0) as u32;

        let mut desc_buf = Vec::new();
        let desc_score = matcher
            .fuzzy_match(Utf32Str::new(&ty.description, &mut desc_buf), needle)
            .unwrap_or(0) as u32;

        let score = (name_score * 2).max(desc_score);
        if score > 0 {
            scored.push((ty, score));
        }
    }

    scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.name.cmp(&b.0.name)));
    scored.into_iter().take(limit).map(|(ty, _)| ty.id).collect()
}

fn load(db: &Db) -> Result<View, simpletally_core::Error> {
    let categories = db
        .list_categories()?
        .into_iter()
        .map(|c| (c.id, c.name))
        .collect();
    Ok(View {
        types: db.list_task_types(true)?,
        today_counts: BTreeMap::new(), // filled by caller once `today` is known — see `show`.
        categories,
    })
}

/// The window height, in logical pixels, that the current state needs: input row + list
/// (at least one row, for the "no match" line) + footer.
pub fn height_for(state: &QuickAddState) -> f32 {
    let row_count = state
        .view
        .as_ref()
        .map(|v| rows_for(v, &state.query).max(1))
        .unwrap_or(1);
    INPUT_ROW_H + (LIST_PAD * 2.0 + row_count as f32 * ROW_H) + FOOTER_H
}

/// How many result rows the current query would produce, without building the painted rows.
fn rows_for(view: &View, query: &str) -> usize {
    let (text, _) = parse_query(query);
    rank(&view.types, &view.today_counts, &text, MAX_ROWS).len()
}

/// Paint one frame. `today` is the date the tally is logged against.
pub fn show(
    ui: &mut egui::Ui,
    state: &mut QuickAddState,
    db: &Db,
    today: chrono::NaiveDate,
) -> Action {
    let pt = t::popup();
    let today_sql = date_sql(today);

    if state.dirty {
        state.dirty = false;
        match load(db) {
            Ok(mut view) => {
                // A failure here must be surfaced, not defaulted away: an empty map makes
                // every preview read `+3 → 3` instead of `+3 → 7`, which looks like real
                // data rather than a missing query.
                match db.day_type_totals(&today_sql) {
                    Ok(rows) => {
                        view.today_counts =
                            rows.into_iter().map(|r| (r.task_type_id, r.count)).collect();
                        state.error = None;
                    }
                    Err(e) => state.error = Some(e.to_string()),
                }
                state.view = Some(view);
            }
            Err(e) => {
                state.view = None;
                state.error = Some(e.to_string());
            }
        }
    }

    let (text, count) = parse_query(&state.query);
    let ids = state
        .view
        .as_ref()
        .map(|v| rank(&v.types, &v.today_counts, &text, MAX_ROWS))
        .unwrap_or_default();
    let rows: Vec<Row> = ids
        .into_iter()
        .filter_map(|id| {
            let view = state.view.as_ref()?;
            let ty = view.types.iter().find(|t| t.id == id)?;
            Some(Row {
                task_type_id: ty.id,
                name: ty.name.clone(),
                category_name: view.categories.get(&ty.category_id).cloned().unwrap_or_default(),
                today_count: view.today_counts.get(&ty.id).copied().unwrap_or(0),
            })
        })
        .collect();

    if state.selected >= rows.len().max(1) {
        state.selected = rows.len().saturating_sub(1);
    }

    // Keyboard is consumed *before* the TextEdit is drawn, so arrow/enter/escape never fall
    // through to the text field's own key handling (which would move the cursor instead).
    let mut action = Action::None;
    let move_down = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown));
    let move_up = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp));
    let confirm = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
    let escape = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));

    if move_down && !rows.is_empty() {
        state.selected = (state.selected + 1).min(rows.len() - 1);
    }
    if move_up {
        state.selected = state.selected.saturating_sub(1);
    }
    if escape {
        action = Action::Dismiss;
    }
    if confirm {
        if let Some(row) = rows.get(state.selected) {
            match db.add_tally(row.task_type_id, &today_sql, count, "") {
                Ok(_) => {
                    action = Action::Logged(format!("+{count} {}", row.name));
                }
                Err(e) => {
                    // A write failure must not dismiss the popup — surface it in place of
                    // the footer hint and let the user retry.
                    state.error = Some(e.to_string());
                }
            }
        }
    }

    let full = ui.max_rect();
    let bg_rect = egui::Rect::from_min_size(full.min, egui::vec2(WIDTH, height_for(state)));
    // The window itself is rounded by the compositor (`winos::round_corners`), so this radius
    // only has to agree with it — painting square corners here would show as dark wedges.
    ui.painter().rect(
        bg_rect,
        egui::CornerRadius::same(10),
        pt.bg,
        egui::Stroke::new(1.0, pt.border),
        egui::StrokeKind::Inside,
    );

    let input_rect =
        egui::Rect::from_min_size(bg_rect.min, egui::vec2(WIDTH, INPUT_ROW_H));
    paint_input_row(ui, &pt, input_rect, &state.query);
    ui.painter().line_segment(
        [egui::pos2(input_rect.left(), input_rect.bottom()), egui::pos2(input_rect.right(), input_rect.bottom())],
        egui::Stroke::new(1.0, pt.divider),
    );

    // A transparent `TextEdit` overlaid on the painted input row: it owns focus/cursor/IME
    // but the row's chrome (badge, hint) is painted separately so the row can be laid out
    // without fighting egui's built-in widget frame.
    let edit_rect = egui::Rect::from_min_size(
        egui::pos2(input_rect.left() + 22.0 + 22.0 + 12.0, input_rect.top()),
        egui::vec2((input_rect.width() - 22.0 - 22.0 - 12.0 - 22.0 - 120.0).max(0.0), input_rect.height()),
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new().max_rect(edit_rect).layout(egui::Layout::top_down(egui::Align::Min)),
    );
    child.visuals_mut().override_text_color = Some(pt.query_text);
    let response = child.add(
        egui::TextEdit::singleline(&mut state.query)
            .font(t::mono(t::QUICK_ADD))
            .frame(egui::Frame::NONE)
            .text_color(pt.query_text)
            .vertical_align(egui::Align::Center)
            .desired_width(edit_rect.width()),
    );
    response.request_focus();

    let list_top = input_rect.bottom();
    let list_h = LIST_PAD * 2.0 + rows.len().max(1) as f32 * ROW_H;
    let list_rect = egui::Rect::from_min_size(egui::pos2(bg_rect.left(), list_top), egui::vec2(WIDTH, list_h));

    if rows.is_empty() {
        ui.painter().text(
            list_rect.center(),
            egui::Align2::CENTER_CENTER,
            "no match",
            t::sans(t::BODY),
            pt.muted_text,
        );
    } else {
        for (i, row) in rows.iter().enumerate() {
            let row_rect = egui::Rect::from_min_size(
                egui::pos2(list_rect.left() + LIST_PAD, list_rect.top() + LIST_PAD + i as f32 * ROW_H),
                egui::vec2(WIDTH - LIST_PAD * 2.0, ROW_H),
            );
            let selected = i == state.selected;
            let resp = ui.interact(row_rect, ui.id().with(("quickadd_row", row.task_type_id)), egui::Sense::click());
            if resp.clicked() {
                state.selected = i;
                match db.add_tally(row.task_type_id, &today_sql, count, "") {
                    Ok(_) => action = Action::Logged(format!("+{count} {}", row.name)),
                    Err(e) => state.error = Some(e.to_string()),
                }
            }
            paint_row(ui, &pt, row_rect, row, count, selected);
        }
    }

    let footer_rect =
        egui::Rect::from_min_size(egui::pos2(bg_rect.left(), list_top + list_h), egui::vec2(WIDTH, FOOTER_H));
    paint_footer(ui, &pt, footer_rect, state.error.as_deref());

    // NB: a click on the popup's own padding, badge or footer does nothing. Dismissing there
    // would throw away a half-typed query for a click the user could only have made by
    // missing. Click-*away* dismissal belongs to the platform shell, which sees focus loss.

    action
}

fn paint_input_row(ui: &mut egui::Ui, pt: &t::PopupTheme, rect: egui::Rect, _query: &str) {
    let painter = ui.painter();
    let badge_rect = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 22.0, rect.center().y - 11.0),
        egui::vec2(22.0, 22.0),
    );
    painter.rect(badge_rect, egui::CornerRadius::same(6), pt.accent, egui::Stroke::NONE, egui::StrokeKind::Inside);
    glyph::zheng(painter, badge_rect, pt.badge_glyph, 1.6);

    painter.text(
        egui::pos2(rect.right() - 22.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        "Enter to log",
        t::mono(11.0),
        pt.muted_text,
    );
}

fn paint_row(ui: &mut egui::Ui, pt: &t::PopupTheme, rect: egui::Rect, row: &Row, count: i64, selected: bool) {
    let painter = ui.painter();
    if selected {
        painter.rect(
            rect,
            egui::CornerRadius::same(7),
            pt.row_selected_bg,
            egui::Stroke::new(1.0, pt.row_selected_border),
            egui::StrokeKind::Inside,
        );
    }

    let name_color = if selected { pt.query_text } else { pt.row_name };
    let name_galley =
        // 14.5 per UI_SPEC; SECTION_TITLE is that size, TILE_NAME (14.0) is the grid's.
        ui.painter().layout_no_wrap(row.name.clone(), t::sans(t::SECTION_TITLE), name_color);
    let name_w = name_galley.rect.width();
    painter.galley(
        egui::pos2(rect.left() + 14.0, rect.center().y - name_galley.rect.height() / 2.0),
        name_galley,
        name_color,
    );

    painter.text(
        egui::pos2(rect.left() + 14.0 + name_w + 10.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        row.category_name.to_uppercase(),
        t::mono(10.5),
        pt.row_category,
    );

    let resulting = row.today_count + count;
    painter.text(
        egui::pos2(rect.right() - 12.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        format!("+{count} \u{2192} {resulting}"),
        t::mono(13.0),
        pt.accent,
    );
}

fn paint_footer(ui: &mut egui::Ui, pt: &t::PopupTheme, rect: egui::Rect, error: Option<&str>) {
    let painter = ui.painter();
    painter.rect(rect, egui::CornerRadius::same(0), pt.footer_bg, egui::Stroke::new(1.0, pt.divider), egui::StrokeKind::Inside);

    let text = error.unwrap_or("\u{2191}\u{2193} choose    trailing digits = count    Esc dismiss");
    let color = if error.is_some() { pt.accent } else { pt.muted_text };
    painter.text(
        egui::pos2(rect.left() + 22.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        text,
        t::mono(11.0),
        color,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ty(id: i64, category_id: i64, name: &str, description: &str, is_active: bool) -> TaskType {
        TaskType {
            id,
            category_id,
            name: name.to_string(),
            description: description.to_string(),
            is_active,
            created_at: "2026-01-01 00:00:00.000".to_string(),
        }
    }

    // --- parse_query ---

    #[test]
    fn parse_query_plain_text() {
        assert_eq!(parse_query("reset pass"), ("reset pass".to_string(), 1));
    }

    #[test]
    fn parse_query_trailing_count() {
        assert_eq!(parse_query("reset pass 3"), ("reset pass".to_string(), 3));
    }

    #[test]
    fn parse_query_count_with_multi_word_text() {
        assert_eq!(parse_query("close support ticket 12"), ("close support ticket".to_string(), 12));
    }

    #[test]
    fn parse_query_bare_number_is_search_text() {
        assert_eq!(parse_query("3"), ("3".to_string(), 1));
    }

    #[test]
    fn parse_query_zero_count_stays_in_text() {
        assert_eq!(parse_query("reset pass 0"), ("reset pass 0".to_string(), 1));
    }

    #[test]
    fn parse_query_out_of_range_count_stays_in_text() {
        assert_eq!(parse_query("reset pass 99999"), ("reset pass 99999".to_string(), 1));
    }

    #[test]
    fn parse_query_trailing_whitespace_trimmed() {
        assert_eq!(parse_query("reset pass 3   "), ("reset pass".to_string(), 3));
    }

    #[test]
    fn parse_query_empty_string() {
        assert_eq!(parse_query(""), ("".to_string(), 1));
    }

    /// The split slices on a byte index from `rfind`, so a multi-byte char anywhere in the
    /// query is the case that would panic if that index ever stopped landing on a boundary.
    /// Type names here are Indonesian and the user can type anything.
    #[test]
    fn parse_query_handles_multi_byte_characters() {
        assert_eq!(parse_query("perékaman berkas 3"), ("perékaman berkas".to_string(), 3));
        assert_eq!(parse_query("笔记 2"), ("笔记".to_string(), 2));
        assert_eq!(parse_query("café"), ("café".to_string(), 1));
        // Non-breaking space is whitespace to `char::is_whitespace`, and multi-byte.
        assert_eq!(parse_query("berkas\u{a0}4"), ("berkas".to_string(), 4));
    }

    /// A negative number parses fine as an `i64`, so only the range check keeps it out of
    /// `add_tally`, where core's `CHECK (count > 0)` would reject it.
    #[test]
    fn parse_query_negative_count_stays_in_text() {
        assert_eq!(parse_query("reset pass -3"), ("reset pass -3".to_string(), 1));
        assert_eq!(parse_query("reset pass 007"), ("reset pass".to_string(), 7));
    }

    // --- rank ---

    #[test]
    fn rank_name_hit_beats_description_hit() {
        let types = vec![
            ty(1, 1, "Unrelated", "reset password for a client", true),
            ty(2, 1, "Reset Password", "administrative task", true),
        ];
        let counts = BTreeMap::new();
        let ids = rank(&types, &counts, "reset password", 6);
        assert_eq!(ids, vec![2, 1]);
    }

    #[test]
    fn rank_excludes_non_matches() {
        let types = vec![ty(1, 1, "Reset Password", "", true), ty(2, 1, "File Taxes", "", true)];
        let counts = BTreeMap::new();
        let ids = rank(&types, &counts, "reset", 6);
        assert_eq!(ids, vec![1]);
    }

    #[test]
    fn rank_honours_limit() {
        let types = vec![
            ty(1, 1, "Task A", "", true),
            ty(2, 1, "Task B", "", true),
            ty(3, 1, "Task C", "", true),
        ];
        let counts = BTreeMap::new();
        let ids = rank(&types, &counts, "task", 2);
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn rank_empty_text_falls_back_to_today_count_order() {
        let types = vec![
            ty(1, 1, "Alpha", "", true),
            ty(2, 1, "Beta", "", true),
            ty(3, 1, "Gamma", "", true),
        ];
        let mut counts = BTreeMap::new();
        counts.insert(1, 2);
        counts.insert(3, 5);
        // Beta has no entry (defaults to 0), so order is Gamma(5), Alpha(2), Beta(0).
        let ids = rank(&types, &counts, "", 6);
        assert_eq!(ids, vec![3, 1, 2]);
    }

    #[test]
    fn rank_never_returns_inactive_types() {
        let types = vec![ty(1, 1, "Reset Password", "", false), ty(2, 1, "Reset Passcode", "", true)];
        let counts = BTreeMap::new();
        let ids = rank(&types, &counts, "reset", 6);
        assert_eq!(ids, vec![2]);
    }
}
