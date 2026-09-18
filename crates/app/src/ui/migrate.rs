//! "Bring your old data across" migration guide.
//!
//! The legacy v3.8 app opened `task_tally.db` via a relative path, so a user's real data
//! usually sits in whatever folder they launched it from, not beside the new portable exe.
//! This modal surfaces that: it shows where *this* app's database lives, lists any other
//! `task_tally.db` files [`crate::service::discovery`] found, and lets the user point at one
//! (or browse for a file) to bring in via [`crate::service::restore::restore`].
//!
//! Drawn from `redraw_main` as an `egui::Modal`, same pattern as
//! `types::manage_categories_modal`. This module only builds the pure decision helpers and
//! the widget; the actual DB swap (releasing the live handle so Windows will let `restore`
//! rename over it, then reopening) is orchestrated by `platform::app::App`, which owns `db`.

use std::path::{Path, PathBuf};

use crate::ui::theme::{self as t, Theme};

/// Whether the guide should open unprompted on first paint. Only when the live DB was just
/// created empty (a fresh/missing file) — a user who already has real data beside the exe
/// must never be nagged, regardless of what else discovery found lying around.
pub fn should_auto_open(created_empty: bool) -> bool {
    created_empty
}

/// True if `chosen` names the same file as `live` — including via a different path spelling
/// (relative vs. absolute, `.`/`..` components, a different drive letter case, ...). Falls
/// back to a raw path comparison if either side fails to canonicalize (e.g. it doesn't exist).
pub fn is_live_database(chosen: &Path, live: &Path) -> bool {
    match (chosen.canonicalize(), live.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => chosen == live,
    }
}

/// A candidate file as shown in the list: its path plus the size/modified-date `describe`
/// could read from the filesystem (both `None` if the metadata read failed — the row still
/// shows the path).
pub struct Candidate {
    pub path: PathBuf,
    pub size: Option<u64>,
    pub modified: Option<String>,
}

fn describe(path: &Path) -> Candidate {
    let meta = std::fs::metadata(path).ok();
    let size = meta.as_ref().map(|m| m.len());
    let modified = meta.and_then(|m| m.modified().ok()).map(|t| {
        let datetime: chrono::DateTime<chrono::Local> = t.into();
        datetime.format("%Y-%m-%d %H:%M").to_string()
    });
    Candidate { path: path.to_path_buf(), size, modified }
}

fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let kb = bytes as f64 / KB;
    if kb < 1024.0 {
        format!("{kb:.0} KB")
    } else {
        format!("{:.1} MB", kb / 1024.0)
    }
}

/// The result of the most recent import attempt, shown inline in place of the picker once
/// something has happened.
pub enum Outcome {
    /// Nothing tried yet (or the picker should still be shown).
    None,
    /// The dialog refused a pick with an inline message and did no file operations — the
    /// picker stays up so the user can try again.
    Refused(String),
    /// `restore` failed; the live DB was left untouched (message says so alongside the error).
    Failed(String),
    /// Import succeeded; the live DB now holds the imported data.
    Imported { task_types: usize, total_tallies: i64 },
    /// The import is about to run. The database lives on the main thread (PHASE0 gate #3,
    /// decided), so the whole window freezes for its duration — this state is painted and
    /// presented for one frame *before* the work starts, so the freeze reads as "working"
    /// rather than as a hang. Without it the user clicks Import and the previous frame just
    /// sits there.
    Importing,
}

pub struct MigrateState {
    pub visible: bool,
    pub candidates: Vec<Candidate>,
    pub outcome: Outcome,
}

impl MigrateState {
    pub fn new(candidate_paths: Vec<PathBuf>) -> Self {
        Self {
            visible: false,
            candidates: candidate_paths.iter().map(|p| describe(p)).collect(),
            outcome: Outcome::None,
        }
    }

    /// Re-run by the tray's "Migrate old data..." item: refresh what discovery finds (a file
    /// may have shown up since the app started) and open on a clean picker.
    pub fn reopen(&mut self, candidate_paths: Vec<PathBuf>) {
        self.candidates = candidate_paths.iter().map(|p| describe(p)).collect();
        self.outcome = Outcome::None;
        self.visible = true;
    }

    /// Open on first paint, when the live DB was just created empty.
    pub fn open_for_first_run(&mut self) {
        self.outcome = Outcome::None;
        self.visible = true;
    }
}

