//! User-visible text in English and Traditional Chinese for Taiwan (spec
//! §27). One table per language, std only (no i18n dependency): static text
//! is a field of [`Strings`], text with numbers or names is a method that
//! formats per language. The development overlay, logs and dev-script step
//! names stay English.
//!
//! The language follows the Windows UI language unless the settings file
//! names one (`language = "zh-TW"`).

use std::fmt::Write as _;

use fastpdf_core::loader::LoadError;
use fastpdf_engine_api::EngineError;

use crate::document::OpenFailure;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Language {
    #[default]
    English,
    /// zh-TW: Traditional characters, Taiwanese wording.
    TraditionalChinese,
}

impl Language {
    pub(crate) const ALL: [Self; 2] = [Self::English, Self::TraditionalChinese];

    /// From a Windows UI language id (`GetUserDefaultUILanguage`): Chinese
    /// written in Traditional characters (Taiwan, Hong Kong, Macao) gets
    /// zh-TW, every other language English.
    pub(crate) fn from_langid(langid: u16) -> Self {
        const LANG_CHINESE: u16 = 0x04;
        let primary = langid & 0x3ff;
        let sub = langid >> 10;
        // SUBLANG_CHINESE_TRADITIONAL (TW), _HONGKONG, _MACAU.
        if primary == LANG_CHINESE && matches!(sub, 0x01 | 0x03 | 0x05) {
            Self::TraditionalChinese
        } else {
            Self::English
        }
    }

    /// BCP 47 tag as written in the settings file.
    pub(crate) fn tag(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::TraditionalChinese => "zh-TW",
        }
    }

    /// Accepts `en`, `en-US`, `zh-TW`, `zh-Hant`, `zh-HK`, ... (any case).
    pub(crate) fn from_tag(tag: &str) -> Option<Self> {
        let tag = tag.trim().replace('_', "-").to_ascii_lowercase();
        let primary = tag.split('-').next().unwrap_or_default();
        match primary {
            "en" => Some(Self::English),
            "zh" if ["tw", "hant", "hk", "mo"]
                .iter()
                .any(|s| tag.split('-').skip(1).any(|part| part == *s)) =>
            {
                Some(Self::TraditionalChinese)
            }
            _ => None,
        }
    }

    /// The language's name in that language (the settings panel lists
    /// each language so its speakers can find it).
    pub(crate) fn native_name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::TraditionalChinese => "繁體中文",
        }
    }

    pub(crate) fn strings(self) -> &'static Strings {
        match self {
            Self::English => &EN,
            Self::TraditionalChinese => &ZH_TW,
        }
    }

    /// UI font: Segoe UI for English; for Traditional Chinese the Windows
    /// zh-TW UI font, whose Latin glyphs match its Han glyphs (with Segoe
    /// UI, DirectWrite would pick a fallback for Han by the system locale,
    /// which may be a Japanese or Simplified Chinese font).
    pub(crate) fn ui_font(self) -> &'static str {
        match self {
            Self::English => "Segoe UI",
            Self::TraditionalChinese => "Microsoft JhengHei UI",
        }
    }
}

