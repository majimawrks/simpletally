//! The Today screen (Screen 01) — daily tallying (PLAN §1.1, §6; design `_rustrefactor`
//! README §Screen 01).
//!
//! This holds the screen's state and its interaction logic. Rendering currently uses plain
//! egui widgets; the designed tally-tile / category-pill / day-log-row widgets and the theme
//! are layered on next (this is the working spine that proves the data + interaction pipe).
//!
//! State model (PLAN §5): query results live in a `View` rebuilt only when a `dirty` flag is
//! set; every mutation sets it. No caching beyond that, no Refresh button. `pending_note`
//! clears only after a write commits, and is discarded when the date changes (PLAN §6).

use crate::ui::theme::Theme;
use crate::ui::widgets::pill;
use chrono::{Datelike, Local, NaiveDate};
use simpletally_core::aggregate::{DayLogRow, DayTypeTotal};
use simpletally_core::entries::RemoveOutcome;
use simpletally_core::{Category, Db, TaskType};

/// Which category the tile grid is filtered to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CategoryFilter {
    All,
    One(i64),
}

pub struct TodayState {
    pub selected_date: NaiveDate,
    /// True while Today tracks the real calendar date frame-to-frame (the normal state).
    /// False once `‹ Prev`/`Next ›` pins to an explicit date; `Today` restores it. Driving
    /// writes off this instead of `selected_date` alone is what lets a per-frame snap-to-
    /// today coexist with a deliberately pinned date (see `show`'s snap check and `plan_for`).
    pub following_today: bool,
    /// The real calendar date on the day the current pin was made (not the date pinned to).
    /// `None` while following. Used to notice a day change under a stale pin: if it no longer
    /// equals "today" when a write is attempted, the pin's intent needs to be renewed first.
    pub pinned_on: Option<NaiveDate>,
    /// A write gesture landed on a stale pin and was consumed instead of written; the banner
    /// is showing the renewal question until `Use today`/`Continue with <date>` resolves it.
    asking: bool,
    pub filter: CategoryFilter,
    pub note_open: bool,
    /// Collapses the day-log band to its newest row, handing the space to the tile grid.
    pub log_collapsed: bool,
    pub pending_note: String,
    pub last_action: Option<String>,
    /// Set by [`TodayState::mark_written`]; drained by `app.rs` to mark Insights dirty.
    data_changed: bool,
    /// The open edit-entry dialog, if any (clicking a log row opens it).
    editing: Option<EditForm>,
    dirty: bool,
    view: Option<View>,
    error: Option<String>,
}

/// Draft state for the edit-entry dialog (PLAN §4; design screen 06).
struct EditForm {
    entry_id: i64,
    task_type_id: i64,
    date: String,
    count: i64,
    notes: String,
    /// `HH:MM` the entry was logged at, for the title row.
    logged_at: String,
    /// The type picker is hidden behind the "Change" link (design 07).
    picking_type: bool,
    /// The delete confirmation modal is open.
    confirm_delete: bool,
    error: Option<String>,
}

/// The screen's query results, rebuilt from the DB when `dirty`.
struct View {
    categories: Vec<Category>,
    /// Active types in the current filter, in display order.
    types: Vec<TaskType>,
    /// All active types (unfiltered), for the edit dialog's type picker.
    all_types: Vec<TaskType>,
    /// task_type_id -> count tallied on the selected day.
    day_counts: std::collections::BTreeMap<i64, i64>,
    day_total: i64,
    distinct_types: i64,
    /// Per-category count for the selected day (category_id -> total), for the pills.
    category_day_counts: std::collections::BTreeMap<i64, i64>,
    log: Vec<DayLogRow>,
}

impl TodayState {
    pub fn new() -> Self {
        Self {
            selected_date: Local::now().date_naive(),
            following_today: true,
            pinned_on: None,
            asking: false,
            filter: CategoryFilter::All,
            note_open: false,
            log_collapsed: false,
            pending_note: String::new(),
            last_action: None,
            data_changed: false,
            editing: None,
            dirty: true,
            view: None,
            error: None,
        }
    }

    /// `pub(crate)`: also called from `app.rs` when the manage-categories modal (on the
    /// Task types screen) writes something that changes the category pill row here.
    pub(crate) fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// A write landed: this screen must rebuild, and so must Insights, whose cached view is
    /// now stale. Set at every mutation site — a filter or date change calls `mark_dirty`
    /// alone, because nothing outside this screen changed.
    fn mark_written(&mut self) {
        self.dirty = true;
        self.data_changed = true;
    }

    /// Consumed by `app.rs` once per frame: did a write here invalidate the other screens?
    /// Without this, tallying on Today left an already-loaded Insights showing stale totals
    /// while its CSV export queried fresh data — the two disagreed.
    pub fn take_data_changed(&mut self) -> bool {
        std::mem::take(&mut self.data_changed)
    }

    /// Reset for a **different database** (the migration import swaps the whole file out).
    /// Marking the view dirty is not enough: any open dialog still holds raw row ids from
    /// the old database, and ids are small sequential integers, so saving one against the
    /// newly imported data would silently overwrite an unrelated row.
    pub(crate) fn reset_for_new_database(&mut self) {
        self.editing = None;
        self.pending_note.clear();
        self.last_action = None;
        self.error = None;
        self.mark_dirty();
    }

    /// Move to another date: rebuild, and discard an abandoned note (PLAN §6).
    fn set_date(&mut self, date: NaiveDate) {
        if date != self.selected_date {
            self.selected_date = date;
            self.pending_note.clear();
            self.last_action = None;
            self.mark_dirty();
        }
    }

    /// The automatic midnight/clock-change advance, as opposed to [`set_date`]'s user-driven
    /// move. **Keeps `pending_note`.**
    ///
    /// PLAN §6 discards an abandoned note when the date changes, but that rule is about the
    /// user *leaving* — pressing `‹ Prev`, or the `Today` pill. Rolling over at midnight is
    /// something the app does on its own, and wiping half-typed text the user is still looking
    /// at would be silent loss of the only content here they can't retype from memory. The
    /// note does end up attached to the new day if they then tally, which is a real trade-off
    /// — but it is visible in the field and they can edit or clear it, where a wipe is not
    /// recoverable.
    fn advance_to(&mut self, today: NaiveDate) {
        if today != self.selected_date {
            self.selected_date = today;
            self.last_action = None;
            self.mark_dirty();
        }
    }

