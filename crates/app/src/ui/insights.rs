//! The Insights screen (Phase 3A; design `_rustrefactor/README.md` §04, PLAN §5
//! "Insights, reweighted").
//!
//! Mirrors `today.rs`'s state shape: a `dirty` flag drives a `View` rebuild, a `Snapshot`
//! is frozen per frame so `state` stays free to mutate during interaction, and rendering is
//! painter calls + `crate::ui::theme` tokens. Deviations from the mock, per PLAN §5: a line
//! chart replaces bars once the range exceeds 31 days, and the category list shows every
//! type rather than the top 8.

use crate::ui::theme::{self as t, Theme};
use crate::ui::widgets::{chart, pill};
use chrono::{Datelike, NaiveDate};
use simpletally_core::dates::{self, DateRange};
use simpletally_core::{
    aggregate::{RangeSummary, TypeRangeTotal},
    csvio::{export_insights, InsightRow},
    Db,
};

/// Which range the toolbar's segmented control has selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeKind {
    Week,
    Month,
    Quarter,
    Year,
    Custom,
}

impl RangeKind {
    const ALL: [RangeKind; 5] =
        [RangeKind::Week, RangeKind::Month, RangeKind::Quarter, RangeKind::Year, RangeKind::Custom];

    fn label(self) -> &'static str {
        match self {
            RangeKind::Week => "Week",
            RangeKind::Month => "Month",
            RangeKind::Quarter => "Quarter",
            RangeKind::Year => "Year",
            RangeKind::Custom => "Custom",
        }
    }

    /// Lowercase noun for the delta sub-line ("vs previous <kind>").
    fn noun(self) -> &'static str {
        match self {
            RangeKind::Week => "week",
            RangeKind::Month => "month",
            RangeKind::Quarter => "quarter",
            RangeKind::Year => "year",
            RangeKind::Custom => "range",
        }
    }

    /// The string persisted in `Settings::range_kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            RangeKind::Week => "week",
            RangeKind::Month => "month",
            RangeKind::Quarter => "quarter",
            RangeKind::Year => "year",
            RangeKind::Custom => "custom",
        }
    }

    /// Inverse of [`RangeKind::as_str`]. An unrecognised value (a hand-edited or
    /// older-version settings file) falls back to `Week` rather than failing to load.
    pub fn from_str(s: &str) -> RangeKind {
        match s {
            "month" => RangeKind::Month,
            "quarter" => RangeKind::Quarter,
            "year" => RangeKind::Year,
            "custom" => RangeKind::Custom,
            _ => RangeKind::Week,
        }
    }
}

pub struct InsightsState {
    pub kind: RangeKind,
    pub anchor: NaiveDate,
    pub custom_from: String,
    pub custom_to: String,
    pub range_error: Option<String>,
    /// Status line beside "Export CSV" (Today's `last_action` pattern) — transient, not
    /// persisted.
    export_status: Option<String>,
    dirty: bool,
    view: Option<View>,
    error: Option<String>,
}

struct View {
    range: DateRange,
    summary: RangeSummary,
    daily: Vec<(NaiveDate, i64)>,
    ranked: Vec<TypeRangeTotal>,
    active_type_count: i64,
}

impl InsightsState {
    pub fn new(today: NaiveDate) -> Self {
        Self {
            kind: RangeKind::Week,
            anchor: today,
            custom_from: dates::to_sql(today),
            custom_to: dates::to_sql(today),
            range_error: None,
            export_status: None,
            dirty: true,
            view: None,
            error: None,
        }
    }

    /// `pub(crate)`: also called from `app.rs` after a migration import replaces the whole
    /// database underneath every screen.
    pub(crate) fn mark_dirty(&mut self) {
        self.dirty = true;
        self.export_status = None;
    }

