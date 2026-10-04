//! UI settings kept between runs (M7): appearance, night mode, sidebar,
//! default zoom and window placement.
//!
//! The file is a handful of `key = value` lines, a subset of TOML, read and
//! written with std only (spec §36: no serde / toml for this). Reading never
//! fails: a missing, unreadable, oversized or corrupt file yields the
//! defaults, and an unknown key or bad value only loses that one entry.
//! Writes go through a temporary file and a rename, so a crash cannot leave
//! a half-written file behind.
//!
//! ```toml
//! appearance = "dark"
//! night_mode = true
//! sidebar_open = true
//! sidebar_tab = "pages"
//! default_zoom = "fit-page"
//! window = [120, 80, 900, 1100]
//! window_maximized = false
//! ```

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use gpui::BackgroundExecutor;

use crate::sidebar::SidebarTab;

/// Name of the file in the per-user FastPDF directory.
pub(crate) const FILE_NAME: &str = "settings.toml";
/// Larger files are not ours; they are ignored rather than parsed.
const MAX_FILE_BYTES: u64 = 64 * 1024;
/// Smallest window (logical pixels): the reader's minimum size, and the
/// smallest saved placement that is believed.
pub(crate) const MIN_WINDOW: (f32, f32) = (360.0, 240.0);
const MAX_WINDOW_EXTENT: f32 = 100_000.0;

/// Light or dark chrome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Appearance {
    /// Follow the Windows app mode (Settings > Personalization > Colors).
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub(crate) const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|a| a.name().eq_ignore_ascii_case(name.trim()))
    }
}

/// Zoom a newly opened document starts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum DefaultZoom {
    #[default]
    FitWidth,
    FitPage,
    ActualSize,
}

impl DefaultZoom {
    pub(crate) const ALL: [Self; 3] = [Self::FitWidth, Self::FitPage, Self::ActualSize];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::FitWidth => "fit-width",
            Self::FitPage => "fit-page",
            Self::ActualSize => "actual-size",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::FitWidth => "Fit width",
            Self::FitPage => "Fit page",
            Self::ActualSize => "Actual size",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|z| z.name().eq_ignore_ascii_case(name.trim()))
    }
}

fn tab_name(tab: SidebarTab) -> &'static str {
    match tab {
        SidebarTab::Outline => "outline",
        SidebarTab::Pages => "pages",
    }
}

pub(crate) fn tab_from_name(name: &str) -> Option<SidebarTab> {
    [SidebarTab::Outline, SidebarTab::Pages]
        .into_iter()
        .find(|t| tab_name(*t).eq_ignore_ascii_case(name.trim()))
}

/// Where the window was, in GPUI logical pixels: its restore bounds, and
/// whether it was maximized.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct WindowPlacement {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
}

