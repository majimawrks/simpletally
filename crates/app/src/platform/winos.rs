//! Small Windows-specific window tweaks that winit doesn't expose directly.

use winit::dpi::PhysicalPosition;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetForegroundWindow, GetWindowLongPtrW, SetForegroundWindow, SetWindowLongPtrW,
    SetWindowPos, GWL_EXSTYLE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_NOZORDER, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};

fn hwnd(window: &winit::window::Window) -> Option<HWND> {
    match window.window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(h)) => Some(h.hwnd.get() as HWND),
        _ => None,
    }
}

/// Turn a window into a "tool window" so it never appears in Alt-Tab.
/// `skip_taskbar` hides it from the taskbar but not from Alt-Tab — this does the rest
/// (PHASE0 acceptance: no stray window in Alt-Tab). Apply while the window is hidden.
pub fn exclude_from_alt_tab(window: &winit::window::Window) {
    let Some(hwnd) = hwnd(window) else { return };
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let ex = (ex | WS_EX_TOOLWINDOW as isize) & !(WS_EX_APPWINDOW as isize);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex);
        // Force the ex-style to take effect now; otherwise Alt-Tab keeps the old
        // eligibility until the window's first hide/show cycle.
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
}

/// Center `window` on the monitor under the mouse cursor — the "active" monitor for a
/// hotkey-summoned popup (PHASE0 sharp edge 2). Uses physical coordinates throughout.
pub fn center_on_active_monitor(window: &winit::window::Window) {
    let mut pt = POINT { x: 0, y: 0 };
    // SAFETY: FFI; `pt` is a valid out-pointer.
    if unsafe { GetCursorPos(&mut pt) } == 0 {
        return;
    }
    let hmon = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST) };
    let mut mi: MONITORINFO = unsafe { std::mem::zeroed() };
    mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    if unsafe { GetMonitorInfoW(hmon, &mut mi) } == 0 {
        return;
    }

    let work = mi.rcWork; // work area excludes the taskbar
    let (mw, mh) = (work.right - work.left, work.bottom - work.top);
    let size = window.outer_size();
    let x = work.left + (mw - size.width as i32) / 2;
    let y = work.top + (mh - size.height as i32) / 2;
    window.set_outer_position(PhysicalPosition::new(x, y));
}

/// The window currently in the foreground, as an opaque handle (`None` if there is
/// none). Captured before the popup steals focus so it can be restored on dismiss.
pub fn foreground_window() -> Option<isize> {
    let h = unsafe { GetForegroundWindow() };
    if h.is_null() {
        None
    } else {
        Some(h as isize)
    }
}

/// Restore the foreground to a window captured earlier with [`foreground_window`].
/// Best-effort: does nothing useful if the window is gone.
pub fn set_foreground(handle: isize) {
    unsafe {
        SetForegroundWindow(handle as HWND);
    }
}
