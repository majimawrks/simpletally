//! The winit `ApplicationHandler`: owns the main window and the quick-add popup,
//! runs their egui passes, and schedules repaints. Wake-up contract per PHASE0 gate
//! item 2; multi-window GL discipline per gate item 5.

use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::platform::windows::WindowAttributesExtWindows;
use winit::window::{Window, WindowId};

use std::path::PathBuf;

use simpletally_core::Db;

use super::event::{ActivationTarget, UserEvent};
use super::hotkey::Hotkey;
use super::icon;
use super::tray::Tray;
use super::win::Win;
use crate::service::settings::Settings;
use crate::service::{db_service, discovery, restore, settings};

/// GL clear colour behind the quick-add popup, as **linear** RGB — the framebuffer is sRGB,
/// so these are not the hex channels. This is `#1E1E1B`, the popup's own background, so the
/// sliver outside the painted panel on a fractional-DPI rounding can't flash a lighter rim.
const POPUP_BG: [f32; 3] = [0.0130, 0.0130, 0.0110];

/// What startup found before the UI existed: whether the live DB was just created empty
/// (PLAN §1.5), and any other `task_tally.db` files discovery turned up elsewhere. Handed to
/// [`App::new`] so it can decide whether the migration guide should open unprompted.
pub struct FirstRun {
    pub created_empty: bool,
    pub candidates: Vec<PathBuf>,
}

pub struct App {
    proxy: EventLoopProxy<UserEvent>,
    main: Option<Win>,
    popup: Option<Win>,
    tray: Option<Tray>,
    hotkey: Option<Hotkey>,
    hotkey_error: Option<String>,
    /// The open database (single-threaded; owned here per PLAN §2). All UI queries and
    /// mutations go through this handle.
    db: Db,
    /// The Today screen's state (selected date, filter, cached view, pending note).
    today: crate::ui::today::TodayState,
    /// The Insights screen's state (range kind/anchor, cached view).
    insights: crate::ui::insights::InsightsState,
    /// The Task types screen's state (search/filter, selection, cached view).
    types: crate::ui::types::TypesState,
    /// Which of the three tabs is showing (transient — not yet persisted, PLAN §5).
    tab: crate::ui::chrome::Tab,
    /// The "bring your old data across" migration guide's state.
    migrate: crate::ui::migrate::MigrateState,
    /// Set once, from [`FirstRun`], and consumed on the first paint: whether the guide
    /// should open unprompted.
    auto_open_migrate: bool,
    /// The resolved color theme (follows the OS light/dark setting).
    theme: crate::ui::theme::Theme,
    /// The quick-add popup's state (query, selection, cached type list).
    quickadd: crate::ui::quickadd::QuickAddState,
    /// Foreground window captured when the popup was summoned, restored on an explicit
    /// dismiss (Esc / hotkey) so the user returns to what they were doing. Not restored
    /// on click-away — there the user already chose a new foreground.
    prev_foreground: Option<isize>,
    /// Re-entrancy guard: SwapBuffers can pump a Windows message that re-enters
    /// window_event synchronously; painting a window while already painting another
    /// makes a second GL context current mid-swap and fails. When set, defer.
    painting: bool,
    /// What this launch should surface once windows exist (Main, or QuickAdd for a
    /// `--quick-add` cold start).
    startup: ActivationTarget,

    // --- settings persistence (PLAN §5) ------------------------------------
    /// Beside the executable; `None` if it couldn't be resolved (e.g. no parent dir for
    /// the running exe), in which case settings are silently never loaded or saved rather
    /// than treated as fatal — same stance as `settings::load` itself.
    settings_path: Option<PathBuf>,
    /// The raw theme preference (Light/Dark/System), as opposed to `theme` which is the
    /// already-resolved color set. There's no UI to change this yet, so it only ever
    /// reflects what was loaded at startup, but it's what gets written back.
    theme_pref: settings::Theme,
    /// What `settings.toml` held after the last successful load or save, used to detect
    /// "did anything actually change" before writing (`Settings` derives `PartialEq`).
    last_saved_settings: Settings,
    last_save_at: Instant,
    /// Logged to stderr at most once per failure streak, so a persistently unwritable
    /// settings path (read-only install dir) doesn't spam the log every throttle tick.
    settings_save_error_logged: bool,
    /// The geometry loaded from settings, applied once the main window is created in
    /// `resumed` (nothing to apply it to before then) and cleared after.
    pending_geometry: Option<settings::WindowGeometry>,
    /// An import the user asked for, run at the *end* of the next frame so the migration
    /// modal's "Importing…" state reaches the screen before the main thread blocks on it.
    pending_import: Option<PathBuf>,
    /// The one-time "closing hides to the tray" notice.
    tray_notice: crate::ui::tray_notice::TrayNoticeState,
    /// Mirrors `Settings::hide_to_tray_notice_dismissed`; the live value `current_settings`
    /// writes back. Loaded at startup, set when the user ticks the checkbox.
    hide_to_tray_notice_dismissed: bool,
}

