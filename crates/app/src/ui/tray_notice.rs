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

/// Interaction state for the notice. Both toggles are lifted into `settings.toml` by the
/// caller — this module never touches the filesystem.
#[derive(Default)]
pub struct TrayNoticeState {
    pub visible: bool,
    /// The checkbox's current value, mirrored back into settings when the notice closes.
    pub dont_show_again: bool,
    /// The switch: make `[x]` quit outright instead of hiding. Seeded from settings each time
    /// the notice opens so it shows the live value, not whatever it was last time.
    pub close_quits: bool,
}

impl TrayNoticeState {
    /// Open it, unless the user has already said not to. Called when the main window is closed
    /// by its `[x]` — not by the tray menu's own Hide, where nothing needs explaining.
    pub fn open(&mut self, close_quits: bool) {
        self.close_quits = close_quits;
        self.visible = true;
    }
}

/// What the notice concluded this frame.
pub enum Action {
    None,
    /// Dismissed. `remember` is the checkbox; `close_quits` is the switch's final value, which
    /// the caller both persists and **acts on now** — the user pressed `[x]` and then said what
    /// `[x]` means, so honouring it immediately is the consistent reading.
    Closed { remember: bool, close_quits: bool },
}

/// Draw the notice if it's open. Returns [`Action::Closed`] on the frame it is dismissed.
pub fn show(ui: &mut egui::Ui, state: &mut TrayNoticeState, theme: &Theme) -> Action {
    if !state.visible {
        return Action::None;
    }
    let mut closed = false;

    let resp = egui::Modal::new(egui::Id::new("hide_to_tray_notice")).show(ui.ctx(), |ui| {
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
        ui.add_space(12.0);

        // The preference, offered where the behaviour is being explained rather than buried in
        // a settings screen the user has no reason to open (PLAN §3, Phase 5 addition).
        let switched = close_quits_switch(ui, theme, &mut state.close_quits);
        if switched && state.close_quits {
            // Nothing left to explain once `[x]` does the obvious thing, so stop offering to
            // suppress a notice that will no longer appear.
            state.dont_show_again = false;
        }
        ui.add_space(12.0);

        ui.horizontal(|ui| {
            // Same trick as the toolbar: without this the checkbox is only its own text tall
            // and sits above the button beside it.
            ui.spacing_mut().interact_size.y = 32.0;
            ui.add_enabled_ui(!state.close_quits, |ui| {
                ui.checkbox(
                    &mut state.dont_show_again,
                    egui::RichText::new("Don't show this again")
                        .font(t::sans(t::BODY))
                        .color(theme.text_body),
                );
            });

            // Measured right-alignment, not a nested `right_to_left` — see the LAYOUT TRAP
            // note in `types.rs`.
            // The label follows the switch: with it on, this button ends the process, and that
            // must be legible before the click rather than a surprise after it.
            let label = if state.close_quits { "Quit SimpleTally" } else { "Got it" };
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

    // Esc / click-away dismisses like "Got it": honour the current switch and remember state.
    if closed || resp.should_close() {
        state.visible = false;
        return Action::Closed {
            remember: state.dont_show_again,
            close_quits: state.close_quits,
        };
    }
    Action::None
}

/// The `[x]` behaviour switch: a painted pill track + knob, matching the Active switch on the
/// edit-type dialog rather than egui's default checkbox, since this is a behaviour toggle and
/// not a form field. Returns whether it was flipped this frame.
fn close_quits_switch(ui: &mut egui::Ui, theme: &Theme, on: &mut bool) -> bool {
    let full_w = ui.available_width();
    let block_h = 46.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(full_w, block_h), egui::Sense::click());
    ui.painter().rect(
        rect,
        egui::CornerRadius::same(6),
        theme.bg_sunken,
        egui::Stroke::NONE,
        egui::StrokeKind::Inside,
    );

    let track_w = 36.0;
    let track_h = 20.0;
    let track = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 12.0, rect.center().y - track_h / 2.0),
        egui::vec2(track_w, track_h),
    );
    let anim = ui.ctx().animate_bool_with_time(ui.id().with("close_quits_switch"), *on, 0.12);
    // Off state has to look like a control before it looks like a state: a lighter-than-any-
    // surface track, a blue outline, and a near-white knob. Drawn from surface/border tokens
    // it was invisible — dark track, dark knob, no edge.
    ui.painter().rect(
        track,
        egui::CornerRadius::same((track_h / 2.0) as u8),
        if *on { theme.accent } else { theme.switch_off_track },
        if *on { egui::Stroke::NONE } else { egui::Stroke::new(1.0, theme.secondary) },
        egui::StrokeKind::Inside,
    );
    let knob_r = track_h / 2.0 - 2.0;
    let knob_x = track.left() + knob_r + 2.0 + anim * (track_w - knob_r * 2.0 - 4.0);
    ui.painter().circle_filled(egui::pos2(knob_x, track.center().y), knob_r, theme.switch_knob);

    // Both lines describe the CURRENT state, not the thing the switch would do. A fixed title
    // reading "Closing quits SimpleTally" above an off switch asserts the opposite of the
    // truth, which is worse than no label.
    let (title, detail) = if *on {
        ("Close button exits the app", "Ctrl+Shift+T quick add stops working while it is closed")
    } else {
        ("Close button minimises to tray", "Keeps running so Ctrl+Shift+T quick add still works")
    };
    let text_x = track.right() + 10.0;
    ui.painter().text(
        egui::pos2(text_x, rect.top() + 9.0),
        egui::Align2::LEFT_TOP,
        title,
        t::sans_medium(t::BODY),
        theme.text_primary,
    );
    ui.painter().text(
        egui::pos2(text_x, rect.top() + 26.0),
        egui::Align2::LEFT_TOP,
        detail,
        t::sans(t::CAPTION),
        theme.text_quiet,
    );

    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if resp.clicked() {
        *on = !*on;
        return true;
    }
    false
}
