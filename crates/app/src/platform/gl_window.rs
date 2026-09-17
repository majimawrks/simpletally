//! One OS window plus its glutin GL context/surface.
//!
//! Adapted from the `egui_glow` `pure_glow` example (itself lifted from eframe) — the
//! canonical winit+glutin bootstrap for these pinned versions. Windows are created
//! **hidden** (the caller decides when to show) so a cold start never flashes an
//! unpainted frame (PHASE0 acceptance list).
//!
//! With more than one window on one thread, only one GL context is current at a time,
//! so `make_current` must be called before painting/resizing each window (PHASE0 gate
//! item 5). A `PossiblyCurrentContext` is not guaranteed to stay current after another
//! window renders.

use std::ffi::CStr;
use std::num::NonZeroU32;

use glutin::context::{
    NotCurrentGlContext as _, PossiblyCurrentContext, PossiblyCurrentGlContext as _,
};
use glutin::display::{Display, GetGlDisplay as _, GlDisplay as _};
use glutin::surface::{GlSurface as _, Surface, SurfaceAttributesBuilder, SwapInterval, WindowSurface};
use winit::event_loop::ActiveEventLoop;
use winit::raw_window_handle::HasWindowHandle as _;
use winit::window::{Window, WindowAttributes};

pub struct GlWindow {
    window: Window,
    gl_context: PossiblyCurrentContext,
    gl_display: Display,
    gl_surface: Surface<WindowSurface>,
}

impl GlWindow {
    /// Create a window (per `attributes`) with a current GL context.
    ///
    /// # Safety
    /// Builds a GL display/context/surface from the raw window handle; must run on
    /// the event-loop thread inside `resumed`.
    pub unsafe fn new(event_loop: &ActiveEventLoop, attributes: WindowAttributes) -> Self {
        let config_template = glutin::config::ConfigTemplateBuilder::new()
            .prefer_hardware_accelerated(None)
            .with_depth_size(0)
            .with_stencil_size(0)
            .with_transparency(false);

        let (mut window, gl_config) = glutin_winit::DisplayBuilder::new()
            .with_preference(glutin_winit::ApiPreference::FallbackEgl)
            .with_window_attributes(Some(attributes.clone()))
            .build(event_loop, config_template, |mut configs| {
                configs
                    .next()
                    .expect("no matching glutin config for window creation")
            })
            .expect("failed to create glutin config");

        let gl_display = gl_config.display();

        let raw_window_handle = window
            .as_ref()
            .map(|w| w.window_handle().expect("window handle").as_raw());

        let context_attributes =
            glutin::context::ContextAttributesBuilder::new().build(raw_window_handle);
        let fallback_attributes = glutin::context::ContextAttributesBuilder::new()
            .with_context_api(glutin::context::ContextApi::Gles(None))
            .build(raw_window_handle);

        let not_current = unsafe {
            gl_display
                .create_context(&gl_config, &context_attributes)
                .unwrap_or_else(|_| {
                    gl_display
                        .create_context(&gl_config, &fallback_attributes)
                        .expect("failed to create GL context (core and GLES both failed)")
                })
        };

        let window = window.take().unwrap_or_else(|| {
            glutin_winit::finalize_window(event_loop, attributes, &gl_config)
                .expect("failed to finalize glutin window")
        });

        let (width, height): (u32, u32) = window.inner_size().into();
        let surface_attributes = SurfaceAttributesBuilder::<WindowSurface>::new().build(
            window.window_handle().expect("window handle").as_raw(),
            NonZeroU32::new(width).unwrap_or(NonZeroU32::MIN),
            NonZeroU32::new(height).unwrap_or(NonZeroU32::MIN),
        );

        let gl_surface = unsafe {
            gl_display
                .create_window_surface(&gl_config, &surface_attributes)
                .expect("failed to create GL window surface")
        };

        let gl_context = not_current
            .make_current(&gl_surface)
            .expect("failed to make GL context current");

        let _ = gl_surface.set_swap_interval(&gl_context, SwapInterval::Wait(NonZeroU32::MIN));

        Self {
            window,
            gl_context,
            gl_display,
            gl_surface,
        }
    }

    pub fn window(&self) -> &Window {
        &self.window
    }

    /// Make this window's GL context current on the calling thread. Call before any
    /// paint/upload/resize for this window when multiple windows exist. Returns false
    /// if it failed (e.g. a transient invalid-handle during window churn) — callers
    /// skip the frame rather than crash.
    #[must_use]
    pub fn make_current(&self) -> bool {
        self.gl_context.make_current(&self.gl_surface).is_ok()
    }

    pub fn resize(&self, size: winit::dpi::PhysicalSize<u32>) {
        if let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) {
            self.gl_surface.resize(&self.gl_context, w, h);
        }
    }

    pub fn swap_buffers(&self) -> glutin::error::Result<()> {
        self.gl_surface.swap_buffers(&self.gl_context)
    }

    pub fn proc_address(&self, addr: &CStr) -> *const std::ffi::c_void {
        self.gl_display.get_proc_address(addr)
    }
}
