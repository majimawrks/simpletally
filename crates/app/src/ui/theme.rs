//! Design tokens, fonts, and the `Theme` struct (PLAN §5; design source `_rustrefactor/README.md`
//! §"Design tokens").
//!
//! Two font families are bundled: DM Sans (proportional / UI labels) and JetBrains Mono
//! (numerics, eyebrows, category labels). Each has a 400 (regular) and 500 (medium) cut,
//! registered as four `egui::FontFamily` entries — see [`install_fonts`].
//!
//! Known deviation from the design spec: egui has no letter-spacing control, so the
//! eyebrow/category letter-spacing called out in the README (`0.12–0.16em`) is not
//! reproducible here and is simply dropped. Uppercasing of eyebrow/category text is left to
//! callers (widgets own the `.to_uppercase()` call); this module only hands out `FontId`s.

use egui::{FontData, FontDefinitions, FontFamily, FontId};

/// Font family name for the DM Sans medium (500) cut.
const SANS_MEDIUM_FAMILY: &str = "sans-medium";
/// Font family name for the JetBrains Mono medium (500) cut.
const MONO_MEDIUM_FAMILY: &str = "mono-medium";

/// Registers the four bundled faces into `ctx`'s fonts:
///
/// - `FontFamily::Proportional` → DM Sans Regular (400 sans default)
/// - `FontFamily::Monospace` → JetBrains Mono Regular (400 mono default)
/// - `FontFamily::Name("sans-medium")` → DM Sans Medium (500)
/// - `FontFamily::Name("mono-medium")` → JetBrains Mono Medium (500)
///
/// egui's built-in fallback fonts (emoji, wide glyph coverage) are kept — the bundled faces
/// are inserted at the front of each family's list rather than replacing it.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    fonts.font_data.insert(
        "DMSans-Regular".to_owned(),
        FontData::from_static(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/fonts/DMSans-Regular.ttf"
        )))
        .into(),
    );
    fonts.font_data.insert(
        "DMSans-Medium".to_owned(),
        FontData::from_static(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/fonts/DMSans-Medium.ttf"
        )))
        .into(),
    );
    fonts.font_data.insert(
        "JetBrainsMono-Regular".to_owned(),
        FontData::from_static(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/fonts/JetBrainsMono-Regular.ttf"
        )))
        .into(),
    );
    fonts.font_data.insert(
        "JetBrainsMono-Medium".to_owned(),
        FontData::from_static(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/fonts/JetBrainsMono-Medium.ttf"
        )))
        .into(),
    );

    // Proportional (default sans): DM Sans Regular first, keep egui's fallbacks after it.
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "DMSans-Regular".to_owned());

    // Monospace (default mono): JetBrains Mono Regular first, keep fallbacks after it.
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, "JetBrainsMono-Regular".to_owned());

    // Named families for the medium cuts. Fall back to the corresponding regular family's
    // existing fallback fonts for glyph coverage.
    let sans_fallbacks = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let mut sans_medium = vec!["DMSans-Medium".to_owned()];
    sans_medium.extend(sans_fallbacks.into_iter().filter(|f| f != "DMSans-Medium"));
    fonts
        .families
        .insert(FontFamily::Name(SANS_MEDIUM_FAMILY.into()), sans_medium);

    let mono_fallbacks = fonts
        .families
        .get(&FontFamily::Monospace)
        .cloned()
        .unwrap_or_default();
    let mut mono_medium = vec!["JetBrainsMono-Medium".to_owned()];
    mono_medium.extend(
        mono_fallbacks
            .into_iter()
            .filter(|f| f != "JetBrainsMono-Medium"),
    );
    fonts
        .families
        .insert(FontFamily::Name(MONO_MEDIUM_FAMILY.into()), mono_medium);

    ctx.set_fonts(fonts);
}

/// `FontId` for DM Sans Regular (Proportional) at `size`.
pub fn sans(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// `FontId` for DM Sans Medium at `size`.
pub fn sans_medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SANS_MEDIUM_FAMILY.into()))
}

/// `FontId` for JetBrains Mono Regular (Monospace) at `size`.
pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

