//! The Task types screen (Screen 05 — Phase 3B; design `_rustrefactor/README.md` §"05 — Task
//! types"). Manages the master list of task types: search/filter, multi-select, create/edit,
//! activate/deactivate, CSV import, and category management.
//!
//! Mirrors `today.rs`/`insights.rs`'s shape: a `dirty` flag drives a `View` rebuild, a `Snap`
//! is frozen per frame so `state` stays free to mutate during interaction, and layout uses the
//! explicit-rect banding pattern (see the LAYOUT TRAP note in both of those files) rather than
//! `allocate_ui_with_layout` inside a horizontal, which staircases children.
//!
//! **Out of scope (Phase 3C):** delete, merge-into, trash/restore/purge. Where the design shows
//! a delete control, nothing is rendered.

use crate::service::import::CombinedReport;
use crate::ui::theme::{self as t, Theme};
use chrono::Datelike;
use std::collections::BTreeSet;

use simpletally_core::{
    aggregate::{TypeLifetimeTotal, TypeUsage},
    Category, Db, Error, TaskType,
};

/// Which category the table is filtered to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CategoryFilter {
    All,
    One(i64),
}

/// Whether the table shows only active types or everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveFilter {
    ActiveOnly,
    All,
}

pub struct TypesState {
    pub search: String,
    pub category_filter: CategoryFilter,
    pub active_filter: ActiveFilter,
    pub selection: BTreeSet<i64>,
    /// A refused write, shown beside the caption. Transient; cleared on the next success
    /// or rebuild. Query failures use `error` and replace the screen.
    notice: Option<String>,
    modal: Modal,
    dirty: bool,
    view: Option<View>,
    error: Option<String>,
    /// Set when the category-management modal writes something that changes the Today
    /// screen's category pill row (create/rename/reorder/remove). `app.rs` drains this
    /// after each frame and marks `TodayState` dirty so the pills pick it up.
    categories_changed: bool,
}

enum Modal {
    None,
    Edit(TypeForm),
    ImportReport(ImportOutcome),
    ManageCategories(CategoriesModal),
}

enum ImportOutcome {
    Report(CombinedReport),
    Error(String),
}

/// Draft state for the new/edit type modal (design 09, screen `_rustrefactor/screens/
/// 09-edit-task-type.png`).
struct TypeForm {
    /// `None` for "new type", `Some(id)` for "edit type".
    target_id: Option<i64>,
    category_id: i64,
    name: String,
    description: String,
    is_active: bool,
    /// A save-time error that isn't the live duplicate-name check below (e.g. a DB error).
    error: Option<String>,
    /// Filter text inside the "more" category popup.
    category_filter: String,
    /// True while that popup is open.
    picker_open: bool,
    /// `Some(buffer)` while the inline "+ New category" field is open in place of its pill.
    new_category: Option<String>,
    new_category_error: Option<String>,
    /// This type's lifetime usage, for the title-row eyebrow and the delete dialog. `None`
    /// for a new type (nothing to show yet) or if the query failed.
    usage: Option<TypeUsage>,
    /// Open on top of this modal when "Delete type" is clicked.
    delete: Option<DeleteForm>,
}

/// Which of the delete dialog's three outcomes is selected (design: Deactivate preselected).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeleteChoice {
    Deactivate,
    Merge,
    Trash,
}

/// Draft state for the delete-type modal opened from the edit dialog's "Delete type" link.
struct DeleteForm {
    type_id: i64,
    type_name: String,
    usage: TypeUsage,
    choice: DeleteChoice,
    /// The chosen merge target, `None` until the picker's radio is used. Confirm is disabled
    /// on the merge branch until this is set.
    merge_target: Option<i64>,
    /// Every other **active** type, as `(id, "CATEGORY", name)`, for the merge picker.
    others: Vec<(i64, String, String)>,
    error: Option<String>,
}

/// The category-management modal's rows (rename-in-place + drag reorder + remove-or-move).
/// Screen 08; see `manage_categories_modal` below.
struct CategoriesModal {
    rows: Vec<CatRow>,
    new_name: String,
    error: Option<String>,
    /// Set while a row's drag handle is held.
    drag: Option<DragState>,
    /// Open when "Remove" is clicked on a category that still holds task types.
    move_dialog: Option<MoveDialogState>,
}

struct DragState {
    id: i64,
    start_index: usize,
    /// Pointer y minus the row's top y, captured at drag start, so the row follows the
    /// pointer directly instead of snapping its centre under it.
    grab_offset: f32,
}

struct CatRow {
    id: i64,
    name: String,
    type_count: i64,
    tally_count: i64,
    /// `Some` while this row is the one being renamed in place.
    rename: Option<RenameState>,
}

struct RenameState {
    buffer: String,
    /// True for the one frame the row entered rename mode, so the text edit grabs focus
    /// once rather than fighting the user's later click-away every frame.
    just_opened: bool,
    error: Option<String>,
}

/// The "remove a category that still holds task types" dialog: move its types elsewhere
/// first, then delete it.
struct MoveDialogState {
    category_id: i64,
    category_name: String,
    type_count: i64,
    tally_count: i64,
    /// The doomed category's task types, kept in full so each can be re-pointed via
    /// `edit_task_type` on confirm.
    types: Vec<TaskType>,
    /// Every other category as `(id, name, type_count)`, for the radio list.
    others: Vec<(i64, String, i64)>,
    target: i64,
    error: Option<String>,
}

/// The screen's query results, rebuilt from the DB when `dirty`.
struct View {
    categories: Vec<Category>,
    /// All types, active and inactive — filters are applied at render time.
    types: Vec<TaskType>,
    lifetime: std::collections::BTreeMap<i64, i64>,
}

impl TypesState {
    pub fn new() -> Self {
        Self {
            search: String::new(),
            category_filter: CategoryFilter::All,
            // All, not ActiveOnly: with the status pill toggling a row in place,
            // deactivating one would otherwise make it vanish out from under the user.
            active_filter: ActiveFilter::All,
            selection: BTreeSet::new(),
            notice: None,
            modal: Modal::None,
            dirty: true,
            view: None,
            error: None,
            categories_changed: false,
        }
    }

    /// `pub(crate)`: also called from `app.rs` after a migration import replaces the whole
    /// database underneath every screen.
    pub(crate) fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Reset for a **different database** (the migration import swaps the whole file out).
    /// Marking the view dirty is not enough: any open dialog still holds raw row ids from
    /// the old database, and ids are small sequential integers, so saving one against the
    /// newly imported data would silently overwrite an unrelated row.
    pub(crate) fn reset_for_new_database(&mut self) {
        self.modal = Modal::None;
        self.selection.clear();
        self.notice = None;
        self.error = None;
        self.mark_dirty();
    }

    /// Consumes the cross-screen "category data changed" flag (see field doc). Called by
    /// `app.rs` once per frame after this screen's `show`.
    pub fn take_categories_changed(&mut self) -> bool {
        std::mem::take(&mut self.categories_changed)
    }

    fn ensure_view(&mut self, db: &Db) {
        if !self.dirty && self.view.is_some() {
            return;
        }
        match self.rebuild(db) {
            Ok(v) => {
                self.view = Some(v);
                self.error = None;
                self.dirty = false;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn rebuild(&self, db: &Db) -> simpletally_core::Result<View> {
        let categories = db.list_categories()?;
        let types = db.list_task_types(false)?; // both active and inactive
        let lifetime = db
            .type_lifetime_totals()?
            .into_iter()
            .map(|t| (t.task_type_id, t.total))
            .collect();
        Ok(View { categories, types, lifetime })
    }
}

impl Default for TypesState {
    fn default() -> Self {
        Self::new()
    }
}

// --- pure logic (unit-tested below; no egui) -----------------------------------------------

/// Case-insensitive substring match over name AND description, combined with the category and
/// active filters. Pure so it can be unit-tested without egui.
pub fn filter_types<'a>(
    types: &'a [TaskType],
    search: &str,
    category_filter: CategoryFilter,
    active_filter: ActiveFilter,
) -> Vec<&'a TaskType> {
    let needle = search.trim().to_lowercase();
    types
        .iter()
        .filter(|t| {
            if let CategoryFilter::One(id) = category_filter {
                if t.category_id != id {
                    return false;
                }
            }
            if let ActiveFilter::ActiveOnly = active_filter {
                if !t.is_active {
                    return false;
                }
            }
            if needle.is_empty() {
                return true;
            }
            t.name.to_lowercase().contains(&needle) || t.description.to_lowercase().contains(&needle)
        })
        .collect()
}

/// Moves the id at `from` to sit at index `to` (post-removal indexing, so `to == len - 1`
/// means "last"), leaving every other id's relative order intact. Pure so the drag-reorder
/// index math is unit-testable without egui. A no-op (`from == to` or an out-of-range
/// `from`) returns `ids` unchanged.
pub fn reordered_ids(ids: &[i64], from: usize, to: usize) -> Vec<i64> {
    let mut v = ids.to_vec();
    if from == to || from >= v.len() {
        return v;
    }
    let item = v.remove(from);
    v.insert(to.min(v.len()), item);
    v
}

/// The slot index a dragged row currently occupies, given the pointer's y, the grab offset
/// captured at drag start (pointer y minus the row's top at that moment, so the row follows
/// the pointer rather than snapping its centre under it), the list's top y, and the uniform
/// slot height (row height + gap). Pure so the drag math is unit-testable without egui.
pub fn drag_target_index(pointer_y: f32, grab_offset: f32, list_top: f32, slot_h: f32, row_count: usize) -> usize {
    if row_count == 0 {
        return 0;
    }
    let row_top = pointer_y - grab_offset;
    let idx = ((row_top - list_top) / slot_h).round().max(0.0) as usize;
    idx.min(row_count - 1)
}

/// One category's aggregated row data for the manage-categories modal.
pub struct CatAgg {
    pub id: i64,
    pub name: String,
    pub type_count: i64,
    pub tally_count: i64,
}

/// Per-category type count and summed lifetime-tally count, from raw DB rows. Pure so it's
/// unit-testable without a `Db`.
pub fn aggregate_categories(
    categories: &[Category],
    task_types: &[TaskType],
    lifetime_totals: &[TypeLifetimeTotal],
) -> Vec<CatAgg> {
    categories
        .iter()
        .map(|c| {
            let cat_types: Vec<&TaskType> = task_types.iter().filter(|t| t.category_id == c.id).collect();
            let tally_count: i64 = cat_types
                .iter()
                .map(|t| {
                    lifetime_totals.iter().find(|l| l.task_type_id == t.id).map(|l| l.total).unwrap_or(0)
                })
                .sum();
            CatAgg { id: c.id, name: c.name.clone(), type_count: cat_types.len() as i64, tally_count }
        })
        .collect()
}

/// Which categories earn a pill in the edit dialog: the `n` most-used (by tallies, ties
/// keeping the user's own drag order), plus the selected one wherever it ranks — a form
/// must never look like nothing is chosen. Returns the visible ids **in the user's order**
/// and how many are left for the "more" popup.
pub fn pill_slots(cats: &[CatAgg], selected: i64, n: usize) -> (Vec<i64>, usize) {
    let mut ranked: Vec<&CatAgg> = cats.iter().collect();
    // Stable sort: equal usage keeps the order the user dragged them into.
    ranked.sort_by_key(|c| std::cmp::Reverse(c.tally_count));
    let mut shown: std::collections::BTreeSet<i64> =
        ranked.iter().take(n).map(|c| c.id).collect();
    if cats.iter().any(|c| c.id == selected) {
        shown.insert(selected);
    }
    let visible: Vec<i64> = cats.iter().filter(|c| shown.contains(&c.id)).map(|c| c.id).collect();
    let hidden = cats.len().saturating_sub(visible.len());
    (visible, hidden)
}

/// Live validation for the new/edit type form's Name field: trimmed-empty, or a
/// case-insensitive duplicate of another type **in the same category** (excluding the type
/// being edited, so it can keep its own name). Pure so it's unit-tested without egui or a
/// `Db`. The returned message omits the category's own name — the caller (which has the
/// `Category` list, not just ids) prefixes it, e.g. `"SAKTI already has a type called ..."`.
pub fn validate_type_name(
    types: &[TaskType],
    category_id: i64,
    name: &str,
    editing_id: Option<i64>,
) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Name can't be empty.".to_string());
    }
    if let Some(dup) = types.iter().find(|t| {
        t.category_id == category_id
            && editing_id != Some(t.id)
            && t.name.trim().eq_ignore_ascii_case(trimmed)
    }) {
        return Err(format!(
            "already has a type called {}. Names must be unique inside a category.",
            dup.name
        ));
    }
    Ok(())
}