    /// Reset for a **different database** (the migration import swaps the whole file out).
    /// Marking the view dirty is not enough: any open dialog still holds raw row ids from
    /// the old database, and ids are small sequential integers, so saving one against the
    /// newly imported data would silently overwrite an unrelated row.
    pub(crate) fn reset_for_new_database(&mut self) {
        self.range_error = None;
        self.error = None;
        self.mark_dirty();
    }

    /// Rebuild `view` from the DB if dirty. A range-resolution failure (bad Custom input)
    /// sets `range_error` and leaves the last-valid `view` in place — never blanks the
    /// screen. A DB error sets `error`, shown in place of the whole screen (matches
    /// `today.rs`).
    fn ensure_view(&mut self, db: &Db) {
        if !self.dirty && self.view.is_some() {
            return;
        }
        self.dirty = false;
        match resolve_range(self.kind, self.anchor, &self.custom_from, &self.custom_to) {
            Ok(range) => {
                self.range_error = None;
                match rebuild(db, range) {
                    Ok(v) => {
                        self.view = Some(v);
                        self.error = None;
                    }
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
            Err(msg) => self.range_error = Some(msg),
        }
    }
}

fn rebuild(db: &Db, range: DateRange) -> simpletally_core::Result<View> {
    Ok(View {
        range,
        summary: db.range_summary(range)?,
        daily: db.range_daily_totals(range)?,
        ranked: db.range_type_totals_ranked(range)?,
        active_type_count: db.list_task_types(true)?.len() as i64,
    })
}

// --- pure logic (unit-tested below; no egui) ----------------------------------------------

/// Resolves the toolbar's selection to a concrete range. `Custom` parses both fields and
/// rejects an inverted range; every other kind is total.
fn resolve_range(
    kind: RangeKind,
    anchor: NaiveDate,
    custom_from: &str,
    custom_to: &str,
) -> Result<DateRange, String> {
    match kind {
        RangeKind::Week => Ok(dates::week_of(anchor)),
        RangeKind::Month => Ok(dates::month_of(anchor)),
        RangeKind::Quarter => Ok(dates::quarter_of(anchor)),
        RangeKind::Year => Ok(dates::year_of(anchor)),
        RangeKind::Custom => {
            let from = dates::parse_sql(custom_from.trim())
                .ok_or_else(|| "from date must be YYYY-MM-DD".to_string())?;
            let to = dates::parse_sql(custom_to.trim())
                .ok_or_else(|| "to date must be YYYY-MM-DD".to_string())?;
            DateRange::new(from, to).ok_or_else(|| "from must not be after to".to_string())
        }
    }
}

/// Steps `anchor` by one unit of `kind` (Custom has no arrows — the caller never calls this
/// for it, but it's a harmless no-op). Month/Quarter/Year clamp the day into the target
/// month (e.g. 31 Jan → 28/29 Feb), matching calendar-UI convention rather than overflowing.
fn step_anchor(kind: RangeKind, anchor: NaiveDate, delta: i64) -> NaiveDate {
    match kind {
        RangeKind::Week => anchor + chrono::Duration::days(7 * delta),
        RangeKind::Month => add_months_clamped(anchor, delta),
        RangeKind::Quarter => add_months_clamped(anchor, delta * 3),
        RangeKind::Year => add_months_clamped(anchor, delta * 12),
        RangeKind::Custom => anchor,
    }
}

fn add_months_clamped(d: NaiveDate, months: i64) -> NaiveDate {
    let total = d.year() as i64 * 12 + (d.month() as i64 - 1) + months;
    let year = total.div_euclid(12) as i32;
    let month = (total.rem_euclid(12) + 1) as u32;
    let last_day = last_day_of_month(year, month);
    NaiveDate::from_ymd_opt(year, month, d.day().min(last_day))
        .expect("clamped day is always valid for its month")
}

fn last_day_of_month(year: i32, month: u32) -> u32 {
    let (ny, nm) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    NaiveDate::from_ymd_opt(ny, nm, 1)
        .expect("year/month rollover is always valid")
        .pred_opt()
        .expect("the day before a valid date always exists")
        .day()
}

/// Total ÷ active days, one decimal; `None` when there were no active days (design: "—").
fn per_active_day(total: i64, active_days: i64) -> Option<f64> {
    if active_days == 0 {
        None
    } else {
        Some((total as f64 / active_days as f64 * 10.0).round() / 10.0)
    }
}

/// The Total cell's sub-line: `+N vs previous <kind>` / `−N` / `same as previous`.
fn delta_line(total: i64, previous_total: i64, kind: RangeKind) -> (String, bool) {
    let diff = total - previous_total;
    if diff > 0 {
        (format!("+{diff} vs previous {}", kind.noun()), true)
    } else if diff < 0 {
        (format!("\u{2212}{} vs previous {}", -diff, kind.noun()), false)
    } else {
        (format!("same as previous {}", kind.noun()), false)
    }
}

/// `17 Aug` (short_date) — delegates to the chart widget's helper so the format is shared.
fn short_date(d: NaiveDate) -> String {
    chart::day_month(d)
}

/// `1–31 Aug`, `1 Jul – 30 Sep`, or `31 Dec 2025 – 5 Jan 2026` depending on how much the
/// range spans (design: mono resolved-range caption beside the period pill).
fn resolved_label(r: DateRange) -> String {
    if r.start.year() == r.end.year() {
        if r.start.month() == r.end.month() {
            format!("{}\u{2013}{} {}", r.start.day(), r.end.day(), r.start.format("%b"))
        } else {
            format!(
                "{} {} \u{2013} {} {}",
                r.start.day(),
                r.start.format("%b"),
                r.end.day(),
                r.end.format("%b")
            )
        }
    } else {
        format!(
            "{} {} {} \u{2013} {} {} {}",
            r.start.day(),
            r.start.format("%b"),
            r.start.year(),
            r.end.day(),
            r.end.format("%b"),
            r.end.year()
        )
    }
}

/// The big period label to the left of the resolved-range caption (design: `August 2026`,
/// `Q3 2026`, `2026`, `Week of 17 Aug`). Not used for Custom, which shows text fields instead.
fn period_label(kind: RangeKind, range: DateRange) -> String {
    match kind {
        RangeKind::Week => format!("Week of {}", short_date(range.start)),
        RangeKind::Month => range.start.format("%B %Y").to_string(),
        RangeKind::Quarter => {
            let q = (range.start.month0() / 3) + 1;
            format!("Q{q} {}", range.start.year())
        }
        RangeKind::Year => range.start.year().to_string(),
        RangeKind::Custom => String::new(),
    }
}

// --- rendering ------------------------------------------------------------------------------

fn pad(x: i8, y: i8) -> egui::Margin {
    egui::Margin::symmetric(x, y)
}

/// Frozen per-frame render data (today.rs's Snapshot pattern), so `state` stays free to
/// mutate below in response to clicks.
struct Snap {
    kind: RangeKind,
    range: DateRange,
    summary: RangeSummary,
    daily: Vec<(NaiveDate, i64)>,
    ranked: Vec<TypeRangeTotal>,
    active_type_count: i64,
}

pub fn show(ui: &mut egui::Ui, state: &mut InsightsState, db: &Db, theme: &Theme) {
    state.ensure_view(db);

    if let Some(err) = &state.error {
        egui::Frame::default().fill(theme.bg_canvas).inner_margin(pad(34, 26)).show(ui, |ui| {
            ui.colored_label(theme.negative, format!("Database error: {err}"));
        });
        return;
    }
    let Some(view) = state.view.as_ref() else { return };
    let snap = Snap {
        kind: state.kind,
        range: view.range,
        summary: view.summary.clone(),
        daily: view.daily.clone(),
        ranked: view.ranked.clone(),
        active_type_count: view.active_type_count,
    };

    egui::Frame::default().fill(theme.bg_canvas).inner_margin(pad(34, 26)).show(ui, |ui| {
        toolbar(ui, state, theme, &snap, db);
        ui.add_space(24.0);
        summary_strip(ui, theme, &snap);
        ui.add_space(4.0);

        if snap.summary.total == 0 {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new("no tallies in this range")
                        .font(t::sans(t::BODY))
                        .color(theme.text_quiet),
                );
            });
            return;
        }

        // Two columns at explicit rects (1.5fr / 1fr). `allocate_ui_with_layout` with a
        // zero height inside a horizontal layout staircases the children and leaves them
        // with no height to scroll in — the same reason today.rs places its bands manually.
        ui.add_space(20.0);
        let gap = 34.0;
        let body = egui::Rect::from_min_max(
            egui::pos2(ui.max_rect().left(), ui.cursor().top()),
            ui.max_rect().max,
        );
        let left_w = (body.width() - gap) * 1.5 / 2.5;
        let mut left = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("insights_left")
                .max_rect(egui::Rect::from_min_size(body.min, egui::vec2(left_w, body.height())))
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        daily_activity_section(&mut left, theme, &snap);
        let mut right = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("insights_right")
                .max_rect(egui::Rect::from_min_max(
                    egui::pos2(body.left() + left_w + gap, body.top()),
                    body.max,
                ))
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        by_category_section(&mut right, theme, &snap);
    });
}

