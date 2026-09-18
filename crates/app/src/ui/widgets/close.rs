//! The dialog close control — one widget, every dialog.
//!
//! It used to be a text `\u{d7}` at caption size: small, lighter than the title beside it, and
//! (as `\u{2715}`) a tofu box in the bundled faces. This paints the mark instead, at the weight
//! and hit size a Windows title-bar close has, so it reads as a close button rather than a
//! stray multiplication sign.

use crate::ui::theme::Theme;

/// Hit area, matching the 28px affordance Windows dialogs use at 100% scale.
pub const SIZE: f32 = 28.0;

/// A close button: a painted Lucide `x` in a square hit area that tints on hover. Allocates
/// [`SIZE`] square in the current layout; place it with `Layout::right_to_left` or by drawing
/// into an explicit rect.
pub fn close_button(ui: &mut egui::Ui, theme: &Theme) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(SIZE, SIZE), egui::Sense::click());
    paint(ui.painter(), rect, theme, resp.hovered());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// As [`close_button`], but at a caller-chosen rect — for headers painted rather than laid out.
pub fn close_button_at(
    ui: &mut egui::Ui,
    theme: &Theme,
    rect: egui::Rect,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
) -> egui::Response {
    let resp = ui.interact(rect, egui::Id::new(("close_button", id_salt)), egui::Sense::click());
    paint(ui.painter(), rect, theme, resp.hovered());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// Lucide `x` (ISC): on a 24x24 grid, `M18 6 6 18` and `m6 6 12 12`. Drawn at 40% of the hit
/// box so the mark stays about 11px on a 28px button, which is the Windows proportion.
fn paint(p: &egui::Painter, rect: egui::Rect, theme: &Theme, hovered: bool) {
    if hovered {
        p.rect_filled(rect, egui::CornerRadius::same(6), theme.bg_sunken);
    }
    let color = if hovered { theme.text_primary } else { theme.text_secondary };
    let r = rect.width().min(rect.height()) * 0.20;
    let c = rect.center();
    let stroke = egui::Stroke::new(1.5, color);
    p.line_segment([egui::pos2(c.x - r, c.y - r), egui::pos2(c.x + r, c.y + r)], stroke);
    p.line_segment([egui::pos2(c.x + r, c.y - r), egui::pos2(c.x - r, c.y + r)], stroke);
}
