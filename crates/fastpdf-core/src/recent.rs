//! Recently opened files (V0.1 "Open Recent").
//!
//! Stored locally only, as one UTF-8 path per line — nothing leaves the
//! machine (spec: zero cloud, zero telemetry). Writes go through a temporary
//! file and a rename so a crash cannot truncate the list.

use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentFiles {
    storage: PathBuf,
    entries: Vec<PathBuf>,
    max: usize,
}

impl RecentFiles {
    pub const DEFAULT_MAX: usize = 20;

    /// `%APPDATA%\FastPDF\recent.txt` on Windows,
    /// `$XDG_CONFIG_HOME/fastpdf/recent.txt` (or `~/.config/...`) elsewhere.
    pub fn default_location() -> Option<PathBuf> {
        crate::paths::config_dir().map(|d| d.join("recent.txt"))
    }

    /// Loads the list; a missing or unreadable file yields an empty list.
    pub fn load(storage: PathBuf, max: usize) -> Self {
        let entries = std::fs::read_to_string(&storage)
            .map(|text| {
                text.lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(PathBuf::from)
                    .take(max)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            storage,
            entries,
            max,
        }
    }

    pub fn entries(&self) -> &[PathBuf] {
        &self.entries
    }

    /// Moves `path` to the front, dropping duplicates and the oldest entries.
    pub fn add(&mut self, path: &Path) {
        self.remove(path);
        self.entries.insert(0, path.to_path_buf());
        self.entries.truncate(self.max);
    }

    pub fn remove(&mut self, path: &Path) {
        self.entries.retain(|p| !same_path(p, path));
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn save(&self) -> io::Result<()> {
        if let Some(dir) = self.storage.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut text = String::new();
        for p in &self.entries {
            // Paths with line breaks cannot round-trip; skip them.
            let s = p.to_string_lossy();
            if !s.contains(['\n', '\r']) {
                text.push_str(&s);
                text.push('\n');
            }
        }
        let tmp = self.storage.with_extension("tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(tmp, &self.storage)
    }
}

/// Windows paths are case-insensitive.
fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn storage(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("fastpdf-recent-{}-{name}", std::process::id()))
            .join("recent.txt")
    }

    #[test]
    fn most_recent_first_without_duplicates() {
        let mut r = RecentFiles::load(storage("order"), 3);
        for p in ["a.pdf", "b.pdf", "c.pdf", "a.pdf", "d.pdf"] {
            r.add(Path::new(p));
        }
        let names: Vec<_> = r
            .entries()
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["d.pdf", "a.pdf", "c.pdf"]);
    }

    #[test]
    fn round_trips_through_disk() {
        let path = storage("disk");
        let mut r = RecentFiles::load(path.clone(), 10);
        r.add(Path::new(r"C:\文件\報告.pdf"));
        r.add(Path::new("/tmp/x.pdf"));
        r.save().unwrap();
        let loaded = RecentFiles::load(path.clone(), 10);
        assert_eq!(loaded.entries(), r.entries());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn missing_storage_is_empty() {
        assert!(
            RecentFiles::load(storage("missing"), 5)
                .entries()
                .is_empty()
        );
    }
}