/// The range toolbar (design §04): segmented control, period pill + nav (or two date fields
/// for Custom), Export CSV pill.
fn toolbar(ui: &mut egui::Ui, state: &mut InsightsState, theme: &Theme, snap: &Snap, db: &Db) {
    let mut new_kind = state.kind;
    let mut new_from = state.custom_from.clone();
    let mut new_to = state.custom_to.clone();
    let mut step: Option<i64> = None;
    let mut export_clicked = false;

    ui.horizontal(|ui| {
        segmented_control(ui, theme, &mut new_kind);
        ui.add_space(8.0);

        if new_kind == RangeKind::Custom {
            ui.add(
                egui::TextEdit::singleline(&mut new_from)
                    .hint_text("YYYY-MM-DD")
                    .desired_width(96.0)
                    .font(egui::FontSelection::FontId(t::mono(t::BODY))),
            );
            ui.label(egui::RichText::new("\u{2013}").color(theme.text_tertiary));
            ui.add(
                egui::TextEdit::singleline(&mut new_to)
                    .hint_text("YYYY-MM-DD")
                    .desired_width(96.0)
                    .font(egui::FontSelection::FontId(t::mono(t::BODY))),
            );
        } else {
            if pill::nav_pill(ui, theme, "\u{2039}", true).clicked() {
                step = Some(-1);
            }
            ui.add_space(6.0);
            period_pill(ui, theme, &period_label(new_kind, snap.range), &resolved_label(snap.range));
            ui.add_space(6.0);
            if pill::nav_pill(ui, theme, "\u{203A}", true).clicked() {
                step = Some(1);
            }
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if export_pill(ui, theme).clicked() {
                export_clicked = true;
            }
            if let Some(msg) = &state.export_status {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(msg).font(t::sans(t::CAPTION)).color(theme.text_secondary),
                );
            }
        });
    });

    if let Some(err) = &state.range_error {
        ui.add_space(4.0);
        ui.colored_label(theme.negative, err);
    }

    // Apply interactions: kind switch and Custom-field edits both re-resolve the range;
    // stepping the anchor only makes sense for the non-Custom kinds (arrows are hidden there).
    if new_kind != state.kind {
        state.kind = new_kind;
        state.mark_dirty();
    }
    if new_from != state.custom_from || new_to != state.custom_to {
        state.custom_from = new_from;
        state.custom_to = new_to;
        state.mark_dirty();
    }
    if let Some(delta) = step {
        state.anchor = step_anchor(state.kind, state.anchor, delta);
        state.mark_dirty();
    }
    if export_clicked {
        match export_csv(db, snap.range) {
            Ok(Some(path)) => state.export_status = Some(format!("exported to {}", path.display())),
            Ok(None) => {} // user cancelled the save dialog — leave any prior status alone
            Err(e) => state.export_status = Some(format!("export failed: {e}")),
        }
    }
}

