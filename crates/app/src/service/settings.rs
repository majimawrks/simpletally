//! Settings file (PLAN §5 "State / Persisted").
//!
//! A `settings.toml` beside the executable, serde + toml, carrying its own `version` field,
//! written atomically (temp file + rename) so a crash mid-write cannot corrupt it, falling
//! back to defaults with a logged warning when missing or unparseable. Settings failure is
//! not database failure — the DB directory may be deliberately unwritable (PLAN §2) and the
//! app must still run rather than conflate the two, so [`load`] never returns an error.
//!
//! Window geometry is persisted as-is here; clamping it to a currently-attached monitor is a
//! Phase-2 UI concern and does not belong in this module.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

/// Current schema version of the settings file.
const CURRENT_VERSION: u32 = 1;

/// User-facing color theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Theme {
    Light,
    Dark,
    /// Follow the OS setting. Default.
    #[default]
    System,
}

/// Saved main window position and size, in the platform's logical/physical pixels as reported
/// by the windowing backend (Phase-2 concern; this module just round-trips the numbers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Everything persisted across launches (PLAN §5). Transient UI state (popups, in-flight
/// edits, selection indices, ...) is never part of this struct — see PLAN §5 "Transient".
///
/// Every field carries `#[serde(default)]` so a partial or older-version file still loads:
/// missing fields fall back to their `Default` rather than failing the parse.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// Schema version of this file; current = [`CURRENT_VERSION`].
    #[serde(default = "default_version")]
    pub version: u32,
    /// "YYYY-MM-DD"; `None` means "pick at launch" (today).
    #[serde(default)]
    pub selected_date: Option<String>,
    /// Category id, or `None` for "All".
    #[serde(default)]
    pub selected_category: Option<i64>,
    #[serde(default)]
    pub theme: Theme,
    /// "week" | "month" | "quarter" | "year" | "custom".
    #[serde(default = "default_range_kind")]
    pub range_kind: String,
    /// "YYYY-MM-DD"; the date the range is computed relative to.
    #[serde(default)]
    pub range_anchor: Option<String>,
    /// "YYYY-MM-DD"; start of a custom range.
    #[serde(default)]
    pub custom_range_from: Option<String>,
    /// "YYYY-MM-DD"; end of a custom range.
    #[serde(default)]
    pub custom_range_to: Option<String>,
    /// "today" | "insights" | "types".
    #[serde(default = "default_active_tab")]
    pub active_tab: String,
    #[serde(default)]
    pub note_field_open: bool,
    /// `None` until the window has been shown and moved/resized at least once.
    #[serde(default)]
    pub main_window_geometry: Option<WindowGeometry>,
    /// Set once the user has ticked "don't show this again" on the notice explaining that
    /// `[x]` hides to the tray rather than quitting. Defaults false, so a fresh install
    /// explains itself once — the behaviour is otherwise indistinguishable from the app
    /// having crashed.
    #[serde(default)]
    pub hide_to_tray_notice_dismissed: bool,
    /// When true, `[x]` quits instead of hiding to the tray. Default false — hide-to-tray is
    /// the behaviour the global hotkey depends on, so it stays the default (BACKLOG:
    /// "close-button behaviour"). Set from the switch on the close notice.
    #[serde(default)]
    pub close_quits: bool,
}

fn default_version() -> u32 {
    CURRENT_VERSION
}

/// "week" is the narrowest range that still shows useful context on first launch.
fn default_range_kind() -> String {
    "week".to_string()
}

fn default_active_tab() -> String {
    "today".to_string()
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            version: CURRENT_VERSION,
            selected_date: None,
            selected_category: None,
            theme: Theme::System,
            range_kind: default_range_kind(),
            range_anchor: None,
            custom_range_from: None,
            custom_range_to: None,
            active_tab: default_active_tab(),
            note_field_open: false,
            main_window_geometry: None,
            hide_to_tray_notice_dismissed: false,
            close_quits: false,
        }
    }
}

/// Load settings from `path`. Never fails: a missing file or a parse error is logged to
/// stderr and [`Settings::default`] is returned instead, since settings failure must not be
/// treated as fatal (the DB directory may be deliberately unwritable, but the app must still
/// run).
pub fn load(path: &Path) -> Settings {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) => {
            eprintln!(
                "warning: could not read settings file {}: {err}; using defaults",
                path.display()
            );
            return Settings::default();
        }
    };
    match toml::from_str(&contents) {
        Ok(settings) => settings,
        Err(err) => {
            eprintln!(
                "warning: could not parse settings file {}: {err}; using defaults",
                path.display()
            );
            Settings::default()
        }
    }
}