/// Minimum time between settings writes. Dragging or resizing the main window would
/// otherwise ask to save every single frame; this throttles that to one write per tick.
const SETTINGS_SAVE_THROTTLE: Duration = Duration::from_secs(2);

/// Everything restorable from `Settings` without touching a window or the database —
/// kept as a pure function of `(Settings, today)` so the "never resume yesterday's date"
/// rule and the various fallbacks can be unit-tested without standing up winit or SQLite.
struct Restored {
    tab: crate::ui::chrome::Tab,
    filter: crate::ui::today::CategoryFilter,
    note_open: bool,
    range_kind: crate::ui::insights::RangeKind,
    range_anchor: chrono::NaiveDate,
    custom_range_from: String,
    custom_range_to: String,
    theme_mode: Option<crate::ui::theme::Mode>,
    selected_date: chrono::NaiveDate,
}

fn restore_from_settings(settings: &Settings, today: chrono::NaiveDate) -> Restored {
    use simpletally_core::dates::{parse_sql, to_sql};

    // Reopening the app tomorrow morning must never silently resume yesterday's date —
    // Today's whole point is logging against *today*, and a stale carried-over date would
    // put tallies on the wrong day with no visible cue. Pinning to another date is now a
    // within-session decision (`TodayState::following_today`/`pinned_on`), never persisted:
    // a pinned date restored from a *previous* launch is exactly the stale-date bug this
    // guards against, so launch always follows today rather than restoring `selected_date`
    // at all. The field is still read from and written to `settings.toml` below, only for
    // file compatibility with older versions of the file.
    let selected_date = today;

    let filter = match settings.selected_category {
        Some(id) => crate::ui::today::CategoryFilter::One(id),
        None => crate::ui::today::CategoryFilter::All,
    };

    let range_anchor = settings
        .range_anchor
        .as_deref()
        .and_then(parse_sql)
        .unwrap_or(today);
    let custom_range_from = settings
        .custom_range_from
        .clone()
        .unwrap_or_else(|| to_sql(today));
    let custom_range_to = settings
        .custom_range_to
        .clone()
        .unwrap_or_else(|| to_sql(today));

    let theme_mode = match settings.theme {
        settings::Theme::Light => Some(crate::ui::theme::Mode::Light),
        settings::Theme::Dark => Some(crate::ui::theme::Mode::Dark),
        settings::Theme::System => None,
    };

    Restored {
        tab: crate::ui::chrome::Tab::from_str(&settings.active_tab),
        filter,
        note_open: settings.note_field_open,
        range_kind: crate::ui::insights::RangeKind::from_str(&settings.range_kind),
        range_anchor,
        custom_range_from,
        custom_range_to,
        theme_mode,
        selected_date,
    }
}

/// A plain rectangle (no winit/DPI types), so the monitor-intersection test can be
/// unit-tested without an `ActiveEventLoop`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

impl Rect {
    fn right(self) -> i64 {
        self.x as i64 + self.width as i64
    }

    fn bottom(self) -> i64 {
        self.y as i64 + self.height as i64
    }

    fn intersects(self, other: Rect) -> bool {
        (self.x as i64) < other.right()
            && self.right() > other.x as i64
            && (self.y as i64) < other.bottom()
            && self.bottom() > other.y as i64
    }
}

/// Whether `saved` (a settings-file window rect) should be kept, i.e. it overlaps at
/// least one currently-attached monitor. An empty monitor list means the caller couldn't
/// enumerate any — keep the saved geometry rather than discard it, since we have no
/// evidence it's actually offscreen.
fn geometry_fits_a_monitor(saved: Rect, monitors: &[Rect]) -> bool {
    if monitors.is_empty() {
        return true;
    }
    monitors.iter().any(|m| saved.intersects(*m))
}

