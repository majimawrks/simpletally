//! The 正 ("tally") mark, taken from the app icon itself.
//!
//! It cannot be drawn as text: neither bundled font (DM Sans, JetBrains Mono) has CJK
//! coverage, so `painter.text` would render tofu. It was first hand-drawn as five straight
//! strokes, which is where this ended up wrong — 正's middle bar runs to the *right* of its
//! tall vertical, the mark is brush-weighted rather than uniform, and none of that survives
//! being approximated by eye in a 22px box.
//!
//! So the real mark is lifted out of `assets/icon.ico`, which is already embedded in the
//! binary for the tray and window icons. The icon is black strokes on a white disc; this
//! turns luminance into coverage (dark pixel = opaque glyph, white disc and everything
//! outside it = transparent), giving a tintable antialiased mask. The badge therefore always
//! matches the app's own mark, and if the icon is ever redrawn the badge follows.

/// Cache key for the mask texture in `egui`'s per-context temp store.
fn texture_id() -> egui::Id {
    egui::Id::new("zheng_mask_texture")
}

/// The glyph as a white, alpha-masked texture, uploaded once per `egui::Context` and reused.
/// White so that `tint` alone decides the drawn colour.
fn texture(ctx: &egui::Context) -> egui::TextureHandle {
    if let Some(h) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(texture_id())) {
        return h;
    }
    let src = crate::platform::icon::decode();
    let mut pixels = Vec::with_capacity((src.width * src.height) as usize);
    for px in src.pixels.chunks_exact(4) {
        let (r, g, b, a) = (px[0] as f32, px[1] as f32, px[2] as f32, px[3] as f32);
        // Rec. 601 luma; the icon is greyscale, so any sane weighting agrees here.
        let luma = (0.299 * r + 0.587 * g + 0.114 * b) / 255.0;
        let coverage = (1.0 - luma) * (a / 255.0);
        pixels.push(egui::Color32::from_white_alpha((coverage * 255.0) as u8));
    }
    let image = egui::ColorImage {
        size: [src.width as usize, src.height as usize],
        pixels,
        source_size: egui::vec2(src.width as f32, src.height as f32),
    };
    let handle = ctx.load_texture("zheng_mask", image, egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(texture_id(), handle.clone()));
    handle
}

/// Draws 正 centred in `rect`, in `color`. The mark keeps its square aspect, so a non-square
/// `rect` is fitted rather than stretched.
pub fn zheng(ui: &egui::Ui, rect: egui::Rect, color: egui::Color32) {
    let tex = texture(ui.ctx());
    let side = rect.width().min(rect.height());
    let square = egui::Rect::from_center_size(rect.center(), egui::vec2(side, side));
    ui.painter().image(
        tex.id(),
        square,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        color,
    );
}