// --- rendering -------------------------------------------------------------------------------

fn pad(x: i8, y: i8) -> egui::Margin {
    egui::Margin::symmetric(x, y)
}

/// Frozen per-frame render data, so `state` stays free to mutate below in response to clicks.
struct Snap {
    categories: Vec<Category>,
    types: Vec<TaskType>,
    lifetime: std::collections::BTreeMap<i64, i64>,
}

pub fn show(ui: &mut egui::Ui, state: &mut TypesState, db: &Db, theme: &Theme) {
    state.ensure_view(db);

    if let Some(err) = &state.error {
        egui::Frame::default().fill(theme.bg_canvas).inner_margin(pad(34, 26)).show(ui, |ui| {
            ui.colored_label(theme.negative, format!("Database error: {err}"));
        });
        return;
    }
    let view = state.view.as_ref().expect("view built above");
    let snap = Snap {
        categories: view.categories.clone(),
        types: view.types.clone(),
        lifetime: view.lifetime.clone(),
    };

    // Explicit-rect banding: toolbar top, caption band pinned bottom, table in between. See
    // the module doc / `today.rs`'s LAYOUT TRAP note — `allocate_ui_with_layout` inside a
    // `horizontal` staircases children and starves them of scroll height.
    let full = ui.max_rect();
    const CAPTION_H: f32 = 44.0;
    let caption_top = full.bottom() - CAPTION_H;

    let mut toolbar_ui = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("types_toolbar")
            .max_rect(egui::Rect::from_min_max(full.min, egui::pos2(full.right(), caption_top)))
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    let toolbar_bottom = egui::Frame::default()
        .fill(theme.bg_canvas)
        .inner_margin(egui::Margin { left: 34, right: 34, top: 26, bottom: 24 })
        .show(&mut toolbar_ui, |ui| toolbar(ui, state, theme, &snap, db))
        .response
        .rect
        .bottom();

    let table_rect = egui::Rect::from_min_max(
        egui::pos2(full.left(), toolbar_bottom),
        egui::pos2(full.right(), caption_top),
    );
    let mut table_ui = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("types_table")
            .max_rect(table_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    egui::Frame::default().fill(theme.bg_canvas).inner_margin(pad(34, 0)).show(&mut table_ui, |ui| {
        table(ui, state, theme, &snap, db);
    });

    let mut caption_ui = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("types_caption")
            .max_rect(egui::Rect::from_min_max(egui::pos2(full.left(), caption_top), full.max))
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    egui::Frame::default().fill(theme.bg_canvas).inner_margin(pad(34, 12)).show(&mut caption_ui, |ui| {
        caption(ui, state, theme, &snap, db);
    });

    // Modals draw last, on top.
    edit_modal(ui, state, db, theme, &snap);
    delete_modal(ui, state, db, theme);
    import_report_modal(ui, state, theme);
    manage_categories_modal(ui, state, db, theme);
}

/// Toolbar: search, category filter, active filter, "Manage categories" link, then
/// right-aligned Import CSV + New type.
fn toolbar(ui: &mut egui::Ui, state: &mut TypesState, theme: &Theme, snap: &Snap, db: &Db) {
    ui.horizontal(|ui| {
        // Every control in the row is CTRL_H tall, so nothing sits proud of its neighbours
        // (egui sizes a button from its text + padding otherwise). Centering follows.
        ui.spacing_mut().interact_size.y = CTRL_H;
        ui.add(
            egui::TextEdit::singleline(&mut state.search)
                .hint_text(format!("Search {} types\u{2026}", snap.types.len()))
                .desired_width(240.0)
                .margin(egui::Margin::symmetric(10, 7))
                .font(egui::FontSelection::FontId(t::sans(t::BODY))),
        );
        ui.add_space(10.0);

        let mut cat_filter = state.category_filter;
        egui::ComboBox::from_id_salt("types_category_filter")
            .selected_text(match cat_filter {
                CategoryFilter::All => "All categories".to_string(),
                CategoryFilter::One(id) => snap
                    .categories
                    .iter()
                    .find(|c| c.id == id)
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| "All categories".to_string()),
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut cat_filter, CategoryFilter::All, "All categories");
                for c in &snap.categories {
                    ui.selectable_value(&mut cat_filter, CategoryFilter::One(c.id), &c.name);
                }
            });
        if cat_filter != state.category_filter {
            state.category_filter = cat_filter;
        }
        ui.add_space(10.0);

        let mut active_filter = state.active_filter;
        egui::ComboBox::from_id_salt("types_active_filter")
            .selected_text(match active_filter {
                ActiveFilter::ActiveOnly => "Active only",
                ActiveFilter::All => "All",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut active_filter, ActiveFilter::ActiveOnly, "Active only");
                ui.selectable_value(&mut active_filter, ActiveFilter::All, "All");
            });
        if active_filter != state.active_filter {
            state.active_filter = active_filter;
        }
        ui.add_space(10.0);

        if outline_button(ui, theme, "Manage categories").clicked() {
            state.modal = Modal::ManageCategories(load_categories_modal(db).unwrap_or(CategoriesModal {
                rows: Vec::new(),
                new_name: String::new(),
                error: None,
                drag: None,
                move_dialog: None,
            }));
        }

        // Right-aligned by measuring and padding, not `Layout::right_to_left`: nested inside
        // this horizontal, that layout claims a rect the left-hand controls already used and
        // the two buttons land on top of "Manage categories".
        let right_w = button_width(ui, "Import CSV") + 10.0 + button_width(ui, "New type");
        let free = ui.max_rect().right() - ui.cursor().left() - right_w;
        ui.add_space(free.max(10.0));
        if outline_button(ui, theme, "Import CSV").clicked() {
            state.modal = Modal::ImportReport(run_import(db));
            state.mark_dirty();
        }
        ui.add_space(10.0);
        if solid_button(ui, theme, "New type").clicked() {
            state.modal = Modal::Edit(new_type_form(snap.categories.first().map(|c| c.id).unwrap_or(0)));
        }
    });
}

/// A frameless text link in the accent color.
fn link(ui: &mut egui::Ui, theme: &Theme, text: &str) -> egui::Response {
    let resp = ui.add(
        egui::Button::new(egui::RichText::new(text).font(t::sans(t::CAPTION)).color(theme.accent))
            .frame(false),
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// Shared height for every toolbar control (search, filters, buttons).
const CTRL_H: f32 = 32.0;

/// The solid-accent primary button (design: `New type` / the edit dialog's `Save`).
fn solid_button(ui: &mut egui::Ui, theme: &Theme, label: &str) -> egui::Response {
    button_at(ui, theme, label, theme.accent, theme.bg_raised, None)
}

/// A bordered `bg_raised` pill (design: `Import CSV`, and Insights' `Export CSV`).
fn outline_button(ui: &mut egui::Ui, theme: &Theme, label: &str) -> egui::Response {
    button_at(ui, theme, label, theme.bg_raised, theme.text_body, Some(theme.border_strong))
}

/// The width [`button_at`] will take for `label`, for laying a row out before drawing it.
fn button_width(ui: &egui::Ui, label: &str) -> f32 {
    ui.painter()
        .layout_no_wrap(label.to_owned(), t::sans_medium(t::BODY), egui::Color32::PLACEHOLDER)
        .rect
        .width()
        + 32.0
}

/// One painted button: CTRL_H tall, 16px side padding, 6px radius, label centred in the rect.
fn button_at(
    ui: &mut egui::Ui,
    theme: &Theme,
    label: &str,
    fill: egui::Color32,
    text: egui::Color32,
    border: Option<egui::Color32>,
) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(label.to_owned(), t::sans_medium(t::BODY), text);
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(galley.rect.width() + 32.0, CTRL_H),
        egui::Sense::click(),
    );
    let stroke = match border {
        Some(c) => egui::Stroke::new(1.0, if resp.hovered() { theme.accent_tint_border } else { c }),
        None => egui::Stroke::NONE,
    };
    let fill = if border.is_none() && resp.hovered() { theme.accent_hover } else { fill };
    ui.painter().rect(rect, egui::CornerRadius::same(6), fill, stroke, egui::StrokeKind::Inside);
    ui.painter().galley(
        egui::pos2(
            rect.center().x - galley.rect.width() / 2.0,
            rect.center().y - galley.rect.height() / 2.0,
        ),
        galley,
        text,
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// Column widths `[category, name, description, used, status]` for the given available width,
/// with 14px gaps (design: `132px 1fr 230px 62px 84px`, gap 14px).
const GAP: f32 = 14.0;
const CAT_W: f32 = 132.0;
const DESC_W: f32 = 230.0;
const USED_W: f32 = 62.0;
const STATUS_W: f32 = 84.0;

fn name_w(avail: f32) -> f32 {
    (avail - CAT_W - DESC_W - USED_W - STATUS_W - GAP * 4.0).max(80.0)
}

fn table(ui: &mut egui::Ui, state: &mut TypesState, theme: &Theme, snap: &Snap, db: &Db) {
    let rows = filter_types(&snap.types, &state.search, state.category_filter, state.active_filter);
    // Reserve the scrollbar's width, or the rightmost column is clipped by it once the
    // table overflows (`available_width` does not exclude it here) — same as today.rs's grid.
    let avail = ui.available_width()
        - ui.spacing().scroll.bar_width
        - ui.spacing().scroll.bar_inner_margin
        - ui.spacing().scroll.bar_outer_margin;
    let nw = name_w(avail);
    let x_cat = 0.0_f32;
    let x_name = x_cat + CAT_W + GAP;
    let x_desc = x_name + nw + GAP;
    let x_used = x_desc + DESC_W + GAP;
    let x_status = x_used + USED_W + GAP;

    // --- header row ---
    let (header_rect, _) = ui.allocate_exact_size(egui::vec2(avail, 28.0), egui::Sense::hover());
    let p = ui.painter();
    let origin = header_rect.left_top();
    p.text(origin + egui::vec2(x_cat, 0.0), egui::Align2::LEFT_TOP, "CATEGORY", t::mono(t::EYEBROW), theme.text_tertiary);
    p.text(origin + egui::vec2(x_name, 0.0), egui::Align2::LEFT_TOP, "NAME", t::mono(t::EYEBROW), theme.text_tertiary);
    p.text(origin + egui::vec2(x_desc, 0.0), egui::Align2::LEFT_TOP, "DESCRIPTION", t::mono(t::EYEBROW), theme.text_tertiary);
    p.text(origin + egui::vec2(x_used + USED_W, 0.0), egui::Align2::RIGHT_TOP, "USED", t::mono(t::EYEBROW), theme.text_tertiary);
    p.text(origin + egui::vec2(x_status + STATUS_W, 0.0), egui::Align2::RIGHT_TOP, "STATUS", t::mono(t::EYEBROW), theme.text_tertiary);
    p.line_segment(
        [egui::pos2(header_rect.left(), header_rect.bottom()), egui::pos2(header_rect.right(), header_rect.bottom())],
        egui::Stroke::new(1.0, theme.border),
    );

    // --- body rows, scrollable; header stays put above ---
    let mut clicked: Option<(i64, bool)> = None; // (id, ctrl)
    let mut open_edit: Option<i64> = None; // double-click opens the edit dialog
    let mut toggle: Option<i64> = None;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.style_mut().interaction.selectable_labels = false;
        const ROW_H: f32 = 42.0;
        for ty in &rows {
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(avail, ROW_H), egui::Sense::click());
            let selected = state.selection.contains(&ty.id);
            if selected {
                ui.painter().rect_filled(rect, egui::CornerRadius::ZERO, theme.accent_tint_bg);
            }
            let cat_name = snap
                .categories
                .iter()
                .find(|c| c.id == ty.category_id)
                .map(|c| c.name.as_str())
                .unwrap_or("");
            let o = rect.left_top() + egui::vec2(0.0, ROW_H / 2.0 - 7.0);
            ui.painter().text(o + egui::vec2(x_cat, 0.0), egui::Align2::LEFT_TOP, cat_name.to_uppercase(), t::mono(t::TILE_CATEGORY), theme.accent_eyebrow);
            ui.painter().text(o + egui::vec2(x_name, 0.0), egui::Align2::LEFT_TOP, &ty.name, t::sans(t::BODY), theme.text_primary);

            // Description, truncated to one line with an ellipsis; tooltip when elided.
            let desc_galley = {
                let mut job = egui::text::LayoutJob::single_section(
                    ty.description.clone(),
                    egui::TextFormat { font_id: t::sans(t::CAPTION), color: theme.text_secondary, ..Default::default() },
                );
                job.wrap.max_width = DESC_W;
                job.wrap.max_rows = 1;
                job.wrap.overflow_character = Some('\u{2026}');
                ui.painter().layout_job(job)
            };
            let desc_elided = desc_galley.elided;
            ui.painter().galley(o + egui::vec2(x_desc, 0.0), desc_galley, theme.text_secondary);

            let used = snap.lifetime.get(&ty.id).copied().unwrap_or(0);
            let used_galley = ui.painter().layout_no_wrap(used.to_string(), t::mono(t::ROW_NUMERIC), theme.text_body);
            ui.painter().galley(
                egui::pos2(rect.left() + x_used + USED_W - used_galley.rect.width(), o.y),
                used_galley,
                theme.text_body,
            );

            let pill_resp = status_pill(
                ui,
                theme,
                egui::pos2(rect.left() + x_status, rect.top() + ROW_H / 2.0),
                STATUS_W,
                ty.is_active,
                ty.id,
            );
            if pill_resp.clicked() {
                toggle = Some(ty.id);
            }

            ui.painter().line_segment(
                [egui::pos2(rect.left(), rect.bottom()), egui::pos2(rect.right(), rect.bottom())],
                egui::Stroke::new(1.0, theme.border_subtle),
            );

            let resp = if desc_elided {
                let tip = if ty.description.is_empty() { ty.name.clone() } else { ty.description.clone() };
                resp.on_hover_text(tip)
            } else {
                resp
            };
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            // The pill sits on top of the row visually and is interacted after it, so it
            // already wins the click at that position — but consume it explicitly here too,
            // so a click on the pill never also flips row selection.
            if resp.double_clicked() && !pill_resp.clicked() {
                open_edit = Some(ty.id);
            } else if resp.clicked() && !pill_resp.clicked() {
                let ctrl = ui.input(|i| i.modifiers.ctrl);
                clicked = Some((ty.id, ctrl));
            }
        }
    });

    if let Some(id) = open_edit {
        if let Some(ty) = snap.types.iter().find(|t| t.id == id) {
            state.selection.clear();
            state.selection.insert(id);
            state.modal = Modal::Edit(edit_type_form(ty, db));
        }
    }
    if let Some((id, ctrl)) = clicked {
        if ctrl {
            if !state.selection.remove(&id) {
                state.selection.insert(id);
            }
        } else {
            state.selection.clear();
            state.selection.insert(id);
        }
    }
    if let Some(id) = toggle {
        if let Some(ty) = snap.types.iter().find(|t| t.id == id) {
            if let Err(e) = db.edit_task_type(ty.id, ty.category_id, &ty.name, &ty.description, !ty.is_active) {
                // A refused write is a notice beside the caption, not the whole screen
                // replaced by an error banner — `error` stays for query failures.
                state.notice = Some(e.to_string());
            } else {
                state.notice = None;
                state.mark_dirty();
            }
        }
    }
}