/// What the user asked the modal to do this frame. The caller (`App`) owns the live `Db` and
/// performs the actual swap; this module only decides and reports.
pub enum Action {
    None,
    /// Bring in this file — already checked not to be the live database.
    Import(PathBuf),
}

/// Draw the modal. `exe_dir` and `live_db` describe this app's own database (for the "this
/// folder" explanation and the live-file guard); `db_summary` is called only after a
/// successful import, to report what arrived, so the caller doesn't have to query the DB on
/// every frame.
pub fn show(ui: &mut egui::Ui, state: &mut MigrateState, theme: &Theme, exe_dir: &Path, live_db: &Path) -> Action {
    if !state.visible {
        return Action::None;
    }
    let mut action = Action::None;
    let mut close = false;
    // Set instead of writing `state.outcome` directly inside the closure below — the match
    // on `&state.outcome` needed for rendering the current outcome stays borrowed for the
    // whole closure, so a refusal is applied once the closure (and that borrow) has ended.
    let mut refuse: Option<String> = None;
    const LIVE_DB_MSG: &str = "that's already this app's live database";

    egui::Modal::new(egui::Id::new("migrate_old_data")).show(ui.ctx(), |ui| {
        const MODAL_W: f32 = 460.0;
        ui.set_width(MODAL_W);
        // Pin the used rect to the full width — the modal frame otherwise resizes as the
        // body switches between the picker and the result line.
        ui.allocate_exact_size(egui::vec2(MODAL_W, 0.0), egui::Sense::hover());

        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Bring your old data across")
                    .font(t::sans_medium(t::SECTION_TITLE))
                    .color(theme.text_primary),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::ui::widgets::close::close_button(ui, theme).clicked() {
                    close = true;
                }
            });
        });
        ui.separator();
        ui.add_space(8.0);

        match &state.outcome {
            Outcome::Importing => {
                ui.label(
                    egui::RichText::new("Importing\u{2026}")
                        .font(t::sans_medium(t::BODY))
                        .color(theme.text_primary),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "Copying, checking and swapping in the file. The window won't respond \
                         until it finishes.",
                    )
                    .font(t::sans(t::CAPTION))
                    .color(theme.text_quiet),
                );
                ui.add_space(10.0);
            }
            Outcome::Imported { task_types, total_tallies } => {
                ui.label(
                    egui::RichText::new(format!(
                        "Imported {task_types} task type{} and {total_tallies} tally{} total.",
                        if *task_types == 1 { "" } else { "s" },
                        if *total_tallies == 1 { "" } else { "s" },
                    ))
                    .font(t::sans(t::BODY))
                    .color(theme.text_body),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "The previous (empty) database was kept as a safety copy only until the swap completed.",
                    )
                    .font(t::sans(t::CAPTION))
                    .color(theme.text_quiet),
                );
                ui.add_space(10.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if close_button(ui, theme, "Done") {
                        close = true;
                    }
                });
            }
            outcome_before_import => {
                ui.label(
                    egui::RichText::new(
                        "SimpleTally keeps everything in task_tally.db, in this folder. The old \
                         app used the same file name, but in whatever folder it was launched from.",
                    )
                    .font(t::sans(t::BODY))
                    .color(theme.text_body),
                );
                ui.add_space(6.0);
                ui.label(egui::RichText::new(exe_dir.display().to_string()).font(t::mono(t::CAPTION)).color(theme.text_secondary));
                ui.add_space(10.0);

                if !state.candidates.is_empty() {
                    ui.label(
                        egui::RichText::new("FOUND NEARBY").font(t::mono(t::EYEBROW)).color(theme.text_tertiary),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("Nothing has been chosen yet — pick one below, or start empty.")
                            .font(t::sans(t::CAPTION))
                            .color(theme.text_quiet),
                    );
                    ui.add_space(6.0);
                    for c in &state.candidates {
                        let path = c.path.clone();
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(egui::RichText::new(c.path.display().to_string()).font(t::mono(t::CAPTION)).color(theme.text_body));
                                let mut detail = String::new();
                                if let Some(size) = c.size {
                                    detail.push_str(&human_size(size));
                                }
                                if let Some(modified) = &c.modified {
                                    if !detail.is_empty() {
                                        detail.push_str(" \u{b7} ");
                                    }
                                    detail.push_str(modified);
                                }
                                if !detail.is_empty() {
                                    ui.label(egui::RichText::new(detail).font(t::sans(t::CAPTION)).color(theme.text_quiet));
                                }
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if close_button(ui, theme, "Use this") {
                                    if is_live_database(&path, live_db) {
                                        refuse = Some(LIVE_DB_MSG.to_string());
                                    } else {
                                        action = Action::Import(path.clone());
                                    }
                                }
                            });
                        });
                        ui.add_space(6.0);
                    }
                    ui.add_space(4.0);
                }

                if let Outcome::Refused(msg) | Outcome::Failed(msg) = outcome_before_import {
                    ui.colored_label(theme.negative, msg.as_str());
                    ui.add_space(6.0);
                }

                ui.horizontal(|ui| {
                    if close_button(ui, theme, "Choose a file\u{2026}") {
                        if let Some(path) = rfd::FileDialog::new().add_filter("SQLite database", &["db"]).pick_file() {
                            if is_live_database(&path, live_db) {
                                refuse = Some(LIVE_DB_MSG.to_string());
                            } else {
                                action = Action::Import(path);
                            }
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if secondary_button(ui, theme, "Start empty") {
                            close = true;
                        }
                    });
                });
            }
        }
    });

    if let Some(msg) = refuse {
        state.outcome = Outcome::Refused(msg);
    }
    if close {
        state.visible = false;
    }
    action
}