/// The Week/Month/Quarter/Year/Custom segmented control (design §04): `bg_track`, 7px
/// radius, 3px padding; selected segment `bg_raised` fill + `text_primary` + sans_medium.
fn segmented_control(ui: &mut egui::Ui, theme: &Theme, kind: &mut RangeKind) {
    egui::Frame::default()
        .fill(theme.bg_track)
        .corner_radius(7)
        .inner_margin(pad(3, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for k in RangeKind::ALL {
                    let selected = *kind == k;
                    let font = if selected { t::sans_medium(13.0) } else { t::sans(13.0) };
                    let color = if selected { theme.text_primary } else { theme.text_secondary };
                    let galley = ui.painter().layout_no_wrap(k.label().to_owned(), font, color);
                    let size = egui::vec2(galley.rect.width() + 30.0, galley.rect.height() + 14.0);
                    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
                    if selected {
                        ui.painter().rect(
                            rect,
                            egui::CornerRadius::same(5),
                            theme.bg_raised,
                            egui::Stroke::NONE,
                            egui::StrokeKind::Inside,
                        );
                    }
                    let pos = egui::pos2(
                        rect.center().x - galley.rect.width() / 2.0,
                        rect.center().y - galley.rect.height() / 2.0,
                    );
                    ui.painter().galley(pos, galley, color);
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if resp.clicked() {
                        *kind = k;
                    }
                }
            });
        });
}