/// A fully-rounded status pill, right-aligned within a `width`-wide column centered at `pos`'s
/// y (design: 11.5px, 3px/9px padding, 999px radius). Doubles as the activate/deactivate
/// toggle for its row — clicking it flips that one type's active state.
fn status_pill(ui: &mut egui::Ui, theme: &Theme, col_left: egui::Pos2, width: f32, active: bool, key: i64) -> egui::Response {
    let (bg, fg, text) = if active {
        (theme.pill_sel_bg, theme.pill_sel_text, "Active")
    } else {
        (theme.bg_track, theme.text_secondary, "Inactive")
    };
    let galley = ui.painter().layout_no_wrap(text.to_owned(), t::sans(11.5), fg);
    let pad_x = 9.0;
    let pad_y = 3.0;
    let h = galley.rect.height() + pad_y * 2.0;
    let w = galley.rect.width() + pad_x * 2.0;
    let rect = egui::Rect::from_min_size(egui::pos2(col_left.x + width - w, col_left.y - h / 2.0), egui::vec2(w, h));
    let resp = ui.interact(rect, ui.id().with(("status_pill", key)), egui::Sense::click());
    ui.painter().rect(rect, egui::CornerRadius::same((h / 2.0) as u8), bg, egui::Stroke::NONE, egui::StrokeKind::Inside);
    ui.painter().galley(egui::pos2(rect.left() + pad_x, rect.top() + pad_y), galley, fg);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let tip = if active { "click to deactivate" } else { "click to activate" };
    resp.on_hover_text(tip)
}

/// The caption line below the table, plus selection actions.
fn caption(ui: &mut egui::Ui, state: &mut TypesState, theme: &Theme, snap: &Snap, db: &Db) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Select rows to edit or deactivate.")
                .font(t::sans(13.0))
                .color(theme.text_secondary),
        );
        if let Some(msg) = state.notice.clone() {
            ui.add_space(14.0);
            ui.label(egui::RichText::new(msg).font(t::sans(t::CAPTION)).color(theme.negative));
        }
        if state.selection.is_empty() {
            return;
        }
        ui.add_space(14.0);
        ui.label(
            egui::RichText::new(format!("{} selected", state.selection.len()))
                .font(t::sans(13.0))
                .color(theme.text_primary),
        );
        ui.add_space(14.0);

        if state.selection.len() == 1 {
            if link(ui, theme, "Edit").clicked() {
                let id = *state.selection.iter().next().expect("selection is non-empty");
                if let Some(ty) = snap.types.iter().find(|t| t.id == id) {
                    state.modal = Modal::Edit(edit_type_form(ty, db));
                }
            }
            ui.add_space(10.0);
        }

        // Deactivating a mixed selection deactivates all of them; only an all-inactive
        // selection offers "Activate".
        let any_active = state.selection.iter().any(|id| {
            snap.types.iter().find(|t| t.id == *id).map(|t| t.is_active).unwrap_or(false)
        });
        let label = if any_active { "Deactivate" } else { "Activate" };
        if link(ui, theme, label).clicked() {
            let target_active = !any_active;
            let mut first_err = None;
            for id in state.selection.clone() {
                if let Some(ty) = snap.types.iter().find(|t| t.id == id) {
                    if let Err(e) = db.edit_task_type(ty.id, ty.category_id, &ty.name, &ty.description, target_active) {
                        first_err.get_or_insert(e.to_string());
                    }
                }
            }
            state.notice = first_err;
            state.mark_dirty();
        }
    });
}

/// Builds a blank "New task type" form for the given default category.
/// How many categories get a pill before the rest move behind the "more" popup. Small
/// enough that the row never wraps past two lines at the dialog's width.
const PILL_TOP_N: usize = 5;
/// The "more" popup's fixed size. It scrolls rather than growing with the list.
const PICKER_W: f32 = 280.0;
const PICKER_LIST_H: f32 = 260.0;
/// The manage-categories list is capped at this many rows (or 40% of the window, whichever
/// is smaller) and scrolls past that, so Done can never walk off the bottom of the screen.
const CATLIST_MAX_ROWS: f32 = 5.0;

fn new_type_form(default_category_id: i64) -> TypeForm {
    TypeForm {
        target_id: None,
        category_id: default_category_id,
        name: String::new(),
        description: String::new(),
        is_active: true,
        error: None,
        category_filter: String::new(),
        picker_open: false,
        new_category: None,
        new_category_error: None,
        usage: None,
        delete: None,
    }
}

/// Builds an "Edit task type" form for `ty`, loading its lifetime usage for the title-row
/// eyebrow. A usage query failure just leaves the eyebrow off — not worth failing the whole
/// dialog over.
fn edit_type_form(ty: &TaskType, db: &Db) -> TypeForm {
    TypeForm {
        target_id: Some(ty.id),
        category_id: ty.category_id,
        name: ty.name.clone(),
        description: ty.description.clone(),
        is_active: ty.is_active,
        error: None,
        category_filter: String::new(),
        picker_open: false,
        new_category: None,
        new_category_error: None,
        usage: db.type_usage(ty.id).ok(),
        delete: None,
    }
}

/// The new/edit type modal (design `_rustrefactor/screens/09-edit-task-type.png`): category
/// pills with an inline "+ New", live per-category name validation, a tile-style live
/// preview, description, and a painted Active switch. "Delete type" opens [`delete_modal`]
/// on top.
fn edit_modal(ui: &mut egui::Ui, state: &mut TypesState, db: &Db, theme: &Theme, snap: &Snap) {
    let Modal::Edit(_) = &state.modal else { return };
    let mut want_save = false;
    let mut want_cancel = false;
    let mut want_delete = false;

    {
        let Modal::Edit(form) = &mut state.modal else { unreachable!() };
        let is_new = form.target_id.is_none();

        let modal_resp = egui::Modal::new(egui::Id::new("type_edit")).show(ui.ctx(), |ui| {
            ui.set_width(420.0);
            ui.spacing_mut().item_spacing.y = 6.0;

            // --- title row: name, usage eyebrow (edit only), close ---
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(if is_new { "New task type" } else { "Edit task type" })
                        .font(t::sans_medium(t::SECTION_TITLE))
                        .color(theme.text_primary),
                );
                if let Some(usage) = &form.usage {
                    ui.add_space(8.0);
                    let mut s = format!("{} tallies", usage.total);
                    if let Some(d) = usage.last_used {
                        s.push_str(&format!(" \u{b7} last used {} {}", d.day(), d.format("%b")));
                    }
                    ui.label(egui::RichText::new(s).font(t::mono(t::EYEBROW)).color(theme.text_quiet));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if link(ui, theme, "\u{d7}").clicked() {
                        want_cancel = true;
                    }
                });
            });
            ui.separator();
            ui.add_space(6.0);

            // --- category pills: the most-used few, the rest behind "more" ---
            eyebrow(ui, theme, "CATEGORY");
            ui.add_space(4.0);
            let lifetime: Vec<TypeLifetimeTotal> = snap
                .lifetime
                .iter()
                .map(|(id, total)| TypeLifetimeTotal { task_type_id: *id, total: *total })
                .collect();
            let aggs = aggregate_categories(&snap.categories, &snap.types, &lifetime);
            let (visible, hidden) = pill_slots(&aggs, form.category_id, PILL_TOP_N);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
                for c in snap.categories.iter().filter(|c| visible.contains(&c.id)) {
                    if category_select_pill(ui, theme, &c.name, form.category_id == c.id).clicked() {
                        form.category_id = c.id;
                    }
                }
                if hidden > 0 && more_pill(ui, theme, hidden).clicked() {
                    form.picker_open = true;
                    form.category_filter.clear();
                }
                if let Some(buf) = &mut form.new_category {
                    ui.add(
                        egui::TextEdit::singleline(buf)
                            .hint_text("Category name")
                            .desired_width(120.0)
                            .margin(egui::Margin::symmetric(8, 5))
                            .font(egui::FontSelection::FontId(t::sans(t::CAPTION))),
                    );
                    let add_clicked = link(ui, theme, "Add").clicked();
                    let cancel_clicked = link(ui, theme, "\u{d7}").clicked();
                    if add_clicked {
                        match db.create_category(buf) {
                            Ok(id) => {
                                form.category_id = id;
                                form.new_category = None;
                                form.new_category_error = None;
                            }
                            Err(Error::Duplicate(_)) => {
                                form.new_category_error =
                                    Some("a category with that name already exists".to_string());
                            }
                            Err(e) => form.new_category_error = Some(e.to_string()),
                        }
                    } else if cancel_clicked {
                        form.new_category = None;
                        form.new_category_error = None;
                    }
                } else if new_category_pill(ui, theme).clicked() {
                    form.new_category = Some(String::new());
                    form.new_category_error = None;
                }
            });
            if let Some(err) = &form.new_category_error {
                ui.add_space(2.0);
                ui.colored_label(theme.negative, err);
            }
            ui.add_space(6.0);

            // --- name, with the live per-category duplicate check ---
            ui.horizontal(|ui| {
                eyebrow(ui, theme, "NAME");
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("no category prefix \u{2014} it's a field now")
                        .font(t::sans(t::CAPTION))
                        .color(theme.text_quiet),
                );
            });
            ui.add(
                egui::TextEdit::singleline(&mut form.name)
                    .desired_width(f32::INFINITY)
                    .font(egui::FontSelection::FontId(t::sans(t::BODY))),
            );
            let validation = validate_type_name(&snap.types, form.category_id, &form.name, form.target_id);
            let cat_name = snap
                .categories
                .iter()
                .find(|c| c.id == form.category_id)
                .map(|c| c.name.as_str())
                .unwrap_or("");
            if let Err(reason) = &validation {
                ui.add_space(2.0);
                ui.colored_label(theme.negative, format!("{cat_name} {reason}"));
            }
            ui.add_space(6.0);

            // --- live "appears as" tile preview ---
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("appears as").font(t::sans(t::CAPTION)).color(theme.text_quiet));
                ui.add_space(8.0);
                appears_as_chip(ui, theme, cat_name, &form.name);
            });
            ui.add_space(8.0);

            // --- description ---
            eyebrow(ui, theme, "DESCRIPTION");
            ui.label(
                egui::RichText::new("shown on tile hover and matched by quick add")
                    .font(t::sans(t::CAPTION))
                    .color(theme.text_quiet),
            );
            ui.add(
                egui::TextEdit::multiline(&mut form.description)
                    .hint_text("optional \u{2014} helps quick add find this type")
                    .desired_rows(2)
                    .desired_width(f32::INFINITY)
                    .font(egui::FontSelection::FontId(t::sans(t::BODY))),
            );
            ui.add_space(8.0);

            // --- active switch ---
            if active_switch(ui, theme, form.is_active).clicked() {
                form.is_active = !form.is_active;
            }

            if let Some(err) = &form.error {
                ui.add_space(6.0);
                ui.colored_label(theme.negative, err);
            }

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(8.0);

            // --- footer: Delete type (edit only, left) · Cancel · Save ---
            ui.horizontal(|ui| {
                if !is_new && link(ui, theme, "Delete type").clicked() {
                    want_delete = true;
                }
                let cancel_w = button_width(ui, "Cancel");
                let save_w = button_width(ui, "Save");
                let free = ui.max_rect().right() - ui.cursor().left() - cancel_w - 8.0 - save_w;
                ui.add_space(free.max(8.0));
                if outline_button(ui, theme, "Cancel").clicked() {
                    want_cancel = true;
                }
                ui.add_space(8.0);
                if primary_button(ui, theme, "Save", validation.is_ok(), theme.accent) {
                    want_save = true;
                }
            });

            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            if enter && form.new_category.is_none() && validation.is_ok() {
                want_save = true;
            }
        });

        if modal_resp.should_close() {
            want_cancel = true;
        }
    }

    // The "more" category popup, drawn on top of the dialog.
    {
        let Modal::Edit(form) = &mut state.modal else { unreachable!() };
        if form.picker_open {
            let lifetime: Vec<TypeLifetimeTotal> = snap
                .lifetime
                .iter()
                .map(|(id, total)| TypeLifetimeTotal { task_type_id: *id, total: *total })
                .collect();
            let aggs = aggregate_categories(&snap.categories, &snap.types, &lifetime);
            let (picked, close) =
                category_picker(ui, theme, &aggs, form.category_id, &mut form.category_filter);
            if let Some(id) = picked {
                form.category_id = id;
                form.picker_open = false;
            } else if close {
                form.picker_open = false;
            }
            // Esc closed the popup, not the dialog underneath it.
            want_cancel = false;
        }
    }

    if want_delete {
        let Modal::Edit(form) = &mut state.modal else { unreachable!() };
        if let Some(id) = form.target_id {
            match load_delete_form(db, id, &form.name) {
                Ok(df) => form.delete = Some(df),
                Err(e) => form.error = Some(e.to_string()),
            }
        }
        return;
    }
    if want_cancel {
        state.modal = Modal::None;
        return;
    }
    if !want_save {
        return;
    }
    let Modal::Edit(form) = &mut state.modal else { unreachable!() };
    let result = match form.target_id {
        None => db.create_task_type(form.category_id, &form.name, &form.description).map(|_| ()),
        Some(id) => db.edit_task_type(id, form.category_id, &form.name, &form.description, form.is_active),
    };
    match result {
        Ok(()) => {
            state.modal = Modal::None;
            state.mark_dirty();
        }
        Err(Error::Duplicate(_)) => {
            form.error = Some("a type with that name already exists in this category".to_string());
        }
        Err(e) => {
            form.error = Some(e.to_string());
        }
    }
}