impl App {
    pub fn new(proxy: EventLoopProxy<UserEvent>, startup: ActivationTarget, db: Db, first_run: FirstRun) -> Self {
        let auto_open_migrate = crate::ui::migrate::should_auto_open(first_run.created_empty);

        let settings_path = db_service::settings_path().ok();
        let loaded_settings = settings_path
            .as_ref()
            .map(|p| settings::load(p))
            .unwrap_or_default();
        let today = chrono::Local::now().date_naive();
        let restored = restore_from_settings(&loaded_settings, today);

        let mut today_state = crate::ui::today::TodayState::new();
        today_state.selected_date = restored.selected_date;
        today_state.filter = restored.filter;
        today_state.note_open = restored.note_open;

        let mut insights_state = crate::ui::insights::InsightsState::new(today);
        insights_state.kind = restored.range_kind;
        insights_state.anchor = restored.range_anchor;
        insights_state.custom_from = restored.custom_range_from;
        insights_state.custom_to = restored.custom_range_to;

        Self {
            proxy,
            main: None,
            popup: None,
            tray: None,
            hotkey: None,
            hotkey_error: None,
            db,
            today: today_state,
            insights: insights_state,
            types: crate::ui::types::TypesState::new(),
            tab: restored.tab,
            migrate: crate::ui::migrate::MigrateState::new(first_run.candidates),
            auto_open_migrate,
            theme: crate::ui::theme::resolve(restored.theme_mode),
            quickadd: crate::ui::quickadd::QuickAddState::new(),
            prev_foreground: None,
            painting: false,
            startup,
            settings_path,
            theme_pref: loaded_settings.theme,
            pending_geometry: loaded_settings.main_window_geometry,
            pending_import: None,
            tray_notice: crate::ui::tray_notice::TrayNoticeState::default(),
            hide_to_tray_notice_dismissed: loaded_settings.hide_to_tray_notice_dismissed,
            last_saved_settings: loaded_settings,
            last_save_at: Instant::now(),
            settings_save_error_logged: false,
        }
    }

    // --- main window -------------------------------------------------------

    fn show_main(&self) {
        if let Some(w) = self.main.as_ref() {
            w.window().set_minimized(false); // un-minimize; focus_window skips minimized
            w.set_visible(true);
            w.window().focus_window();
            w.window().request_redraw();
        }
    }

    fn hide_main(&mut self) {
        if let Some(w) = self.main.as_ref() {
            w.set_visible(false);
        }
        // Hiding to tray is a common "exit" from the user's point of view — flush
        // unconditionally rather than leave up to 2s of state (PLAN §5) sitting unsaved
        // behind the throttle.
        self.flush_settings();
    }

    fn redraw_main(&mut self) {
        if self.painting {
            // Re-entrant (SwapBuffers pumped a message). Defer to a fresh iteration.
            if let Some(w) = self.main.as_ref() {
                w.window().request_redraw();
            }
            return;
        }
        self.painting = true;
        // Disjoint field borrows: `today`/`insights`/`tab`/`db`/`theme`/`hotkey_error` are
        // separate fields.
        let today = &mut self.today;
        let insights = &mut self.insights;
        let types = &mut self.types;
        let tab = &mut self.tab;
        let migrate = &mut self.migrate;
        let tray_notice = &mut self.tray_notice;
        let db = &self.db;
        let theme = &self.theme;
        let hotkey_error = self.hotkey_error.as_deref();
        let clear = crate::ui::theme::canvas_clear(theme);
        let mut migrate_action = crate::ui::migrate::Action::None;
        let mut notice_action = crate::ui::tray_notice::Action::None;
        if let Some(win) = self.main.as_mut() {
            win.next_repaint = None;
            win.paint(clear, |ui| {
                let full = ui.max_rect();
                let strip_bottom = crate::ui::chrome::tab_strip(ui, theme, tab);
                let body_rect = egui::Rect::from_min_max(
                    egui::pos2(full.left(), strip_bottom),
                    full.max,
                );
                let mut body = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt("tab_body")
                        .max_rect(body_rect)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                match tab {
                    crate::ui::chrome::Tab::Today => {
                        crate::ui::today::show(&mut body, today, db, theme, hotkey_error);
                    }
                    crate::ui::chrome::Tab::Insights => {
                        crate::ui::insights::show(&mut body, insights, db, theme);
                    }
                    crate::ui::chrome::Tab::TaskTypes => {
                        crate::ui::types::show(&mut body, types, db, theme);
                    }
                }

                // Above the screens, like the other modals.
                if let (Ok(exe_dir), Ok(live_db)) = (db_service::exe_dir(), db_service::database_path()) {
                    migrate_action = crate::ui::migrate::show(ui, migrate, theme, &exe_dir, &live_db);
                }
                notice_action = crate::ui::tray_notice::show(ui, tray_notice, theme);
            });
            // The Task types screen can write data the other screens show (categories behind
            // Today's pill row, trash restore/purge moving entries) while they aren't the
            // visible tab; drain the cross-screen flag every frame so they rebuild next time.
            if types.take_data_changed() {
                today.mark_dirty();
                insights.mark_dirty();
            }
            // The same the other way: a tally, edit or delete on Today changes the totals an
            // already-loaded Insights is showing. Without this its view stayed stale while its
            // CSV export queried fresh data, so the screen and the export disagreed.
            if today.take_data_changed() {
                insights.mark_dirty();
            }
            // NB: painting must NOT change visibility — a stray repaint would otherwise
            // re-show a window closed to the tray. Visibility is owned by show_/hide_.
        }
        self.painting = false;

        // Order matters. A requested import runs only on the frame *after* it was asked for,
        // so the `Importing…` state set below has been painted and presented first. The DB is
        // on the main thread (PHASE0 gate #3), so this call freezes the window for its whole
        // duration; without the earlier frame the user would just see the pre-click frame
        // sitting there, indistinguishable from a hang.
        // The notice is the last thing standing between `[x]` and the window going away, so
        // the hide happens here, once it's dismissed.
        if let crate::ui::tray_notice::Action::Closed { remember } = notice_action {
            // Held on `App`, not written into `last_saved_settings`: that field is what the
            // change detector compares *against*, so setting it there would make the new value
            // look already-saved and it would never reach the file. `current_settings` picks
            // it up, `hide_main`'s flush writes it.
            self.hide_to_tray_notice_dismissed |= remember;
            self.hide_main();
        }

        if let Some(chosen) = self.pending_import.take() {
            self.handle_migrate_import(chosen);
        }
        if let crate::ui::migrate::Action::Import(chosen) = migrate_action {
            self.migrate.outcome = crate::ui::migrate::Outcome::Importing;
            self.pending_import = Some(chosen);
            if let Some(m) = self.main.as_ref() {
                m.window().request_redraw();
            }
        }
    }