    /// `Today` pill / `Return to today` link / `Use today` renewal answer: go back to
    /// following the real calendar date.
    fn resume_following(&mut self, today: NaiveDate) {
        self.following_today = true;
        self.pinned_on = None;
        self.asking = false;
        self.set_date(today);
    }

    /// `‹ Prev` / `Next ›`: pin to an explicit date. `pinned_on` records *today*, the day the
    /// pin was made — not `date` — so a later frame can tell whether the calendar has since
    /// rolled over on a pin that's still in effect.
    fn pin_to(&mut self, date: NaiveDate, today: NaiveDate) {
        self.following_today = false;
        self.pinned_on = Some(today);
        self.set_date(date);
    }

    /// Rebuild `view` from the DB if dirty. Errors are captured into `error` and shown.
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
        let date = date_sql(self.selected_date);
        let categories = db.list_categories()?;
        let all_types = db.list_task_types(true)?; // active only
        let day_type_totals = db.day_type_totals(&date)?;

        let mut day_counts = std::collections::BTreeMap::new();
        let mut category_day_counts = std::collections::BTreeMap::new();
        for DayTypeTotal { task_type_id, category_id, count, .. } in &day_type_totals {
            day_counts.insert(*task_type_id, *count);
            *category_day_counts.entry(*category_id).or_insert(0) += *count;
        }

        let types = all_types
            .iter()
            .filter(|t| match self.filter {
                CategoryFilter::All => true,
                CategoryFilter::One(id) => t.category_id == id,
            })
            .cloned()
            .collect();

        Ok(View {
            categories,
            types,
            all_types,
            day_counts,
            day_total: db.day_total(&date)?,
            distinct_types: db.day_distinct_types(&date)?,
            category_day_counts,
            log: db.day_log_rows(&date)?,
        })
    }
}

impl Default for TodayState {
    fn default() -> Self {
        Self::new()
    }
}

/// `YYYY-MM-DD` for the DB.
fn date_sql(d: NaiveDate) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day())
}

/// `TUESDAY 25 AUGUST 2026` — built manually (chrono's `%-d` isn't portable on Windows).
fn date_headline(d: NaiveDate) -> String {
    format!(
        "{} {} {} {}",
        d.format("%A"),
        d.day(),
        d.format("%B"),
        d.year()
    )
    .to_uppercase()
}

/// `17 Sep 2026` — the compact form used in the pinned-date banner (the full uppercase
/// headline reads as shouting for a one-line status message).
fn short_date(d: NaiveDate) -> String {
    format!("{} {} {}", d.day(), d.format("%b"), d.year())
}

/// `17 Sep` — same, without the year, for the renewal banner's "Continue with …" button
/// (the year is redundant right next to "Still logging to <date with year>?").
fn short_date_no_year(d: NaiveDate) -> String {
    format!("{} {}", d.day(), d.format("%b"))
}

/// What today's per-frame date snap and this frame's write gestures should do, given the
/// follow/pin state and the real calendar date. Pure and DB/window-free so the stale-date
/// rules (PLAN: date-handling redesign) are unit-testable directly — see the tests module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DatePlan {
    /// Following today, but `selected_date` has drifted from it (day rolled over, or a
    /// backward clock correction moved `today` under a stale `selected_date`) — snap before
    /// rendering, unconditionally, not just on a write.
    Snap,
    /// Pinned to a date whose pin was made on an earlier calendar day: the pin's intent is
    /// stale. A write must not go through silently; ask the user to renew it first.
    Ask,
    /// Safe to write to `selected_date` as-is.
    AllowWrite,
    /// `selected_date` itself is in the future (only reachable via a backward clock change
    /// while pinned — `Next ›` already refuses to pin past today).
    RefuseFuture,
}

fn plan_for(
    following_today: bool,
    selected_date: NaiveDate,
    pinned_on: Option<NaiveDate>,
    today: NaiveDate,
) -> DatePlan {
    if following_today {
        // `!=`, never `<`/`>`: a clock correction or timezone change can move `today`
        // backwards, and the snap must still fire rather than get stuck comparing the wrong
        // way.
        return if selected_date != today { DatePlan::Snap } else { DatePlan::AllowWrite };
    }
    if refuse_future(selected_date, today).is_some() {
        return DatePlan::RefuseFuture;
    }
    if pinned_on != Some(today) {
        return DatePlan::Ask;
    }
    DatePlan::AllowWrite
}

/// The shared future-date guard (date-handling redesign, item 4): every interactive write
/// path in this file routes through this before it reaches the DB — the tile tally and the
/// number keys and the right-click remove via `plan_for`'s `RefuseFuture` case, and the edit
/// dialog's free-text date field directly, since that date isn't `selected_date` at all.
///
/// Deliberately not in `simpletally_core`: core stays time-agnostic on purpose, because a
/// v3.8 migration import or a backup restore may legitimately carry any date, and rejecting
/// future dates in core would make those paths fail. "No future dates" is a rule about
/// interactive entry, not about stored data, so it belongs at this UI submission boundary.
fn refuse_future(date: NaiveDate, today: NaiveDate) -> Option<String> {
    if date > today {
        Some(format!("Can't log to {} — that hasn't happened yet.", short_date(date)))
    } else {
        None
    }
}

