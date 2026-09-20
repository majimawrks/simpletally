//! The app icon, embedded from `assets/icon.ico` and decoded to RGBA once so it can
//! serve both the tray (`tray_icon::Icon`) and the window/taskbar (`winit::window::Icon`).
//!
//! Embedding (rather than loading a file at runtime) keeps the exe portable — a
//! single self-contained binary, as PLAN requires. The exe's own resource icon is a
//! separate concern handled by winresource in Phase 5.

use std::io::Cursor;

/// The `.ico` bytes, baked into the binary at compile time.
const ICON_BYTES: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/icon.ico"));
/// The GitHub mark (white silhouette on transparent), for the About dialog's repo link. Tinted
/// at render time, so it's stored white. Same `.ico` container so the same decoder serves it.
const GITHUB_BYTES: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/github.ico"));

pub struct Rgba {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Decode the largest frame of an `.ico` to RGBA.
fn decode_bytes(bytes: &[u8], what: &str) -> Rgba {
    let dir = ico::IconDir::read(Cursor::new(bytes)).unwrap_or_else(|_| panic!("{what} is not a valid ICO"));
    let entry = dir
        .entries()
        .iter()
        .max_by_key(|e| u32::from(e.width()))
        .unwrap_or_else(|| panic!("{what} has no image entries"));
    let image = entry.decode().expect("failed to decode icon frame");
    Rgba {
        pixels: image.rgba_data().to_vec(),
        width: image.width(),
        height: image.height(),
    }
}

/// The app icon, decoded to RGBA.
pub fn decode() -> Rgba {
    decode_bytes(ICON_BYTES, "assets/icon.ico")
}

/// The GitHub mark, decoded to RGBA (white on transparent — tint when drawing).
pub fn github() -> Rgba {
    decode_bytes(GITHUB_BYTES, "assets/github.ico")
}

/// Tray icon.
pub fn tray() -> tray_icon::Icon {
    let d = decode();
    tray_icon::Icon::from_rgba(d.pixels, d.width, d.height).expect("bad tray icon rgba")
}

/// Window / taskbar / Alt-Tab icon.
pub fn window() -> winit::window::Icon {
    let d = decode();
    winit::window::Icon::from_rgba(d.pixels, d.width, d.height).expect("bad window icon rgba")
}
