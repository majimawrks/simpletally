//! The 正 ("correct"/tally) mark, painted as five strokes rather than drawn as text — the
//! bundled fonts (DM Sans, JetBrains Mono) have no CJK coverage, so the glyph would render as
//! tofu if drawn with `painter.text`.

/// Paints 正 inside `rect` (with a little padding), stroke by stroke:
/// top bar, centre vertical, a left-half middle bar, a lower-left vertical, bottom bar.
pub fn zheng(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32, stroke_width: f32) {
    let pad = rect.width().min(rect.height()) * 0.18;
    let r = rect.shrink(pad);
    let stroke = egui::Stroke::new(stroke_width, color);

    let mid_x = r.center().x;
    let mid_y = r.center().y;

    // 1. Top horizontal, full width.
    painter.line_segment([egui::pos2(r.left(), r.top()), egui::pos2(r.right(), r.top())], stroke);
    // 2. Centre vertical, top to bottom.
    painter.line_segment([egui::pos2(mid_x, r.top()), egui::pos2(mid_x, r.bottom())], stroke);
    // 3. Middle horizontal, left half only.
    painter.line_segment([egui::pos2(r.left(), mid_y), egui::pos2(mid_x, mid_y)], stroke);
    // 4. Left vertical, from the middle horizontal down to the bottom.
    painter.line_segment([egui::pos2(r.left(), mid_y), egui::pos2(r.left(), r.bottom())], stroke);
    // 5. Bottom horizontal, full width.
    painter.line_segment([egui::pos2(r.left(), r.bottom()), egui::pos2(r.right(), r.bottom())], stroke);
}