    /// Bring `chosen` in as the live database (PLAN task: "bring your old data across").
    ///
    /// `restore::restore` deletes/renames the live file, which Windows refuses while this
    /// process still holds an open connection to it — so the live `Db` handle is swapped out
    /// to an in-memory placeholder and dropped *before* calling `restore`, then the real file
    /// is reopened afterwards, on both the success and failure paths (on failure `restore`
    /// leaves the live file untouched, but our handle still needs to point back at it).
    fn handle_migrate_import(&mut self, chosen: std::path::PathBuf) {
        let base = match db_service::database_path() {
            Ok(p) => p,
            Err(e) => {
                self.migrate.outcome = crate::ui::migrate::Outcome::Failed(e.to_string());
                return;
            }
        };

        let placeholder = match Db::open_in_memory() {
            Ok(db) => db,
            Err(e) => {
                self.migrate.outcome = crate::ui::migrate::Outcome::Failed(e.to_string());
                return;
            }
        };
        let old_db = std::mem::replace(&mut self.db, placeholder);
        drop(old_db); // release the OS handle so `restore` can delete/rename `base`

        let restore_result = restore::restore(&base, &chosen);

        // Reopen the real file either way: on success it now holds the imported data; on
        // failure `restore` left it untouched, but our handle is still the placeholder.
        match db_service::open_at(&base) {
            Ok((db, _outcome)) => self.db = db,
            Err(reopen_err) => {
                // Fatal: we cannot leave the app running on an in-memory DB the user can't
                // see or save. Surface it the same way main.rs does a fatal open failure.
                rfd::MessageDialog::new()
                    .set_level(rfd::MessageLevel::Error)
                    .set_title("SimpleTally — cannot reopen database")
                    .set_description(reopen_err.to_string())
                    .show();
                std::process::exit(1);
            }
        }

        match restore_result {
            Ok(()) => {
                // Not just dirty: every screen's open dialogs hold ids from the database
                // that is no longer there.
                self.today.reset_for_new_database();
                self.insights.reset_for_new_database();
                self.types.reset_for_new_database();
                let task_types = self.db.list_task_types(false).map(|v| v.len()).unwrap_or(0);
                let total_tallies: i64 = self
                    .db
                    .type_lifetime_totals()
                    .map(|v| v.iter().map(|l| l.total).sum())
                    .unwrap_or(0);
                self.migrate.outcome = crate::ui::migrate::Outcome::Imported { task_types, total_tallies };
            }
            Err(e) => {
                self.migrate.outcome = crate::ui::migrate::Outcome::Failed(e.to_string());
            }
        }
    }

    /// Re-run discovery and open the migration guide fresh — the tray's "Migrate old data..."
    /// item, since a user may have clicked past the auto-opened guide on first run.
    fn open_migrate_dialog(&mut self) {
        let candidates = db_service::exe_dir()
            .map(|exe_dir| discovery::discover(&exe_dir, &discovery::default_search_dirs()).other_candidates)
            .unwrap_or_default();
        self.migrate.reopen(candidates);
        self.show_main();
    }

    // --- quick-add popup ---------------------------------------------------

    fn show_popup(&mut self) {
        // Remember what was in front, before we steal focus, so an explicit dismiss
        // can hand focus back. Don't overwrite a capture from an earlier still-open
        // summon.
        if self.prev_foreground.is_none() {
            self.prev_foreground = super::winos::foreground_window();
        }
        // A fresh summon starts empty: the query from last time is never what you want now,
        // and the type list may have changed since.
        self.quickadd.reset();
        // Paint once while hidden, then center on the active monitor and show + focus
        // — no unpainted-frame flash, appears where the user is working. The size must be
        // right *before* centering, or the popup lands off-centre by half the difference.
        self.paint_popup();
        self.fit_popup();
        if let Some(p) = self.popup.as_ref() {
            super::winos::center_on_active_monitor(p.window());
            p.set_visible(true);
            // winit's set_visible rewrites GWL_EXSTYLE and drops our WS_EX_TOOLWINDOW,
            // so reapply it after every show or the popup returns to Alt-Tab (#6).
            super::winos::exclude_from_alt_tab(p.window());
            p.window().focus_window();
        }
    }