/// Loads the delete-type dialog's data: fresh usage (not the possibly-stale copy cached on
/// the edit form) and every other **active** type for the merge picker.
fn load_delete_form(db: &Db, type_id: i64, type_name: &str) -> simpletally_core::Result<DeleteForm> {
    let usage = db.type_usage(type_id)?;
    let types = db.list_task_types(true)?; // active only
    let categories = db.list_categories()?;
    let others: Vec<(i64, String, String)> = types
        .iter()
        .filter(|t| t.id != type_id)
        .map(|t| {
            let cat = categories.iter().find(|c| c.id == t.category_id).map(|c| c.name.clone()).unwrap_or_default();
            (t.id, cat, t.name.clone())
        })
        .collect();
    Ok(DeleteForm {
        type_id,
        type_name: type_name.to_string(),
        usage,
        choice: DeleteChoice::Deactivate,
        merge_target: None,
        others,
        error: None,
    })
}

/// Deactivates a type in place, keeping its category/name/description — used by the
/// delete dialog's "Deactivate instead" branch, which is not itself an `edit_task_type` call
/// site elsewhere.
fn deactivate_type(db: &Db, id: i64) -> simpletally_core::Result<()> {
    let types = db.list_task_types(false)?;
    let ty = types.iter().find(|t| t.id == id).ok_or_else(|| Error::NotFound(format!("task type {id}")))?;
    db.edit_task_type(ty.id, ty.category_id, &ty.name, &ty.description, false)
}

/// The delete-type dialog (design 09's third panel), opened on top of the edit modal by its
/// "Delete type" link. A type with zero tallies skips the radio list (spec: "needs no
/// ceremony") and goes straight to Cancel/Delete, taking the trash path.
fn delete_modal(ui: &mut egui::Ui, state: &mut TypesState, db: &Db, theme: &Theme) {
    let Modal::Edit(form) = &state.modal else { return };
    if form.delete.is_none() {
        return;
    }
    let mut close = false;
    let mut confirm = false;

    {
        let Modal::Edit(form) = &mut state.modal else { unreachable!() };
        let df = form.delete.as_mut().expect("checked above");
        let no_tallies = df.usage.total == 0;

        let modal_resp = egui::Modal::new(egui::Id::new("type_delete")).show(ui.ctx(), |ui| {
            ui.set_width(380.0);
            ui.label(
                egui::RichText::new(format!("Delete {}?", df.type_name))
                    .font(t::sans_medium(t::SECTION_TITLE))
                    .color(theme.text_primary),
            );
            ui.separator();
            ui.add_space(6.0);

            if no_tallies {
                ui.label(egui::RichText::new("It has no tallies.").font(t::sans(t::BODY)).color(theme.text_body));
            } else {
                ui.label(
                    egui::RichText::new(format!(
                        "It has {} tallies across {} days. Deactivating hides it from the grid and keeps the history.",
                        df.usage.total, df.usage.active_days
                    ))
                    .font(t::sans(t::BODY))
                    .color(theme.text_body),
                );
                ui.add_space(10.0);

                delete_radio(
                    ui, theme, &mut df.choice, DeleteChoice::Deactivate,
                    "Deactivate instead", &format!("Keeps all {} tallies in reports", df.usage.total),
                );
                delete_radio(
                    ui, theme, &mut df.choice, DeleteChoice::Merge,
                    "Move its tallies to another type", "Then delete this one",
                );
                delete_radio(
                    ui, theme, &mut df.choice, DeleteChoice::Trash,
                    "Move the type and its tallies to the trash",
                    "Restorable from the trash until it is purged",
                );

                if df.choice == DeleteChoice::Merge {
                    ui.add_space(6.0);
                    eyebrow(ui, theme, "MOVE TO");
                    if df.others.is_empty() {
                        ui.label(
                            egui::RichText::new("no other active type to move to")
                                .font(t::sans(t::CAPTION))
                                .color(theme.text_quiet),
                        );
                    } else {
                        egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                            for (id, cat, name) in &df.others {
                                ui.radio_value(&mut df.merge_target, Some(*id), format!("{cat} \u{b7} {name}"));
                            }
                        });
                    }
                }
            }

            if let Some(err) = &df.error {
                ui.add_space(8.0);
                ui.colored_label(theme.negative, err);
            }

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(8.0);

            let (primary_label, primary_fill) = if no_tallies {
                ("Delete", theme.negative)
            } else {
                match df.choice {
                    DeleteChoice::Deactivate => ("Deactivate", theme.accent),
                    DeleteChoice::Merge => ("Move and delete", theme.accent),
                    DeleteChoice::Trash => ("Move to trash", theme.negative),
                }
            };
            let can_confirm = no_tallies || df.choice != DeleteChoice::Merge || df.merge_target.is_some();

            ui.horizontal(|ui| {
                let cancel_w = button_width(ui, "Cancel");
                let primary_w = button_width(ui, primary_label);
                let free = ui.max_rect().right() - ui.cursor().left() - cancel_w - 8.0 - primary_w;
                ui.add_space(free.max(8.0));
                if outline_button(ui, theme, "Cancel").clicked() {
                    close = true;
                }
                ui.add_space(8.0);
                if primary_button(ui, theme, primary_label, can_confirm, primary_fill) {
                    confirm = true;
                }
            });

        });

        if modal_resp.should_close() {
            close = true;
        }
    }

    if close {
        let Modal::Edit(form) = &mut state.modal else { unreachable!() };
        form.delete = None;
        return;
    }
    if !confirm {
        return;
    }
    let Modal::Edit(form) = &mut state.modal else { unreachable!() };
    let df = form.delete.as_mut().expect("checked above");
    let no_tallies = df.usage.total == 0;
    let result = if no_tallies {
        db.trash_task_type(df.type_id).map(|_| ())
    } else {
        match df.choice {
            DeleteChoice::Deactivate => deactivate_type(db, df.type_id),
            DeleteChoice::Merge => match df.merge_target {
                Some(target) => db.merge_task_type(df.type_id, target),
                None => return, // Save/primary_button already refuses this — nothing to do
            },
            DeleteChoice::Trash => db.trash_task_type(df.type_id).map(|_| ()),
        }
    };
    match result {
        Ok(()) => {
            state.modal = Modal::None;
            state.mark_dirty();
        }
        Err(e) => df.error = Some(e.to_string()),
    }
}

/// One radio row in the delete dialog: a label + consequence sub-line, the whole block
/// clickable (not just the radio dot) via a post-hoc `ui.interact` over the frame's rect —
/// same layered-interaction pattern as `status_pill`.
fn delete_radio(
    ui: &mut egui::Ui,
    theme: &Theme,
    choice: &mut DeleteChoice,
    value: DeleteChoice,
    label: &str,
    sub: &str,
) {
    let selected = *choice == value;
    let resp = egui::Frame::default()
        .fill(if selected { theme.pill_sel_bg } else { theme.bg_raised })
        .stroke(egui::Stroke::new(1.0, if selected { theme.pill_sel_border } else { theme.border }))
        .corner_radius(6)
        .inner_margin(pad(12, 10))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let _ = ui.radio(selected, "");
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(label).font(t::sans_medium(t::TILE_NAME)).color(theme.text_primary));
                    ui.label(egui::RichText::new(sub).font(t::sans(t::CAPTION)).color(theme.text_quiet));
                });
            });
        })
        .response;
    let click_resp = ui.interact(resp.rect, ui.id().with(("delete_radio", label)), egui::Sense::click());
    if click_resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if click_resp.clicked() {
        *choice = value;
    }
    ui.add_space(6.0);
}

/// A selectable category pill for the edit form (design 09): fully rounded, no count. Reuses
/// the `pill_sel_*` tokens for the selected state, same as [`pill::category_pill`] but
/// without the trailing count — this row is a picker, not a filter.
fn category_select_pill(ui: &mut egui::Ui, theme: &Theme, name: &str, selected: bool) -> egui::Response {
    let (bg, border, text_col) = if selected {
        (theme.pill_sel_bg, theme.pill_sel_border, theme.pill_sel_text)
    } else {
        (theme.bg_raised, theme.border_strong, theme.text_secondary)
    };
    let galley = ui.painter().layout_no_wrap(name.to_owned(), t::sans_medium(t::PILL_TEXT), text_col);
    let pad_x = 14.0;
    let h = 28.0;
    let w = pad_x * 2.0 + galley.rect.width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::click());
    let border_c = if resp.hovered() && !selected { theme.accent_tint_border } else { border };
    ui.painter().rect(rect, egui::CornerRadius::same((h / 2.0) as u8), bg, egui::Stroke::new(1.0, border_c), egui::StrokeKind::Inside);
    let pos = egui::pos2(rect.center().x - galley.rect.width() / 2.0, rect.center().y - galley.rect.height() / 2.0);
    ui.painter().galley(pos, galley, text_col);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// The dashed "+ New" pill that swaps for an inline category-name field when clicked.