/// `FontId` for JetBrains Mono Medium at `size`.
pub fn mono_medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(MONO_MEDIUM_FAMILY.into()))
}

// --- Type scale (README §"Design tokens" → "Type scale") ---------------------------------

/// Day total (`mono_medium`). README: Mono 48px / 500.
pub const DAY_TOTAL: f32 = 48.0;
/// Metric value (`mono`). README: Mono 34px / 400.
pub const METRIC_VALUE: f32 = 34.0;
/// Tile count (`mono_medium`). README: Mono 28px / 500.
pub const TILE_COUNT: f32 = 28.0;
/// Quick-add query (`mono`). README: Mono 20px / 400.
pub const QUICK_ADD: f32 = 20.0;
/// Section title (`sans_medium`). README: Sans 14.5px / 500.
pub const SECTION_TITLE: f32 = 14.5;
/// Tile name (`sans_medium`). README: Sans 14px / 500.
pub const TILE_NAME: f32 = 14.0;
/// Body / row text (`sans`). README: Sans 13.5px / 400.
pub const BODY: f32 = 13.5;
/// Numeric in rows (`mono`). README: Mono 13px / 400.
pub const ROW_NUMERIC: f32 = 13.0;
/// Tab label (`sans`).
pub const TAB: f32 = 13.5;
/// Day-nav label (`sans`).
pub const DAY_NAV: f32 = 13.0;
/// Secondary / caption text (`sans`). README: Sans 12.5–13px / 400.
pub const CAPTION: f32 = 12.5;
/// Time label (`mono`).
pub const TIME: f32 = 12.0;
/// Eyebrow label (`mono`, caller uppercases). README: Mono 10.5–11px / 400.
pub const EYEBROW: f32 = 11.0;
/// Pill text (`sans`).
pub const PILL_TEXT: f32 = 13.5;
/// Pill count (`mono`).
pub const PILL_COUNT: f32 = 11.5;
/// Tile category (`mono`, caller uppercases). README: Mono 9.5px / 400.
pub const TILE_CATEGORY: f32 = 9.5;
/// "Today" label (`mono`).
pub const TODAY_LABEL: f32 = 10.5;

// --- Theme -------------------------------------------------------------------------------

/// Light/dark selection for [`resolve`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Light,
    Dark,
}

/// All color tokens used by the UI (PLAN §5: "No literal hex outside the two theme
/// constructors" — [`Theme::light`] and [`Theme::dark`]). Every field is an `egui::Color32`.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    // Surfaces
    pub bg_canvas: egui::Color32,
    pub bg_raised: egui::Color32,
    pub bg_sunken: egui::Color32,
    pub bg_chrome: egui::Color32,
    pub bg_track: egui::Color32,
    pub bg_tab_active: egui::Color32,

    // Borders
    pub border_strong: egui::Color32,
    pub border: egui::Color32,
    pub border_subtle: egui::Color32,

    // Text
    pub text_primary: egui::Color32,
    pub text_body: egui::Color32,
    pub text_secondary: egui::Color32,
    pub text_tertiary: egui::Color32,
    pub text_quiet: egui::Color32,
    pub text_disabled: egui::Color32,

    // Accent / status
    pub accent: egui::Color32,
    pub accent_hover: egui::Color32,
    pub accent_tint_bg: egui::Color32,
    pub accent_tint_border: egui::Color32,
    pub accent_eyebrow: egui::Color32,
    pub accent_muted_bar: egui::Color32,
    pub negative: egui::Color32,

    // Selected pill
    pub pill_sel_bg: egui::Color32,
    pub pill_sel_border: egui::Color32,
    pub pill_sel_text: egui::Color32,
    pub pill_sel_count: egui::Color32,

    // Tiles — empty (untallied) state
    pub tile_empty_bg: egui::Color32,
    pub tile_empty_border: egui::Color32,
    pub tile_empty_name: egui::Color32,
    pub tile_empty_number: egui::Color32,
    pub tile_empty_category: egui::Color32,

    // Tiles — active (tallied) state
    pub tile_active_bg: egui::Color32,
    pub tile_active_border: egui::Color32,
    pub tile_active_name: egui::Color32,
    pub tile_active_number: egui::Color32,
    pub tile_active_category: egui::Color32,

    /// The 8-colour green ramp for Insights' category list / stacked bar (README §04
    /// "Green ramp"), rank order. Same values in both themes — it's an accent-family ramp,
    /// not a surface color, so it doesn't need a dark variant.
    pub ramp: [egui::Color32; 8],
}