/// The period pill: `bg_raised`, 1px `border_strong`, 6px radius — label in 13px sans plus
/// the resolved range in mono 11 `text_quiet`.
fn period_pill(ui: &mut egui::Ui, theme: &Theme, label: &str, resolved: &str) {
    egui::Frame::default()
        .fill(theme.bg_raised)
        .stroke(egui::Stroke::new(1.0, theme.border_strong))
        .corner_radius(6)
        .inner_margin(pad(14, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(label).font(t::sans(13.0)).color(theme.text_primary));
                ui.add_space(8.0);
                ui.label(egui::RichText::new(resolved).font(t::mono(11.0)).color(theme.text_quiet));
            });
        });
}

/// The right-aligned "Export CSV" pill: same shape as the period pill.
fn export_pill(ui: &mut egui::Ui, theme: &Theme) -> egui::Response {
    let resp = egui::Frame::default()
        .fill(theme.bg_raised)
        .stroke(egui::Stroke::new(1.0, theme.border_strong))
        .corner_radius(6)
        .inner_margin(pad(14, 8))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new("Export CSV").font(t::sans_medium(13.0)).color(theme.text_body),
            );
        })
        .response
        .interact(egui::Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// `Ok(Some(path))` on a completed export, `Ok(None)` if the user cancelled the save dialog,
/// `Err` on a DB or I/O failure.
fn export_csv(db: &Db, range: DateRange) -> Result<Option<std::path::PathBuf>, String> {
    let ranked = db.range_type_totals_ranked(range).map_err(|e| e.to_string())?;
    let rows: Vec<InsightRow> = ranked
        .into_iter()
        .filter(|r| r.total > 0)
        .map(|r| InsightRow { category: r.category_name, task_type: r.type_name, count: r.total })
        .collect();
    let default_name = format!("simpletally_{}_{}.csv", dates::to_sql(range.start), dates::to_sql(range.end));
    let Some(path) = rfd::FileDialog::new().set_file_name(&default_name).add_filter("CSV", &["csv"]).save_file()
    else {
        return Ok(None);
    };
    let file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
    export_insights(file, &rows, &dates::to_sql(range.start), &dates::to_sql(range.end))
        .map_err(|e| e.to_string())?;
    Ok(Some(path))
}

/// The 4-cell summary strip (design §04): hairline gaps over `theme.border`, 8px radius.
fn summary_strip(ui: &mut egui::Ui, theme: &Theme, snap: &Snap) {
    let per_day = per_active_day(snap.summary.total, snap.summary.active_days);
    let (total_delta_text, total_delta_pos) =
        delta_line(snap.summary.total, snap.summary.previous_total, snap.kind);
    let (busiest_value, busiest_sub) = match snap.summary.busiest_day {
        Some((date, count)) => (count.to_string(), short_date(date)),
        None => ("\u{2014}".to_string(), String::new()),
    };
    let cells = [
        ("TOTAL", snap.summary.total.to_string(), total_delta_text, total_delta_pos),
        (
            "PER ACTIVE DAY",
            per_day.map(|v| format!("{v:.1}")).unwrap_or_else(|| "\u{2014}".to_string()),
            String::new(),
            false,
        ),
        ("BUSIEST DAY", busiest_value, busiest_sub, false),
        (
            "TYPES USED",
            snap.summary.distinct_types.to_string(),
            format!("of {} active", snap.active_type_count),
            false,
        ),
    ];

    // Painted at explicit rects: four cells of equal width over a `border`-filled backing,
    // separated by 1px gaps (the design's hairline effect). Nested Uis were staircasing.
    const H: f32 = 118.0;
    const GAP: f32 = 1.0;
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), H), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, egui::CornerRadius::same(8), theme.border);
    let inner = rect.shrink(1.0);
    let cw = (inner.width() - GAP * 3.0) / 4.0;
    for (i, (label, value, sub, sub_positive)) in cells.iter().enumerate() {
        let cell = egui::Rect::from_min_size(
            egui::pos2(inner.left() + i as f32 * (cw + GAP), inner.top()),
            egui::vec2(cw, inner.height()),
        );
        // Only the outer corners follow the strip's 8px radius.
        let r = match i {
            0 => egui::CornerRadius { nw: 7, sw: 7, ne: 0, se: 0 },
            3 => egui::CornerRadius { nw: 0, sw: 0, ne: 7, se: 7 },
            _ => egui::CornerRadius::ZERO,
        };
        p.rect_filled(cell, r, theme.bg_raised);
        let origin = cell.left_top() + egui::vec2(22.0, 20.0);
        p.text(origin, egui::Align2::LEFT_TOP, *label, t::mono(t::EYEBROW), theme.text_tertiary);
        p.text(
            origin + egui::vec2(0.0, 22.0),
            egui::Align2::LEFT_TOP,
            value,
            t::mono(t::METRIC_VALUE),
            theme.text_primary,
        );
        if !sub.is_empty() {
            let color = if *sub_positive { theme.accent } else { theme.text_quiet };
            p.text(
                origin + egui::vec2(0.0, 64.0),
                egui::Align2::LEFT_TOP,
                sub,
                t::sans(t::CAPTION),
                color,
            );
        }
    }
}