///
/// `ponytail:` egui has no dashed-stroke primitive, so the dash is approximated with a
/// plain lighter border rather than a hand-tessellated dashed rounded rect; upgrade to a
/// real dashed outline (walk the rounded rect's perimeter into `Shape::dashed_line`) if the
/// solid border reads wrong next to the mock in the running app.
fn new_category_pill(ui: &mut egui::Ui, theme: &Theme) -> egui::Response {
    let galley = ui.painter().layout_no_wrap("+ New".to_owned(), t::sans_medium(t::PILL_TEXT), theme.text_secondary);
    let pad_x = 14.0;
    let h = 28.0;
    let w = pad_x * 2.0 + galley.rect.width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::click());
    let border_c = if resp.hovered() { theme.accent_tint_border } else { theme.border };
    ui.painter().rect(rect, egui::CornerRadius::same((h / 2.0) as u8), theme.bg_canvas, egui::Stroke::new(1.0, border_c), egui::StrokeKind::Inside);
    let pos = egui::pos2(rect.center().x - galley.rect.width() / 2.0, rect.center().y - galley.rect.height() / 2.0);
    ui.painter().galley(pos, galley, theme.text_secondary);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// A small painted chevron, pointing up or down, inside `rect`.
fn chevron(painter: &egui::Painter, rect: egui::Rect, up: bool, color: egui::Color32) {
    let c = rect.center();
    let w = rect.width() * 0.32;
    let h = rect.height() * 0.18;
    let (a, b, d) = if up {
        (c + egui::vec2(-w, h), c + egui::vec2(0.0, -h), c + egui::vec2(w, h))
    } else {
        (c + egui::vec2(-w, -h), c + egui::vec2(0.0, h), c + egui::vec2(w, -h))
    };
    let stroke = egui::Stroke::new(1.6, color);
    painter.line_segment([a, b], stroke);
    painter.line_segment([b, d], stroke);
}

/// The `more (N)` pill that opens the full category picker.
fn more_pill(ui: &mut egui::Ui, theme: &Theme, hidden: usize) -> egui::Response {
    let label = format!("more ({hidden})");
    let galley = ui.painter().layout_no_wrap(label, t::sans(t::PILL_TEXT), theme.text_secondary);
    let h = 28.0;
    let w = 28.0 + galley.rect.width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::click());
    let border = if resp.hovered() { theme.accent_tint_border } else { theme.border_strong };
    ui.painter().rect(
        rect,
        egui::CornerRadius::same((h / 2.0) as u8),
        theme.bg_raised,
        egui::Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
    let pos = egui::pos2(
        rect.center().x - galley.rect.width() / 2.0,
        rect.center().y - galley.rect.height() / 2.0,
    );
    ui.painter().galley(pos, galley, theme.text_secondary);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// The full category picker behind `more (N)`: fixed size, filter at the top, scrolling
/// list — it never grows with the number of categories. Returns the id the user picked.
fn category_picker(
    ui: &mut egui::Ui,
    theme: &Theme,
    aggs: &[CatAgg],
    selected: i64,
    filter: &mut String,
) -> (Option<i64>, bool) {
    let mut picked = None;
    let mut close = false;
    let modal = egui::Modal::new(egui::Id::new("category_picker")).show(ui.ctx(), |ui| {
        ui.set_width(PICKER_W);
        ui.allocate_exact_size(egui::vec2(PICKER_W, 0.0), egui::Sense::hover());
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Choose a category")
                    .font(t::sans_medium(t::SECTION_TITLE))
                    .color(theme.text_primary),
            );
            let x_w = text_width(ui, "\u{d7}", t::sans(t::SECTION_TITLE));
            ui.add_space((ui.max_rect().right() - ui.cursor().left() - x_w - 4.0).max(4.0));
            if link(ui, theme, "\u{d7}").clicked() {
                close = true;
            }
        });
        ui.add_space(6.0);
        ui.add(
            egui::TextEdit::singleline(filter)
                .hint_text("filter")
                .desired_width(PICKER_W - 8.0)
                .margin(egui::Margin::symmetric(10, 6))
                .font(egui::FontSelection::FontId(t::sans(t::BODY))),
        );
        ui.add_space(6.0);
        let needle = filter.trim().to_lowercase();
        egui::ScrollArea::vertical()
            .max_height(PICKER_LIST_H)
            .auto_shrink([false, false])
            .id_salt("category_picker_list")
            .show(ui, |ui| {
                for c in aggs.iter().filter(|c| {
                    needle.is_empty() || c.name.to_lowercase().contains(&needle)
                }) {
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 38.0),
                        egui::Sense::click(),
                    );
                    if c.id == selected || resp.hovered() {
                        ui.painter().rect_filled(
                            rect,
                            egui::CornerRadius::same(6),
                            if c.id == selected { theme.accent_tint_bg } else { theme.bg_sunken },
                        );
                    }
                    ui.painter().text(
                        rect.left_center() + egui::vec2(10.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        &c.name,
                        t::sans(t::BODY),
                        theme.text_primary,
                    );
                    ui.painter().text(
                        rect.right_center() - egui::vec2(10.0, 0.0),
                        egui::Align2::RIGHT_CENTER,
                        format!("{} types", c.type_count),
                        t::mono(t::EYEBROW),
                        theme.text_quiet,
                    );
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if resp.clicked() {
                        picked = Some(c.id);
                    }
                }
            });
    });
    if modal.should_close() {
        close = true;
    }
    (picked, close)
}

/// The "appears as" live preview: a mono uppercase category eyebrow over a sans-medium name,
/// in the tile's active colors — the same look a tallied tile uses on Today. A blank name
/// shows a placeholder so the chip never renders empty.
fn appears_as_chip(ui: &mut egui::Ui, theme: &Theme, category_name: &str, name: &str) {
    let trimmed = name.trim();
    let display_name = if trimmed.is_empty() { "Type name" } else { trimmed };
    egui::Frame::default()
        .fill(theme.tile_active_bg)
        .stroke(egui::Stroke::new(1.0, theme.tile_active_border))
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.label(
                    egui::RichText::new(category_name.to_uppercase())
                        .font(t::mono(t::TILE_CATEGORY))
                        .color(theme.tile_active_category),
                );
                ui.label(egui::RichText::new(display_name).font(t::sans_medium(t::TILE_NAME)).color(theme.tile_active_name));
            });
        });
}

/// The painted Active switch: a pill-shaped track + knob (accent when on, `bg_track` when
/// off), with the label and its consequence sub-line to the right. The whole block is
/// clickable, not just the track, and the knob eases with `animate_bool_with_time`.
fn active_switch(ui: &mut egui::Ui, theme: &Theme, on: bool) -> egui::Response {
    let full_w = ui.available_width();
    let block_h = 44.0;
    let (block_rect, resp) = ui.allocate_exact_size(egui::vec2(full_w, block_h), egui::Sense::click());
    ui.painter().rect(block_rect, egui::CornerRadius::same(6), theme.bg_sunken, egui::Stroke::NONE, egui::StrokeKind::Inside);

    let track_w = 36.0;
    let track_h = 20.0;
    let track_rect = egui::Rect::from_min_size(
        egui::pos2(block_rect.left() + 12.0, block_rect.center().y - track_h / 2.0),
        egui::vec2(track_w, track_h),
    );
    let anim = ui.ctx().animate_bool_with_time(ui.id().with("type_active_switch"), on, 0.12);
    let track_bg = if on { theme.accent } else { theme.bg_track };
    ui.painter().rect(track_rect, egui::CornerRadius::same((track_h / 2.0) as u8), track_bg, egui::Stroke::NONE, egui::StrokeKind::Inside);

    let knob_r = track_h / 2.0 - 2.0;
    let knob_x = track_rect.left() + knob_r + 2.0 + anim * (track_w - knob_r * 2.0 - 4.0);
    ui.painter().circle_filled(egui::pos2(knob_x, track_rect.center().y), knob_r, theme.bg_raised);

    let text_x = track_rect.right() + 10.0;
    ui.painter().text(
        egui::pos2(text_x, block_rect.top() + 8.0),
        egui::Align2::LEFT_TOP,
        "Active",
        t::sans_medium(t::BODY),
        theme.text_primary,
    );
    ui.painter().text(
        egui::pos2(text_x, block_rect.top() + 25.0),
        egui::Align2::LEFT_TOP,
        "Shows in the tile grid and quick add",
        t::sans(t::CAPTION),
        theme.text_quiet,
    );

    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// A primary (filled) button that can be disabled: dimmed fill, ignores hover/clicks. Used
/// for `Save` (disabled while the name validation fails) and the delete dialog's confirm
/// (whose fill and label both follow the selected option).
fn primary_button(ui: &mut egui::Ui, theme: &Theme, label: &str, enabled: bool, fill: egui::Color32) -> bool {
    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), t::sans_medium(t::BODY), theme.bg_raised);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(galley.rect.width() + 32.0, CTRL_H), sense);
    let draw_fill = if !enabled {
        fill.gamma_multiply(0.45)
    } else if resp.hovered() {
        // Brighten whatever fill was given (accent or negative) rather than hard-coding
        // `theme.accent_hover`, which would be wrong for the delete dialog's negative-fill
        // confirm button.
        fill.linear_multiply(1.15)
    } else {
        fill
    };
    ui.painter().rect(rect, egui::CornerRadius::same(6), draw_fill, egui::Stroke::NONE, egui::StrokeKind::Inside);
    ui.painter().galley(
        egui::pos2(rect.center().x - galley.rect.width() / 2.0, rect.center().y - galley.rect.height() / 2.0),
        galley,
        theme.bg_raised,
    );
    if enabled && resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    enabled && resp.clicked()
}

fn eyebrow(ui: &mut egui::Ui, theme: &Theme, text: &str) {
    ui.label(egui::RichText::new(text).font(t::mono(t::EYEBROW)).color(theme.text_tertiary));
}

// --- CSV import --------------------------------------------------------------------------

fn run_import(db: &Db) -> ImportOutcome {
    let Some(path) = rfd::FileDialog::new().add_filter("CSV", &["csv"]).pick_file() else {
        return ImportOutcome::Error(String::new()); // sentinel: caller checks for empty below
    };
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => return ImportOutcome::Error(e.to_string()),
    };
    let (rows, core_report) = match simpletally_core::csvio::import_task_types(&bytes, None) {
        Ok(v) => v,
        Err(e) => return ImportOutcome::Error(e.to_string()),
    };
    match crate::service::import::import_rows(db, &rows, &core_report) {
        Ok(report) => ImportOutcome::Report(report),
        Err(e) => ImportOutcome::Error(e.to_string()),
    }
}

fn import_report_modal(ui: &mut egui::Ui, state: &mut TypesState, theme: &Theme) {
    let Modal::ImportReport(outcome) = &state.modal else { return };
    // The user-cancelled file dialog is a no-op, not a modal (empty-string sentinel from
    // `run_import`).
    if let ImportOutcome::Error(msg) = outcome {
        if msg.is_empty() {
            state.modal = Modal::None;
            return;
        }
    }
    let mut close = false;
    egui::Modal::new(egui::Id::new("type_import_report")).show(ui.ctx(), |ui| {
        ui.set_width(360.0);
        ui.label(egui::RichText::new("Import CSV").font(t::sans_medium(t::SECTION_TITLE)).color(theme.text_primary));
        ui.separator();
        match outcome {
            ImportOutcome::Report(r) => {
                ui.label(egui::RichText::new(format!("Imported: {}", r.imported)).font(t::sans(t::BODY)).color(theme.text_body));
                ui.label(
                    egui::RichText::new(format!("Already existed: {}", r.skipped_existing))
                        .font(t::sans(t::BODY))
                        .color(theme.text_body),
                );
                ui.label(
                    egui::RichText::new(format!("Duplicate in file: {}", r.skipped_in_file))
                        .font(t::sans(t::BODY))
                        .color(theme.text_body),
                );
                ui.label(
                    egui::RichText::new(format!("Malformed rows skipped: {}", r.skipped_malformed))
                        .font(t::sans(t::BODY))
                        .color(theme.text_body),
                );
                ui.label(
                    egui::RichText::new(format!("Categories created: {}", r.categories_created))
                        .font(t::sans(t::BODY))
                        .color(theme.text_body),
                );
                if r.encoding == "windows-1252" {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("used windows-1252 encoding").font(t::sans(t::CAPTION)).color(theme.text_quiet),
                    );
                }
            }
            ImportOutcome::Error(e) => {
                ui.colored_label(theme.negative, e);
            }
        }
        ui.add_space(10.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(
                    egui::Button::new(egui::RichText::new("Close").font(t::sans_medium(t::BODY)).color(theme.bg_raised))
                        .fill(theme.accent)
                        .corner_radius(6)
                        .min_size(egui::vec2(74.0, 32.0)),
                )
                .clicked()
            {
                close = true;
            }
        });
    });
    if close {
        state.modal = Modal::None;
    }
}

