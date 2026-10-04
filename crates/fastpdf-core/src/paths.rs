//! Where FastPDF keeps its local state (recent files, settings). Nothing is
//! ever stored anywhere else, and nothing leaves the machine.

use std::path::PathBuf;

/// `%APPDATA%\FastPDF` on Windows; `$XDG_CONFIG_HOME/fastpdf` (or
/// `~/.config/fastpdf`) elsewhere. `None` when the environment gives no
/// usable location.
pub fn config_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("FastPDF"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .map(|d| d.join("fastpdf"))
    }
}