/// Every piece of static UI text.
#[derive(Debug)]
pub(crate) struct Strings {
    pub language: Language,
    // Toolbar.
    pub sidebar: &'static str,
    pub open: &'static str,
    pub fit_width: &'static str,
    pub fit_page: &'static str,
    pub find: &'static str,
    pub night: &'static str,
    pub tip_sidebar: &'static str,
    pub tip_open: &'static str,
    pub tip_previous_page: &'static str,
    pub tip_next_page: &'static str,
    pub tip_zoom_out: &'static str,
    pub tip_zoom_in: &'static str,
    pub tip_fit_width: &'static str,
    pub tip_fit_page: &'static str,
    pub tip_rotate_left: &'static str,
    pub tip_rotate_right: &'static str,
    pub tip_find: &'static str,
    pub tip_night: &'static str,
    pub tip_settings: &'static str,
    // Find bar.
    pub find_placeholder: &'static str,
    pub no_results: &'static str,
    pub search_unavailable: &'static str,
    pub tip_previous_match: &'static str,
    pub tip_next_match: &'static str,
    pub close: &'static str,
    // Print panel.
    pub print: &'static str,
    pub printer: &'static str,
    pub pages: &'static str,
    pub all_pages: &'static str,
    pub current_page: &'static str,
    pub custom_pages: &'static str,
    pub range_placeholder: &'static str,
    pub copies: &'static str,
    pub size: &'static str,
    pub shrink_to_fit: &'static str,
    pub actual_size: &'static str,
    pub cancel_printing: &'static str,
    pub no_printers: &'static str,
    pub looking_for_printers: &'static str,
    pub default_printer: &'static str,
    pub starting_print: &'static str,
    pub cancelling: &'static str,
    pub print_cancelled: &'static str,
    pub open_a_document_to_print: &'static str,
    pub no_printer_available: &'static str,
    pub invalid_page_range: &'static str,
    // Settings panel.
    pub settings: &'static str,
    pub appearance: &'static str,
    pub follow_system: &'static str,
    pub light: &'static str,
    pub dark: &'static str,
    pub night_mode: &'static str,
    pub off: &'static str,
    pub on: &'static str,
    pub default_zoom: &'static str,
    pub language_label: &'static str,
    pub smooth_scrolling: &'static str,
    pub not_saved: &'static str,
    // Sidebar.
    pub outline: &'static str,
    pub no_document: &'static str,
    pub loading: &'static str,
    pub no_outline: &'static str,
    pub untitled: &'static str,
    // Empty window and errors.
    pub empty_hint: &'static str,
    pub recent: &'static str,
    pub opening_interrupted: &'static str,
    pub file_not_found: &'static str,
    pub no_permission: &'static str,
    pub file_empty: &'static str,
    pub password_protected: &'static str,
}

const EN: Strings = Strings {
    language: Language::English,
    sidebar: "Sidebar",
    open: "Open",
    fit_width: "Fit width",
    fit_page: "Fit page",
    find: "Find",
    night: "Night",
    tip_sidebar: "Show or hide the sidebar",
    tip_open: "Open a PDF",
    tip_previous_page: "Previous page",
    tip_next_page: "Next page",
    tip_zoom_out: "Zoom out",
    tip_zoom_in: "Zoom in",
    tip_fit_width: "Fit the page width to the window",
    tip_fit_page: "Show the whole page",
    tip_rotate_left: "Rotate left",
    tip_rotate_right: "Rotate right",
    tip_find: "Find in document",
    tip_night: "Night mode: inverted page colors",
    tip_settings: "Settings",
    find_placeholder: "Find in document",
    no_results: "No results",
    search_unavailable: "Search is not available",
    tip_previous_match: "Previous match",
    tip_next_match: "Next match",
    close: "Close",
    print: "Print",
    printer: "Printer",
    pages: "Pages",
    all_pages: "All",
    current_page: "Current",
    custom_pages: "Custom",
    range_placeholder: "e.g. 1-3, 5",
    copies: "Copies",
    size: "Size",
    shrink_to_fit: "Shrink to fit",
    actual_size: "Actual size",
    cancel_printing: "Cancel printing",
    no_printers: "No printers installed",
    looking_for_printers: "Looking for printers\u{2026}",
    default_printer: "  (default)",
    starting_print: "Starting the print job\u{2026}",
    cancelling: "Cancelling\u{2026}",
    print_cancelled: "Printing cancelled.",
    open_a_document_to_print: "Open a document to print.",
    no_printer_available: "No printer is available.",
    invalid_page_range: "Invalid page range. Example: 1-3, 5",
    settings: "Settings",
    appearance: "Appearance",
    follow_system: "System",
    light: "Light",
    dark: "Dark",
    night_mode: "Night mode",
    off: "Off",
    on: "On",
    default_zoom: "Default zoom",
    language_label: "Language",
    smooth_scrolling: "Smooth scrolling",
    not_saved: "Not saved: FASTPDF_SETTINGS_FILE is empty.",
    outline: "Outline",
    no_document: "No document",
    loading: "Loading\u{2026}",
    no_outline: "This document has no outline.",
    untitled: "(untitled)",
    empty_hint: "Open a PDF with Ctrl+O, or drop one onto this window.",
    recent: "Recent",
    opening_interrupted: "Opening was interrupted.",
    file_not_found: "The file does not exist.",
    no_permission: "FastPDF is not allowed to read this file.",
    file_empty: "The file is empty.",
    password_protected: "This document is password protected. FastPDF cannot open protected documents yet.",
};