impl Theme {
    /// Light theme. Exact hex values from README §"Design tokens" (Neutrals / Accent
    /// tables); this and [`Theme::dark`] are the only places hex literals appear.
    pub fn light() -> Theme {
        use egui::Color32 as C;
        Theme {
            bg_canvas: C::from_rgb(0xFC, 0xFB, 0xF8),
            bg_raised: C::from_rgb(0xFF, 0xFF, 0xFF),
            bg_sunken: C::from_rgb(0xF7, 0xF5, 0xF0),
            bg_chrome: C::from_rgb(0xEF, 0xED, 0xE7),
            bg_track: C::from_rgb(0xF1, 0xEF, 0xE9),
            bg_tab_active: C::from_rgb(0xED, 0xEA, 0xE2),

            border_strong: C::from_rgb(0xD8, 0xD4, 0xCB),
            border: C::from_rgb(0xE4, 0xE1, 0xD9),
            border_subtle: C::from_rgb(0xEB, 0xE8, 0xE1),

            text_primary: C::from_rgb(0x1C, 0x1B, 0x19),
            text_body: C::from_rgb(0x35, 0x32, 0x2D),
            text_secondary: C::from_rgb(0x6C, 0x68, 0x60),
            text_tertiary: C::from_rgb(0x9A, 0x95, 0x8C),
            text_quiet: C::from_rgb(0xA8, 0xA3, 0x9A),
            text_disabled: C::from_rgb(0xC2, 0xBD, 0xB4),

            accent: C::from_rgb(0x3F, 0x9C, 0x6A),
            accent_hover: C::from_rgb(0x2E, 0x7A, 0x51),
            accent_tint_bg: C::from_rgb(0xEF, 0xF7, 0xF2),
            accent_tint_border: C::from_rgb(0x9C, 0xC9, 0xB0),
            accent_eyebrow: C::from_rgb(0x7A, 0x9E, 0x88),
            accent_muted_bar: C::from_rgb(0x8F, 0xC7, 0xA8),
            negative: C::from_rgb(0xB4, 0x48, 0x3F),

            pill_sel_bg: C::from_rgb(0xEA, 0xF3, 0xEE),
            pill_sel_border: C::from_rgb(0x9C, 0xC9, 0xB0),
            pill_sel_text: C::from_rgb(0x2E, 0x7A, 0x51),
            pill_sel_count: C::from_rgb(0x3F, 0x9C, 0x6A),

            tile_empty_bg: C::from_rgb(0xFF, 0xFF, 0xFF),
            tile_empty_border: C::from_rgb(0xE4, 0xE1, 0xD9),
            tile_empty_name: C::from_rgb(0x35, 0x32, 0x2D),
            tile_empty_number: C::from_rgb(0xD2, 0xCE, 0xC5),
            tile_empty_category: C::from_rgb(0xBE, 0xB9, 0xB0),

            tile_active_bg: C::from_rgb(0xEF, 0xF7, 0xF2),
            tile_active_border: C::from_rgb(0x9C, 0xC9, 0xB0),
            tile_active_name: C::from_rgb(0x1C, 0x1B, 0x19),
            tile_active_number: C::from_rgb(0x3F, 0x9C, 0x6A),
            tile_active_category: C::from_rgb(0x7A, 0x9E, 0x88),

            ramp: RAMP,
        }
    }