/// Render the Today screen and apply any interaction to the DB. `hotkey_error`, if present,
/// is surfaced as a banner (PLAN §1.2: registration failure must be visible).
pub fn show(
    ui: &mut egui::Ui,
    state: &mut TodayState,
    db: &Db,
    theme: &Theme,
    hotkey_error: Option<&str>,
) {
    use crate::ui::theme as t;
    let today = Local::now().date_naive();
    // The window can be left open across midnight, hidden to tray overnight and restored, or
    // resumed from sleep — none of those fire a visibility hook, so this has to be a plain
    // per-frame check, not something triggered on window-show. `set_date` no-ops when the
    // date hasn't actually moved, so this costs nothing once you're already caught up.
    if state.following_today {
        state.advance_to(today);
    }
    state.ensure_view(db);

    if let Some(err) = &state.error {
        egui::Frame::default()
            .fill(theme.bg_canvas)
            .inner_margin(pad(34, 26))
            .show(ui, |ui| {
                ui.colored_label(theme.negative, format!("Database error: {err}"));
            });
        return;
    }

    // Snapshot everything we render from, so `state` is free to be mutated by interactions.
    let snap = {
        let v = state.view.as_ref().expect("view built above");
        Snapshot {
            headline: date_headline(state.selected_date),
            day_total: v.day_total,
            distinct_types: v.distinct_types,
            categories: v.categories.clone(),
            category_counts: v.category_day_counts.clone(),
            tiles: v
                .types
                .iter()
                .map(|ty| {
                    let cat = v
                        .categories
                        .iter()
                        .find(|c| c.id == ty.category_id)
                        .map(|c| c.name.clone())
                        .unwrap_or_default();
                    TileData {
                        type_id: ty.id,
                        name: ty.name.clone(),
                        category: cat,
                        description: ty.description.clone(),
                        count: v.day_counts.get(&ty.id).copied().unwrap_or(0),
                    }
                })
                .collect(),
            log: v.log.clone(),
            type_choices: v
                .all_types
                .iter()
                .map(|ty| {
                    let cat = v
                        .categories
                        .iter()
                        .find(|c| c.id == ty.category_id)
                        .map(|c| c.name.clone())
                        .unwrap_or_default();
                    (ty.id, cat, ty.name.clone())
                })
                .collect(),
        }
    };

    let is_today = state.selected_date >= today;
    let mut action: Option<Action> = None;
    let mut open_edit: Option<DayLogRow> = None;

    // The log band's frame + content (rendered into a child Ui at an explicit rect below).
    let log_frame = egui::Frame::default().fill(theme.bg_sunken).inner_margin(pad(34, 18));
    let collapsed = state.log_collapsed;
    let mut toggle_log = false;
    let show_log = |ui: &mut egui::Ui| {
        // Labels are selectable by default in egui: that hands the row an I-beam cursor and
        // swallows the click meant for the edit dialog. The whole band is click-to-edit.
        ui.style_mut().interaction.selectable_labels = false;
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("TODAY'S LOG")
                    .font(t::mono(t::EYEBROW))
                    .color(theme.text_tertiary),
            );
            let label = if collapsed {
                format!("show all ({})", snap.log.len())
            } else {
                "collapse".to_string()
            };
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let resp = ui.add(
                    egui::Button::new(
                        egui::RichText::new(label).font(t::sans(t::CAPTION)).color(theme.accent),
                    )
                    .frame(false),
                );
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if resp.clicked() {
                    toggle_log = true;
                }
            });
        });
        ui.add_space(4.0);
        if snap.log.is_empty() {
            ui.label(
                egui::RichText::new("no entries yet — click a row to edit")
                    .font(t::sans(t::CAPTION))
                    .color(theme.text_quiet),
            );
        }
        // Collapsed shows only the newest entry (the query orders newest first).
        let visible = if collapsed { &snap.log[..snap.log.len().min(1)] } else { &snap.log[..] };
        for row in visible {
            // The whole row is clickable → opens the edit dialog (design: "click a row").
            // Reserve a shape slot first so the hover fill can be painted *under* the row.
            let bg = ui.painter().add(egui::Shape::Noop);
            let resp = ui
                .horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(row.created_at.get(11..16).unwrap_or(""))
                            .font(t::mono(t::TIME))
                            .color(theme.text_quiet),
                    );
                    ui.label(
                        egui::RichText::new(format!("{} · {}", row.category_name, row.type_name))
                            .font(t::sans(t::BODY))
                            .color(theme.text_body),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !row.notes.is_empty() {
                            ui.label(egui::RichText::new(&row.notes).font(t::sans(t::CAPTION)).color(theme.text_quiet));
                        }
                        ui.label(
                            egui::RichText::new(format!("+{}", row.count))
                                .font(t::mono(t::BODY))
                                .color(theme.accent),
                        );
                    });
                })
                .response
                .interact(egui::Sense::click());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                // Highlight across the full band width, so it reads as one row. Uses the
                // accent tint (bg_chrome sits 3 steps off bg_sunken — effectively invisible).
                let band = ui.max_rect();
                let hl = egui::Rect::from_x_y_ranges(band.x_range(), resp.rect.y_range())
                    .expand2(egui::vec2(10.0, 3.0));
                ui.painter().set(
                    bg,
                    egui::epaint::RectShape::new(
                        hl,
                        egui::CornerRadius::same(6),
                        theme.accent_tint_bg,
                        egui::Stroke::new(1.0, theme.accent_tint_border),
                        egui::StrokeKind::Inside,
                    ),
                );
            }
            if resp.clicked() {
                open_edit = Some(row.clone());
            }
        }
    };
    // Manual band layout: nested egui panels don't reliably shrink the parent Ui inside
    // egui_glow's CentralPanel (the tile grid overran the log band), so the three regions are
    // placed at explicit rects. Header at the top, log band pinned at the bottom, grid bounded
    // in between. Deterministic band height: heading + up to 5 rows (the query's cap) + margins.
    let full = ui.max_rect();
    let rows = if collapsed { 1.0 } else { snap.log.len().max(1) as f32 };
    let log_height = 36.0 + 22.0 + 6.0 + rows * 24.0;
    let log_top = full.bottom() - log_height;

    // --- top: header + category pills ---
    let mut header_ui = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("today_header")
            .max_rect(egui::Rect::from_min_max(full.min, egui::pos2(full.right(), log_top)))
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    let header_bottom = egui::Frame::default()
        .fill(theme.bg_canvas)
        .inner_margin(pad(34, 20))
        .show(&mut header_ui, |ui| {
            if let Some(err) = hotkey_error {
                ui.label(egui::RichText::new(format!("⚠ Hotkey unavailable: {err}")).color(theme.negative).font(t::sans(t::CAPTION)));
            }
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(&snap.headline)
                            .font(t::mono_medium(t::DAY_NAV))
                            .color(if is_today { theme.accent } else { theme.pinned_date }),
                    );
                    ui.label(
                        egui::RichText::new(snap.day_total.to_string())
                            .font(t::mono_medium(t::DAY_TOTAL))
                            .color(theme.text_primary),
                    );
                    let caption = if snap.day_total == 0 {
                        "nothing logged yet — click a tile".to_string()
                    } else {
                        format!("{} tallies across {} types", snap.day_total, snap.distinct_types)
                    };
                    ui.label(egui::RichText::new(caption).font(t::sans(t::BODY)).color(theme.text_secondary));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                    if pill::nav_pill(ui, theme, "Next ›", !is_today).clicked() {
                        let d = state.selected_date.succ_opt().unwrap_or(state.selected_date);
                        state.pin_to(d, today);
                    }
                    ui.add_space(6.0);
                    if pill::nav_pill(ui, theme, "Today", true).clicked() {
                        state.resume_following(today);
                    }
                    ui.add_space(6.0);
                    if pill::nav_pill(ui, theme, "‹ Prev", true).clicked() {
                        let d = state.selected_date.pred_opt().unwrap_or(state.selected_date);
                        state.pin_to(d, today);
                    }
                });
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if pill::category_pill(ui, theme, "All", snap.day_total, state.filter == CategoryFilter::All).clicked() {
                        state.filter = CategoryFilter::All;
                        state.mark_dirty();
                    }
                    for c in &snap.categories {
                        let selected = state.filter == CategoryFilter::One(c.id);
                        let count = snap.category_counts.get(&c.id).copied().unwrap_or(0);
                        if pill::category_pill(ui, theme, &c.name, count, selected).clicked() {
                            state.filter = CategoryFilter::One(c.id);
                            state.mark_dirty();
                        }
                    }
                });
                // Right-aligned caption: "N types in <category>" when a category is selected.
                if let CategoryFilter::One(id) = state.filter {
                    if let Some(cat) = snap.categories.iter().find(|c| c.id == id) {
                        let n = snap.tiles.len();
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(format!("{n} types in {}", cat.name))
                                    .font(t::sans(t::CAPTION))
                                    .color(theme.text_tertiary),
                            );
                        });
                    }
                }
            });
        })
        .response
        .rect
        .bottom();

    // --- center: note row + scrollable tile grid, bounded between the header and the log ---
    let central_rect = egui::Rect::from_min_max(
        egui::pos2(full.left(), header_bottom.min(log_top)),
        egui::pos2(full.right(), log_top),
    );
    let mut central_ui = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("today_central")
            .max_rect(central_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    egui::Frame::default()
        .fill(theme.bg_canvas)
        .inner_margin(pad(34, 12))
        .show(&mut central_ui, |ui| {
            // Persistent banner while pinned (date-handling redesign, item 2/3): context, not
            // an error — the date header alone isn't enough since tallying is a one-click
            // habit that doesn't stop to read it.
            if !state.following_today {
                ui.horizontal(|ui| {
                    if state.asking {
                        ui.label(
                            egui::RichText::new(format!(
                                "Still logging to {}?",
                                short_date(state.selected_date)
                            ))
                            .font(t::sans(t::BODY))
                            .color(theme.text_secondary),
                        );
                        ui.add_space(10.0);
                        if link(ui, "Use today", theme.accent, t::sans(t::CAPTION)).clicked() {
                            state.resume_following(today);
                        }
                        ui.add_space(10.0);
                        if link(
                            ui,
                            &format!("Continue with {}", short_date_no_year(state.selected_date)),
                            theme.accent,
                            t::sans(t::CAPTION),
                        )
                        .clicked()
                        {
                            state.pinned_on = Some(today);
                            state.asking = false;
                        }
                    } else if let Some(msg) = refuse_future(state.selected_date, today) {
                        // Self-heal: a backward clock change while pinned can leave
                        // `selected_date` in the future. Never silently redirect — say so
                        // and let `Return to today` fix it.
                        ui.label(
                            egui::RichText::new(msg).font(t::sans(t::BODY)).color(theme.text_secondary),
                        );
                        ui.add_space(10.0);
                        if link(ui, "Return to today", theme.accent, t::sans(t::CAPTION)).clicked() {
                            state.resume_following(today);
                        }
                    } else {
                        ui.label(
                            egui::RichText::new(format!("Logging to {}", short_date(state.selected_date)))
                                .font(t::sans(t::BODY))
                                .color(theme.text_secondary),
                        );
                        ui.add_space(10.0);
                        if link(ui, "Return to today", theme.accent, t::sans(t::CAPTION)).clicked() {
                            state.resume_following(today);
                        }
                    }
                });
                ui.add_space(8.0);
            }

            ui.horizontal(|ui| {
                let toggle = if state.note_open { "− hide note field" } else { "+ add a note" };
                if ui.add(egui::Button::new(egui::RichText::new(toggle).font(t::sans(t::BODY)).color(theme.accent)).frame(false)).clicked() {
                    state.note_open = !state.note_open;
                }
                if state.note_open {
                    ui.add(
                        egui::TextEdit::singleline(&mut state.pending_note)
                            .hint_text("attached to the next tally — optional")
                            .desired_width(360.0)
                            .font(egui::FontSelection::FontId(t::sans(t::BODY))),
                    );
                }
                if let Some(msg) = &state.last_action {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(msg).font(t::sans(t::CAPTION)).color(theme.text_secondary));
                    });
                }
            });
            ui.add_space(10.0);

            // Responsive 4-column grid (design: repeat(4, 1fr)).
            const COLS: usize = 4;
            const GAP: f32 = 10.0;
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .min_scrolled_height(0.0) // Allow the middle to shrink below egui's 64-point minimum.
                .show(ui, |ui| {
                    // Reserve the scrollbar's width so the 4th column isn't clipped by it when
                    // the grid overflows (available_width doesn't always exclude it here).
                    let sb = ui.spacing().scroll.bar_width
                        + ui.spacing().scroll.bar_inner_margin
                        + ui.spacing().scroll.bar_outer_margin;
                    let tile_w =
                        ((ui.available_width() - sb - GAP * (COLS as f32 - 1.0)) / COLS as f32)
                            .max(150.0);
                    egui::Grid::new("tile_grid")
                        .num_columns(COLS)
                        .spacing([GAP, GAP])
                        .show(ui, |ui| {
                            for (i, tile) in snap.tiles.iter().enumerate() {
                                if let Some(a) = tile_button(ui, theme, tile, tile_w) {
                                    action = Some(a);
                                }
                                if (i + 1) % COLS == 0 {
                                    ui.end_row();
                                }
                            }
                        });
                });
        });

    // --- bottom: today's log band, pinned at the bottom ---
    let mut log_ui = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("today_logband")
            .max_rect(egui::Rect::from_min_max(egui::pos2(full.left(), log_top), full.max))
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    log_frame.show(&mut log_ui, show_log);

    if toggle_log {
        state.log_collapsed = !state.log_collapsed;
    }

    // Open the edit dialog for a clicked log row.
    if let Some(row) = open_edit {
        state.editing = Some(EditForm {
            entry_id: row.entry_id,
            task_type_id: row.task_type_id,
            date: date_sql(state.selected_date),
            count: row.count,
            notes: row.notes,
            logged_at: row.created_at.get(11..16).unwrap_or("").to_string(),
            picking_type: false,
            confirm_delete: false,
            error: None,
        });
    }
    edit_dialog(ui, state, db, theme, &snap.type_choices);

    // Number keys 1–9 tally the first nine tiles of the current category (README §Keyboard),
    // unless a dialog is open or a text field has focus (the note field eats its own digits).
    if action.is_none() && state.editing.is_none() && !ui.ctx().memory(|m| m.focused().is_some()) {
        let pressed = ui
            .ctx()
            .input(|i| NUM_KEYS.iter().position(|k| i.key_pressed(*k)));
        if let Some(tile) = pressed.and_then(|n| snap.tiles.get(n)) {
            action = Some(Action::Add(tile.type_id, tile.name.clone()));
        }
    }

    if let Some(a) = action {
        // Shared write guard (date-handling redesign, items 3/4): route every tally/remove
        // gesture through the same follow/pin/future decision the per-frame snap above used,
        // instead of writing straight to `selected_date`.
        match plan_for(state.following_today, state.selected_date, state.pinned_on, today) {
            DatePlan::AllowWrite => apply(state, db, a),
            DatePlan::Ask => {
                // Consume the gesture without writing; the banner turns into the renewal
                // question above. Repeated gestures while it's showing stay consumed too —
                // asked once per day change, not once per tally.
                state.asking = true;
            }
            DatePlan::RefuseFuture => {
                if let Some(msg) = refuse_future(state.selected_date, today) {
                    state.last_action = Some(msg);
                }
            }
            // Following-today was already snapped to `today` above this frame, so this can't
            // actually be reached here — but stay safe rather than write to a stale date.
            DatePlan::Snap => {}
        }
    }
}