const ZH_TW: Strings = Strings {
    language: Language::TraditionalChinese,
    sidebar: "側邊欄",
    open: "開啟",
    fit_width: "符合寬度",
    fit_page: "符合頁面",
    find: "尋找",
    night: "夜間",
    tip_sidebar: "顯示或隱藏側邊欄",
    tip_open: "開啟 PDF 檔案",
    tip_previous_page: "上一頁",
    tip_next_page: "下一頁",
    tip_zoom_out: "縮小",
    tip_zoom_in: "放大",
    tip_fit_width: "頁面寬度符合視窗",
    tip_fit_page: "顯示整頁",
    tip_rotate_left: "向左旋轉",
    tip_rotate_right: "向右旋轉",
    tip_find: "在文件中尋找",
    tip_night: "夜間模式：反轉頁面顏色",
    tip_settings: "設定",
    find_placeholder: "在文件中尋找",
    no_results: "找不到符合的結果",
    search_unavailable: "這份文件無法搜尋",
    tip_previous_match: "上一筆",
    tip_next_match: "下一筆",
    close: "關閉",
    print: "列印",
    printer: "印表機",
    pages: "頁面",
    all_pages: "全部",
    current_page: "目前頁面",
    custom_pages: "自訂",
    range_placeholder: "例如 1-3, 5",
    copies: "份數",
    size: "大小",
    shrink_to_fit: "縮小以符合紙張",
    actual_size: "實際大小",
    cancel_printing: "取消列印",
    no_printers: "沒有安裝印表機",
    looking_for_printers: "正在尋找印表機\u{2026}",
    default_printer: "（預設）",
    starting_print: "正在開始列印\u{2026}",
    cancelling: "正在取消\u{2026}",
    print_cancelled: "已取消列印。",
    open_a_document_to_print: "請先開啟要列印的文件。",
    no_printer_available: "沒有可用的印表機。",
    invalid_page_range: "頁碼範圍無效，例如：1-3, 5",
    settings: "設定",
    appearance: "外觀",
    follow_system: "跟隨系統",
    light: "淺色",
    dark: "深色",
    night_mode: "夜間模式",
    off: "關閉",
    on: "開啟",
    default_zoom: "預設縮放",
    language_label: "語言",
    smooth_scrolling: "平滑捲動",
    not_saved: "不會儲存：FASTPDF_SETTINGS_FILE 為空字串。",
    outline: "大綱",
    no_document: "沒有開啟的文件",
    loading: "載入中\u{2026}",
    no_outline: "這份文件沒有大綱。",
    untitled: "（無標題）",
    empty_hint: "按 Ctrl+O 開啟 PDF，或把檔案拖曳到這個視窗。",
    recent: "最近開啟的檔案",
    opening_interrupted: "開啟作業被中斷。",
    file_not_found: "找不到這個檔案。",
    no_permission: "沒有權限讀取這個檔案。",
    file_empty: "檔案是空的。",
    password_protected: "這份文件受密碼保護，FastPDF 目前還不支援開啟加密文件。",
};

impl Strings {
    fn zh(&self) -> bool {
        self.language == Language::TraditionalChinese
    }

    /// A tooltip with its shortcut, e.g. "Open a PDF (Ctrl+O)".
    pub(crate) fn with_shortcut(&self, tip: &str, keystroke: Option<&str>) -> String {
        match keystroke {
            None => tip.to_string(),
            Some(keys) if self.zh() => format!("{tip}（{}）", display_keystroke(keys)),
            Some(keys) => format!("{tip} ({})", display_keystroke(keys)),
        }
    }

    pub(crate) fn found_so_far(&self, found: usize, pages_done: u32, page_count: u32) -> String {
        if self.zh() {
            format!("已找到 {found} 筆\u{2026}（{pages_done}／{page_count} 頁）")
        } else {
            format!("{found} found so far\u{2026} ({pages_done}/{page_count} pages)")
        }
    }

