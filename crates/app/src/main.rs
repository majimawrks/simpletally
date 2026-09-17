//! SimpleTally — tray-resident tally app (Rust rewrite of v3.8).
//!
//! The binary owns the winit event loop directly (egui + egui-winit + egui_glow,
//! not eframe — PLAN §1.2). This is the Phase-0 windowing spike; see notes/PHASE0.md.
//!
//! Built incrementally: single window → tray → global hotkey → quick-add popup →
//! single instance (this file's election gate).

// Hide the console window in release builds; keep it in debug for the spike's logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod platform;
// Some service/ui items are wired incrementally across Phase 2–4; silence dead-code until then.
#[allow(dead_code)]
mod service;
#[allow(dead_code)]
mod ui;

use std::ffi::OsStr;

use platform::{elect, ActivationTarget, App, Election, FirstRun, UserEvent};

fn main() {
    // A second launch signals the running instance and exits without opening anything.
    // args_os avoids a panic on non-UTF-16 argv; we only compare an ASCII switch.
    let activation = if std::env::args_os().any(|a| a == *OsStr::new("--quick-add")) {
        ActivationTarget::QuickAdd
    } else {
        ActivationTarget::Main
    };
    let primary = match elect(activation) {
        Election::Primary(p) => p,
        Election::Secondary => return,
    };

    let event_loop = winit::event_loop::EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("failed to build winit event loop");

    let proxy = event_loop.create_proxy();
    // Start the activation server now that we have a proxy to forward into the loop.
    primary.serve(proxy.clone());

    // Open (or create/migrate) the database beside the executable before building the UI.
    // A failure here (unwritable directory, malformed/newer file) is fatal and shown loudly.
    let (db, outcome) = match service::db_service::database_path()
        .and_then(|path| service::db_service::open_at(&path))
    {
        Ok(v) => v,
        Err(e) => {
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Error)
                .set_title("SimpleTally — cannot open database")
                .set_description(e.to_string())
                .show();
            return;
        }
    };

    // Never surface migration guidance for `Opened`/`Migrated` — only a brand-new empty file
    // means the user might actually have real data sitting elsewhere (PLAN §1.5).
    let created_empty = matches!(outcome, simpletally_core::OpenOutcome::Created);
    let candidates = service::db_service::exe_dir()
        .map(|exe_dir| {
            service::discovery::discover(&exe_dir, &service::discovery::default_search_dirs())
                .other_candidates
        })
        .unwrap_or_default();
    let first_run = FirstRun { created_empty, candidates };

    let mut app = App::new(proxy, activation, db, first_run);
    event_loop
        .run_app(&mut app)
        .expect("winit event loop exited with an error");

    // Keep the primary guard (and its mutex) alive until the loop has exited.
    drop(primary);
}
