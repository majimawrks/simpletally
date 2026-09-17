//! The app icon, embedded from `assets/icon.ico` and decoded to RGBA once so it can
//! serve both the tray (`tray_icon::Icon`) and the window/taskbar (`winit::window::Icon`).
//!
//! Embedding (rather than loading a file at runtime) keeps the exe portable — a
//! single self-contained binary, as PLAN requires. The exe's own resource icon is a
//! separate concern handled by winresource in Phase 5.

use std::io::Cursor;

/// The `.ico` bytes, baked into the binary at compile time.
const ICON_BYTES: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/icon.ico"));

struct Rgba {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
}

/// Decode the largest frame in the `.ico` to RGBA.
fn decode() -> Rgba {
    let dir = ico::IconDir::read(Cursor::new(ICON_BYTES)).expect("assets/icon.ico is not a valid ICO");
    let entry = dir
        .entries()
        .iter()
        .max_by_key(|e| u32::from(e.width()))
        .expect("assets/icon.ico has no image entries");
    let image = entry.decode().expect("failed to decode icon frame");
    Rgba {
        pixels: image.rgba_data().to_vec(),
        width: image.width(),
        height: image.height(),
    }
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
