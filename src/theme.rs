//! Light / dark theme preference, shared by every VENUS rust tool.
//!
//! The preference lives in one file (`~/.config/venus_rust_tools/theme`,
//! containing `dark` or `light`) so switching the theme in any of the tools
//! switches all of them — the next time each one starts. Dark is the default:
//! it is what every tool shipped with before the preference existed.
//!
//! This module is deliberately self-contained (egui + std only) so it can be
//! copied verbatim into the other tools' crates. egui 0.28 predates
//! `egui::Theme` and per-theme styles, so the theme is its own enum here and
//! is applied by swapping the `Visuals`.

use eframe::egui;
use std::path::PathBuf;

/// Dark or light mode; egui 0.28 has no such type of its own.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Light,
}

impl Theme {
    /// The `Visuals` implementing this theme.
    pub fn visuals(self) -> egui::Visuals {
        match self {
            Theme::Dark => egui::Visuals::dark(),
            Theme::Light => egui::Visuals::light(),
        }
    }
}

/// The preference file, under `$XDG_CONFIG_HOME` (or `~/.config`).
fn pref_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
        })?;
    Some(base.join("venus_rust_tools").join("theme"))
}

/// The saved preference, or dark when there is none (or it is unreadable).
pub fn load() -> Theme {
    match pref_path().and_then(|p| std::fs::read_to_string(p).ok()) {
        Some(s) if s.trim().eq_ignore_ascii_case("light") => Theme::Light,
        _ => Theme::Dark,
    }
}

/// Persist the preference. Best effort: a read-only home directory only
/// costs the user their choice on the next start, not an error dialog.
pub fn save(theme: Theme) {
    let Some(path) = pref_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(
        path,
        match theme {
            Theme::Light => "light\n",
            Theme::Dark => "dark\n",
        },
    );
}

/// A sun / moon button that flips the theme of the whole application and
/// saves the choice for every VENUS rust tool. Drop it anywhere in a toolbar.
pub fn toggle_button(ui: &mut egui::Ui) {
    let (icon, tip, next) = if ui.visuals().dark_mode {
        ("☀", "Switch to the light theme", Theme::Light)
    } else {
        ("🌙", "Switch to the dark theme", Theme::Dark)
    };
    if ui
        .button(icon)
        .on_hover_text(format!("{tip} (applies to all the VENUS rust tools)"))
        .clicked()
    {
        ui.ctx().set_visuals(next.visuals());
        save(next);
    }
}
