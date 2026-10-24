//! Text of Back and Forward between views.

use super::{Language, Strings};

impl Strings {
    pub(crate) fn tip_back(&self) -> &'static str {
        match self.language {
            Language::English => "Back to the previous view",
            Language::TraditionalChinese => "返回上一個檢視",
        }
    }

    pub(crate) fn tip_forward(&self) -> &'static str {
        match self.language {
            Language::English => "Forward to the next view",
            Language::TraditionalChinese => "前往下一個檢視",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::display_keystroke;
    use super::*;

    #[test]
    fn both_languages_have_the_text() {
        let en = Language::English.strings();
        let zh = Language::TraditionalChinese.strings();
        for (en, zh) in [
            (en.tip_back(), zh.tip_back()),
            (en.tip_forward(), zh.tip_forward()),
        ] {
            assert!(!en.is_empty());
            assert!(
                zh.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "{zh}"
            );
        }
        assert_eq!(display_keystroke("alt-left"), "Alt+\u{2190}");
        assert_eq!(
            zh.with_shortcut(zh.tip_back(), Some("alt-left")),
            "返回上一個檢視（Alt+\u{2190}）"
        );
    }
}