/// Left column: "Daily activity" title + caption, then the chart.
fn daily_activity_section(ui: &mut egui::Ui, theme: &Theme, snap: &Snap) {
    ui.label(
        egui::RichText::new("Daily activity").font(t::sans_medium(t::SECTION_TITLE)).color(theme.text_primary),
    );
    ui.label(egui::RichText::new("tallies per day").font(t::mono(11.0)).color(theme.text_quiet));
    ui.add_space(10.0);
    chart::daily_activity(ui, theme, &snap.daily);
}

/// Right column: "By category" title + caption, stacked ratio bar, then the full ranked
/// list (every type, scrollable — PLAN §5: not just the top 8).
fn by_category_section(ui: &mut egui::Ui, theme: &Theme, snap: &Snap) {
    ui.label(
        egui::RichText::new("By category").font(t::sans_medium(t::SECTION_TITLE)).color(theme.text_primary),
    );
    ui.label(
        egui::RichText::new(format!("share of {}", snap.summary.total))
            .font(t::mono(11.0))
            .color(theme.text_quiet),
    );
    ui.add_space(10.0);

    let segments: Vec<(egui::Color32, i64)> = snap
        .ranked
        .iter()
        .enumerate()
        .map(|(i, r)| (ramp_color(theme, i), r.total))
        .collect();
    chart::stacked_ratio_bar(ui, &segments);
    ui.add_space(12.0);

    // Fills the rest of the column; the list is every type, so it scrolls (PLAN §0).
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for (i, row) in snap.ranked.iter().enumerate() {
            category_row(ui, theme, i, row, snap.summary.total);
        }
    });
}