/// Keys 1–9, in tile order.
const NUM_KEYS: [egui::Key; 9] = [
    egui::Key::Num1,
    egui::Key::Num2,
    egui::Key::Num3,
    egui::Key::Num4,
    egui::Key::Num5,
    egui::Key::Num6,
    egui::Key::Num7,
    egui::Key::Num8,
    egui::Key::Num9,
];

/// The edit-entry modal (design 07 / `notes/UI_SPEC.md`), opened by clicking a day-log row.
/// The task type shows as a tinted badge and only becomes a picker on "Change"; count gets the
/// 44px stepper treatment (`↑↓` adjusts it); Delete sits apart from Save with its own
/// confirmation modal. `Enter` saves, `Esc` cancels. Core validation errors are shown inline
/// and keep the dialog open.
fn edit_dialog(
    ui: &mut egui::Ui,
    state: &mut TodayState,
    db: &Db,
    theme: &Theme,
    type_choices: &[(i64, String, String)],
) {
    use crate::ui::theme as t;
    use egui::RichText;
    if state.editing.is_none() {
        return;
    }
    let mut want_save = false;
    let mut want_delete = false;
    let mut want_cancel = false;
    let today_date = Local::now().date_naive();
    let today = date_sql(today_date);

    // Scope the mutable borrow of the form to the modal render; the DB calls below re-borrow.
    {
        let form = state.editing.as_mut().expect("checked Some above");
        let (cat, name) = type_choices
            .iter()
            .find(|(id, _, _)| *id == form.task_type_id)
            .map(|(_, c, n)| (c.clone(), n.clone()))
            .unwrap_or_default();

        let modal = egui::Modal::new(egui::Id::new("edit_entry")).show(ui.ctx(), |ui| {
            ui.set_width(400.0);
            ui.spacing_mut().item_spacing.y = 6.0;

            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Edit entry")
                        .font(t::sans_medium(t::SECTION_TITLE))
                        .color(theme.text_primary),
                );
                if !form.logged_at.is_empty() {
                    ui.label(
                        RichText::new(format!("logged {}", form.logged_at))
                            .font(t::mono(t::EYEBROW))
                            .color(theme.text_quiet),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if crate::ui::widgets::close::close_button(ui, theme).clicked() {
                        want_cancel = true;
                    }
                });
            });
            ui.separator();

            // --- task type: tinted badge, picker only on "Change" ---
            eyebrow(ui, theme, "TASK TYPE");
            let inner_w = ui.available_width();
            egui::Frame::default()
                .fill(theme.accent_tint_bg)
                .stroke(egui::Stroke::new(1.0, theme.accent_tint_border))
                .corner_radius(8)
                .inner_margin(pad(14, 10))
                .show(ui, |ui| {
                    ui.set_width(inner_w - 30.0);
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new(cat.to_uppercase())
                                    .font(t::mono(t::TILE_CATEGORY))
                                    .color(theme.accent_eyebrow),
                            );
                            ui.label(
                                RichText::new(&name)
                                    .font(t::sans_medium(t::TILE_NAME))
                                    .color(theme.text_primary),
                            );
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let label = if form.picking_type { "Done" } else { "Change" };
                            if link(ui, label, theme.accent, t::sans(t::CAPTION)).clicked() {
                                form.picking_type = !form.picking_type;
                            }
                        });
                    });
                });
            if form.picking_type {
                egui::ComboBox::from_id_salt("edit_type")
                    .selected_text(format!("{cat} · {name}"))
                    .width(inner_w - 30.0)
                    .show_ui(ui, |ui| {
                        for (id, c, n) in type_choices {
                            ui.selectable_value(&mut form.task_type_id, *id, format!("{c} · {n}"));
                        }
                    });
            }
            ui.add_space(6.0);

            // --- count (hero steppers) + date ---
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    eyebrow(ui, theme, "COUNT");
                    count_stepper(ui, theme, &mut form.count);
                });
                ui.add_space(14.0);
                ui.vertical(|ui| {
                    eyebrow(ui, theme, "DATE");
                    egui::Frame::default()
                        .fill(theme.bg_raised)
                        .stroke(egui::Stroke::new(1.0, theme.border_strong))
                        .corner_radius(8)
                        .inner_margin(pad(12, 0))
                        .show(ui, |ui| {
                            ui.set_height(44.0);
                            ui.horizontal_centered(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut form.date)
                                        .desired_width(110.0)
                                        .frame(egui::Frame::NONE)
                                        .font(egui::FontSelection::FontId(t::mono(t::BODY))),
                                );
                                if form.date != today
                                    && link(ui, "Today", theme.accent, t::sans(t::CAPTION)).clicked()
                                {
                                    form.date = today.clone();
                                }
                            });
                        });
                });
            });
            ui.add_space(6.0);

            // --- note ---
            eyebrow(ui, theme, "NOTE");
            ui.add(
                egui::TextEdit::multiline(&mut form.notes)
                    .hint_text("optional — what this entry was about")
                    .desired_rows(2)
                    .desired_width(inner_w)
                    .font(egui::FontSelection::FontId(t::sans(t::BODY))),
            );
            if let Some(err) = &form.error {
                ui.colored_label(theme.negative, err);
            }
            ui.add_space(8.0);
            ui.separator();

            // --- footer: Delete apart on the left; Cancel + Save right ---
            ui.horizontal(|ui| {
                if link(ui, "Delete entry", theme.negative, t::sans(t::BODY)).clicked() {
                    form.confirm_delete = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Save")
                                    .font(t::sans_medium(t::BODY))
                                    .color(theme.bg_raised),
                            )
                            .fill(theme.accent)
                            .corner_radius(6)
                            .min_size(egui::vec2(74.0, 32.0)),
                        )
                        .clicked()
                    {
                        want_save = true;
                    }
                    ui.add_space(8.0);
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Cancel")
                                    .font(t::sans(t::BODY))
                                    .color(theme.text_body),
                            )
                            .fill(theme.bg_raised)
                            .stroke(egui::Stroke::new(1.0, theme.border_strong))
                            .corner_radius(6)
                            .min_size(egui::vec2(74.0, 32.0)),
                        )
                        .clicked()
                    {
                        want_cancel = true;
                    }
                });
            });
        });

        // --- delete confirmation, its own modal on top (never shares an edge with Save) ---
        let confirm = if form.confirm_delete {
            Some(
                egui::Modal::new(egui::Id::new("confirm_delete_entry")).show(ui.ctx(), |ui| {
                    ui.set_width(340.0);
                    ui.label(
                        RichText::new("Delete this entry?")
                            .font(t::sans_medium(t::SECTION_TITLE))
                            .color(theme.text_primary),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(format!(
                            "{} tall{} of {} · {} on {}. This cannot be undone.",
                            form.count,
                            if form.count == 1 { "y" } else { "ies" },
                            cat.to_uppercase(),
                            name,
                            form.date
                        ))
                        .font(t::sans(t::BODY))
                        .color(theme.text_secondary),
                    );
                    ui.add_space(14.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Delete")
                                        .font(t::sans_medium(t::BODY))
                                        .color(theme.bg_raised),
                                )
                                .fill(theme.negative)
                                .corner_radius(6)
                                .min_size(egui::vec2(74.0, 32.0)),
                            )
                            .clicked()
                        {
                            want_delete = true;
                        }
                        ui.add_space(8.0);
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Keep it")
                                        .font(t::sans(t::BODY))
                                        .color(theme.text_body),
                                )
                                .fill(theme.bg_raised)
                                .stroke(egui::Stroke::new(1.0, theme.border_strong))
                                .corner_radius(6)
                                .min_size(egui::vec2(74.0, 32.0)),
                            )
                            .clicked()
                        {
                            form.confirm_delete = false;
                        }
                    });
                }),
            )
        } else {
            None
        };

        // Keyboard: Esc closes the confirmation first, then the dialog; ↑↓ adjusts the count;
        // Enter saves (never deletes).
        if let Some(confirm) = confirm {
            if confirm.should_close() {
                form.confirm_delete = false;
            }
        } else {
            if modal.should_close() {
                want_cancel = true; // click-outside or Esc
            }
            let (up, down, enter) = ui.ctx().input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::Enter),
                )
            });
            if up {
                form.count += 1;
            }
            if down {
                form.count = (form.count - 1).max(1);
            }
            if enter {
                want_save = true;
            }
        }
    } // form borrow ends

    if want_cancel {
        state.editing = None;
        return;
    }
    if !(want_save || want_delete) {
        return;
    }
    let (id, ty, date, count, notes) = {
        let f = state.editing.as_ref().expect("editing is Some");
        (f.entry_id, f.task_type_id, f.date.clone(), f.count, f.notes.clone())
    };
    // Shared future-date guard (date-handling redesign, item 4): this free-text field is the
    // one write path that isn't `selected_date` at all, so it goes through `refuse_future`
    // directly rather than via `plan_for`. Only Save writes a date; Delete removes whatever
    // is already there regardless of when it happened.
    if want_save {
        if let Some(parsed) = simpletally_core::dates::parse_sql(&date) {
            if let Some(msg) = refuse_future(parsed, today_date) {
                if let Some(f) = state.editing.as_mut() {
                    f.error = Some(msg);
                }
                return;
            }
        }
        // An unparseable date falls through to `db.edit_entry`, whose own validation error
        // surfaces below — same as any other invalid input in this field.
    }
    let result = if want_delete {
        db.delete_entry(id)
    } else {
        db.edit_entry(id, ty, &date, count, &notes)
    };
    match result {
        Ok(()) => {
            state.editing = None;
            state.mark_written();
        }
        Err(e) => {
            if let Some(f) = state.editing.as_mut() {
                f.confirm_delete = false;
                f.error = Some(e.to_string());
            }
        }
    }
}