    /// Dark theme. Values given explicitly in the brief are used verbatim; anything not
    /// specified is derived to stay coherent with the given dark tones. Each derived field
    /// says what it's derived from.
    pub fn dark() -> Theme {
        use egui::Color32 as C;
        Theme {
            // Given.
            bg_canvas: C::from_rgb(0x1A, 0x1A, 0x18),
            bg_chrome: C::from_rgb(0x22, 0x22, 0x20),
            // Derived: one step up from bg_canvas, mirrors light's bg_raised being lighter
            // than bg_canvas (there raised is *above* canvas; on dark, raised surfaces are
            // still lighter than the canvas so content pops).
            bg_raised: C::from_rgb(0x24, 0x24, 0x21),
            // Derived: between bg_canvas and bg_chrome, mirrors light's bg_sunken sitting
            // just below bg_canvas in the ramp.
            bg_sunken: C::from_rgb(0x1F, 0x1F, 0x1D),
            // Derived: same tone family as bg_chrome/bg_tab_active, a touch lighter.
            bg_track: C::from_rgb(0x26, 0x26, 0x23),
            bg_tab_active: C::from_rgb(0x28, 0x28, 0x25),

            border_strong: C::from_rgb(0x35, 0x35, 0x2F),
            // Derived: halfway between border_strong and border_subtle.
            border: C::from_rgb(0x30, 0x30, 0x2B),
            border_subtle: C::from_rgb(0x2E, 0x2E, 0x29),

            text_primary: C::from_rgb(0xF2, 0xF1, 0xEC),
            // Derived: one step down from text_primary, mirrors light's text_body sitting
            // just under text_primary.
            text_body: C::from_rgb(0xD8, 0xD6, 0xD0),
            text_secondary: C::from_rgb(0x8B, 0x87, 0x7E),
            text_tertiary: C::from_rgb(0x85, 0x81, 0x7A),
            // Derived: a touch dimmer than text_tertiary, mirrors the light ramp's ordering.
            text_quiet: C::from_rgb(0x6E, 0x6A, 0x63),
            // Derived: dimmest step in the ramp, near bg_chrome but still legible.
            text_disabled: C::from_rgb(0x51, 0x4F, 0x49),

            accent: C::from_rgb(0x4F, 0xB3, 0x7D),
            // Derived: lighter than accent for a dark surface (hover states brighten, rather
            // than darken, against a dark background).
            accent_hover: C::from_rgb(0x6C, 0xC4, 0x95),
            // Derived: from tile_active_bg, which is the accent-tinted background given below.
            accent_tint_bg: C::from_rgb(0x22, 0x32, 0x2A),
            // Derived: from tile_active_border, the given accent-tinted border.
            accent_tint_border: C::from_rgb(0x3B, 0x5A, 0x47),
            // Derived: mirrors light's accent_eyebrow sitting between text_tertiary and
            // accent in hue/lightness.
            accent_eyebrow: C::from_rgb(0x6F, 0x9A, 0x82),
            // Derived: between accent and accent_tint_border in lightness.
            accent_muted_bar: C::from_rgb(0x4E, 0x7D, 0x63),
            // Derived: same hue as light's negative, lightened for dark-surface contrast.
            negative: C::from_rgb(0xD1, 0x6E, 0x64),

            pill_sel_bg: C::from_rgb(0x2A, 0x3A, 0x31),
            pill_sel_border: C::from_rgb(0x3B, 0x5A, 0x47),
            pill_sel_text: C::from_rgb(0x7E, 0xCB, 0xA0),
            // Derived: same as accent, mirrors light's pill_sel_count == accent.
            pill_sel_count: C::from_rgb(0x4F, 0xB3, 0x7D),

            tile_empty_bg: C::from_rgb(0x20, 0x1F, 0x1D),
            tile_empty_border: C::from_rgb(0x30, 0x2F, 0x2A),
            tile_empty_name: C::from_rgb(0xCF, 0xCC, 0xC4),
            tile_empty_number: C::from_rgb(0x48, 0x46, 0x3F),
            // Derived: mirrors light's tile_empty_category sitting between text_tertiary and
            // text_disabled.
            tile_empty_category: C::from_rgb(0x5C, 0x59, 0x52),

            tile_active_bg: C::from_rgb(0x22, 0x32, 0x2A),
            tile_active_border: C::from_rgb(0x3B, 0x5A, 0x47),
            // Derived: mirrors light's tile_active_name == text_primary.
            tile_active_name: C::from_rgb(0xF2, 0xF1, 0xEC),
            tile_active_number: C::from_rgb(0x7E, 0xCB, 0xA0),
            // Derived: mirrors light's tile_active_category == accent_eyebrow.
            tile_active_category: C::from_rgb(0x6F, 0x9A, 0x82),

            ramp: RAMP,
        }
    }
}