fn ramp_color(theme: &Theme, rank: usize) -> egui::Color32 {
    theme.ramp.get(rank).copied().unwrap_or(theme.border_strong)
}

fn category_row(ui: &mut egui::Ui, theme: &Theme, rank: usize, row: &TypeRangeTotal, total: i64) {
    let name_color = if rank < 3 { theme.text_primary } else { theme.text_secondary };
    let pct = if total > 0 { row.total as f64 / total as f64 * 100.0 } else { 0.0 };

    ui.add_space(9.0);
    ui.horizontal(|ui| {
        let (swatch_rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
        ui.painter().rect_filled(swatch_rect, egui::CornerRadius::same(2), ramp_color(theme, rank));
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(format!("{} \u{b7} {}", row.category_name.to_uppercase(), row.type_name))
                .font(t::sans(13.0))
                .color(name_color),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.allocate_ui_with_layout(egui::vec2(42.0, 0.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(format!("{pct:.1}%")).font(t::mono(11.5)).color(theme.text_quiet),
                );
            });
            ui.add_space(6.0);
            ui.label(egui::RichText::new(row.total.to_string()).font(t::mono(13.0)).color(theme.text_body));
        });
    });
    ui.add_space(9.0);
    let bottom = ui.painter().add(egui::Shape::Noop);
    let rect = ui.min_rect();
    ui.painter().set(
        bottom,
        egui::Shape::line_segment(
            [egui::pos2(rect.left(), rect.bottom()), egui::pos2(rect.right(), rect.bottom())],
            egui::Stroke::new(1.0, theme.border_subtle),
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    // -- RangeKind string round-trip --

    #[test]
    fn range_kind_string_round_trips() {
        for k in RangeKind::ALL {
            assert_eq!(RangeKind::from_str(k.as_str()), k);
        }
    }

    #[test]
    fn range_kind_from_str_falls_back_to_week_on_unknown() {
        assert_eq!(RangeKind::from_str("nonsense"), RangeKind::Week);
        assert_eq!(RangeKind::from_str(""), RangeKind::Week);
    }

    // -- resolve_range --

    #[test]
    fn resolve_range_week_month_quarter_year_use_the_dates_module() {
        let anchor = d(2026, 8, 25); // Tuesday
        assert_eq!(resolve_range(RangeKind::Week, anchor, "", "").unwrap(), dates::week_of(anchor));
        assert_eq!(resolve_range(RangeKind::Month, anchor, "", "").unwrap(), dates::month_of(anchor));
        assert_eq!(resolve_range(RangeKind::Quarter, anchor, "", "").unwrap(), dates::quarter_of(anchor));
        assert_eq!(resolve_range(RangeKind::Year, anchor, "", "").unwrap(), dates::year_of(anchor));
    }

    #[test]
    fn resolve_range_custom_parses_both_fields() {
        let r = resolve_range(RangeKind::Custom, d(2026, 1, 1), "2026-08-01", "2026-08-31").unwrap();
        assert_eq!(r, DateRange::new(d(2026, 8, 1), d(2026, 8, 31)).unwrap());
    }

    #[test]
    fn resolve_range_custom_rejects_unparseable_dates() {
        assert!(resolve_range(RangeKind::Custom, d(2026, 1, 1), "not-a-date", "2026-08-31").is_err());
        assert!(resolve_range(RangeKind::Custom, d(2026, 1, 1), "2026-08-01", "nope").is_err());
    }

    #[test]
    fn resolve_range_custom_rejects_inverted_range() {
        assert!(resolve_range(RangeKind::Custom, d(2026, 1, 1), "2026-08-31", "2026-08-01").is_err());
    }

    // -- step_anchor --

    #[test]
    fn step_week_moves_by_seven_days() {
        let a = d(2026, 8, 25);
        assert_eq!(step_anchor(RangeKind::Week, a, 1), d(2026, 9, 1));
        assert_eq!(step_anchor(RangeKind::Week, a, -1), d(2026, 8, 18));
    }

    #[test]
    fn step_month_clamps_month_end() {
        // 31 Jan -> Feb has no 31st; clamp to the 28th (2026 is not a leap year).
        let a = d(2026, 1, 31);
        assert_eq!(step_anchor(RangeKind::Month, a, 1), d(2026, 2, 28));
    }

    #[test]
    fn step_month_clamps_on_a_leap_year() {
        let a = d(2024, 1, 31);
        assert_eq!(step_anchor(RangeKind::Month, a, 1), d(2024, 2, 29));
    }

    #[test]
    fn step_quarter_moves_three_months_and_wraps_the_year() {
        assert_eq!(step_anchor(RangeKind::Quarter, d(2026, 11, 15), 1), d(2027, 2, 15));
    }

    #[test]
    fn step_year_moves_twelve_months() {
        assert_eq!(step_anchor(RangeKind::Year, d(2026, 8, 25), 1), d(2027, 8, 25));
        // Leap-day anchor clamps in a non-leap target year.
        assert_eq!(step_anchor(RangeKind::Year, d(2024, 2, 29), 1), d(2025, 2, 28));
    }

    #[test]
    fn step_custom_is_a_no_op() {
        let a = d(2026, 8, 25);
        assert_eq!(step_anchor(RangeKind::Custom, a, 1), a);
    }

    // -- per_active_day --

    #[test]
    fn per_active_day_rounds_to_one_decimal() {
        assert_eq!(per_active_day(10, 3), Some(3.3));
        assert_eq!(per_active_day(7, 2), Some(3.5));
    }

    #[test]
    fn per_active_day_is_none_when_no_active_days() {
        assert_eq!(per_active_day(0, 0), None);
    }

    // -- delta_line --

    #[test]
    fn delta_line_positive() {
        let (text, positive) = delta_line(15, 10, RangeKind::Month);
        assert_eq!(text, "+5 vs previous month");
        assert!(positive);
    }

    #[test]
    fn delta_line_negative() {
        let (text, positive) = delta_line(10, 15, RangeKind::Month);
        assert_eq!(text, "\u{2212}5 vs previous month");
        assert!(!positive);
    }

    #[test]
    fn delta_line_zero() {
        let (text, positive) = delta_line(10, 10, RangeKind::Month);
        assert_eq!(text, "same as previous month");
        assert!(!positive);
    }

    // -- labels --

    #[test]
    fn resolved_label_same_month() {
        let r = DateRange::new(d(2026, 8, 1), d(2026, 8, 31)).unwrap();
        assert_eq!(resolved_label(r), "1\u{2013}31 Aug");
    }

    #[test]
    fn period_label_formats_per_kind() {
        assert_eq!(period_label(RangeKind::Month, dates::month_of(d(2026, 8, 25))), "August 2026");
        assert_eq!(period_label(RangeKind::Year, dates::year_of(d(2026, 8, 25))), "2026");
        assert_eq!(period_label(RangeKind::Quarter, dates::quarter_of(d(2026, 8, 25))), "Q3 2026");
    }
}
