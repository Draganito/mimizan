//! Where the programs keep what is the user's and is not a picture: the
//! settings and the caches. The darkroom and the loupe share the folders.

use std::path::PathBuf;

/// Login directory. `$HOME` on Linux and macOS. On Windows,
/// `$USERPROFILE` when a Unix shell has not set `$HOME`.
pub fn home_dir() -> Option<PathBuf> {
    let set = |name: &str| std::env::var_os(name).filter(|h| !h.is_empty()).map(PathBuf::from);
    set("HOME").or_else(|| set("USERPROFILE"))
}

/// `$XDG_CONFIG_HOME/mimizan`, `%APPDATA%/mimizan`, or `~/.config/mimizan`.
pub fn settings_dir() -> Option<PathBuf> {
    Some(xdg_or_home("XDG_CONFIG_HOME", "APPDATA", ".config")?.join("mimizan"))
}

/// `$XDG_CACHE_HOME/mimizan`, `%LOCALAPPDATA%/mimizan`, or
/// `~/.cache/mimizan`.
pub fn cache_dir() -> Option<PathBuf> {
    Some(xdg_or_home("XDG_CACHE_HOME", "LOCALAPPDATA", ".cache")?.join("mimizan"))
}

/// `$XDG_*` when it is absolute, otherwise the Windows known folder,
/// otherwise `$HOME/<fallback>`.
fn xdg_or_home(xdg: &str, windows: &str, fallback: &str) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(xdg).map(PathBuf::from).filter(|p| p.is_absolute()) {
        return Some(path);
    }
    if cfg!(windows) {
        if let Some(path) = std::env::var_os(windows).filter(|h| !h.is_empty()) {
            return Some(PathBuf::from(path));
        }
    }
    Some(home_dir()?.join(fallback))
}
