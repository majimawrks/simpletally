//! The global hotkey (Ctrl+Shift+T), registered on the winit thread (its message
//! pump delivers the events). Registration can fail if the combination is already
//! taken by another app — the caller surfaces that visibly (PHASE0 sharp edge 1).
//! Events are routed through the proxy via the crate's global handler (OnceCell —
//! installed once). Callers filter `HotKeyState::Pressed` and the id.

use egui::mutex::Mutex;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};
use winit::event_loop::EventLoopProxy;

use super::event::UserEvent;

pub struct Hotkey {
    /// Kept alive: dropping the manager unregisters the hotkey.
    _manager: GlobalHotKeyManager,
    /// Id of our registered hotkey, for matching incoming events.
    pub id: u32,
}

impl Hotkey {
    /// Register Ctrl+Shift+T and route its events through the proxy.
    /// `Err` carries a human-readable reason (e.g. the combo is already taken).
    pub fn new(proxy: &EventLoopProxy<UserEvent>) -> Result<Self, String> {
        let manager =
            GlobalHotKeyManager::new().map_err(|e| format!("create hotkey manager: {e}"))?;
        let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyT);
        manager
            .register(hotkey)
            .map_err(|e| format!("register Ctrl+Shift+T (already in use?): {e}"))?;
        let id = hotkey.id();

        let p = Mutex::new(proxy.clone());
        GlobalHotKeyEvent::set_event_handler(Some(move |e| {
            let _ = p.lock().send_event(UserEvent::Hotkey(e));
        }));

        Ok(Self {
            _manager: manager,
            id,
        })
    }
}
