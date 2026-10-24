//! Text of page labels and the go-to-page field.

use super::{Language, Strings};

impl Strings {
    pub(crate) fn tip_go_to_page(&self) -> &'static str {
        match self.language {
            Language::English => "Go to a page: type its label or number; #12 is the 12th page",
            Language::TraditionalChinese => "跳至頁面：輸入頁面標籤或頁碼，#12 指第 12 頁",
        }
    }

    pub(crate) fn go_to_placeholder(&self) -> &'static str {
        match self.language {
            Language::English => "Page",
            Language::TraditionalChinese => "頁碼",
        }
    }

    pub(crate) fn go_to_no_such_page(&self) -> &'static str {
        match self.language {
            Language::English => "No such page",
            Language::TraditionalChinese => "找不到這一頁",
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
            (en.tip_go_to_page(), zh.tip_go_to_page()),
            (en.go_to_placeholder(), zh.go_to_placeholder()),
            (en.go_to_no_such_page(), zh.go_to_no_such_page()),
        ] {
            assert!(!en.is_empty());
            assert!(
                zh.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "{zh}"
            );
        }
        assert_eq!(
            zh.with_shortcut(zh.tip_go_to_page(), Some("ctrl-g")),
            "跳至頁面：輸入頁面標籤或頁碼，#12 指第 12 頁（Ctrl+G）"
        );
        assert_eq!(
            en.with_shortcut(en.tip_go_to_page(), Some("ctrl-g")),
            "Go to a page: type its label or number; #12 is the 12th page (Ctrl+G)"
        );
    }
}
