//! Text of the find options, of a search that reached its hit cap and of
//! the search result list.

use super::{Language, Strings, display_keystroke};

impl Strings {
    pub(crate) fn tip_match_case(&self) -> &'static str {
        match self.language {
            Language::English => "Match case",
            Language::TraditionalChinese => "大小寫須相符",
        }
    }

    pub(crate) fn tip_whole_word(&self) -> &'static str {
        match self.language {
            Language::English => "Match whole word",
            Language::TraditionalChinese => "全字拼寫須相符",
        }
    }

    /// "Match `index` (one-based) of at least `count`": the search stopped
    /// at its cap, more may exist.
    pub(crate) fn hit_position_capped(&self, index: usize, count: usize) -> String {
        match self.language {
            Language::English => format!("{index} of {count}+"),
            Language::TraditionalChinese => format!("第 {index}／{count}+ 筆"),
        }
    }

    /// The search stopped at `count` hits, its cap.
    pub(crate) fn limit_reached(&self, count: usize) -> String {
        match self.language {
            Language::English => format!("Stopped at {count} results"),
            Language::TraditionalChinese => format!("已達 {count} 筆上限，搜尋已停止"),
        }
    }
}

impl Strings {
    pub(crate) fn results_tab(&self) -> &'static str {
        match self.language {
            Language::English => "Results",
            Language::TraditionalChinese => "搜尋結果",
        }
    }

    pub(crate) fn tip_show_results(&self) -> &'static str {
        match self.language {
            Language::English => "List all results",
            Language::TraditionalChinese => "列出所有結果",
        }
    }

    pub(crate) fn searching(&self) -> &'static str {
        match self.language {
            Language::English => "Searching\u{2026}",
            Language::TraditionalChinese => "搜尋中\u{2026}",
        }
    }

    /// The empty result list: how to search (`find_keys`, the find
    /// command's keystroke).
    pub(crate) fn results_hint(&self, find_keys: Option<&str>) -> String {
        let keys = find_keys.map(display_keystroke);
        match (self.language, keys) {
            (Language::English, Some(keys)) => {
                format!("Search with {keys}; every match is listed here.")
            }
            (Language::English, None) => "Every match of a search is listed here.".to_owned(),
            (Language::TraditionalChinese, Some(keys)) => {
                format!("按 {keys} 搜尋，所有符合的結果都會列在這裡。")
            }
            (Language::TraditionalChinese, None) => {
                "搜尋時，所有符合的結果都會列在這裡。".to_owned()
            }
        }
    }

    /// A search that stopped at its cap looked at pages `first..=last`
    /// (one-based).
    pub(crate) fn searched_pages(&self, first: u32, last: u32) -> String {
        match self.language {
            Language::English => format!("Searched pages {first}\u{2013}{last}"),
            Language::TraditionalChinese => format!("已搜尋第 {first}\u{2013}{last} 頁"),
        }
    }

    /// The page of a result: its number, and its label when it has one.
    pub(crate) fn result_page(&self, number: u32, label: Option<&str>) -> String {
        match (self.language, label) {
            (Language::English, Some(label)) => format!("{label} (page {number})"),
            (Language::English, None) => format!("Page {number}"),
            (Language::TraditionalChinese, Some(label)) => format!("{label}（第 {number} 頁）"),
            (Language::TraditionalChinese, None) => format!("第 {number} 頁"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_languages_have_the_text() {
        let en = Language::English.strings();
        let zh = Language::TraditionalChinese.strings();
        for (en, zh) in [
            (
                en.tip_match_case().to_owned(),
                zh.tip_match_case().to_owned(),
            ),
            (
                en.tip_whole_word().to_owned(),
                zh.tip_whole_word().to_owned(),
            ),
            (en.hit_position_capped(3, 20), zh.hit_position_capped(3, 20)),
            (en.limit_reached(20), zh.limit_reached(20)),
            (en.results_tab().to_owned(), zh.results_tab().to_owned()),
            (
                en.tip_show_results().to_owned(),
                zh.tip_show_results().to_owned(),
            ),
            (en.searching().to_owned(), zh.searching().to_owned()),
            (en.results_hint(None), zh.results_hint(None)),
            (en.searched_pages(1, 2), zh.searched_pages(1, 2)),
            (en.result_page(4, None), zh.result_page(4, None)),
        ] {
            assert!(!en.is_empty());
            assert!(
                zh.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "{zh}"
            );
        }
        assert_eq!(zh.hit_position_capped(3, 20000), "第 3／20000+ 筆");
        assert_eq!(en.hit_position_capped(3, 20000), "3 of 20000+");
        assert_eq!(zh.limit_reached(20000), "已達 20000 筆上限，搜尋已停止");
        assert_eq!(en.limit_reached(20000), "Stopped at 20000 results");
        assert_eq!(
            zh.with_shortcut(zh.tip_match_case(), Some("alt-c")),
            "大小寫須相符（Alt+C）"
        );
        assert_eq!(
            en.with_shortcut(en.tip_whole_word(), Some("alt-w")),
            "Match whole word (Alt+W)"
        );
        assert_eq!(
            zh.results_hint(Some("ctrl-f")),
            "按 Ctrl+F 搜尋，所有符合的結果都會列在這裡。"
        );
        assert_eq!(
            en.results_hint(Some("ctrl-f")),
            "Search with Ctrl+F; every match is listed here."
        );
        assert_eq!(zh.searched_pages(12, 48), "已搜尋第 12\u{2013}48 頁");
        assert_eq!(en.searched_pages(12, 48), "Searched pages 12\u{2013}48");
        assert_eq!(zh.result_page(4, Some("iv")), "iv（第 4 頁）");
        assert_eq!(zh.result_page(4, None), "第 4 頁");
        assert_eq!(en.result_page(4, Some("iv")), "iv (page 4)");
        assert_eq!(en.result_page(4, None), "Page 4");
        assert_eq!(
            zh.with_shortcut(zh.tip_show_results(), Some("ctrl-shift-f")),
            "列出所有結果（Ctrl+Shift+F）"
        );
    }
}