    /// Hide the popup. `restore_focus` returns the foreground to the window captured
    /// when it was summoned (Esc / hotkey dismiss); pass false for click-away, where
    /// the user already picked a new foreground.
    fn hide_popup(&mut self, restore_focus: bool) {
        if let Some(p) = self.popup.as_ref() {
            p.set_visible(false);
        }
        self.quickadd.reset();
        let prev = self.prev_foreground.take();
        if restore_focus {
            if let Some(h) = prev {
                super::winos::set_foreground(h);
            }
        }
    }

    fn toggle_popup(&mut self) {
        // Hide only when it's visible AND focused; otherwise raise it. This makes a
        // press bring a behind/unfocused popup to the front instead of hiding it
        // (fixes the "press twice to reopen after Alt-Tab" case).
        let (visible, focused) = self
            .popup
            .as_ref()
            .map(|p| (p.is_visible(), p.window().has_focus()))
            .unwrap_or((false, false));
        if visible && focused {
            self.hide_popup(true); // explicit dismiss — return focus
        } else {
            self.show_popup();
        }
    }

    /// Paint one popup frame. Returns true if the popup should close — Esc, or a tally that
    /// was logged (quick add's whole point is log-and-dismiss without raising the app).
    fn paint_popup(&mut self) -> bool {
        if self.painting {
            if let Some(p) = self.popup.as_ref() {
                p.window().request_redraw();
            }
            return false;
        }
        self.painting = true;

        // Disjoint field borrows: `paint` takes `&mut self.popup` while the closure needs
        // `&self.db` and `&mut self.quickadd`, which the borrow checker only allows as
        // separate locals (same pattern as `redraw_main`).
        let db = &self.db;
        let quickadd = &mut self.quickadd;
        let today = chrono::Local::now().date_naive();
        let mut logged = None;
        let mut close = false;

        if let Some(win) = self.popup.as_mut() {
            win.next_repaint = None;
            win.paint(POPUP_BG, |ui| {
                match crate::ui::quickadd::show(ui, quickadd, db, today) {
                    crate::ui::quickadd::Action::None => {}
                    crate::ui::quickadd::Action::Dismiss => close = true,
                    crate::ui::quickadd::Action::Logged(line) => {
                        logged = Some(line);
                        close = true;
                    }
                }
            });
        }
        self.painting = false;

        if logged.is_some() {
            // The write landed behind the other screens' cached views; they must re-query
            // before they are next shown, exactly as a write from the Task types screen does.
            self.today.mark_dirty();
            self.insights.mark_dirty();
        }
        close
    }

    /// Size the popup window to the rows it currently shows. Called after painting, so the
    /// height reflects the result list the user just filtered to.
    fn fit_popup(&mut self) {
        let Some(p) = self.popup.as_ref() else { return };
        let size = LogicalSize::new(
            crate::ui::quickadd::WIDTH as f64,
            crate::ui::quickadd::height_for(&self.quickadd) as f64,
        );
        let scale = p.window().scale_factor();
        if p.window().inner_size() != size.to_physical(scale) {
            let _ = p.window().request_inner_size(size);
        }
    }

    fn redraw_popup(&mut self) {
        let close = self.paint_popup();
        // Grow/shrink to the filtered list before the next frame. Only while it's on screen:
        // a resize of a hidden window would fight `show_popup`'s own fit-then-centre.
        if !close {
            self.fit_popup();
        }
        if close {
            self.hide_popup(true); // Esc or a logged tally — return focus to where they were
        }
    }

    // --- settings persistence -----------------------------------------------

    /// The main window's current outer position + inner size, in physical pixels (matching
    /// what `resumed` feeds back in via `with_position`/`with_inner_size`). `None` before the
    /// window exists or if the platform can't report a position (matches
    /// `Settings::main_window_geometry`'s own doc: "`None` until shown and moved/resized").
    fn current_geometry(&self) -> Option<settings::WindowGeometry> {
        let w = self.main.as_ref()?.window();
        let pos = w.outer_position().ok()?;
        let size = w.inner_size();
        Some(settings::WindowGeometry { x: pos.x, y: pos.y, width: size.width, height: size.height })
    }