    /// "Match `index` (one-based) of `count`".
    pub(crate) fn hit_position(&self, index: usize, count: usize) -> String {
        if self.zh() {
            format!("第 {index}／{count} 筆")
        } else {
            format!("{index} of {count}")
        }
    }

    pub(crate) fn result_count(&self, count: usize) -> String {
        if self.zh() {
            format!("共 {count} 筆")
        } else {
            format!("{count} results")
        }
    }

    pub(crate) fn current_page_number(&self, page: u32) -> String {
        if self.zh() {
            format!("{}（第 {page} 頁）", self.current_page)
        } else {
            format!("{} ({page})", self.current_page)
        }
    }

    pub(crate) fn printing_page(&self, page: u32, sheet: u32, sheets: u32) -> String {
        if self.zh() {
            format!("正在列印第 {page} 頁（第 {sheet}／{sheets} 張）\u{2026}")
        } else {
            format!("Printing page {page} (sheet {sheet} of {sheets})\u{2026}")
        }
    }

    fn sheets(&self, sheets: u32) -> String {
        match (self.zh(), sheets) {
            (true, n) => format!("{n} 張"),
            (false, 1) => "1 sheet".into(),
            (false, n) => format!("{n} sheets"),
        }
    }

    /// Where a job went: the printer, or (development) a file.
    pub(crate) fn print_target(&self, printer: &str, file: Option<&std::path::Path>) -> String {
        match file {
            Some(path) if self.zh() => format!("{printer}（寫入 {}）", path.display()),
            Some(path) => format!("{printer} (into {})", path.display()),
            None => printer.to_string(),
        }
    }

    pub(crate) fn print_done(&self, sheets: u32, target: &str) -> String {
        if self.zh() {
            format!("已將 {}送至 {target}。", self.sheets(sheets))
        } else {
            format!("Sent {} to {target}.", self.sheets(sheets))
        }
    }

    /// A job that printed, with pages that came out blank. `pages` are
    /// one-based, sorted and unique; at most `listed` are named.
    pub(crate) fn print_partial(
        &self,
        sheets: u32,
        target: &str,
        pages: &[u32],
        listed: usize,
    ) -> String {
        let mut names = pages
            .iter()
            .take(listed)
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let more = pages.len().saturating_sub(listed);
        if self.zh() {
            let list = if more > 0 {
                format!("第 {names} 頁等共 {} 頁", pages.len())
            } else {
                format!("第 {names} 頁")
            };
            format!(
                "{}以下頁面無法繪製，印出空白：{list}。",
                self.print_done(sheets, target)
            )
        } else {
            if more > 0 {
                let _ = write!(names, " and {more} more");
            }
            format!(
                "{} Could not render (printed blank): page {names}.",
                self.print_done(sheets, target)
            )
        }
    }

    pub(crate) fn print_failed(&self, detail: &str) -> String {
        if self.zh() {
            format!("列印失敗：{detail}")
        } else {
            format!("Printing failed: {detail}")
        }
    }

    pub(crate) fn dev_print_output(&self, path: &std::path::Path) -> String {
        if self.zh() {
            format!("開發用：列印輸出寫入 {}", path.display())
        } else {
            format!("Development: output goes into {}", path.display())
        }
    }

    pub(crate) fn saved_in(&self, path: &std::path::Path) -> String {
        if self.zh() {
            format!("自動儲存於 {}", path.display())
        } else {
            format!("Saved automatically in {}", path.display())
        }
    }

    pub(crate) fn outline_unavailable(&self, error: &str) -> String {
        if self.zh() {
            format!("無法讀取大綱：{error}")
        } else {
            format!("Outline unavailable: {error}")
        }
    }

    pub(crate) fn thumbnail_failed(&self, page: u32) -> String {
        if self.zh() {
            format!("{page}（錯誤）")
        } else {
            format!("{page} (error)")
        }
    }

    pub(crate) fn opening(&self, name: &str) -> String {
        if self.zh() {
            format!("正在開啟 {name}\u{2026}")
        } else {
            format!("Opening {name}\u{2026}")
        }
    }

    pub(crate) fn cannot_open(&self, name: &str) -> String {
        if self.zh() {
            format!("無法開啟 {name}")
        } else {
            format!("Cannot open {name}")
        }
    }

