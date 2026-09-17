//! One managed window: its GL window, glow context, egui integration, and repaint
//! deadline. The app owns one `Win` per real window (main, popup) — giving each its
//! own independent egui input state (PHASE0 gate item 2).

use std::sync::Arc;
use std::time::Instant;

use egui_glow::glow;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::window::{Window, WindowAttributes, WindowId};

use super::event::UserEvent;
use super::gl_window::GlWindow;

pub struct Win {
    gl_window: GlWindow,
    gl: Arc<glow::Context>,
    egui: egui_glow::EguiGlow,
    /// When this window next wants to paint (from egui's repaint callback). `None`
    /// means nothing pending.
    pub next_repaint: Option<Instant>,
}

impl Win {
    pub fn new(
        event_loop: &ActiveEventLoop,
        attributes: WindowAttributes,
        proxy: &EventLoopProxy<UserEvent>,
    ) -> Self {
        let gl_window = unsafe { GlWindow::new(event_loop, attributes) };
        let gl = Arc::new(unsafe {
            glow::Context::from_loader_function_cstr(|s| gl_window.proc_address(s))
        });
        let egui = egui_glow::EguiGlow::new(event_loop, Arc::clone(&gl), None, None, true);

        // Route this window's repaint requests through the proxy, tagged by WindowId.
        // Compute the absolute deadline here so queue latency doesn't push it late.
        let id = gl_window.window().id();
        let p = egui::mutex::Mutex::new(proxy.clone());
        egui.egui_ctx.set_request_repaint_callback(move |info| {
            let at = Instant::now() + info.delay;
            let _ = p.lock().send_event(UserEvent::Repaint { window: id, at });
        });

        Self {
            gl_window,
            gl,
            egui,
            next_repaint: None,
        }
    }

    pub fn id(&self) -> WindowId {
        self.gl_window.window().id()
    }

    pub fn window(&self) -> &Window {
        self.gl_window.window()
    }

    /// This window's egui context, for one-time setup like installing fonts and theme.
    pub fn egui_ctx(&self) -> &egui::Context {
        &self.egui.egui_ctx
    }

    pub fn set_visible(&self, visible: bool) {
        self.gl_window.window().set_visible(visible);
    }

    pub fn is_visible(&self) -> bool {
        self.gl_window.window().is_visible().unwrap_or(false)
    }

    /// Feed a window event to egui (handling resize); returns whether egui wants a
    /// repaint as a result (clicks, typing, DPI changes).
    pub fn on_window_event(&mut self, event: &WindowEvent) -> bool {
        if let WindowEvent::Resized(size) = event {
            self.gl_window.resize(*size);
        }
        self.egui
            .on_window_event(self.gl_window.window(), event)
            .repaint
    }

    /// Run the egui pass with `ui` and present. `ui` builds directly on the
    /// full-window `Ui` that `EguiGlow::run` provides. Makes this window's GL context
    /// current first (multi-window discipline, PHASE0 gate item 5).
    pub fn paint(&mut self, clear: [f32; 3], ui: impl FnMut(&mut egui::Ui)) {
        if !self.gl_window.make_current() {
            // Couldn't bind this window's context — skip the frame. Do NOT
            // request_redraw here: a persistent failure would spin the loop. The next
            // real event or scheduled repaint retries naturally.
            return;
        }
        self.egui.run(self.gl_window.window(), ui);
        unsafe {
            use glow::HasContext as _;
            self.gl.clear_color(clear[0], clear[1], clear[2], 1.0);
            self.gl.clear(glow::COLOR_BUFFER_BIT);
        }
        self.egui.paint(self.gl_window.window());
        // Present. A transient swap failure must not kill an always-on tray app.
        if let Err(e) = self.gl_window.swap_buffers() {
            eprintln!("swap_buffers failed: {e}");
        }
    }

    pub fn destroy(&mut self) {
        // Only delete GL resources if this window's context is actually current.
        if self.gl_window.make_current() {
            self.egui.destroy();
        }
    }
}
