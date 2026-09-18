//! Platform layer: the winit event loop, windows, and (later) tray, global hotkey
//! and single-instance IPC. See notes/PHASE0.md.

mod app;
mod event;
mod gl_window;
mod hotkey;
/// `pub(crate)`: the UI reads the same embedded icon to build the quick-add badge's 正 mask,
/// so the badge and the app icon can never drift apart.
pub(crate) mod icon;
mod single_instance;
mod tray;
mod win;
mod winos;

pub use app::{App, FirstRun};
pub use event::{ActivationTarget, UserEvent};
pub use single_instance::{elect, Election};