impl WindowPlacement {
    /// Rejects values no real window has (NaN, tiny, enormous, far away).
    pub(crate) fn is_plausible(&self) -> bool {
        let finite = [self.x, self.y, self.width, self.height]
            .iter()
            .all(|v| v.is_finite());
        finite
            && (MIN_WINDOW.0..=MAX_WINDOW_EXTENT).contains(&self.width)
            && (MIN_WINDOW.1..=MAX_WINDOW_EXTENT).contains(&self.height)
            && self.x.abs() <= MAX_WINDOW_EXTENT
            && self.y.abs() <= MAX_WINDOW_EXTENT
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Settings {
    pub appearance: Appearance,
    /// Pages drawn inverted (spec §33 night mode).
    pub night_mode: bool,
    pub sidebar_open: bool,
    pub sidebar_tab: SidebarTab,
    pub default_zoom: DefaultZoom,
    pub window: Option<WindowPlacement>,
}

/// One parsed right-hand side.
#[derive(Debug, PartialEq)]
enum Value<'a> {
    Text(&'a str),
    Bool(bool),
    Numbers(Vec<f32>),
}

/// `"quoted"`, `true` / `false`, `[1, 2.5]`, or a bare word; a trailing
/// `# comment` is allowed. `None` for anything else.
fn parse_value(raw: &str) -> Option<Value<'_>> {
    let raw = raw.trim();
    let after = |rest: &str| {
        let rest = rest.trim();
        rest.is_empty() || rest.starts_with('#')
    };
    if let Some(body) = raw.strip_prefix('"') {
        let end = body.find('"')?;
        return after(&body[end + 1..]).then_some(Value::Text(&body[..end]));
    }
    if let Some(body) = raw.strip_prefix('[') {
        let end = body.find(']')?;
        if !after(&body[end + 1..]) {
            return None;
        }
        let numbers = body[..end]
            .split(',')
            .map(|n| n.trim().parse::<f32>().ok())
            .collect::<Option<Vec<f32>>>()?;
        return Some(Value::Numbers(numbers));
    }
    let word = raw.split('#').next().unwrap_or_default().trim();
    match word {
        "" => None,
        "true" => Some(Value::Bool(true)),
        "false" => Some(Value::Bool(false)),
        word if word.contains(char::is_whitespace) => None,
        word => Some(Value::Text(word)),
    }
}

impl Settings {
    /// Parses settings text. Never fails: every entry that cannot be used
    /// is skipped and described in the returned problems.
    pub(crate) fn parse(text: &str) -> (Self, Vec<String>) {
        let mut settings = Self::default();
        let mut problems = Vec::new();
        let mut window: Option<[f32; 4]> = None;
        let mut maximized = false;
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        for (number, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut bad = |what: &str| problems.push(format!("line {}: {what}", number + 1));
            let Some((key, raw)) = line.split_once('=') else {
                bad("not a `key = value` line");
                continue;
            };
            let key = key.trim();
            let Some(value) = parse_value(raw) else {
                bad(&format!("bad value for `{key}`"));
                continue;
            };
            let applied = match (key, value) {
                ("appearance", Value::Text(name)) => Appearance::from_name(name)
                    .map(|a| settings.appearance = a)
                    .is_some(),
                ("night_mode", Value::Bool(on)) => {
                    settings.night_mode = on;
                    true
                }
                ("sidebar_open", Value::Bool(open)) => {
                    settings.sidebar_open = open;
                    true
                }
                ("sidebar_tab", Value::Text(name)) => tab_from_name(name)
                    .map(|t| settings.sidebar_tab = t)
                    .is_some(),
                ("default_zoom", Value::Text(name)) => DefaultZoom::from_name(name)
                    .map(|z| settings.default_zoom = z)
                    .is_some(),
                ("window", Value::Numbers(n)) if n.len() == 4 => {
                    window = Some([n[0], n[1], n[2], n[3]]);
                    true
                }
                ("window_maximized", Value::Bool(on)) => {
                    maximized = on;
                    true
                }
                (
                    "appearance" | "night_mode" | "sidebar_open" | "sidebar_tab" | "default_zoom"
                    | "window" | "window_maximized",
                    _,
                ) => false,
                _ => {
                    bad(&format!("unknown key `{key}`"));
                    continue;
                }
            };
            if !applied {
                bad(&format!("unusable value for `{key}`"));
            }
        }
        if let Some([x, y, width, height]) = window {
            let placement = WindowPlacement {
                x,
                y,
                width,
                height,
                maximized,
            };
            if placement.is_plausible() {
                settings.window = Some(placement);
            } else {
                problems.push("implausible window placement ignored".into());
            }
        }
        (settings, problems)
    }

    /// The file contents for these settings.
    pub(crate) fn to_text(&self) -> String {
        let mut out = String::from(
            "# FastPDF settings, written by FastPDF. Unknown keys and bad values are ignored.\n\
             # appearance: \"system\", \"light\" or \"dark\"\n\
             # default_zoom: \"fit-width\", \"fit-page\" or \"actual-size\"\n",
        );
        let mut line = |key: &str, value: String| {
            out.push_str(key);
            out.push_str(" = ");
            out.push_str(&value);
            out.push('\n');
        };
        line("appearance", format!("\"{}\"", self.appearance.name()));
        line("night_mode", self.night_mode.to_string());
        line("sidebar_open", self.sidebar_open.to_string());
        line("sidebar_tab", format!("\"{}\"", tab_name(self.sidebar_tab)));
        line("default_zoom", format!("\"{}\"", self.default_zoom.name()));
        if let Some(w) = self.window.filter(WindowPlacement::is_plausible) {
            line(
                "window",
                format!("[{}, {}, {}, {}]", w.x, w.y, w.width, w.height),
            );
            line("window_maximized", w.maximized.to_string());
        }
        out
    }