/// A small uppercase field label (mono eyebrow).
fn eyebrow(ui: &mut egui::Ui, theme: &Theme, text: &str) {
    use crate::ui::theme as t;
    ui.label(
        egui::RichText::new(text)
            .font(t::mono(t::EYEBROW))
            .color(theme.text_tertiary),
    );
}

/// A frameless text link in `color`.
fn link(ui: &mut egui::Ui, text: &str, color: egui::Color32, font: egui::FontId) -> egui::Response {
    let resp =
        ui.add(egui::Button::new(egui::RichText::new(text).font(font).color(color)).frame(false));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// The 44px `[−] n [+]` count stepper (design 07: count is the field most often wrong, so it
/// gets the hero treatment). Clamped at 1 — core's `CHECK (count > 0)`.
fn count_stepper(ui: &mut egui::Ui, theme: &Theme, count: &mut i64) {
    use crate::ui::theme as t;
    const H: f32 = 44.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(H * 3.0, H), egui::Sense::hover());
    ui.painter().rect(
        rect,
        egui::CornerRadius::same(8),
        theme.bg_raised,
        egui::Stroke::new(1.0, theme.border_strong),
        egui::StrokeKind::Inside,
    );
    let minus = egui::Rect::from_min_size(rect.min, egui::vec2(H, H));
    let plus = egui::Rect::from_min_size(egui::pos2(rect.right() - H, rect.top()), egui::vec2(H, H));
    for x in [minus.right(), plus.left()] {
        ui.painter().line_segment(
            [egui::pos2(x, rect.top() + 6.0), egui::pos2(x, rect.bottom() - 6.0)],
            egui::Stroke::new(1.0, theme.border_subtle),
        );
    }
    let m = ui.interact(minus, ui.id().with("count_minus"), egui::Sense::click());
    let p = ui.interact(plus, ui.id().with("count_plus"), egui::Sense::click());
    if m.clicked() {
        *count = (*count - 1).max(1);
    }
    if p.clicked() {
        *count += 1;
    }
    for (r, resp, glyph, enabled) in [(minus, &m, "−", *count > 1), (plus, &p, "+", true)] {
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let col = if !enabled {
            theme.text_disabled
        } else if resp.hovered() {
            theme.accent
        } else {
            theme.text_secondary
        };
        ui.painter().text(
            r.center(),
            egui::Align2::CENTER_CENTER,
            glyph,
            t::sans(t::SECTION_TITLE + 4.0),
            col,
        );
    }
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        count.to_string(),
        t::mono_medium(t::QUICK_ADD),
        theme.text_primary,
    );
}

