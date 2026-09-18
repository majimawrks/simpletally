//! "Back up your data" dialog (PLAN §1.4).
//!
//! `Db::backup_to` (VACUUM INTO) already existed for `service::restore`'s own safety copy, but
//! nothing let a user take one on demand. This is that surface: opened from the Task types
//! toolbar's Archive button, it shows what's in the database (queried once, on open — not
//! every frame) and offers two backups: the task types + categories as a CSV the app's own
//! importer can read back, or the whole database as a `.tally` file via `VACUUM INTO`.
//!
//! Same shape as `migrate`/`tray_notice`: this module owns its interaction state and a `show`
//! that draws it; the Task types screen owns the field since its toolbar is where the button
//! lives.

use std::path::{Path, PathBuf};

use chrono::NaiveDateTime;

use crate::ui::theme::{self as t, Theme};
use simpletally_core::{
    csvio::{export_task_types, TaskTypeExportRow},
    Db,
};

/// What's in the database, queried once when the dialog opens.
struct Counts {
    categories: i64,
    task_types: i64,
    /// Lifetime total across every type — deliberately not broken down (PLAN §1.4: this
    /// dialog answers "is there something to lose", not a report).
    tallies: i64,
}

impl Counts {
    fn load(db: &Db) -> simpletally_core::Result<Counts> {
        let categories = db.list_categories()?.len() as i64;
        let task_types = db.list_task_types(false)?.len() as i64; // include inactive
        let tallies = db.type_lifetime_totals()?.iter().map(|t| t.total).sum();
        Ok(Counts { categories, task_types, tallies })
    }
}

/// The last backup attempt, shown inline instead of closing the dialog — so a written path or
/// an error stays on screen until the user dismisses it themselves.
enum Outcome {
    Wrote(PathBuf),
    Err(String),
}

pub struct BackupState {
    visible: bool,
    counts: Option<Counts>,
    outcome: Option<Outcome>,
}

impl BackupState {
    pub fn new() -> Self {
        Self { visible: false, counts: None, outcome: None }
    }

    /// Open the dialog, freshly querying what's in the database. A query failure just leaves
    /// the cards off — not worth failing the whole dialog over, and the backup buttons still
    /// work independently.
    pub fn open(&mut self, db: &Db) {
        self.counts = Counts::load(db).ok();
        self.outcome = None;
        self.visible = true;
    }
}

impl Default for BackupState {
    fn default() -> Self {
        Self::new()
    }
}

/// `simpletally-task-types-YYYYMMDD.csv`. Pure so the date formatting is unit-tested without
/// a filesystem or a file dialog.
pub fn task_types_filename(now: NaiveDateTime) -> String {
    format!("simpletally-task-types-{}.csv", now.format("%Y%m%d"))
}

/// `task_tally_backup_YYYYMMDD_HHMMSS.tally` (PLAN §1.4's naming).
pub fn full_backup_filename(now: NaiveDateTime) -> String {
    format!("task_tally_backup_{}.tally", now.format("%Y%m%d_%H%M%S"))
}

/// Task types + their categories, in the same order `csvio`'s export expects
/// (`categories.sort_order ASC, categories.name ASC, task_types.name ASC` — already how
/// `list_task_types` orders its join, so no separate sort is needed here).
fn task_type_export_rows(db: &Db) -> simpletally_core::Result<Vec<TaskTypeExportRow>> {
    let categories = db.list_categories()?;
    let types = db.list_task_types(false)?; // both active and inactive
    let lifetime: std::collections::BTreeMap<i64, i64> =
        db.type_lifetime_totals()?.into_iter().map(|t| (t.task_type_id, t.total)).collect();
    Ok(types
        .iter()
        .map(|ty| {
            let category =
                categories.iter().find(|c| c.id == ty.category_id).map(|c| c.name.clone()).unwrap_or_default();
            TaskTypeExportRow {
                category,
                name: ty.name.clone(),
                description: ty.description.clone(),
                usage_count: lifetime.get(&ty.id).copied().unwrap_or(0),
                is_active: ty.is_active,
            }
        })
        .collect())
}

fn write_task_types_backup(db: &Db, path: &Path) -> Outcome {
    let rows = match task_type_export_rows(db) {
        Ok(r) => r,
        Err(e) => return Outcome::Err(e.to_string()),
    };
    let mut buf = Vec::new();
    if let Err(e) = export_task_types(&mut buf, &rows) {
        return Outcome::Err(e.to_string());
    }
    match std::fs::write(path, buf) {
        Ok(()) => Outcome::Wrote(path.to_path_buf()),
        Err(e) => Outcome::Err(e.to_string()),
    }
}

fn write_full_backup(db: &Db, path: &Path) -> Outcome {
    match db.backup_to(path) {
        Ok(()) => Outcome::Wrote(path.to_path_buf()),
        Err(e) => Outcome::Err(e.to_string()),
    }
}

