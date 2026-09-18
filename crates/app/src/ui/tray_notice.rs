//! The one-time "closing hides to the tray" notice.
//!
//! `[x]` hides the main window and leaves the app running (BACKLOG: making that a preference
//! needs a Preferences window, which doesn't exist yet). Without a word of explanation that is
//! indistinguishable from the app having quit — or worse, from it having crashed — and the
//! user's next move is to launch it again, which the single-instance guard turns into "nothing
//! happened". So it explains itself once, and never again after that.
//!
//! It is shown *before* the window hides, and the hide happens when the notice is dismissed.
//! Not for consent — the user already pressed `[x]` and that is not in question — but because
//! this modal is drawn in the main window's egui pass, and a hidden window paints nothing. An
//! explanation nobody can see is not an explanation.
//!
//! It keeps appearing on every close until "Don't show this again" is ticked. That is the
//! point of the checkbox: the dismissal is the user's to make, not something inferred from
//! their having read it once.

use crate::ui::theme::{self as t, Theme};

/// Interaction state for the notice. `dismissed_forever` is lifted into `settings.toml` by the
/// caller — this module never touches the filesystem.
#[derive(Default)]
pub struct TrayNoticeState {
    pub visible: bool,
    /// The checkbox's current value, mirrored back into settings when the notice closes.
    pub dont_show_again: bool,
}

impl TrayNoticeState {
    /// Open it, unless the user has already said not to. Called when the main window is hidden
    /// by the close button — not by the tray menu's own Hide, where nothing needs explaining.
    pub fn maybe_open(&mut self, already_dismissed: bool) {
        if !already_dismissed {
            self.visible = true;
        }
    }
}

/// What the notice concluded this frame.
pub enum Action {
    None,
    /// Closed. `remember` is the checkbox: persist it as "never show again".
    Closed { remember: bool },
}

/// Draw the notice if it's open. Returns [`Action::Closed`] on the frame it is dismissed.
pub fn show(ui: &mut egui::Ui, state: &mut TrayNoticeState, theme: &Theme) -> Action {
    if !state.visible {
        return Action::None;
    }
    let mut closed = false;

    egui::Modal::new(egui::Id::new("hide_to_tray_notice")).show(ui.ctx(), |ui| {
        const W: f32 = 400.0;
        ui.set_width(W);
        ui.allocate_exact_size(egui::vec2(W, 0.0), egui::Sense::hover());

        ui.label(
            egui::RichText::new("SimpleTally is still running")
                .font(t::sans_medium(t::SECTION_TITLE))
                .color(theme.text_primary),
        );
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(
                "Closing the window hides it to the notification area — the app keeps running \
                 so the Ctrl+Shift+T quick add still works. To stop it completely, right-click \
                 its tray icon and choose Quit.",
            )
            .font(t::sans(t::BODY))
            .color(theme.text_body),
        );
        ui.add_space(14.0);

        ui.horizontal(|ui| {
            // Same trick as the toolbar: without this the checkbox is only its own text tall
            // and sits above the button beside it.
            ui.spacing_mut().interact_size.y = 32.0;
            ui.checkbox(
                &mut state.dont_show_again,
                egui::RichText::new("Don't show this again")
                    .font(t::sans(t::BODY))
                    .color(theme.text_body),
            );

            // Measured right-alignment, not a nested `right_to_left` — see the LAYOUT TRAP
            // note in `types.rs`.
            let label = "Got it";
            let btn_w = ui
                .painter()
                .layout_no_wrap(label.to_owned(), t::sans_medium(t::BODY), egui::Color32::PLACEHOLDER)
                .rect
                .width()
                + 32.0;
            let free = ui.max_rect().right() - ui.cursor().left() - btn_w;
            ui.add_space(free.max(8.0));
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new(label).font(t::sans_medium(t::BODY)).color(theme.bg_raised),
                    )
                    .fill(theme.accent)
                    .corner_radius(6)
                    .min_size(egui::vec2(btn_w, 32.0)),
                )
                .clicked()
            {
                closed = true;
            }
        });
    });

    if closed {
        state.visible = false;
        return Action::Closed { remember: state.dont_show_again };
    }
    Action::None
}