    /// Build a `Settings` snapshot from the app's current state.
    fn current_settings(&self) -> Settings {
        use simpletally_core::dates::to_sql;
        let mut s = self.last_saved_settings.clone();
        s.selected_date = Some(to_sql(self.today.selected_date));
        s.selected_category = match self.today.filter {
            crate::ui::today::CategoryFilter::All => None,
            crate::ui::today::CategoryFilter::One(id) => Some(id),
        };
        s.theme = self.theme_pref;
        s.range_kind = self.insights.kind.as_str().to_string();
        s.range_anchor = Some(to_sql(self.insights.anchor));
        s.custom_range_from = Some(self.insights.custom_from.clone());
        s.custom_range_to = Some(self.insights.custom_to.clone());
        s.active_tab = self.tab.as_str().to_string();
        s.note_field_open = self.today.note_open;
        s.main_window_geometry = self.current_geometry();
        s.hide_to_tray_notice_dismissed = self.hide_to_tray_notice_dismissed;
        s
    }

    /// Save if the built-from-state `Settings` differ from what's on disk AND the throttle
    /// has elapsed. Called once per event-loop iteration (`about_to_wait`) — cheap even at
    /// that frequency, since it's just a struct comparison until something actually changed.
    fn maybe_save_settings(&mut self) {
        if Instant::now().duration_since(self.last_save_at) < SETTINGS_SAVE_THROTTLE {
            return;
        }
        self.save_if_changed();
    }

    /// Save regardless of the throttle, but still only if something changed — used on the
    /// exits where losing the last couple of seconds of state would be user-visible (hide
    /// to tray, quit).
    fn flush_settings(&mut self) {
        self.save_if_changed();
    }

