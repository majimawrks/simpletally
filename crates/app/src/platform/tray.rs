//! System-tray icon + context menu.
//!
//! Created on the winit thread (in `resumed`): that thread runs the Win32 message
//! pump the tray and hotkey depend on (PHASE0 gate item 2). Events are routed to the
//! event loop through `EventLoopProxy` using the crates' global handlers. Those
//! handlers live in a `OnceCell`, so they are installed exactly once — here.

use egui::mutex::Mutex;
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder, TrayIconEvent};
use winit::event_loop::EventLoopProxy;

use super::event::UserEvent;
use super::icon;

pub struct Tray {
    /// Kept alive: dropping the `TrayIcon` removes it from the tray.
    _tray: TrayIcon,
    pub show_id: MenuId,
    pub migrate_id: MenuId,
    pub quit_id: MenuId,
}

impl Tray {
    pub fn new(proxy: &EventLoopProxy<UserEvent>) -> Self {
        let menu = Menu::new();
        let show = MenuItem::new("Show SimpleTally", true, None);
        let migrate = MenuItem::new("Migrate old data\u{2026}", true, None);
        let quit = MenuItem::new("Quit", true, None);
        menu.append(&show).expect("append show");
        menu.append(&migrate).expect("append migrate");
        menu.append(&PredefinedMenuItem::separator())
            .expect("append separator");
        menu.append(&quit).expect("append quit");
        let show_id = show.id().clone();
        let migrate_id = migrate.id().clone();
        let quit_id = quit.id().clone();

        let tray = TrayIconBuilder::new()
            .with_tooltip("SimpleTally")
            .with_icon(icon::tray())
            .with_menu(Box::new(menu))
            .build()
            .expect("failed to build tray icon");

        // The handler bound is `Fn + Send + Sync`; `EventLoopProxy` is `Send` but not
        // `Sync`, so wrap each clone in a `Mutex`.
        let p = Mutex::new(proxy.clone());
        TrayIconEvent::set_event_handler(Some(move |e| {
            let _ = p.lock().send_event(UserEvent::TrayIcon(e));
        }));
        let p = Mutex::new(proxy.clone());
        MenuEvent::set_event_handler(Some(move |e| {
            let _ = p.lock().send_event(UserEvent::TrayMenu(e));
        }));

        Self {
            _tray: tray,
            show_id,
            migrate_id,
            quit_id,
        }
    }
}
