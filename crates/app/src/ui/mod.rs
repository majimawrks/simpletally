//! UI layer (Phases 2–4): theme tokens + fonts, custom widgets, and the screens drawn on
//! the Phase-0 winit shell. Talks to the data layer only through the core `Db` handle.

pub mod about;
pub mod backup;
pub mod chrome;
pub mod dialog;
pub mod insights;
pub mod migrate;
pub mod quickadd;
pub mod theme;
pub mod today;
pub mod tray_notice;
pub mod types;
pub mod widgets;