// --- category management (screen 08) ------------------------------------------------------

/// Shared row height (drag handle / name+subline / pencil+Remove all sized to this).
const CATROW_H: f32 = 60.0;
const CATROW_GAP: f32 = 8.0;
const CATROW_HANDLE_X: f32 = 24.0;
const CATROW_NAME_X: f32 = 62.0;
const CATROW_ICON_W: f32 = 20.0;
const CATROW_PAD_R: f32 = 14.0;

fn load_categories_modal(db: &Db) -> simpletally_core::Result<CategoriesModal> {
    let categories = db.list_categories()?;
    let types = db.list_task_types(false)?;
    let lifetime = db.type_lifetime_totals()?;
    let rows = aggregate_categories(&categories, &types, &lifetime)
        .into_iter()
        .map(|a| CatRow { id: a.id, name: a.name, type_count: a.type_count, tally_count: a.tally_count, rename: None })
        .collect();
    Ok(CategoriesModal { rows, new_name: String::new(), error: None, drag: None, move_dialog: None })
}

/// Builds the move-category dialog for a category that still holds task types: the types
/// themselves (needed to re-point each via `edit_task_type`) and every other category with
/// its own type count, for the radio list.
fn open_move_dialog(db: &Db, category_id: i64, category_name: &str, rows: &[CatRow]) -> simpletally_core::Result<MoveDialogState> {
    let types: Vec<TaskType> =
        db.list_task_types(false)?.into_iter().filter(|t| t.category_id == category_id).collect();
    let lifetime = db.type_lifetime_totals()?;
    let tally_count: i64 = types
        .iter()
        .map(|t| lifetime.iter().find(|l| l.task_type_id == t.id).map(|l| l.total).unwrap_or(0))
        .sum();
    let others: Vec<(i64, String, i64)> =
        rows.iter().filter(|r| r.id != category_id).map(|r| (r.id, r.name.clone(), r.type_count)).collect();
    let target = others.first().map(|o| o.0).unwrap_or(0);
    Ok(MoveDialogState {
        category_id,
        category_name: category_name.to_string(),
        type_count: types.len() as i64,
        tally_count,
        types,
        others,
        target,
        error: None,
    })
}

fn manage_categories_modal(ui: &mut egui::Ui, state: &mut TypesState, db: &Db, theme: &Theme) {
    let Modal::ManageCategories(_) = &state.modal else { return };
    let mut close = false;
    let mut data_changed = false; // counts changed: reload rows from the DB
    let mut today_dirty = false; // Today's pill row needs a rebuild

    {
        let Modal::ManageCategories(modal) = &mut state.modal else { unreachable!() };
        egui::Modal::new(egui::Id::new("manage_categories")).show(ui.ctx(), |ui| {
            const MODAL_W: f32 = 440.0;
            ui.set_width(MODAL_W);
            // Pin the used rect to the full width: the modal frame sizes itself to its
            // content, so swapping a row into rename mode otherwise nudges it narrower.
            ui.allocate_exact_size(egui::vec2(MODAL_W, 0.0), egui::Sense::hover());

            // Header: title + counts, then × (single right_to_left after fixed-width
            // labels is fine — the LAYOUT TRAP is nested right_to_left colliding with
            // *several* left-hand controls, not this).
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Manage categories").font(t::sans_medium(t::SECTION_TITLE)).color(theme.text_primary),
                );
                ui.add_space(8.0);
                let n_types: i64 = modal.rows.iter().map(|r| r.type_count).sum();
                ui.label(
                    egui::RichText::new(format!("{} categories \u{b7} {} types", modal.rows.len(), n_types))
                        .font(t::mono(t::EYEBROW))
                        .color(theme.text_quiet),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if link(ui, theme, "\u{d7}").clicked() {
                        close = true;
                    }
                });
            });
            ui.separator();
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("ORDER ON TODAY").font(t::mono(t::EYEBROW)).color(theme.text_tertiary));
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("drag to reorder the pill row").font(t::sans(t::CAPTION)).color(theme.text_quiet),
                );
            });
            ui.add_space(8.0);

            // The list is capped and scrolls: with a dozen categories an uncapped list
            // pushed "Done" off the bottom of the screen. Header, the add row and the
            // footer stay pinned outside the scroll area.
            let cap = CATLIST_MAX_ROWS * (CATROW_H + CATROW_GAP);
            let actions = egui::ScrollArea::vertical()
                .max_height(cap)
                .auto_shrink([false, false])
                .id_salt("category_list")
                .show(ui, |ui| category_rows(ui, theme, modal))
                .inner;

            // --- apply row actions -------------------------------------------------------
            if let Some((from, to)) = actions.nudge {
                let ids: Vec<i64> = modal.rows.iter().map(|r| r.id).collect();
                let ordered = reordered_ids(&ids, from, to);
                if ordered != ids {
                    match db.reorder_categories(&ordered) {
                        Ok(()) => {
                            data_changed = true;
                            today_dirty = true;
                        }
                        Err(e) => modal.error = Some(e.to_string()),
                    }
                }
            }
            if let Some(id) = actions.start_rename {
                for r in modal.rows.iter_mut() {
                    r.rename = None;
                }
                if let Some(r) = modal.rows.iter_mut().find(|r| r.id == id) {
                    r.rename = Some(RenameState { buffer: r.name.clone(), just_opened: true, error: None });
                }
            }
            if let Some(id) = actions.cancel_rename {
                if let Some(r) = modal.rows.iter_mut().find(|r| r.id == id) {
                    r.rename = None;
                }
            }
            if let Some((id, name)) = actions.save_rename {
                match db.rename_category(id, &name) {
                    Ok(()) => {
                        data_changed = true;
                        today_dirty = true;
                    }
                    Err(Error::Duplicate(_)) => {
                        if let Some(r) = modal.rows.iter_mut().find(|r| r.id == id) {
                            if let Some(rs) = &mut r.rename {
                                rs.error = Some("a category with that name already exists".to_string());
                            }
                        }
                    }
                    Err(e) => {
                        if let Some(r) = modal.rows.iter_mut().find(|r| r.id == id) {
                            if let Some(rs) = &mut r.rename {
                                rs.error = Some(e.to_string());
                            }
                        }
                    }
                }
            }
            if let Some(id) = actions.remove {
                let hit = modal.rows.iter().find(|r| r.id == id).map(|r| (r.name.clone(), r.type_count));
                if let Some((name, type_count)) = hit {
                    if type_count == 0 {
                        match db.delete_category(id) {
                            Ok(()) => {
                                modal.error = None;
                                data_changed = true;
                                today_dirty = true;
                            }
                            Err(Error::Conflict(_)) => {
                                modal.error = Some("move or remove its task types first".to_string());
                            }
                            Err(e) => modal.error = Some(e.to_string()),
                        }
                    } else {
                        match open_move_dialog(db, id, &name, &modal.rows) {
                            Ok(md) => modal.move_dialog = Some(md),
                            Err(e) => modal.error = Some(e.to_string()),
                        }
                    }
                }
            }

            // --- drag reorder ------------------------------------------------------------
            if let Some((id, start_index, grab_offset)) = actions.drag_start {
                if modal.drag.is_none() {
                    modal.drag = Some(DragState { id, start_index, grab_offset });
                }
            }
            if let (Some(d), Some(target)) = (modal.drag.as_ref().map(|d| d.id), actions.drag_target) {
                let cur = modal.rows.iter().position(|r| r.id == d);
                if let Some(cur) = cur {
                    if cur != target {
                        let ids: Vec<i64> = modal.rows.iter().map(|r| r.id).collect();
                        let new_order = reordered_ids(&ids, cur, target);
                        modal.rows.sort_by_key(|r| new_order.iter().position(|&x| x == r.id).unwrap_or(0));
                    }
                }
            }
            if let Some((id, y)) = actions.drag_release_y {
                // Reseed the row's animation state at the position it was actually visually
                // released, or the next frame's `animate_value_with_time` call would ease it
                // from its stale pre-drag slot instead of from where the pointer left it.
                ui.ctx().animate_value_with_time(egui::Id::new(("catrow_y", id)), y, 0.0);
            }
            if actions.drag_end {
                if let Some(d) = modal.drag.take() {
                    let cur = modal.rows.iter().position(|r| r.id == d.id).unwrap_or(d.start_index);
                    if cur != d.start_index {
                        let ids: Vec<i64> = modal.rows.iter().map(|r| r.id).collect();
                        if let Err(e) = db.reorder_categories(&ids) {
                            modal.error = Some(e.to_string());
                        } else {
                            today_dirty = true;
                        }
                    }
                }
            }

            // --- add row -------------------------------------------------------------------
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().interact_size.y = CTRL_H;
                ui.add(
                    egui::TextEdit::singleline(&mut modal.new_name)
                        .hint_text("New category name")
                        .desired_width(240.0)
                        .margin(egui::Margin::symmetric(10, 7))
                        .font(egui::FontSelection::FontId(t::sans(t::BODY))),
                );
                ui.add_space(10.0);
                if solid_button(ui, theme, "Add").clicked() {
                    match db.create_category(&modal.new_name) {
                        Ok(_) => {
                            modal.new_name.clear();
                            modal.error = None;
                            data_changed = true;
                            today_dirty = true;
                        }
                        Err(Error::Duplicate(_)) => {
                            modal.error = Some("a category with that name already exists".to_string());
                        }
                        Err(e) => modal.error = Some(e.to_string()),
                    }
                }
            });

            if let Some(err) = &modal.error {
                ui.add_space(6.0);
                ui.colored_label(theme.negative, err);
            }

            // --- footer ---------------------------------------------------------------------
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Changes apply immediately").font(t::sans(t::CAPTION)).color(theme.text_quiet),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if outline_button(ui, theme, "Done").clicked() {
                        close = true;
                    }
                });
            });
        });

        move_dialog(ui, modal, db, theme, &mut data_changed, &mut today_dirty);
    }

    if today_dirty {
        state.categories_changed = true;
    }
    if close {
        state.modal = Modal::None;
        state.mark_dirty();
        return;
    }
    if data_changed {
        if let Ok(fresh) = load_categories_modal(db) {
            let Modal::ManageCategories(modal) = &mut state.modal else { unreachable!() };
            let error = modal.error.take();
            let new_name = std::mem::take(&mut modal.new_name);
            *modal = fresh;
            modal.error = error;
            modal.new_name = new_name;
        }
        state.mark_dirty();
    }
}

/// Per-frame outcome of drawing the category rows — collected during the (immutable-ish)
/// paint pass and applied afterwards, same shape as the rest of the file's modals.
#[derive(Default)]
struct RowActions {
    start_rename: Option<i64>,
    save_rename: Option<(i64, String)>,
    cancel_rename: Option<i64>,
    remove: Option<i64>,
    /// A row asked to move one slot up / down via its arrows — the alternative to dragging,
    /// which is hard to use once the list scrolls.
    nudge: Option<(usize, usize)>,
    /// (dragged row id, its index when the drag started, grab offset).
    drag_start: Option<(i64, usize, f32)>,
    /// The index the dragged row should currently occupy.
    drag_target: Option<usize>,
    drag_end: bool,
    /// (dragged row id, its on-screen y the frame the drag ended) — reseeds that row's
    /// animation state so it doesn't jump back to its pre-drag slot before easing out.
    drag_release_y: Option<(i64, f32)>,
}