    /// Reads `path`; defaults when it is missing or cannot be used.
    pub(crate) fn load(path: &Path) -> Self {
        match read_limited(path) {
            Ok(None) => Self::default(),
            Ok(Some(text)) => {
                let (settings, problems) = Self::parse(&text);
                for problem in problems {
                    log::warn!("settings {}: {problem}", path.display());
                }
                settings
            }
            Err(e) => {
                log::warn!("settings {} ignored ({e}); using defaults", path.display());
                Self::default()
            }
        }
    }
}

/// `%APPDATA%\FastPDF\settings.toml` (next to the recent-files list).
pub(crate) fn default_location() -> Option<PathBuf> {
    fastpdf_core::paths::config_dir().map(|d| d.join(FILE_NAME))
}

/// The file's text; `None` when it does not exist.
fn read_limited(path: &Path) -> io::Result<Option<String>> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if !meta.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(io::Error::other(format!(
            "larger than {MAX_FILE_BYTES} bytes"
        )));
    }
    let bytes = std::fs::read(path)?;
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

/// Writes `text` to `path` through a temporary file and a rename.
fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(".{}.tmp", std::process::id()));
    let temp = PathBuf::from(temp);
    std::fs::write(&temp, text)?;
    std::fs::rename(&temp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}

/// Saves settings when they change. Background writes are ordered: an
/// older snapshot never overwrites a newer one.
#[derive(Debug, Clone)]
pub(crate) struct SettingsStore {
    path: Option<PathBuf>,
    /// The settings as last written (or loaded).
    saved: Settings,
    generation: u64,
    /// Generation of the newest snapshot on disk; also serializes writes.
    written: Arc<Mutex<u64>>,
}

impl SettingsStore {
    /// `path`: where settings live; `None` keeps them for this run only.
    pub(crate) fn new(path: Option<PathBuf>, loaded: &Settings) -> Self {
        Self {
            path,
            saved: loaded.clone(),
            generation: 0,
            written: Arc::new(Mutex::new(0)),
        }
    }

    pub(crate) fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Next snapshot to write, or `None` when nothing changed.
    fn snapshot(&mut self, settings: &Settings) -> Option<(PathBuf, String, u64)> {
        let path = self.path.clone()?;
        if *settings == self.saved {
            return None;
        }
        self.saved = settings.clone();
        self.generation += 1;
        Some((path, settings.to_text(), self.generation))
    }

    /// Writes changed settings on a background thread.
    pub(crate) fn save(&mut self, settings: &Settings, executor: &BackgroundExecutor) {
        if let Some((path, text, generation)) = self.snapshot(settings) {
            let written = Arc::clone(&self.written);
            executor
                .spawn(async move { write_ordered(&written, &path, &text, generation) })
                .detach();
        }
    }

    /// Writes changed settings on the calling thread (tests).
    #[cfg(test)]
    pub(crate) fn save_now(&mut self, settings: &Settings) {
        if let Some((path, text, generation)) = self.snapshot(settings) {
            write_ordered(&self.written, &path, &text, generation);
        }
    }
}