    fn save_if_changed(&mut self) {
        let current = self.current_settings();
        if current == self.last_saved_settings {
            return;
        }
        let Some(path) = self.settings_path.as_ref() else { return };
        match settings::save(&current, path) {
            Ok(()) => {
                self.last_saved_settings = current;
                self.last_save_at = Instant::now();
                self.settings_save_error_logged = false;
            }
            Err(e) => {
                // Stamp the failure too, or the throttle never engages: `last_saved_settings`
                // stays stale, so every loop iteration would see a difference and retry the
                // write. An unwritable install dir would then mean a failing file write per
                // frame, invisibly — the error log is one-shot.
                self.last_save_at = Instant::now();
                // Settings failure is not database failure (settings.rs module doc) — never
                // fatal, and logged at most once per failure streak so a persistently
                // unwritable path doesn't spam stderr every throttle tick.
                if !self.settings_save_error_logged {
                    eprintln!("warning: could not save settings to {}: {e}", path.display());
                    self.settings_save_error_logged = true;
                }
            }
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.main.is_some() {
            return;
        }

        let mut main_attrs = Window::default_attributes()
            .with_title("SimpleTally")
            .with_inner_size(LogicalSize::new(960.0, 720.0))
            .with_min_inner_size(LogicalSize::new(820.0, 560.0))
            .with_window_icon(Some(icon::window()))
            .with_visible(false);
        // Apply the saved geometry only if it still lands on a currently-attached monitor —
        // unplugging a second screen since the last run must not open the window offscreen
        // and unreachable.
        if let Some(g) = self.pending_geometry.take() {
            let saved = Rect { x: g.x, y: g.y, width: g.width, height: g.height };
            let monitors: Vec<Rect> = event_loop
                .available_monitors()
                .map(|m| {
                    let p = m.position();
                    let s = m.size();
                    Rect { x: p.x, y: p.y, width: s.width, height: s.height }
                })
                .collect();
            if geometry_fits_a_monitor(saved, &monitors) {
                main_attrs = main_attrs
                    .with_inner_size(PhysicalSize::new(g.width, g.height))
                    .with_position(PhysicalPosition::new(g.x, g.y));
            }
        }
        let main = Win::new(event_loop, main_attrs, &self.proxy);
        // Install fonts + theme on the main window's egui context, once, before first paint.
        crate::ui::theme::apply(main.egui_ctx(), &self.theme);
        self.main = Some(main);

        // The popup is a borderless, taskbar-hidden window (keeps it out of Alt-Tab).
        let popup_attrs = Window::default_attributes()
            .with_title("SimpleTally — Quick add")
            // Starting size only; `fit_popup` resizes to the result list on every summon.
            .with_inner_size(LogicalSize::new(
                crate::ui::quickadd::WIDTH as f64,
                crate::ui::quickadd::height_for(&self.quickadd) as f64,
            ))
            .with_decorations(false)
            .with_resizable(false)
            .with_visible(false)
            .with_skip_taskbar(true);
        let popup = Win::new(event_loop, popup_attrs, &self.proxy);
        crate::ui::theme::apply(popup.egui_ctx(), &self.theme);
        // skip_taskbar hides it from the taskbar but not Alt-Tab; make it a tool
        // window so it never shows there (PHASE0 acceptance).
        super::winos::exclude_from_alt_tab(popup.window());
        super::winos::round_corners(popup.window());
        self.popup = Some(popup);

        self.tray = Some(Tray::new(&self.proxy));
        match Hotkey::new(&self.proxy) {
            Ok(h) => self.hotkey = Some(h),
            Err(e) => {
                eprintln!("hotkey registration failed: {e}");
                self.hotkey_error = Some(e);
            }
        }

        // Decided once at startup (PLAN §1.5): only when the live DB was just created empty,
        // so a user with real data already beside the exe is never nagged.
        if self.auto_open_migrate {
            self.migrate.open_for_first_run();
        }

        // Paint the main window once while hidden (no cold-start flash), then surface
        // whatever this launch asked for. A --quick-add cold start shows the popup and
        // leaves main in the tray; otherwise show main.
        self.redraw_main();
        match self.startup {
            ActivationTarget::Main => self.show_main(),
            ActivationTarget::QuickAdd => self.show_popup(),
        }
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let is_main = self.main.as_ref().map(|w| w.id() == window_id).unwrap_or(false);
        let is_popup = self.popup.as_ref().map(|w| w.id() == window_id).unwrap_or(false);

        match &event {
            // Click-away: the popup dismisses when it loses focus (quick-add UX).
            WindowEvent::Focused(false) if is_popup => {
                // Let egui see the focus-loss first (it resets modifier/interaction
                // state); the repaint it may request is moot once hidden.
                if let Some(w) = self.popup.as_mut() {
                    let _ = w.on_window_event(&event);
                }
                self.hide_popup(false); // click-away — user chose a new foreground
                return;
            }
            WindowEvent::CloseRequested => {
                if is_main {
                    // First close (and every close until the user ticks "don't show again"):
                    // explain that this hides rather than quits, and hide only once the notice
                    // is dismissed. The notice is drawn in this window's own egui pass, so
                    // hiding first would show it to nobody.
                    if self.hide_to_tray_notice_dismissed {
                        self.hide_main();
                    } else {
                        self.tray_notice.maybe_open(false);
                        if let Some(w) = self.main.as_ref() {
                            w.window().request_redraw();
                        }
                    }
                } else if is_popup {
                    self.hide_popup(true);
                }
                return;
            }
            WindowEvent::RedrawRequested => {
                if is_main {
                    self.redraw_main();
                } else if is_popup {
                    self.redraw_popup();
                }
                return;
            }
            _ => {}
        }

        // Route input to the owning window; honour its repaint request.
        let repaint = if is_main {
            self.main.as_mut().map(|w| w.on_window_event(&event)).unwrap_or(false)
        } else if is_popup {
            self.popup.as_mut().map(|w| w.on_window_event(&event)).unwrap_or(false)
        } else {
            false
        };
        if repaint {
            if is_main {
                if let Some(w) = self.main.as_ref() {
                    w.window().request_redraw();
                }
            } else if is_popup {
                if let Some(w) = self.popup.as_ref() {
                    w.window().request_redraw();
                }
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Repaint { window, at } => {
                let now = Instant::now();
                for slot in [&mut self.main, &mut self.popup] {
                    if let Some(win) = slot.as_mut() {
                        if win.id() == window {
                            win.next_repaint =
                                Some(win.next_repaint.map_or(at, |prev| prev.min(at)));
                            // Only kick an immediate redraw for a visible window; hidden
                            // windows are painted only via explicit show paths.
                            if at <= now && win.is_visible() {
                                win.window().request_redraw();
                            }
                        }
                    }
                }
            }
            UserEvent::TrayMenu(ev) => {
                if let Some(tray) = self.tray.as_ref() {
                    if ev.id == tray.quit_id {
                        event_loop.exit();
                    } else if ev.id == tray.show_id {
                        self.show_main();
                    } else if ev.id == tray.migrate_id {
                        self.open_migrate_dialog();
                    }
                }
            }
            UserEvent::TrayIcon(ev) => {
                use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};
                let surface = matches!(
                    ev,
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } | TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    }
                );
                if surface {
                    self.show_main();
                }
            }
            UserEvent::Hotkey(ev) => {
                let ours = self.hotkey.as_ref().map(|h| h.id) == Some(ev.id);
                if ours && matches!(ev.state, global_hotkey::HotKeyState::Pressed) {
                    self.toggle_popup();
                }
            }
            UserEvent::Activate(target) => match target {
                super::event::ActivationTarget::Main => self.show_main(),
                super::event::ActivationTarget::QuickAdd => self.show_popup(),
            },
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let mut earliest: Option<Instant> = None;
        for slot in [&mut self.main, &mut self.popup] {
            if let Some(win) = slot.as_mut() {
                if !win.is_visible() {
                    win.next_repaint = None; // don't spin on a hidden window
                    continue;
                }
                if let Some(at) = win.next_repaint {
                    if at <= now {
                        win.window().request_redraw();
                        win.next_repaint = None;
                    } else {
                        earliest = Some(earliest.map_or(at, |e| e.min(at)));
                    }
                }
            }
        }
        event_loop.set_control_flow(match earliest {
            Some(at) => ControlFlow::WaitUntil(at),
            None => ControlFlow::Wait,
        });

        // Cheapest spot to check: runs once per loop iteration, not once per frame, and
        // the check itself is just a struct comparison until something actually changed.
        self.maybe_save_settings();
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Covers every quit route (tray "Quit", not just this one call site) — flush
        // unconditionally so the last few seconds of state aren't lost to the throttle.
        self.flush_settings();
        if let Some(w) = self.main.as_mut() {
            w.destroy();
        }
        if let Some(w) = self.popup.as_mut() {
            w.destroy();
        }
    }
}

