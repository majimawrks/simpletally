//! The Insights daily-activity chart (design `_rustrefactor/README.md` §04, PLAN §5
//! "Insights, reweighted"): bars for ranges of 31 days or fewer, a line for longer ranges —
//! zero-day stubs so weekends read as gaps rather than empty bars.

use crate::ui::theme::{self as t, Theme};
use chrono::{Datelike, NaiveDate};

const HEIGHT: f32 = 168.0;
const BAR_CEILING: f32 = 138.0;
const BAR_GAP: f32 = 5.0;

/// `17 Aug` — built manually; chrono's `%-d` isn't portable on Windows (see `today.rs`).
/// `pub(crate)` so `insights.rs` can reuse it for date labels outside the chart.
pub(crate) fn day_month(d: NaiveDate) -> String {
    format!("{} {}", d.day(), d.format("%b"))
}

/// Renders the chart into the full width of `ui` at [`HEIGHT`]. `days` is the zero-filled
/// `(date, count)` spine for the range, in order.
pub fn daily_activity(ui: &mut egui::Ui, theme: &Theme, days: &[(NaiveDate, i64)]) {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), HEIGHT), egui::Sense::hover());
    if days.is_empty() {
        return;
    }
    let max = days.iter().map(|(_, c)| *c).max().unwrap_or(0).max(1);

    if days.len() <= 31 {
        bars(ui, theme, rect, days, max, &resp);
    } else {
        line(ui, theme, rect, days, max, &resp);
    }
}

fn bars(
    ui: &mut egui::Ui,
    theme: &Theme,
    rect: egui::Rect,
    days: &[(NaiveDate, i64)],
    max: i64,
    resp: &egui::Response,
) {
    let n = days.len() as f32;
    let baseline = rect.bottom() - 18.0; // room for the day-number label below
    let bar_area_top = rect.top();
    let bar_w = ((rect.width() - BAR_GAP * (n - 1.0)) / n).max(1.0);

    // Thin day-number labels so they don't collide: show every Kth label so each kept
    // label's slot is at least ~16px wide.
    let label_stride = ((16.0 / (bar_w + BAR_GAP)).ceil() as usize).max(1);

    let mut hovered: Option<(NaiveDate, i64)> = None;
    for (i, (date, count)) in days.iter().enumerate() {
        let x0 = rect.left() + i as f32 * (bar_w + BAR_GAP);
        let x1 = x0 + bar_w;
        let h = if *count == 0 {
            2.0
        } else {
            (*count as f32 / max as f32 * BAR_CEILING).max(2.0)
        };
        let bar_rect = egui::Rect::from_min_max(
            egui::pos2(x0, baseline - h),
            egui::pos2(x1, baseline),
        );
        let color = if *count == 0 {
            theme.border_subtle
        } else if *count >= 18 {
            theme.accent
        } else {
            theme.accent_muted_bar
        };
        // Radius on the top corners only: paint a fully-rounded rect, then square off the
        // bottom by overpainting a flat rect over the bottom `radius` slice.
        let radius = 3.0_f32.min(h / 2.0);
        ui.painter().rect_filled(bar_rect, egui::CornerRadius::same(radius as u8), color);
        if radius > 0.0 {
            let squared_bottom = egui::Rect::from_min_max(
                egui::pos2(bar_rect.left(), bar_rect.bottom() - radius),
                bar_rect.max,
            );
            ui.painter().rect_filled(squared_bottom, egui::CornerRadius::ZERO, color);
        }

        if i % label_stride == 0 {
            ui.painter().text(
                egui::pos2((x0 + x1) / 2.0, baseline + 4.0),
                egui::Align2::CENTER_TOP,
                date.day().to_string(),
                t::mono(9.5),
                theme.text_quiet,
            );
        }

        if let Some(pos) = resp.hover_pos() {
            let hit = egui::Rect::from_min_max(
                egui::pos2(x0, bar_area_top),
                egui::pos2(x1, baseline),
            );
            if hit.contains(pos) {
                hovered = Some((*date, *count));
            }
        }
    }

    if let Some((date, count)) = hovered {
        resp.clone()
            .on_hover_text(format!("{} — {count}", day_month(date)));
    }
}

fn line(
    ui: &mut egui::Ui,
    theme: &Theme,
    rect: egui::Rect,
    days: &[(NaiveDate, i64)],
    max: i64,
    resp: &egui::Response,
) {
    let n = days.len();
    let baseline = rect.bottom() - 18.0;
    let top = rect.top();
    let step = if n > 1 { rect.width() / (n - 1) as f32 } else { 0.0 };

    let points: Vec<egui::Pos2> = days
        .iter()
        .enumerate()
        .map(|(i, (_, count))| {
            let x = rect.left() + i as f32 * step;
            let h = (*count as f32 / max as f32 * BAR_CEILING).max(0.0);
            egui::pos2(x, baseline - h)
        })
        .collect();
    ui.painter().add(egui::Shape::line(points.clone(), egui::Stroke::new(1.5, theme.accent)));

    // Label first / middle / last dates.
    let label_idxs = [0, n / 2, n - 1];
    for &i in &label_idxs {
        if let Some((date, _)) = days.get(i) {
            ui.painter().text(
                egui::pos2(points[i].x, baseline + 4.0),
                egui::Align2::CENTER_TOP,
                day_month(*date),
                t::mono(9.5),
                theme.text_quiet,
            );
        }
    }

    if let Some(pos) = resp.hover_pos() {
        // Nearest point by x-distance, within the plotted vertical band.
        if pos.y >= top && pos.y <= baseline {
            if let Some((idx, _)) = points
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| (a.x - pos.x).abs().total_cmp(&(b.x - pos.x).abs()))
            {
                if (points[idx].x - pos.x).abs() <= step.max(8.0) / 2.0 {
                    let (date, count) = days[idx];
                    resp.clone()
                        .on_hover_text(format!("{} — {count}", day_month(date)));
                }
            }
        }
    }
}

/// The 12px-tall stacked ratio bar over the ranked type totals (design §04 "By category"):
/// 3px radius per segment, 2px gaps between segments, proportional widths. `segments` is
/// `(color, total)` in rank order; zero-total segments are skipped.
pub fn stacked_ratio_bar(ui: &mut egui::Ui, segments: &[(egui::Color32, i64)]) {
    const H: f32 = 12.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), H), egui::Sense::hover());
    let total: i64 = segments.iter().map(|(_, n)| *n).sum();
    if total <= 0 {
        return;
    }
    let gap = 2.0;
    let nonzero = segments.iter().filter(|(_, n)| *n > 0).count();
    let usable = rect.width() - gap * nonzero.saturating_sub(1) as f32;
    let mut x = rect.left();
    for (color, n) in segments {
        if *n <= 0 {
            continue;
        }
        let w = usable * (*n as f32 / total as f32);
        let seg = egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(w, H));
        ui.painter().rect_filled(seg, egui::CornerRadius::same(3), *color);
        x += w + gap;
    }
}