/// Frozen view data for one frame's render.
struct Snapshot {
    headline: String,
    day_total: i64,
    distinct_types: i64,
    categories: Vec<Category>,
    category_counts: std::collections::BTreeMap<i64, i64>,
    tiles: Vec<TileData>,
    log: Vec<DayLogRow>,
    /// (task_type_id, category name, type name) for the edit dialog's badge + picker.
    type_choices: Vec<(i64, String, String)>,
}

struct TileData {
    type_id: i64,
    name: String,
    category: String,
    description: String,
    count: i64,
}

fn pad(x: i8, y: i8) -> egui::Margin {
    egui::Margin::symmetric(x, y)
}

/// A single tally tile (plain, themed; the designed hover/press tile widget lands next).
/// Left-click → +1, right-click → remove most recent.
fn tile_button(ui: &mut egui::Ui, theme: &Theme, tile: &TileData, width: f32) -> Option<Action> {
    use crate::ui::theme as t;
    let active = tile.count > 0;
    let (fill, border, name_col, num_col, cat_col) = if active {
        (theme.tile_active_bg, theme.tile_active_border, theme.tile_active_name, theme.tile_active_number, theme.tile_active_category)
    } else {
        (theme.tile_empty_bg, theme.tile_empty_border, theme.tile_empty_name, theme.tile_empty_number, theme.tile_empty_category)
    };

    // Tall enough for a 2-line name above the 28px count without overlap (design min-height
    // is 104; long Indonesian type names routinely wrap to two lines here).
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 116.0), egui::Sense::click());

    // Hover highlights the border (accent). No translate/scale "bump": lifting the top row's
    // card above the scroll viewport clipped its highlighted top edge.
    let stroke_col = if resp.hovered() { theme.accent } else { border };
    ui.painter().rect(
        rect,
        egui::CornerRadius::same(8),
        fill,
        egui::Stroke::new(1.0, stroke_col),
        egui::StrokeKind::Inside,
    );

    let inner = rect.shrink2(egui::vec2(16.0, 14.0));
    let p = ui.painter();
    p.text(
        inner.left_top(),
        egui::Align2::LEFT_TOP,
        tile.category.to_uppercase(),
        t::mono(t::TILE_CATEGORY),
        cat_col,
    );
    // Name: wrap to the tile width, capped at 2 rows with an ellipsis so it never grows into
    // the count. `elided` tells us whether it was actually truncated (→ tooltip).
    let name_galley = {
        let mut job = egui::text::LayoutJob::single_section(
            tile.name.clone(),
            egui::TextFormat {
                font_id: t::sans_medium(t::TILE_NAME),
                color: name_col,
                ..Default::default()
            },
        );
        job.wrap.max_width = inner.width();
        job.wrap.max_rows = 2;
        job.wrap.overflow_character = Some('…');
        p.layout_job(job)
    };
    let name_truncated = name_galley.elided;
    p.galley(inner.left_top() + egui::vec2(0.0, 15.0), name_galley, name_col);
    p.text(
        inner.left_bottom(),
        egui::Align2::LEFT_BOTTOM,
        tile.count.to_string(),
        t::mono_medium(t::TILE_COUNT),
        num_col,
    );
    if active {
        p.text(
            inner.right_bottom(),
            egui::Align2::RIGHT_BOTTOM,
            "today",
            t::mono(t::TODAY_LABEL),
            theme.text_quiet,
        );
    }
    // Tooltip only when the name is truncated (shows the full name, plus the description if any).
    let resp = if name_truncated {
        let tip = if tile.description.is_empty() {
            tile.name.clone()
        } else {
            format!("{}\n\n{}", tile.name, tile.description)
        };
        resp.on_hover_text(tip)
    } else {
        resp
    };

    if resp.clicked() {
        Some(Action::Add(tile.type_id, tile.name.clone()))
    } else if resp.secondary_clicked() {
        Some(Action::Remove(tile.type_id, tile.name.clone()))
    } else {
        None
    }
}

