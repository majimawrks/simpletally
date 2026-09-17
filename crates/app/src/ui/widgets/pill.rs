//! Pill widgets: the fully-rounded category filter chips and the day-navigation pills
//! (design `_rustrefactor/README.md` §Screen 01 → category pill row / day nav).

use crate::ui::theme::{self as t, Theme};

/// A category filter chip: `Name  count`, fully rounded (design radius 999px). Selected uses
/// the accent-tinted pill tokens; unselected is a raised chip. A zero count shows an em dash.
/// Returns the click response.
pub fn category_pill(
    ui: &mut egui::Ui,
    theme: &Theme,
    name: &str,
    count: i64,
    selected: bool,
) -> egui::Response {
    let (bg, border, text_col, count_col) = if selected {
        (theme.pill_sel_bg, theme.pill_sel_border, theme.pill_sel_text, theme.pill_sel_count)
    } else {
        (theme.bg_raised, theme.border_strong, theme.text_secondary, theme.text_disabled)
    };

    let count_str = if count == 0 { "—".to_string() } else { count.to_string() };
    let name_galley = ui.painter().layout_no_wrap(name.to_owned(), t::sans(t::PILL_TEXT), text_col);
    let count_galley =
        ui.painter().layout_no_wrap(count_str, t::mono(t::PILL_COUNT), count_col);

    let pad_x = 16.0;
    let gap = 8.0;
    let h = 30.0;
    let w = pad_x * 2.0 + name_galley.rect.width() + gap + count_galley.rect.width();

    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::click());
    let border_c = if resp.hovered() && !selected { theme.accent_tint_border } else { border };
    ui.painter().rect(
        rect,
        egui::CornerRadius::same((h / 2.0) as u8),
        bg,
        egui::Stroke::new(1.0, border_c),
        egui::StrokeKind::Inside,
    );

    let cy = rect.center().y;
    let name_pos = egui::pos2(rect.left() + pad_x, cy - name_galley.rect.height() / 2.0);
    let count_pos = egui::pos2(
        rect.right() - pad_x - count_galley.rect.width(),
        cy - count_galley.rect.height() / 2.0,
    );
    ui.painter().galley(name_pos, name_galley, text_col);
    ui.painter().galley(count_pos, count_galley, count_col);

    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// A day-navigation pill (`‹ Prev`, `Today`, `Next ›`), 6px radius. When `enabled` is false it
/// renders in the disabled tokens and does not report clicks (e.g. "Next" on the current day).
pub fn nav_pill(ui: &mut egui::Ui, theme: &Theme, label: &str, enabled: bool) -> egui::Response {
    let (bg, border, col) = if enabled {
        (theme.bg_raised, theme.border_strong, theme.text_body)
    } else {
        (theme.bg_sunken, theme.border, theme.text_disabled)
    };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), t::sans(t::DAY_NAV), col);
    let pad_x = 14.0;
    let h = 30.0;
    let w = pad_x * 2.0 + galley.rect.width();

    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, h), sense);
    let border_c = if enabled && resp.hovered() { theme.accent } else { border };
    ui.painter().rect(
        rect,
        egui::CornerRadius::same(6),
        bg,
        egui::Stroke::new(1.0, border_c),
        egui::StrokeKind::Inside,
    );
    let pos = egui::pos2(
        rect.center().x - galley.rect.width() / 2.0,
        rect.center().y - galley.rect.height() / 2.0,
    );
    ui.painter().galley(pos, galley, col);
    if enabled && resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}