fn write_ordered(written: &Mutex<u64>, path: &Path, text: &str, generation: u64) {
    let mut newest = written.lock().unwrap_or_else(|e| e.into_inner());
    if *newest > generation {
        return; // a newer snapshot is already on disk
    }
    match write_atomic(path, text) {
        Ok(()) => *newest = generation,
        Err(e) => log::warn!("cannot save settings to {}: {e}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Settings {
        Settings {
            appearance: Appearance::Dark,
            night_mode: true,
            sidebar_open: true,
            sidebar_tab: SidebarTab::Pages,
            default_zoom: DefaultZoom::FitPage,
            window: Some(WindowPlacement {
                x: -1200.5,
                y: 40.0,
                width: 900.25,
                height: 1100.0,
                maximized: true,
            }),
        }
    }

    /// A unique scratch directory under the system temp directory.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fastpdf-settings-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn settings_round_trip_through_text() {
        let s = sample();
        let (back, problems) = Settings::parse(&s.to_text());
        assert_eq!(back, s);
        assert!(problems.is_empty(), "{problems:?}");
        let (default, problems) = Settings::parse(&Settings::default().to_text());
        assert_eq!(default, Settings::default());
        assert!(problems.is_empty());
    }

    #[test]
    fn hand_edited_files_are_accepted() {
        let text = "\u{feff}# mine\r\n\
                    appearance = Dark   # bare word, any case\r\n\
                    night_mode=true\r\n\
                    \r\n\
                    default_zoom = \"ACTUAL-SIZE\" # comment\r\n\
                    window = [ 10 , 20, 800, 600 ]\r\n";
        let (s, problems) = Settings::parse(text);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(s.appearance, Appearance::Dark);
        assert!(s.night_mode);
        assert_eq!(s.default_zoom, DefaultZoom::ActualSize);
        assert_eq!(
            s.window,
            Some(WindowPlacement {
                x: 10.0,
                y: 20.0,
                width: 800.0,
                height: 600.0,
                maximized: false
            })
        );
    }

    #[test]
    fn bad_entries_are_skipped_one_by_one() {
        let text = "appearance = \"purple\"\n\
                    night_mode = yes\n\
                    sidebar_open = true\n\
                    colour = \"red\"\n\
                    just some words\n\
                    sidebar_tab = \"pages\n\
                    window = [1, 2, 3]\n\
                    default_zoom = \"fit-page\"\n";
        let (s, problems) = Settings::parse(text);
        assert_eq!(s.appearance, Appearance::System);
        assert!(!s.night_mode);
        assert!(s.sidebar_open, "good lines still apply");
        assert_eq!(s.sidebar_tab, SidebarTab::Outline);
        assert_eq!(s.default_zoom, DefaultZoom::FitPage);
        assert_eq!(s.window, None);
        assert_eq!(problems.len(), 6, "{problems:?}");
        assert!(problems[3].starts_with("line 5"));
    }

    #[test]
    fn implausible_windows_are_dropped() {
        for window in [
            "[0, 0, 10, 10]",
            "[0, 0, 800, 1e9]",
            "[NaN, 0, 800, 600]",
            "[0, inf, 800, 600]",
            "[1e7, 0, 800, 600]",
        ] {
            let (s, problems) = Settings::parse(&format!("window = {window}\n"));
            assert_eq!(s.window, None, "{window}");
            assert_eq!(problems.len(), 1, "{window}: {problems:?}");
        }
        let mut s = sample();
        if let Some(w) = s.window.as_mut() {
            w.width = f32::NAN;
        }
        assert!(!s.to_text().contains("window"), "never written either");
    }

    #[test]
    fn garbage_yields_defaults() {
        let (s, _) = Settings::parse("\u{0}\u{1}\u{fffd}=\u{fffd}\n[[[\n= = =\n\"");
        assert_eq!(s, Settings::default());
        let (s, problems) = Settings::parse("");
        assert_eq!(s, Settings::default());
        assert!(problems.is_empty());
    }

    #[test]
    fn load_and_save_survive_missing_corrupt_and_odd_files() {
        let dir = scratch("io");
        let path = dir.join("nested").join(FILE_NAME);
        assert_eq!(Settings::load(&path), Settings::default(), "missing");

        let mut store = SettingsStore::new(Some(path.clone()), &Settings::default());
        store.save_now(&Settings::default());
        assert!(!path.exists(), "unchanged settings are not written");
        store.save_now(&sample());
        assert_eq!(Settings::load(&path), sample());
        let leftovers = std::fs::read_dir(path.parent().unwrap_or(&dir))
            .map(|d| d.count())
            .unwrap_or(0);
        assert_eq!(leftovers, 1, "no temporary file left behind");

        std::fs::write(&path, [0xff, 0xfe, b'=', 0x00, 0xc3]).ok();
        assert_eq!(Settings::load(&path), Settings::default(), "not UTF-8");
        std::fs::write(&path, vec![b'#'; MAX_FILE_BYTES as usize + 1]).ok();
        assert_eq!(Settings::load(&path), Settings::default(), "too large");
        assert_eq!(Settings::load(&dir), Settings::default(), "a directory");

        let mut off = SettingsStore::new(None, &Settings::default());
        off.save_now(&sample());
        assert!(off.path().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn older_snapshots_never_overwrite_newer_ones() {
        let dir = scratch("order");
        let path = dir.join(FILE_NAME);
        let written = Mutex::new(0);
        let newer = sample();
        write_ordered(&written, &path, &newer.to_text(), 2);
        write_ordered(&written, &path, &Settings::default().to_text(), 1);
        assert_eq!(Settings::load(&path), newer);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_parse_back() {
        for a in Appearance::ALL {
            assert_eq!(Appearance::from_name(a.name()), Some(a));
        }
        for z in DefaultZoom::ALL {
            assert_eq!(DefaultZoom::from_name(z.name()), Some(z));
        }
        assert_eq!(tab_from_name(" Pages "), Some(SidebarTab::Pages));
        assert_eq!(Appearance::from_name("auto"), None);
    }
}