/// A small filled button, styled like `types::import_report_modal`'s "Close".
fn close_button(ui: &mut egui::Ui, theme: &Theme, label: &str) -> bool {
    ui.add(
        egui::Button::new(egui::RichText::new(label).font(t::sans_medium(t::BODY)).color(theme.bg_raised))
            .fill(theme.accent)
            .corner_radius(6)
            .min_size(egui::vec2(0.0, 32.0)),
    )
    .clicked()
}

/// The secondary button beside the accent-filled primary ("Start empty" next to "Choose a
/// file…"). Same silhouette as [`close_button`] — 32px tall, 6px radius — so the two read as a
/// pair of real buttons, but neutral-filled with a border instead of accent.
///
/// It was first transparent text in `text_secondary` (how this codebase paints *disabled*
/// text), then a neutral outline — which in dark mode was a grey box on a near-black surface,
/// still barely a button. It now uses `theme.secondary`, the blue sibling of the accent: same
/// saturation and lightness, hue rotated, so it belongs to the palette rather than arriving
/// from outside it. Tinted fill rather than solid, so it stays subordinate to the
/// accent-filled primary beside it — the same treatment the trash view's `Restore` uses.
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

    // --- should_auto_open ---------------------------------------------------------------

    #[test]
    fn created_empty_with_candidates_auto_opens() {
        assert!(should_auto_open(true));
    }

    #[test]
    fn created_empty_without_candidates_auto_opens() {
        assert!(should_auto_open(true));
    }

    #[test]
    fn not_created_empty_never_auto_opens() {
        assert!(!should_auto_open(false));
        assert!(!should_auto_open(false));
    }

    // --- is_live_database ----------------------------------------------------------------

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("st_migrate_ui_test_{tag}_{}_{nanos}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn different_files_are_not_the_live_database() {
        let dir = TempDir::new("diff");
        let live = dir.0.join("task_tally.db");
        let other = dir.0.join("other.db");
        std::fs::write(&live, b"a").unwrap();
        std::fs::write(&other, b"b").unwrap();

        assert!(!is_live_database(&other, &live));
    }

    #[test]
    fn same_file_reached_by_different_spelling_is_the_live_database() {
        let dir = TempDir::new("same");
        let live = dir.0.join("task_tally.db");
        std::fs::write(&live, b"a").unwrap();
        // A differently-spelled path to the exact same file (extra "." component).
        let spelled = dir.0.join(".").join("task_tally.db");

        assert!(is_live_database(&spelled, &live));
        assert!(is_live_database(&live, &live));
    }

    #[test]
    fn nonexistent_paths_fall_back_to_raw_comparison() {
        let dir = TempDir::new("missing");
        let live = dir.0.join("task_tally.db"); // never created
        assert!(is_live_database(&live, &live));
        assert!(!is_live_database(&dir.0.join("nope.db"), &live));
    }
}
