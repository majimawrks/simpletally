//! Stamps the Win32 resources into `SimpleTally.exe` (PLAN §3, Phase 5).
//!
//! This is a different thing from the icon `platform::icon` decodes at runtime. That one is
//! read by the *running process* for its window and tray. This one lives in the binary's
//! resource table, where Explorer, the taskbar, Alt-Tab and the Properties dialog read it
//! **without running the program** — so it has to be compiled in, and nothing the app does at
//! runtime can supply it. Without this the file shows the generic default icon and has a blank
//! Details tab.
//!
//! MSVC toolchains invoke `rc.exe` from the Windows SDK to compile the resource script. A
//! machine with the Rust MSVC toolchain but no SDK will fail here; the build deliberately does
//! not swallow that, because silently shipping an unbranded exe is the bug this file exists to
//! prevent.

fn main() {
    // The resource format is Windows-only; on any other host this is simply not applicable.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/icon.ico")
        .set("ProductName", "SimpleTally")
        .set("FileDescription", "SimpleTally — task tally")
        .set("CompanyName", "SimpleTally")
        // No apostrophes: winresource escapes them into the resource script literally, and
        // they surface in Explorer's Details tab as `project\'s`.
        .set("LegalCopyright", "Copyright (c) 2026")
        .set("OriginalFilename", "SimpleTally.exe")
        .set("InternalName", "SimpleTally");

    if let Err(e) = res.compile() {
        // Fail loudly rather than producing an unbranded binary that looks fine until someone
        // sees it in Explorer.
        panic!("failed to embed Windows resources (icon/version): {e}");
    }

    // Only the icon and this script affect the resources; don't re-run on every source change.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/icon.ico");
}