    pub(crate) fn page_error(&self, page: u32, error: &str) -> String {
        if self.zh() {
            format!("第 {page} 頁無法繪製：{error}")
        } else {
            format!("Page {page} could not be rendered: {error}")
        }
    }

    /// Why a document did not open, for the empty window. Engine details
    /// (English, from the engine) follow in parentheses where they help.
    pub(crate) fn open_failure(&self, failure: &OpenFailure) -> String {
        let zh = self.zh();
        let detail = |zh_text: &str, en_text: &str, detail: &dyn std::fmt::Display| {
            if zh {
                format!("{zh_text}（{detail}）")
            } else {
                format!("{en_text} ({detail})")
            }
        };
        match failure {
            OpenFailure::Abandoned => self.opening_interrupted.into(),
            OpenFailure::Load(LoadError::Io(e)) => match e.kind() {
                std::io::ErrorKind::NotFound => self.file_not_found.into(),
                std::io::ErrorKind::PermissionDenied => self.no_permission.into(),
                _ => detail("無法讀取檔案", "The file cannot be read", e),
            },
            OpenFailure::Load(LoadError::Empty) => self.file_empty.into(),
            OpenFailure::Load(LoadError::TooLarge(bytes)) => {
                let mib = *bytes as f64 / (1024.0 * 1024.0);
                if zh {
                    format!("檔案太大，無法開啟（{mib:.0} MiB）。")
                } else {
                    format!("The file is too large to open ({mib:.0} MiB).")
                }
            }
            OpenFailure::Engine(EngineError::PasswordRequired | EngineError::InvalidPassword) => {
                self.password_protected.into()
            }
            OpenFailure::Engine(EngineError::Malformed(m)) => detail(
                "檔案已損毀，或不是有效的 PDF",
                "The file is damaged or is not a PDF",
                m,
            ),
            OpenFailure::Engine(EngineError::Unsupported(m)) => detail(
                "這份 PDF 使用了 FastPDF 尚未支援的功能",
                "This PDF uses a feature FastPDF does not support yet",
                m,
            ),
            OpenFailure::Engine(EngineError::LimitExceeded(kind)) => detail(
                "文件超過安全限制",
                "The document exceeds a safety limit",
                kind,
            ),
            OpenFailure::Engine(e) => detail("PDF 引擎發生錯誤", "The PDF engine failed", e),
        }
    }
}

