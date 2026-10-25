//! Where FastPDF keeps its local state (recent files, reading positions,
//! settings), and how it writes it. Nothing is ever stored anywhere else,
//! and nothing leaves the machine.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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

/// Writes `text` to `path` through a temporary file next to it
/// (`<path>.<pid>.<n>.tmp`) and a rename, so a crash never leaves a
/// half-written file and two processes (or two writers in one) never share
/// a temporary file. The directory is created when missing; the temporary
/// file is removed when the write fails.
pub fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let temp = PathBuf::from(temp);
    std::fs::write(&temp, text)
        .and_then(|()| std::fs::rename(&temp, path))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&temp);
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_writes_replace_the_file_and_leave_nothing_else() {
        let dir = std::env::temp_dir().join(format!("fastpdf-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("state.txt");
        write_atomic(&path, "one").unwrap();
        write_atomic(&path, "two").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names, ["state.txt"]);
        // A directory in the way: the write fails and leaves no temporary file.
        assert!(write_atomic(&dir.join("nested"), "x").is_err());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
