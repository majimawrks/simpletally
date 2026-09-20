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

impl Tab {
    /// The string persisted in `Settings::active_tab`.
    pub fn as_str(self) -> &'static str {
        match self {
            Tab::Today => "today",
            Tab::Insights => "insights",
            Tab::TaskTypes => "types",
        }
    }

    /// Inverse of [`Tab::as_str`]. An unrecognised value (a hand-edited or older-version
    /// settings file) falls back to the default tab rather than failing to load.
    pub fn from_str(s: &str) -> Tab {
        match s {
            "insights" => Tab::Insights,
            "types" => Tab::TaskTypes,
            _ => Tab::Today,
        }
    }
}

/// The outcome of drawing the tab strip for one frame.
pub struct TabStrip {
    /// The strip's bottom edge (in `ui`'s coordinate space), so the caller can hand the screen
    /// below it the remaining rect.
    pub bottom: f32,
    /// The info glyph on the right was clicked — open the About dialog.
    pub about_clicked: bool,
}

/// Renders the tab strip and updates `active` on a click. Row below the title bar: 10px top
/// padding, 22px horizontal padding. The right end always carries an info glyph (About), with
/// the quick-add hint to its left on the Today tab.
pub fn tab_strip(ui: &mut egui::Ui, theme: &Theme, active: &mut Tab) -> TabStrip {
    let mut about_clicked = false;
    let bottom = egui::Frame::default()
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
                // Right end (added right-to-left, so the glyph is rightmost): the About glyph,
                // then the quick-add hint on Today.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    about_clicked = about_glyph(ui, theme).clicked();
                    if *active == Tab::Today {
                        ui.add_space(12.0);
                        ui.label(
                            egui::RichText::new("Ctrl+Shift+T quick add")
                                .font(t::mono(11.5))
                                .color(theme.text_tertiary),
                        );
                    }
                });
            });
        })
        .response
        .rect
        .bottom();
    TabStrip { bottom, about_clicked }
}

/// The info-circle glyph that opens the About dialog: a painted `(i)`, quiet in `text_tertiary`,
/// tinting to accent on hover. Painted, not a font glyph — the bundled fonts have no symbol set.
fn about_glyph(ui: &mut egui::Ui, theme: &Theme) -> egui::Response {
    let d = 26.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(d, d), egui::Sense::click());
    let color = if resp.hovered() { theme.accent } else { theme.text_tertiary };
    let c = rect.center();
    let p = ui.painter();
    p.circle(c, 9.0, egui::Color32::TRANSPARENT, egui::Stroke::new(1.5, color));
    // The dot of the 'i'.
    p.circle_filled(egui::pos2(c.x, c.y - 4.0), 1.3, color);
    // The stem.
    p.line_segment(
        [egui::pos2(c.x, c.y - 1.0), egui::pos2(c.x, c.y + 4.5)],
        egui::Stroke::new(1.7, color),
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.on_hover_text("About SimpleTally")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_string_round_trips() {
        for t in [Tab::Today, Tab::Insights, Tab::TaskTypes] {
            assert_eq!(Tab::from_str(t.as_str()), t);
        }
    }

    #[test]
    fn tab_from_str_falls_back_to_today_on_unknown() {
        assert_eq!(Tab::from_str("nonsense"), Tab::Today);
        assert_eq!(Tab::from_str(""), Tab::Today);
    }
}