#[cfg(test)]
mod settings_apply_tests {
    use super::*;
    use chrono::NaiveDate;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    // -- selected_date: the "never resume yesterday" rule --

    #[test]
    fn selected_date_of_today_is_restored() {
        let today = d(2026, 9, 18);
        let mut s = Settings::default();
        s.selected_date = Some("2026-09-18".to_string());
        assert_eq!(restore_from_settings(&s, today).selected_date, today);
    }

    #[test]
    fn selected_date_of_yesterday_falls_back_to_today() {
        let today = d(2026, 9, 18);
        let mut s = Settings::default();
        s.selected_date = Some("2026-09-17".to_string());
        assert_eq!(restore_from_settings(&s, today).selected_date, today);
    }

    #[test]
    fn missing_or_garbage_selected_date_falls_back_to_today_without_panicking() {
        let today = d(2026, 9, 18);
        let mut s = Settings::default();
        s.selected_date = None;
        assert_eq!(restore_from_settings(&s, today).selected_date, today);

        s.selected_date = Some("not-a-date".to_string());
        assert_eq!(restore_from_settings(&s, today).selected_date, today);
    }

    // -- garbage values fall back instead of panicking --

    #[test]
    fn unknown_tab_and_range_kind_fall_back_to_defaults() {
        let today = d(2026, 9, 18);
        let mut s = Settings::default();
        s.active_tab = "made_up".to_string();
        s.range_kind = "made_up".to_string();
        s.range_anchor = Some("garbage".to_string());
        let r = restore_from_settings(&s, today);
        assert_eq!(r.tab, crate::ui::chrome::Tab::Today);
        assert_eq!(r.range_kind, crate::ui::insights::RangeKind::Week);
        assert_eq!(r.range_anchor, today); // unparseable anchor falls back to today
    }

    #[test]
    fn category_filter_maps_none_to_all_and_some_to_one() {
        let today = d(2026, 9, 18);
        let mut s = Settings::default();
        s.selected_category = None;
        assert_eq!(restore_from_settings(&s, today).filter, crate::ui::today::CategoryFilter::All);
        s.selected_category = Some(42);
        assert_eq!(
            restore_from_settings(&s, today).filter,
            crate::ui::today::CategoryFilter::One(42)
        );
    }

    #[test]
    fn theme_maps_light_dark_to_some_and_system_to_none() {
        let today = d(2026, 9, 18);
        let mut s = Settings::default();
        s.theme = settings::Theme::Light;
        assert_eq!(restore_from_settings(&s, today).theme_mode, Some(crate::ui::theme::Mode::Light));
        s.theme = settings::Theme::Dark;
        assert_eq!(restore_from_settings(&s, today).theme_mode, Some(crate::ui::theme::Mode::Dark));
        s.theme = settings::Theme::System;
        assert_eq!(restore_from_settings(&s, today).theme_mode, None);
    }

    // -- monitor-intersection test --

    fn r(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, width: w, height: h }
    }

    #[test]
    fn geometry_fully_inside_one_monitor_fits() {
        let saved = r(100, 100, 800, 600);
        let monitors = [r(0, 0, 1920, 1080)];
        assert!(geometry_fits_a_monitor(saved, &monitors));
    }

    #[test]
    fn geometry_straddling_two_monitors_fits() {
        // Second monitor starts where the first ends; the saved window straddles the seam.
        let saved = r(1800, 100, 400, 300);
        let monitors = [r(0, 0, 1920, 1080), r(1920, 0, 1920, 1080)];
        assert!(geometry_fits_a_monitor(saved, &monitors));
    }

    #[test]
    fn geometry_entirely_offscreen_does_not_fit() {
        let saved = r(5000, 5000, 800, 600);
        let monitors = [r(0, 0, 1920, 1080)];
        assert!(!geometry_fits_a_monitor(saved, &monitors));
    }

    #[test]
    fn empty_monitor_list_keeps_geometry_as_is() {
        let saved = r(5000, 5000, 800, 600);
        assert!(geometry_fits_a_monitor(saved, &[]));
    }
}
