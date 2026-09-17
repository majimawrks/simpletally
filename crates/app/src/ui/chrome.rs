//! The tab strip below the title bar (design `_rustrefactor/README.md` §"Window shell").
//!
//! Only the tab strip lives here — no Backup button (out of scope for Phase 3A) and no
//! settings persistence yet (tab state is transient, PLAN §5 "Insights, reweighted").

use crate::ui::theme::{self as t, Theme};

/// Which of the three screens is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Today,
    Insights,
    TaskTypes,
}

/// Renders the tab strip and updates `active` on a click. Row below the title bar: 10px top
/// padding, 22px horizontal padding. Returns the strip's bottom edge (in `ui`'s coordinate
/// space) so the caller can hand the screen below it the remaining rect.
pub fn tab_strip(ui: &mut egui::Ui, theme: &Theme, active: &mut Tab) -> f32 {
    egui::Frame::default()
        .fill(theme.bg_canvas)
        .inner_margin(egui::Margin { left: 22, right: 22, top: 10, bottom: 0i8 })
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                for (tab, label) in [
                    (Tab::Today, "Today"),
                    (Tab::Insights, "Insights"),
                    (Tab::TaskTypes, "Task types"),
                ] {
                    if tab_button(ui, theme, label, *active == tab).clicked() {
                        *active = tab;
                    }
                    ui.add_space(4.0);
                }
                if *active == Tab::Today {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new("Ctrl+Shift+T quick add")
                                .font(t::mono(11.5))
                                .color(theme.text_tertiary),
                        );
                    });
                }
            });
        })
        .response
        .rect
        .bottom()
}

/// One tab button: 8px/16px padding, 6px radius, 13.5px. Active gets `bg_tab_active` fill +
/// `text_primary` + sans_medium; inactive is `text_secondary`, no fill.
fn tab_button(ui: &mut egui::Ui, theme: &Theme, label: &str, active: bool) -> egui::Response {
    let font = if active { t::sans_medium(t::TAB) } else { t::sans(t::TAB) };
    let color = if active { theme.text_primary } else { theme.text_secondary };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, color);

    let pad_x = 16.0;
    let pad_y = 8.0;
    let size = egui::vec2(galley.rect.width() + pad_x * 2.0, galley.rect.height() + pad_y * 2.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());

    if active {
        ui.painter().rect(
            rect,
            egui::CornerRadius::same(6),
            theme.bg_tab_active,
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
    resp
}