/// The 8-colour green ramp (README §04), same in both themes.
const RAMP: [egui::Color32; 8] = {
    use egui::Color32 as C;
    [
        C::from_rgb(0x3F, 0x9C, 0x6A),
        C::from_rgb(0x4F, 0xAB, 0x78),
        C::from_rgb(0x6F, 0xBB, 0x90),
        C::from_rgb(0x8F, 0xCB, 0xA8),
        C::from_rgb(0xA5, 0xD5, 0xB9),
        C::from_rgb(0xBB, 0xDF, 0xC9),
        C::from_rgb(0xD5, 0xE7, 0xDC),
        C::from_rgb(0xE4, 0xEF, 0xE8),
    ]
};

/// Resolves the effective theme: `override_mode` wins if given, otherwise the OS-reported
/// dark/light setting (via the `dark-light` crate, `dark_light::detect() -> Result<Mode, _>`
/// with `Mode::{Dark, Light, Unspecified}` in v3.0) is used, falling back to [`Theme::light`]
/// when the OS setting is `Unspecified` or detection errors.
pub fn resolve(override_mode: Option<Mode>) -> Theme {
    let mode = override_mode.unwrap_or_else(|| match dark_light::detect() {
        Ok(dark_light::Mode::Dark) => Mode::Dark,
        Ok(dark_light::Mode::Light) => Mode::Light,
        Ok(dark_light::Mode::Unspecified) | Err(_) => Mode::Light,
    });

    match mode {
        Mode::Light => Theme::light(),
        Mode::Dark => Theme::dark(),
    }
}

/// Install the fonts and apply `theme` to `ctx`'s egui style: base text styles at the design
/// sizes, and `Visuals` filled from the theme tokens (surfaces, borders, text ramp, accent).
/// Call once per window context after creation (and again when the theme changes).
pub fn apply(ctx: &egui::Context, theme: &Theme) {
    install_fonts(ctx);

    use egui::TextStyle;
    let text_styles: std::collections::BTreeMap<TextStyle, FontId> = [
        (TextStyle::Small, mono(TIME)),
        (TextStyle::Body, sans(BODY)),
        (TextStyle::Button, sans(BODY)),
        (TextStyle::Heading, sans_medium(SECTION_TITLE)),
        (TextStyle::Monospace, mono(ROW_NUMERIC)),
    ]
    .into_iter()
    .collect();

    let visuals = build_visuals(theme);
    // egui 0.36 keeps a Style per theme; set both so our tokens apply regardless of which
    // the OS reports (we already resolved light/dark into `theme`).
    ctx.all_styles_mut(|style| {
        style.text_styles = text_styles.clone();
        style.visuals = visuals.clone();
    });
}

