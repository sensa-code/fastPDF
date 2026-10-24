//! Text of the find options and of a search that reached its hit cap.

use super::{Language, Strings};

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
    }
}
