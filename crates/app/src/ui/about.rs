//! The "About SimpleTally" dialog, opened from the info glyph in the tab strip.
//!
//! Same shape as the other modal modules (`backup`, `tray_notice`): owns its interaction state
//! and a `show` that draws it. The app icon shown here is the real `assets/icon.ico`, decoded
//! once by `platform::icon` and uploaded to an egui texture the first time the dialog opens —
//! not a redrawn placeholder.

use crate::ui::theme::{self as t, Theme};

/// The repository the GitHub row links to.
const GITHUB_URL: &str = "https://github.com/majimawrks/simpletally";
/// The text shown for that link (the URL without its scheme).
const GITHUB_LABEL: &str = "github.com/majimawrks/simpletally";

pub struct AboutState {
    visible: bool,
    /// The app icon, loaded lazily on first open and kept for the session.
    icon: Option<egui::TextureHandle>,
    /// The GitHub mark (white; tinted at draw time), loaded lazily alongside the app icon.
    github: Option<egui::TextureHandle>,
}

impl AboutState {
    pub fn new() -> Self {
        Self { visible: false, icon: None, github: None }
    }

    pub fn open(&mut self) {
        self.visible = true;
    }
}

impl Default for AboutState {
    fn default() -> Self {
        Self::new()
    }
}

/// Draw the dialog if it's open.
pub fn show(ui: &mut egui::Ui, state: &mut AboutState, theme: &Theme) {
    if !state.visible {
        return;
    }

    // Upload the .ico assets to textures the first time we need them.
    if state.icon.is_none() {
        let d = crate::platform::icon::decode();
        let img =
            egui::ColorImage::from_rgba_unmultiplied([d.width as usize, d.height as usize], &d.pixels);
        state.icon = Some(ui.ctx().load_texture("about_app_icon", img, egui::TextureOptions::LINEAR));
    }
    if state.github.is_none() {
        let g = crate::platform::icon::github();
        let img =
            egui::ColorImage::from_rgba_unmultiplied([g.width as usize, g.height as usize], &g.pixels);
        state.github = Some(ui.ctx().load_texture("about_github", img, egui::TextureOptions::LINEAR));
    }

    let mut close = false;
    let resp = egui::Modal::new(egui::Id::new("about")).show(ui.ctx(), |ui| {
        const W: f32 = 340.0;
        ui.set_width(W);
        ui.allocate_exact_size(egui::vec2(W, 0.0), egui::Sense::hover());

        // Close button in the top-right corner, drawn into an explicit rect so it sits over the
        // centred content rather than pushing it down.
        let x = crate::ui::widgets::close::SIZE;
        let x_rect = egui::Rect::from_min_size(
            egui::pos2(ui.max_rect().right() - x, ui.max_rect().top()),
            egui::vec2(x, x),
        );
        if crate::ui::widgets::close::close_button_at(ui, theme, x_rect, "about_close").clicked() {
            close = true;
        }

        ui.vertical_centered(|ui| {
            ui.add_space(6.0);
            if let Some(tex) = &state.icon {
                ui.add(egui::Image::new(&*tex).fit_to_exact_size(egui::vec2(64.0, 64.0)));
            }
            ui.add_space(12.0);
            ui.label(egui::RichText::new("SimpleTally").font(t::sans_medium(22.0)).color(theme.text_primary));
            ui.add_space(3.0);
            ui.label(
                egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                    .font(t::mono(13.0))
                    .color(theme.text_secondary),
            );

            ui.add_space(16.0);
            ui.separator();
            ui.add_space(14.0);

            if let Some(gh) = &state.github {
                github_link(ui, theme, gh);
            }

            ui.add_space(16.0);
            ui.label(egui::RichText::new("Made by majima").font(t::sans(13.0)).color(theme.text_body));
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Built with AI assistance")
                    .font(t::sans(t::CAPTION))
                    .color(theme.text_secondary),
            );
            ui.add_space(10.0);
            ui.label(
                egui::RichText::new("\u{00A9} 2026 majimawrks")
                    .font(t::sans(12.0))
                    .color(theme.text_tertiary),
            );
            ui.add_space(6.0);
        });
    });

    if close || resp.should_close() {
        state.visible = false;
    }
}

/// The GitHub row: the tinted GitHub mark + the URL as one clickable, hover-underlined link that
/// opens the default browser. The mark is `assets/github.ico` (white, tinted here); the bundled
/// fonts carry no brand glyph.
fn github_link(ui: &mut egui::Ui, theme: &Theme, gh: &egui::TextureHandle) {
    let font = t::mono(t::CAPTION);
    let text_w = ui
        .painter()
        .layout_no_wrap(GITHUB_LABEL.to_owned(), font.clone(), egui::Color32::PLACEHOLDER)
        .rect
        .width();
    let mark = 16.0;
    let gap = 7.0;
    let total = mark + gap + text_w;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(total, 20.0), egui::Sense::click());

    let color = if resp.hovered() { theme.accent } else { theme.secondary };
    let mark_rect = egui::Rect::from_min_size(
        egui::pos2(rect.left(), rect.center().y - mark / 2.0),
        egui::vec2(mark, mark),
    );
    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    ui.painter().image(gh.id(), mark_rect, uv, color);

    let galley = ui.painter().layout_no_wrap(GITHUB_LABEL.to_owned(), font, color);
    let ty = rect.center().y - galley.rect.height() / 2.0;
    ui.painter().galley(egui::pos2(rect.left() + mark + gap, ty), galley, color);

    if resp.hovered() {
        // Underline the text on hover, so the link reads as clickable.
        ui.painter().line_segment(
            [
                egui::pos2(rect.left() + mark + gap, rect.bottom() - 1.0),
                egui::pos2(rect.right(), rect.bottom() - 1.0),
            ],
            egui::Stroke::new(1.0, color),
        );
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if resp.clicked() {
        open_url(GITHUB_URL);
    }
}

/// Open `url` in the default browser. Best-effort: an About link that fails to open is not worth
/// surfacing.
fn open_url(url: &str) {
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(not(target_os = "windows"))]
    let _ = url;
}