/// Draws every category row. Explicit-rect banding per the module's LAYOUT TRAP note: one
/// `allocate_exact_size` for the whole row, then interactive sub-regions via `ui.interact`
/// / `ui.put` at computed rects — never a nested `right_to_left` inside this loop, which is
/// exactly what the old implementation collided on.
fn category_rows(ui: &mut egui::Ui, theme: &Theme, modal: &mut CategoriesModal) -> RowActions {
    let mut actions = RowActions::default();
    let avail = ui.available_width();
    let list_top = ui.cursor().top();
    let n = modal.rows.len();

    let slot_h = CATROW_H + CATROW_GAP;
    let max_top = list_top + slot_h * (n.saturating_sub(1)) as f32;

    for (i, row) in modal.rows.iter_mut().enumerate() {
        let (slot_rect, _) = ui.allocate_exact_size(egui::vec2(avail, CATROW_H), egui::Sense::hover());
        let being_dragged = modal.drag.as_ref().map(|d| d.id) == Some(row.id);

        // Non-dragged rows glide to their new slot; the dragged row follows the pointer
        // directly (grab-offset adjusted, clamped to the list) with no animation, so it
        // doesn't fight the user's hand.
        let anim_id = egui::Id::new(("catrow_y", row.id));
        let draw_y = if being_dragged {
            let grab_offset = modal.drag.as_ref().map(|d| d.grab_offset).unwrap_or(CATROW_H / 2.0);
            let p = ui.ctx().pointer_interact_pos().or_else(|| ui.ctx().pointer_hover_pos());
            let y = p.map(|p| p.y - grab_offset).unwrap_or(slot_rect.top());
            y.clamp(list_top, max_top)
        } else {
            ui.ctx().animate_value_with_time(anim_id, slot_rect.top(), 0.12)
        };
        let rect = egui::Rect::from_min_size(egui::pos2(slot_rect.left(), draw_y), slot_rect.size());

        let border = if being_dragged { theme.accent_tint_border } else { theme.border_subtle };
        ui.painter().rect(rect, egui::CornerRadius::same(6), theme.bg_raised, egui::Stroke::new(1.0, border), egui::StrokeKind::Inside);

        // Drag handle.
        let handle_rect = egui::Rect::from_center_size(
            egui::pos2(rect.left() + CATROW_HANDLE_X, rect.center().y),
            egui::vec2(20.0, 20.0),
        );
        // Up/down arrows beside the handle: dragging is nice but unusable once the list
        // scrolls, and it is the only way to reorder without a steady hand.
        let arrow_x = rect.left() + CATROW_HANDLE_X + 14.0;
        let up_rect = egui::Rect::from_min_size(
            egui::pos2(arrow_x, rect.center().y - 15.0),
            egui::vec2(14.0, 14.0),
        );
        let down_rect = egui::Rect::from_min_size(
            egui::pos2(arrow_x, rect.center().y + 1.0),
            egui::vec2(14.0, 14.0),
        );
        for (r, up, enabled) in [(up_rect, true, i > 0), (down_rect, false, i + 1 < n)] {
            let resp = ui.interact(
                r,
                ui.id().with(("cat_nudge", row.id, up)),
                if enabled { egui::Sense::click() } else { egui::Sense::hover() },
            );
            let color = if !enabled {
                theme.text_disabled
            } else if resp.hovered() {
                theme.accent
            } else {
                theme.text_tertiary
            };
            chevron(ui.painter(), r, up, color);
            if enabled && resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                actions.nudge = Some((i, if up { i - 1 } else { i + 1 }));
            }
        }

        let handle_resp = ui.interact(handle_rect, ui.id().with(("cat_drag", row.id)), egui::Sense::drag());
        grip_icon(ui.painter(), handle_rect.center(), theme.text_tertiary);
        if handle_resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if handle_resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        if handle_resp.drag_started() {
            // Not dragging yet this frame, so `rect` above is still the settled slot rect —
            // exactly the baseline the offset should be measured from.
            let grab_offset = handle_resp.interact_pointer_pos().map(|p| p.y - rect.top()).unwrap_or(CATROW_H / 2.0);
            actions.drag_start = Some((row.id, i, grab_offset));
        }
        if being_dragged {
            if let Some(p) = handle_resp.interact_pointer_pos().or_else(|| ui.ctx().pointer_interact_pos()) {
                let grab_offset = modal.drag.as_ref().map(|d| d.grab_offset).unwrap_or(CATROW_H / 2.0);
                actions.drag_target = Some(drag_target_index(p.y, grab_offset, list_top, slot_h, n));
            }
            if handle_resp.drag_stopped() {
                actions.drag_end = true;
                actions.drag_release_y = Some((row.id, draw_y));
            }
        }

        if let Some(rs) = &mut row.rename {
            let save_w = text_width(ui, "Save", t::sans_medium(t::BODY));
            let save_rect = egui::Rect::from_min_size(
                egui::pos2(rect.right() - CATROW_PAD_R - save_w, rect.top()),
                egui::vec2(save_w, CATROW_H),
            );
            let save_resp = text_action(ui, save_rect, "Save", t::sans_medium(t::BODY), theme.accent, row.id, "save");

            let te_left = rect.left() + CATROW_NAME_X;
            let te_rect = egui::Rect::from_min_size(
                egui::pos2(te_left, rect.center().y - 14.0),
                egui::vec2((save_rect.left() - 10.0 - te_left).max(40.0), 28.0),
            );
            // A child Ui, not `ui.put`: `put` allocates into the parent layout, which
            // collapsed the renaming row and shifted every row below it up -- the modal
            // visibly resized the moment a rename started. The row rect is already
            // allocated; this only draws inside it.
            let te_resp = ui
                .new_child(
                    egui::UiBuilder::new()
                        .id_salt(("cat_rename", row.id))
                        .max_rect(te_rect)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                )
                .add(
                    egui::TextEdit::singleline(&mut rs.buffer)
                        .desired_width(te_rect.width())
                        .font(egui::FontSelection::FontId(t::sans(t::BODY))),
                );
            if rs.just_opened {
                te_resp.request_focus();
                rs.just_opened = false;
            }

            let esc = ui.input(|inp| inp.key_pressed(egui::Key::Escape));
            if save_resp.clicked() {
                actions.save_rename = Some((row.id, rs.buffer.clone()));
            } else if esc || te_resp.lost_focus() {
                // Esc, or clicking away (lost_focus without Save), both cancel back to
                // the DB's name.
                actions.cancel_rename = Some(row.id);
            }

            if let Some(err) = &rs.error {
                ui.painter().text(
                    rect.left_top() + egui::vec2(CATROW_NAME_X, 34.0),
                    egui::Align2::LEFT_TOP,
                    err,
                    t::sans(t::CAPTION),
                    theme.negative,
                );
            }
        } else {
            let removable = row.type_count == 0;
            let remove_color = if removable { theme.negative } else { theme.text_disabled };
            let remove_w = text_width(ui, "Remove", t::sans_medium(t::BODY));
            let remove_rect = egui::Rect::from_min_size(
                egui::pos2(rect.right() - CATROW_PAD_R - remove_w, rect.top()),
                egui::vec2(remove_w, CATROW_H),
            );
            // Always clickable — a greyed Remove opens the move dialog rather than refusing.
            let remove_resp = text_action(ui, remove_rect, "Remove", t::sans_medium(t::BODY), remove_color, row.id, "remove");

            let pencil_rect = egui::Rect::from_min_size(
                egui::pos2(remove_rect.left() - 10.0 - CATROW_ICON_W, rect.center().y - CATROW_ICON_W / 2.0),
                egui::vec2(CATROW_ICON_W, CATROW_ICON_W),
            );
            let pencil_resp = ui.interact(pencil_rect, ui.id().with(("cat_pencil", row.id)), egui::Sense::click());
            pencil_icon(ui.painter(), pencil_rect, theme.text_secondary);
            if pencil_resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }

            ui.painter().text(
                rect.left_top() + egui::vec2(CATROW_NAME_X, 10.0),
                egui::Align2::LEFT_TOP,
                &row.name,
                t::sans_medium(t::TILE_NAME),
                theme.text_primary,
            );
            ui.painter().text(
                rect.left_top() + egui::vec2(CATROW_NAME_X, 30.0),
                egui::Align2::LEFT_TOP,
                format!(
                    "{} {} \u{b7} {} {}",
                    row.type_count,
                    if row.type_count == 1 { "type" } else { "types" },
                    row.tally_count,
                    if row.tally_count == 1 { "tally" } else { "tallies" }
                ),
                t::sans(t::CAPTION),
                theme.text_quiet,
            );

            if pencil_resp.clicked() {
                actions.start_rename = Some(row.id);
            }
            if remove_resp.clicked() {
                actions.remove = Some(row.id);
            }
        }

        ui.add_space(CATROW_GAP);
    }
    actions
}

/// The move-then-remove dialog (design's third panel): shown on top of the categories
/// modal when "Remove" is clicked on a category that still holds task types.
fn move_dialog(
    ui: &mut egui::Ui,
    modal: &mut CategoriesModal,
    db: &Db,
    theme: &Theme,
    data_changed: &mut bool,
    today_dirty: &mut bool,
) {
    let Some(md) = &mut modal.move_dialog else { return };
    let mut close = false;
    let mut confirm = false;

    egui::Modal::new(egui::Id::new("cat_move")).show(ui.ctx(), |ui| {
        ui.set_width(380.0);
        ui.label(
            egui::RichText::new(format!("Remove {}", md.category_name.to_uppercase()))
                .font(t::sans_medium(t::SECTION_TITLE))
                .color(theme.text_primary),
        );
        ui.separator();
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(format!(
                "It holds {} task type(s) with {} tallies. Move the type(s) somewhere before the category goes.",
                md.type_count, md.tally_count
            ))
            .font(t::sans(t::BODY))
            .color(theme.text_body),
        );
        ui.add_space(10.0);
        eyebrow(ui, theme, &format!("MOVE ITS {} TYPE(S) TO", md.type_count));
        ui.add_space(4.0);
        for (id, name, count) in &md.others {
            ui.horizontal(|ui| {
                ui.radio_value(&mut md.target, *id, name);
                let label = format!("{count} types");
                let w = text_width(ui, &label, t::mono(t::EYEBROW));
                let free = ui.max_rect().right() - ui.cursor().left() - w;
                ui.add_space(free.max(8.0));
                ui.label(egui::RichText::new(label).font(t::mono(t::EYEBROW)).color(theme.text_tertiary));
            });
        }
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new("No tallies are deleted \u{2014} the entries follow their task type.")
                .font(t::sans(t::CAPTION))
                .color(theme.text_quiet),
        );
        if let Some(err) = &md.error {
            ui.add_space(6.0);
            ui.colored_label(theme.negative, err);
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if outline_button(ui, theme, "Cancel").clicked() {
                close = true;
            }
            let w = button_width(ui, "Move and remove");
            let free = ui.max_rect().right() - ui.cursor().left() - w;
            ui.add_space(free.max(10.0));
            if solid_button(ui, theme, "Move and remove").clicked() {
                confirm = true;
            }
        });
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            close = true;
        }
    });

    if close {
        modal.move_dialog = None;
        return;
    }
    if confirm {
        let md = modal.move_dialog.as_mut().expect("checked above");
        let mut ok = true;
        for ty in &md.types {
            if let Err(e) = db.edit_task_type(ty.id, md.target, &ty.name, &ty.description, ty.is_active) {
                md.error = Some(e.to_string());
                ok = false;
                break;
            }
        }
        if ok {
            match db.delete_category(md.category_id) {
                Ok(()) => {
                    modal.move_dialog = None;
                    *data_changed = true;
                    *today_dirty = true;
                }
                Err(e) => md.error = Some(e.to_string()),
            }
        }
    }
}

/// The width [`text_action`]/the row layout will need for `label`, unpadded (unlike
/// [`button_width`], which adds button chrome).
fn text_width(ui: &egui::Ui, label: &str, font: egui::FontId) -> f32 {
    ui.painter().layout_no_wrap(label.to_owned(), font, egui::Color32::PLACEHOLDER).rect.width()
}