/// Serialize `settings` to `path` atomically: write to a temp file in the same directory,
/// flush, then rename over `path`. The rename is atomic (and replaces an existing file, on
/// both Windows and Unix), so a crash mid-write leaves the previous `path` intact rather than
/// a truncated/corrupt one. The temp file is removed if any step before the rename fails.
pub fn save(settings: &Settings, path: &Path) -> std::io::Result<()> {
    let toml = toml::to_string_pretty(settings)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;

    let tmp_path = tmp_path_for(path);
    if let Err(err) = write_and_sync(&tmp_path, &toml) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err);
    }

    if let Err(err) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err);
    }

    Ok(())
}

/// The temp file path used by [`save`]: `path` with `.tmp` appended, in the same directory so
/// the final rename stays on one filesystem/volume.
fn tmp_path_for(path: &Path) -> std::path::PathBuf {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::path::PathBuf::from(tmp)
}

fn write_and_sync(path: &Path, contents: &str) -> std::io::Result<()> {
    let mut file = std::fs::File::create(path)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A settings path inside a fresh temp subdirectory, so tests don't collide and cleanup
    /// is one `remove_dir_all`.
    struct TempSettingsPath {
        dir: std::path::PathBuf,
        path: std::path::PathBuf,
    }

    impl TempSettingsPath {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "simpletally-settings-test-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("system clock before UNIX epoch")
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            let path = dir.join("settings.toml");
            TempSettingsPath { dir, path }
        }
    }

    impl Drop for TempSettingsPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn round_trip_save_then_load() {
        let tmp = TempSettingsPath::new("round-trip");
        let settings = Settings {
            version: CURRENT_VERSION,
            selected_date: Some("2026-09-16".to_string()),
            selected_category: Some(42),
            theme: Theme::Dark,
            range_kind: "custom".to_string(),
            range_anchor: Some("2026-09-01".to_string()),
            custom_range_from: Some("2026-09-01".to_string()),
            custom_range_to: Some("2026-09-16".to_string()),
            active_tab: "insights".to_string(),
            note_field_open: true,
            main_window_geometry: Some(WindowGeometry {
                x: 10,
                y: 20,
                width: 1280,
                height: 720,
            }),
            hide_to_tray_notice_dismissed: true,
            close_quits: true,
        };

        save(&settings, &tmp.path).expect("save should succeed");
        let loaded = load(&tmp.path);

        assert_eq!(loaded, settings);
    }

    #[test]
    fn missing_file_returns_defaults_without_panicking() {
        let tmp = TempSettingsPath::new("missing-file");
        // Do not create tmp.path.

        let loaded = load(&tmp.path);

        assert_eq!(loaded, Settings::default());
    }

    #[test]
    fn corrupt_file_returns_defaults_not_an_error() {
        let tmp = TempSettingsPath::new("corrupt-file");
        std::fs::write(&tmp.path, b"this is not valid toml {{{ ]]] ===").expect("write garbage");

        let loaded = load(&tmp.path);

        assert_eq!(loaded, Settings::default());
    }

    #[test]
    fn partial_toml_defaults_missing_fields() {
        let tmp = TempSettingsPath::new("partial-toml");
        std::fs::write(
            &tmp.path,
            "version = 1\nactive_tab = \"types\"\nnote_field_open = true\n",
        )
        .expect("write partial toml");

        let loaded = load(&tmp.path);

        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.active_tab, "types");
        assert!(loaded.note_field_open);
        // Fields absent from the file fall back to Settings::default().
        assert_eq!(loaded.selected_date, None);
        assert_eq!(loaded.selected_category, None);
        assert_eq!(loaded.theme, Theme::System);
        assert_eq!(loaded.range_kind, "week");
        assert_eq!(loaded.main_window_geometry, None);
    }

    #[test]
    fn atomic_save_leaves_no_leftover_tmp_file() {
        let tmp = TempSettingsPath::new("no-leftover-tmp");
        let settings = Settings::default();

        save(&settings, &tmp.path).expect("save should succeed");

        assert!(tmp.path.exists());
        assert!(!tmp_path_for(&tmp.path).exists());
    }

    #[test]
    fn version_field_round_trips() {
        let tmp = TempSettingsPath::new("version-field");
        let settings = Settings {
            version: 7,
            ..Settings::default()
        };

        save(&settings, &tmp.path).expect("save should succeed");
        let loaded = load(&tmp.path);

        assert_eq!(loaded.version, 7);
    }
}