enum Action {
    Add(i64, String),
    Remove(i64, String),
}

fn apply(state: &mut TodayState, db: &Db, action: Action) {
    let date = date_sql(state.selected_date);
    match action {
        Action::Add(type_id, name) => {
            let note = state.pending_note.clone();
            match db.add_tally(type_id, &date, 1, &note) {
                Ok(_) => {
                    // pending_note clears only after the write commits (PLAN §5).
                    state.pending_note.clear();
                    state.last_action = Some(format!("logged {name}"));
                    state.mark_written();
                }
                Err(e) => state.error = Some(e.to_string()),
            }
        }
        Action::Remove(type_id, name) => match db.remove_most_recent(type_id, &date) {
            Ok(RemoveOutcome::NoOp) => {}
            Ok(_) => {
                state.last_action = Some(format!("removed {name}"));
                state.mark_written();
            }
            Err(e) => state.error = Some(e.to_string()),
        },
    }
}

#[cfg(test)]
mod date_handling_tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn following_day_rollover_snaps() {
        let today = d(2026, 9, 18);
        let yesterday = d(2026, 9, 17);
        assert_eq!(plan_for(true, yesterday, None, today), DatePlan::Snap);
    }

    /// The automatic rollover must not eat a half-typed note; only a *user* date change does
    /// (PLAN §6). Losing text nobody can retype is worse than carrying it to the new day,
    /// where it stays visible and editable.
    #[test]
    fn the_automatic_advance_keeps_a_pending_note_but_set_date_clears_it() {
        let mut s = TodayState::new();
        s.selected_date = d(2026, 9, 17);
        s.pending_note = "half typed".to_string();

        s.advance_to(d(2026, 9, 18));
        assert_eq!(s.selected_date, d(2026, 9, 18));
        assert_eq!(s.pending_note, "half typed", "midnight must not discard the draft");

        s.set_date(d(2026, 9, 19));
        assert!(s.pending_note.is_empty(), "a user-driven move still discards it");
    }

    #[test]
    fn following_same_day_is_unchanged() {
        let today = d(2026, 9, 18);
        assert_eq!(plan_for(true, today, None, today), DatePlan::AllowWrite);
    }

    #[test]
    fn pinned_same_day_allows_write_without_asking() {
        let today = d(2026, 9, 18);
        let pinned_date = d(2026, 9, 10);
        assert_eq!(plan_for(false, pinned_date, Some(today), today), DatePlan::AllowWrite);
    }

    #[test]
    fn pinned_after_day_rollover_asks_and_refuses_write() {
        let pin_made_on = d(2026, 9, 17);
        let today = d(2026, 9, 18);
        let pinned_date = d(2026, 9, 10);
        assert_eq!(plan_for(false, pinned_date, Some(pin_made_on), today), DatePlan::Ask);
    }

    #[test]
    fn continue_with_pinned_date_stops_asking_for_the_rest_of_the_day() {
        // Simulates the "Continue with <date>" answer: `pinned_on` renews to today, so the
        // *next* gesture no longer asks.
        let today = d(2026, 9, 18);
        let pinned_date = d(2026, 9, 10);
        assert_eq!(plan_for(false, pinned_date, Some(today), today), DatePlan::AllowWrite);
    }

    #[test]
    fn use_today_resumes_following() {
        let today = d(2026, 9, 18);
        let mut state = TodayState::new();
        state.pin_to(d(2026, 9, 10), d(2026, 9, 17));
        state.asking = true;

        state.resume_following(today);

        assert!(state.following_today);
        assert_eq!(state.pinned_on, None);
        assert!(!state.asking);
        assert_eq!(state.selected_date, today);
    }

    #[test]
    fn future_date_is_refused_by_the_shared_guard() {
        let today = d(2026, 9, 18);
        let future = d(2026, 9, 19);
        // The guard the edit dialog's free-text Save calls directly (item 4's fourth site).
        assert!(refuse_future(future, today).is_some());
        // The same guard, reached through `plan_for`, for a pinned date that's gone future
        // (item 4's self-heal case).
        assert_eq!(plan_for(false, future, Some(today), today), DatePlan::RefuseFuture);
    }

    #[test]
    fn backward_clock_change_while_following_snaps_instead_of_getting_stuck() {
        let today = d(2026, 9, 18);
        let selected_ahead_of_today = d(2026, 9, 20); // clock ran ahead, then corrected back
        assert_eq!(plan_for(true, selected_ahead_of_today, None, today), DatePlan::Snap);
    }
}
