//! The `UserEvent` carried through `EventLoopProxy` to wake the winit loop.
//!
//! Grows toward the full wake-up contract in notes/PHASE0.md gate item 2. Present so
//! far: per-window repaint requests, tray icon + tray-menu events, and the global
//! hotkey. Still to come: second-instance activation, theme changes, forwarder-lost.

use std::time::Instant;

use winit::window::WindowId;

/// What a second launch asks the running instance to surface.
#[derive(Debug, Clone, Copy)]
pub enum ActivationTarget {
    Main,
    QuickAdd,
}

#[derive(Debug)]
pub enum UserEvent {
    /// egui asked to repaint a specific window. `at` is an absolute deadline computed
    /// in the repaint callback (not a delay resolved on delivery — avoids queue
    /// latency); `at <= now` means repaint immediately.
    Repaint { window: WindowId, at: Instant },
    /// A click on the tray icon.
    TrayIcon(tray_icon::TrayIconEvent),
    /// A tray context-menu item was chosen.
    TrayMenu(tray_icon::menu::MenuEvent),
    /// The global hotkey (Ctrl+Shift+T) changed state.
    Hotkey(global_hotkey::GlobalHotKeyEvent),
    /// A second launch asked us (the running instance) to surface a window.
    Activate(ActivationTarget),
}