/// Build egui `Visuals` from the theme tokens.
fn build_visuals(theme: &Theme) -> egui::Visuals {
    // Base off egui's light/dark visuals (chosen by canvas luminance), then override the
    // colors that matter with theme tokens.
    let is_dark = theme.bg_canvas.r() < 128;
    let mut v = if is_dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    let stroke = |c| egui::Stroke::new(1.0, c);

    v.override_text_color = Some(theme.text_primary);
    v.panel_fill = theme.bg_canvas;
    v.window_fill = theme.bg_raised;
    v.window_stroke = stroke(theme.border_strong);
    v.extreme_bg_color = theme.bg_raised; // text-edit background
    v.faint_bg_color = theme.bg_sunken;
    v.hyperlink_color = theme.accent;

    v.widgets.noninteractive.bg_fill = theme.bg_canvas;
    v.widgets.noninteractive.weak_bg_fill = theme.bg_canvas;
    v.widgets.noninteractive.bg_stroke = stroke(theme.border_subtle);
    v.widgets.noninteractive.fg_stroke.color = theme.text_secondary;

    v.widgets.inactive.bg_fill = theme.bg_raised;
    v.widgets.inactive.weak_bg_fill = theme.bg_raised;
    v.widgets.inactive.bg_stroke = stroke(theme.border);
    v.widgets.inactive.fg_stroke.color = theme.text_body;

    v.widgets.hovered.bg_fill = theme.accent_tint_bg;
    v.widgets.hovered.weak_bg_fill = theme.accent_tint_bg;
    v.widgets.hovered.bg_stroke = stroke(theme.accent);
    v.widgets.hovered.fg_stroke.color = theme.text_primary;

    v.widgets.active.bg_fill = theme.accent_tint_bg;
    v.widgets.active.weak_bg_fill = theme.accent_tint_bg;
    v.widgets.active.bg_stroke = stroke(theme.accent);
    v.widgets.active.fg_stroke.color = theme.text_primary;

    v.selection.bg_fill = theme.pill_sel_bg;
    v.selection.stroke = stroke(theme.pill_sel_text);

    v
}

/// The theme's canvas color as a linear-ish `[r, g, b]` for the GL clear (mostly covered by
/// egui's panel fill; matters only for uncovered edges during resize).
pub fn canvas_clear(theme: &Theme) -> [f32; 3] {
    let c = theme.bg_canvas;
    [
        c.r() as f32 / 255.0,
        c.g() as f32 / 255.0,
        c.b() as f32 / 255.0,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_and_dark_are_constructible_and_differ() {
        let light = Theme::light();
        let dark = Theme::dark();
        assert_ne!(light.bg_canvas, dark.bg_canvas);
        assert_ne!(light.text_primary, dark.text_primary);
        assert_ne!(light.accent, dark.accent);
    }

    #[test]
    fn resolve_honors_explicit_override() {
        let light = resolve(Some(Mode::Light));
        let dark = resolve(Some(Mode::Dark));
        assert_eq!(light.bg_canvas, Theme::light().bg_canvas);
        assert_eq!(dark.bg_canvas, Theme::dark().bg_canvas);
    }

    #[test]
    fn font_id_helpers_return_expected_family_and_size() {
        assert_eq!(sans(13.5), FontId::new(13.5, FontFamily::Proportional));
        assert_eq!(mono(13.0), FontId::new(13.0, FontFamily::Monospace));
        assert_eq!(
            sans_medium(14.0),
            FontId::new(14.0, FontFamily::Name(SANS_MEDIUM_FAMILY.into()))
        );
        assert_eq!(
            mono_medium(48.0),
            FontId::new(48.0, FontFamily::Name(MONO_MEDIUM_FAMILY.into()))
        );
    }

    #[test]
    fn install_fonts_registers_all_four_families_without_panicking() {
        let ctx = egui::Context::default();
        install_fonts(&ctx);

        // `Context::fonts`/`fonts_mut` only work once the context has run a pass, so drive
        // the assertions from inside `Context::run`.
        let mut output = ctx.run_ui(egui::RawInput::default(), |ctx| {
            // Laying out text with each FontId should succeed and produce non-empty glyph
            // runs, confirming the family resolved to an installed font rather than
            // silently falling back to nothing.
            let families = [
                sans(BODY),
                sans_medium(TILE_NAME),
                mono(ROW_NUMERIC),
                mono_medium(DAY_TOTAL),
            ];
            for font_id in families {
                let galley = ctx.fonts_mut(|f| {
                    f.layout_no_wrap("Hg1".to_owned(), font_id.clone(), egui::Color32::BLACK)
                });
                assert!(
                    galley.rect.width() > 0.0,
                    "expected non-empty layout for {font_id:?}"
                );
            }
        });
        // `run_ui`'s output carries a texture delta (the newly-uploaded font atlas) that
        // must be consumed or explicitly dropped; there's no renderer in this test to hand
        // it to.
        output.textures_delta.clear();
    }
}