/// Draw the dialog if it's open.
pub fn show(ui: &mut egui::Ui, state: &mut BackupState, db: &Db, theme: &Theme) {
    if !state.visible {
        return;
    }
    let mut close = false;

    egui::Modal::new(egui::Id::new("backup_data")).show(ui.ctx(), |ui| {
        const W: f32 = 420.0;
        ui.set_width(W);
        // Pin the width: the body's card row is a fixed shape, but the outcome line below it
        // varies, and the modal frame would otherwise resize under the user.
        ui.allocate_exact_size(egui::vec2(W, 0.0), egui::Sense::hover());

        if crate::ui::types::modal_header(ui, theme, "Back up your data", "") {
            close = true;
        }
        ui.add_space(6.0);
        ui.separator();
        ui.add_space(10.0);

        if let Some(c) = &state.counts {
            crate::ui::widgets::card::stat_row(
                ui,
                theme,
                W,
                &[(c.categories, "CATEGORIES", false), (c.task_types, "TASK TYPES", false)],
            );
            ui.add_space(8.0);
            crate::ui::widgets::card::stat_row(ui, theme, W, &[(c.tallies, "TALLIES", true)]);
            ui.add_space(14.0);
        }

        if let Some(outcome) = &state.outcome {
            match outcome {
                Outcome::Wrote(path) => {
                    ui.label(
                        egui::RichText::new(format!("Saved to {}", path.display()))
                            .font(t::sans(t::CAPTION))
                            .color(theme.text_body),
                    );
                }
                Outcome::Err(e) => {
                    ui.colored_label(theme.negative, e);
                }
            }
            ui.add_space(10.0);
        }

        ui.horizontal(|ui| {
            if secondary_button(ui, theme, "Back up task types") {
                let now = chrono::Local::now().naive_local();
                if let Some(path) = rfd::FileDialog::new()
                    .set_file_name(&task_types_filename(now))
                    .add_filter("CSV", &["csv"])
                    .save_file()
                {
                    state.outcome = Some(write_task_types_backup(db, &path));
                }
            }
            ui.add_space(8.0);
            if primary_button(ui, theme, "Back up all data") {
                let now = chrono::Local::now().naive_local();
                if let Some(path) = rfd::FileDialog::new()
                    .set_file_name(&full_backup_filename(now))
                    .add_filter("SimpleTally backup", &["tally"])
                    .save_file()
                {
                    state.outcome = Some(write_full_backup(db, &path));
                }
            }
        });
        ui.add_space(10.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if outline_button(ui, theme, "Close") {
                close = true;
            }
        });
    });

    if close {
        state.visible = false;
    }
}

/// The solid-accent primary button ("Back up all data"), same silhouette as
/// `types::import_report_modal`'s "Close".
fn primary_button(ui: &mut egui::Ui, theme: &Theme, label: &str) -> bool {
    ui.add(
        egui::Button::new(egui::RichText::new(label).font(t::sans_medium(t::BODY)).color(theme.bg_raised))
            .fill(theme.accent)
            .corner_radius(6)
            .min_size(egui::vec2(0.0, 32.0)),
    )
    .clicked()
}

/// A neutral bordered button ("Close") — bg_raised fill, a plain border, no color claim.
fn outline_button(ui: &mut egui::Ui, theme: &Theme, label: &str) -> bool {
    ui.add(
        egui::Button::new(egui::RichText::new(label).font(t::sans_medium(t::BODY)).color(theme.text_body))
            .fill(theme.bg_raised)
            .stroke(egui::Stroke::new(1.0, theme.border_strong))
            .corner_radius(6)
            .min_size(egui::vec2(0.0, 32.0)),
    )
    .clicked()
}

/// The secondary button beside the accent-filled primary ("Back up task types" next to "Back
/// up all data"). Copies `migrate::secondary_button`'s treatment: `theme.secondary`, the blue
/// sibling of the accent, tinted rather than solid so it stays subordinate to the primary.
fn secondary_button(ui: &mut egui::Ui, theme: &Theme, label: &str) -> bool {
    ui.add(
        egui::Button::new(egui::RichText::new(label).font(t::sans_medium(t::BODY)).color(theme.secondary))
            .fill(theme.secondary_tint_bg)
            .stroke(egui::Stroke::new(1.0, theme.secondary_tint_border))
            .corner_radius(6)
            .min_size(egui::vec2(0.0, 32.0)),
    )
    .clicked()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn ts(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, mi, s).unwrap()
    }

    #[test]
    fn task_types_filename_zero_pads_month_and_day() {
        assert_eq!(task_types_filename(ts(2026, 1, 5, 0, 0, 0)), "simpletally-task-types-20260105.csv");
    }

    #[test]
    fn full_backup_filename_zero_pads_time() {
        assert_eq!(
            full_backup_filename(ts(2026, 1, 5, 9, 5, 3)),
            "task_tally_backup_20260105_090503.tally"
        );
    }

    #[test]
    fn full_backup_filename_double_digit_time() {
        assert_eq!(
            full_backup_filename(ts(2026, 12, 31, 23, 59, 9)),
            "task_tally_backup_20261231_235909.tally"
        );
    }
}