/// `ctrl-shift-=` → `Ctrl+Shift+=`, `pagedown` → `PgDn`, `f3` → `F3`.
pub(crate) fn display_keystroke(keystroke: &str) -> String {
    let mut rest = keystroke;
    let mut parts: Vec<String> = Vec::new();
    loop {
        let modifier = [("ctrl-", "Ctrl"), ("shift-", "Shift"), ("alt-", "Alt")]
            .into_iter()
            .find(|(prefix, _)| rest.len() > prefix.len() && rest.starts_with(prefix));
        let Some((prefix, name)) = modifier else {
            break;
        };
        parts.push(name.to_string());
        rest = &rest[prefix.len()..];
    }
    let key = match rest {
        "pageup" => "PgUp".to_string(),
        "pagedown" => "PgDn".to_string(),
        "escape" => "Esc".to_string(),
        "left" => "\u{2190}".to_string(),
        "right" => "\u{2192}".to_string(),
        "up" => "\u{2191}".to_string(),
        "down" => "\u{2193}".to_string(),
        key => {
            let mut chars = key.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        }
    };
    parts.push(key);
    parts.join("+")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_ui_languages_map_to_tables() {
        assert_eq!(Language::from_langid(0x0404), Language::TraditionalChinese); // zh-TW
        assert_eq!(Language::from_langid(0x0c04), Language::TraditionalChinese); // zh-HK
        assert_eq!(Language::from_langid(0x1404), Language::TraditionalChinese); // zh-MO
        assert_eq!(Language::from_langid(0x0804), Language::English); // zh-CN
        assert_eq!(Language::from_langid(0x0409), Language::English); // en-US
        assert_eq!(Language::from_langid(0x0411), Language::English); // ja-JP
    }

    #[test]
    fn tags_round_trip_and_tolerate_spelling() {
        for language in Language::ALL {
            assert_eq!(Language::from_tag(language.tag()), Some(language));
        }
        assert_eq!(
            Language::from_tag(" ZH_hant "),
            Some(Language::TraditionalChinese)
        );
        assert_eq!(
            Language::from_tag("zh-Hant-TW"),
            Some(Language::TraditionalChinese)
        );
        assert_eq!(Language::from_tag("en-GB"), Some(Language::English));
        assert_eq!(Language::from_tag("zh-CN"), None);
        assert_eq!(Language::from_tag("fr"), None);
    }

    #[test]
    fn both_tables_are_complete() {
        // Every field is set in both tables (the compiler checks presence);
        // here: nothing left empty, and Chinese text is actually Chinese.
        for (en, zh) in [
            (EN.sidebar, ZH_TW.sidebar),
            (EN.find_placeholder, ZH_TW.find_placeholder),
            (EN.print, ZH_TW.print),
            (EN.settings, ZH_TW.settings),
            (EN.outline, ZH_TW.outline),
            (EN.empty_hint, ZH_TW.empty_hint),
            (EN.password_protected, ZH_TW.password_protected),
        ] {
            assert!(!en.is_empty() && !zh.is_empty());
            assert!(
                zh.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "{zh}"
            );
        }
        assert_eq!(EN.language, Language::English);
        assert_eq!(ZH_TW.language, Language::TraditionalChinese);
    }

    #[test]
    fn formatted_messages_follow_the_language() {
        let zh = Language::TraditionalChinese.strings();
        let en = Language::English.strings();
        assert_eq!(zh.found_so_far(3, 12, 300), "已找到 3 筆…（12／300 頁）");
        assert_eq!(
            en.found_so_far(3, 12, 300),
            "3 found so far… (12/300 pages)"
        );
        assert_eq!(zh.hit_position(1, 7), "第 1／7 筆");
        assert_eq!(en.hit_position(1, 7), "1 of 7");
        assert_eq!(en.print_done(1, "P"), "Sent 1 sheet to P.");
        assert_eq!(zh.print_done(3, "P"), "已將 3 張送至 P。");
        let many: Vec<u32> = (1..=20).collect();
        assert!(
            en.print_partial(20, "P", &many, 12)
                .ends_with("11, 12 and 8 more.")
        );
        assert!(
            zh.print_partial(20, "P", &many, 12)
                .ends_with("11, 12 頁等共 20 頁。")
        );
        assert_eq!(
            zh.with_shortcut(zh.tip_open, Some("ctrl-o")),
            "開啟 PDF 檔案（Ctrl+O）"
        );
        assert_eq!(
            en.with_shortcut("Zoom in", Some("ctrl-=")),
            "Zoom in (Ctrl+=)"
        );
    }

    #[test]
    fn open_failures_name_the_cause() {
        let zh = Language::TraditionalChinese.strings();
        let en = Language::English.strings();
        let missing = OpenFailure::Load(LoadError::Io(std::io::Error::from(
            std::io::ErrorKind::NotFound,
        )));
        assert_eq!(zh.open_failure(&missing), zh.file_not_found);
        let locked = OpenFailure::Engine(EngineError::PasswordRequired);
        assert_eq!(en.open_failure(&locked), en.password_protected);
        let broken = OpenFailure::Engine(EngineError::Malformed("no xref".into()));
        assert_eq!(
            zh.open_failure(&broken),
            "檔案已損毀，或不是有效的 PDF（no xref）"
        );
        assert!(
            en.open_failure(&OpenFailure::Load(LoadError::TooLarge(3 << 30)))
                .contains("3072 MiB")
        );
    }

    #[test]
    fn keystrokes_read_like_windows_menus() {
        assert_eq!(display_keystroke("ctrl-o"), "Ctrl+O");
        assert_eq!(display_keystroke("ctrl--"), "Ctrl+-");
        assert_eq!(display_keystroke("ctrl-shift-="), "Ctrl+Shift+=");
        assert_eq!(display_keystroke("shift-f3"), "Shift+F3");
        assert_eq!(display_keystroke("pagedown"), "PgDn");
        assert_eq!(display_keystroke("f4"), "F4");
        assert_eq!(display_keystroke("right"), "\u{2192}");
    }
}