/// A right-aligned text action within an explicit `rect` (design: row-level `Save` /
/// `Remove`). Painted + `ui.interact`, not a flow widget — see the module's LAYOUT TRAP
/// note on why rows here avoid nested layouts.
fn text_action(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    label: &str,
    font: egui::FontId,
    color: egui::Color32,
    key: i64,
    tag: &str,
) -> egui::Response {
    let resp = ui.interact(rect, ui.id().with((tag, key)), egui::Sense::click());
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, color);
    ui.painter().galley(
        egui::pos2(rect.right() - galley.rect.width(), rect.center().y - galley.rect.height() / 2.0),
        galley,
        color,
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// The drag handle: a painted 2x3 dot grid (design allows `\u{2807}` or a painted grid —
/// the bundled DM Sans doesn't carry the Braille glyph reliably at UI sizes, so this is
/// painted rather than risking tofu, same call as the `\u{d7}` glyph note elsewhere).
fn grip_icon(painter: &egui::Painter, center: egui::Pos2, color: egui::Color32) {
    const R: f32 = 1.3;
    const DX: f32 = 5.0;
    const DY: f32 = 5.0;
    for col in 0..2 {
        for row in 0..3 {
            let x = center.x - DX / 2.0 + col as f32 * DX;
            let y = center.y - DY + row as f32 * DY;
            painter.circle_filled(egui::pos2(x, y), R, color);
        }
    }
}

/// The rename affordance: a small painted pencil (body + tip), for the same tofu-risk
/// reason as [`grip_icon`] — DM Sans has no reliable pencil glyph at this size.
fn pencil_icon(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    // Geometry from Lucide's `pencil` (ISC), on its 24x24 grid, with the four corner arcs
    // flattened to their endpoints — they are 0.5-2px roundings, invisible at this size.
    // A line plus a dot (what this used to be) reads as an arrow, not a pencil.
    const BODY: [(f32, f32); 6] = [
        (21.17, 6.81),
        (17.19, 2.83),
        (3.84, 16.17),
        (2.02, 21.36),
        (2.64, 21.98),
        (7.83, 20.16),
    ];
    const FERRULE: [(f32, f32); 2] = [(15.0, 5.0), (19.0, 9.0)];

    let scale = rect.width().min(rect.height()) / 24.0;
    let at = |(x, y): (f32, f32)| rect.left_top() + egui::vec2(x * scale, y * scale);
    let stroke = egui::Stroke::new((1.6 * scale).max(1.0), color);
    painter.add(egui::Shape::closed_line(BODY.iter().copied().map(at).collect(), stroke));
    painter.line_segment([at(FERRULE[0]), at(FERRULE[1])], stroke);
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
            created_at: String::new(),
        }
    }

    fn sample() -> Vec<TaskType> {
        vec![
            ty(1, 10, "Rekonsiliasi", "monthly bank recon", true),
            ty(2, 10, "Setoran", "", false),
            ty(3, 20, "Approval", "final sign-off step", true),
        ]
    }

    #[test]
    fn no_filters_returns_everything() {
        let types = sample();
        let rows = filter_types(&types, "", CategoryFilter::All, ActiveFilter::All);
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn active_only_excludes_inactive() {
        let types = sample();
        let rows = filter_types(&types, "", CategoryFilter::All, ActiveFilter::ActiveOnly);
        assert_eq!(rows.iter().map(|t| t.id).collect::<Vec<_>>(), vec![1, 3]);
    }

    #[test]
    fn category_filter_restricts_to_one_category() {
        let types = sample();
        let rows = filter_types(&types, "", CategoryFilter::One(10), ActiveFilter::All);
        assert_eq!(rows.iter().map(|t| t.id).collect::<Vec<_>>(), vec![1, 2]);
    }

    #[test]
    fn search_matches_name_case_insensitively() {
        let types = sample();
        let rows = filter_types(&types, "rekon", CategoryFilter::All, ActiveFilter::All);
        assert_eq!(rows.iter().map(|t| t.id).collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn search_matches_description_case_insensitively() {
        let types = sample();
        let rows = filter_types(&types, "SIGN-OFF", CategoryFilter::All, ActiveFilter::All);
        assert_eq!(rows.iter().map(|t| t.id).collect::<Vec<_>>(), vec![3]);
    }

    #[test]
    fn search_and_category_and_active_combine() {
        let types = sample();
        // "Setoran" matches the search but is inactive and ActiveOnly excludes it.
        let rows = filter_types(&types, "setoran", CategoryFilter::One(10), ActiveFilter::ActiveOnly);
        assert!(rows.is_empty());
        let rows = filter_types(&types, "setoran", CategoryFilter::One(10), ActiveFilter::All);
        assert_eq!(rows.iter().map(|t| t.id).collect::<Vec<_>>(), vec![2]);
    }

    #[test]
    fn blank_search_is_treated_as_no_filter() {
        let types = sample();
        let rows = filter_types(&types, "   ", CategoryFilter::All, ActiveFilter::All);
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn no_match_returns_empty() {
        let types = sample();
        let rows = filter_types(&types, "nonexistent", CategoryFilter::All, ActiveFilter::All);
        assert!(rows.is_empty());
    }

    // --- reordered_ids ---------------------------------------------------------------------

    #[test]
    fn reordered_ids_moves_middle_forward() {
        assert_eq!(reordered_ids(&[1, 2, 3, 4], 1, 2), vec![1, 3, 2, 4]);
    }

    #[test]
    fn reordered_ids_moves_middle_backward() {
        assert_eq!(reordered_ids(&[1, 2, 3, 4], 2, 0), vec![3, 1, 2, 4]);
    }

    #[test]
    fn reordered_ids_noop_when_from_equals_to() {
        assert_eq!(reordered_ids(&[1, 2, 3], 1, 1), vec![1, 2, 3]);
    }

    #[test]
    fn reordered_ids_to_the_front() {
        assert_eq!(reordered_ids(&[1, 2, 3], 2, 0), vec![3, 1, 2]);
    }

    #[test]
    fn reordered_ids_to_the_back() {
        assert_eq!(reordered_ids(&[1, 2, 3], 0, 2), vec![2, 3, 1]);
    }

    #[test]
    fn reordered_ids_out_of_range_from_is_noop() {
        assert_eq!(reordered_ids(&[1, 2, 3], 9, 0), vec![1, 2, 3]);
    }

    // --- drag_target_index -------------------------------------------------------------------

    #[test]
    fn drag_target_index_at_rest_stays_in_its_own_slot() {
        // Row 1 (slot_h 68, list_top 100) grabbed at its own top: pointer sitting exactly at
        // the slot boundary with no movement resolves back to index 1.
        let list_top = 100.0;
        let slot_h = 68.0;
        let grab_offset = 10.0; // grabbed 10px below the row's top edge
        let pointer_y = list_top + slot_h + grab_offset;
        assert_eq!(drag_target_index(pointer_y, grab_offset, list_top, slot_h, 4), 1);
    }

    #[test]
    fn drag_target_index_moves_down_past_the_midpoint() {
        let list_top = 0.0;
        let slot_h = 60.0;
        let grab_offset = 20.0;
        // Row top pushed just past the midpoint between slot 0 and slot 1 (30px) snaps to 1.
        let pointer_y = 31.0 + grab_offset;
        assert_eq!(drag_target_index(pointer_y, grab_offset, list_top, slot_h, 5), 1);
    }

    #[test]
    fn drag_target_index_clamps_above_the_list() {
        let grab_offset = 15.0;
        // Pointer above the list entirely — clamps to the first slot, not a negative/huge index.
        assert_eq!(drag_target_index(-500.0, grab_offset, 100.0, 60.0, 3), 0);
    }

    #[test]
    fn drag_target_index_clamps_below_the_list() {
        let grab_offset = 15.0;
        assert_eq!(drag_target_index(5000.0, grab_offset, 100.0, 60.0, 3), 2);
    }

    #[test]
    fn drag_target_index_respects_the_grab_offset() {
        // Same pointer position, two different grab offsets land in different slots.
        let list_top = 0.0;
        let slot_h = 60.0;
        let pointer_y = 100.0;
        assert_eq!(drag_target_index(pointer_y, 10.0, list_top, slot_h, 5), 2); // row top 90 -> slot 2 (round(1.5)=2)
        assert_eq!(drag_target_index(pointer_y, 40.0, list_top, slot_h, 5), 1); // row top 60 -> slot 1
    }

    // --- aggregate_categories ---------------------------------------------------------------

    fn cat(id: i64, name: &str) -> Category {
        Category { id, name: name.to_string(), sort_order: id, created_at: String::new() }
    }

    fn lt(task_type_id: i64, total: i64) -> TypeLifetimeTotal {
        TypeLifetimeTotal { task_type_id, total }
    }

    #[test]
    fn aggregate_categories_counts_types_and_sums_tallies_per_category() {
        let categories = vec![cat(10, "SAKTI"), cat(20, "DIGIPAY")];
        let types = vec![
            ty(1, 10, "Rekonsiliasi", "", true),
            ty(2, 10, "Setoran", "", true),
            ty(3, 20, "Approval", "", true),
        ];
        let lifetime = vec![lt(1, 200), lt(2, 9), lt(3, 49)];
        let aggs = aggregate_categories(&categories, &types, &lifetime);
        assert_eq!(aggs.len(), 2);
        assert_eq!((aggs[0].id, aggs[0].type_count, aggs[0].tally_count), (10, 2, 209));
        assert_eq!((aggs[1].id, aggs[1].type_count, aggs[1].tally_count), (20, 1, 49));
    }

    #[test]
    fn aggregate_categories_empty_category_has_zero_counts() {
        let categories = vec![cat(30, "DIGITALISASI")];
        let aggs = aggregate_categories(&categories, &[], &[]);
        assert_eq!((aggs[0].type_count, aggs[0].tally_count), (0, 0));
    }

    // --- pill_slots --------------------------------------------------------------------

    fn agg(id: i64, name: &str, tallies: i64) -> CatAgg {
        CatAgg { id, name: name.to_string(), type_count: 0, tally_count: tallies }
    }

    #[test]
    fn pill_slots_shows_everything_when_it_fits() {
        let cats = vec![agg(1, "SAKTI", 5), agg(2, "DIGIPAY", 3)];
        let (visible, hidden) = pill_slots(&cats, 1, 5);
        assert_eq!(visible, vec![1, 2]);
        assert_eq!(hidden, 0);
    }

    #[test]
    fn pill_slots_keeps_the_most_used_in_the_users_own_order() {
        // Usage picks WHICH survive; the drag order decides how they read.
        let cats = vec![agg(1, "A", 1), agg(2, "B", 90), agg(3, "C", 50), agg(4, "D", 2)];
        let (visible, hidden) = pill_slots(&cats, 2, 2);
        assert_eq!(visible, vec![2, 3]);
        assert_eq!(hidden, 2);
    }

    #[test]
    fn pill_slots_always_keeps_the_selected_one_even_when_unused() {
        let cats = vec![agg(1, "A", 90), agg(2, "B", 50), agg(3, "NEVER", 0)];
        let (visible, hidden) = pill_slots(&cats, 3, 2);
        assert_eq!(visible, vec![1, 2, 3]);
        assert_eq!(hidden, 0);
    }

    #[test]
    fn pill_slots_ties_keep_the_users_order() {
        let cats = vec![agg(1, "A", 7), agg(2, "B", 7), agg(3, "C", 7)];
        let (visible, _) = pill_slots(&cats, 1, 2);
        assert_eq!(visible, vec![1, 2]);
    }

    #[test]
    fn pill_slots_unknown_selection_is_not_invented() {
        let cats = vec![agg(1, "A", 5)];
        let (visible, hidden) = pill_slots(&cats, 999, 5);
        assert_eq!(visible, vec![1]);
        assert_eq!(hidden, 0);
    }

    // --- validate_type_name ------------------------------------------------------------

    #[test]
    fn validate_type_name_clean_name_is_ok() {
        let types = sample();
        assert!(validate_type_name(&types, 10, "Brand New", None).is_ok());
    }

    #[test]
    fn validate_type_name_exact_duplicate_in_same_category_is_blocked() {
        let types = sample();
        assert!(validate_type_name(&types, 10, "Rekonsiliasi", None).is_err());
    }

    #[test]
    fn validate_type_name_case_only_duplicate_is_blocked() {
        let types = sample();
        assert!(validate_type_name(&types, 10, "rekonsiliasi", None).is_err());
    }

    #[test]
    fn validate_type_name_same_name_in_a_different_category_is_allowed() {
        let types = sample();
        // "Approval" (id 3) lives in category 20; the same name in category 10 is fine.
        assert!(validate_type_name(&types, 10, "Approval", None).is_ok());
    }

    #[test]
    fn validate_type_name_editing_type_keeps_its_own_name() {
        let types = sample();
        // Type 1 ("Rekonsiliasi") checked against its own unchanged name must not collide
        // with itself.
        assert!(validate_type_name(&types, 10, "Rekonsiliasi", Some(1)).is_ok());
    }

    #[test]
    fn validate_type_name_empty_or_whitespace_name_is_blocked() {
        let types = sample();
        assert!(validate_type_name(&types, 10, "", None).is_err());
        assert!(validate_type_name(&types, 10, "   ", None).is_err());
    }

    #[test]
    fn aggregate_categories_missing_lifetime_row_counts_as_zero() {
        // A task type with no lifetime_totals row (never tallied) contributes 0, not a panic.
        let categories = vec![cat(10, "SAKTI")];
        let types = vec![ty(1, 10, "Rekonsiliasi", "", true)];
        let aggs = aggregate_categories(&categories, &types, &[]);
        assert_eq!((aggs[0].type_count, aggs[0].tally_count), (1, 0));
    }
}
