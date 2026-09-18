//! A row of equal-width stat cards, shared by the CSV import report and the backup dialog.

use crate::ui::theme::{self as t, Theme};

/// A row of equal-width stat cards: a number over a mono eyebrow label. `hero` gives the card
/// the accent number and a bigger type size — used for the one figure that answers the
/// question the dialog exists to answer.
///
/// A zero is drawn in `text_disabled` rather than hidden: "0 malformed" is reassurance, and a
/// card that disappears would move the ones beside it between imports.
pub fn stat_row(ui: &mut egui::Ui, theme: &Theme, width: f32, cards: &[(i64, &str, bool)]) {
    const GAP: f32 = 8.0;
    const PAD: f32 = 12.0;
    /// Between the number's baseline block and the label under it.
    const LEAD: f32 = 6.0;

    let hero = cards.iter().any(|(_, _, h)| *h);
    let value_font = t::mono_medium(if hero { 22.0 } else { 17.0 });
    let label_font = t::mono(t::EYEBROW);

    // Measure, never assume. The first version hard-coded the card height and positioned the
    // label from the bottom edge, so at 48px the number and its label overlapped outright.
    let probe = |font: egui::FontId| {
        ui.painter().layout_no_wrap("0".to_owned(), font, egui::Color32::PLACEHOLDER).rect.height()
    };
    let value_h = probe(value_font.clone());
    let label_h = probe(label_font.clone());
    let height = PAD * 2.0 + value_h + LEAD + label_h;

    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let n = cards.len().max(1) as f32;
    let card_w = (width - GAP * (n - 1.0)) / n;

    for (i, (value, label, is_hero)) in cards.iter().enumerate() {
        let card = egui::Rect::from_min_size(
            egui::pos2(rect.left() + i as f32 * (card_w + GAP), rect.top()),
            egui::vec2(card_w, height),
        );
        let p = ui.painter();
        p.rect(
            card,
            egui::CornerRadius::same(8),
            theme.bg_sunken,
            egui::Stroke::new(1.0, theme.border_subtle),
            egui::StrokeKind::Inside,
        );
        let value_color = if *value == 0 {
            theme.text_disabled
        } else if *is_hero {
            theme.accent
        } else {
            theme.text_primary
        };
        p.text(
            egui::pos2(card.left() + PAD, card.top() + PAD),
            egui::Align2::LEFT_TOP,
            value.to_string(),
            value_font.clone(),
            value_color,
        );
        p.text(
            egui::pos2(card.left() + PAD, card.top() + PAD + value_h + LEAD),
            egui::Align2::LEFT_TOP,
            *label,
            label_font.clone(),
            theme.text_tertiary,
        );
    }
}
