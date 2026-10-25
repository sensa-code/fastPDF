//! Text of the toolbar's overflow menu (`crate::toolbar`).

use super::{Language, Strings};

impl Strings {
    /// The "⋯" button that holds the controls a narrow window has no room for.
    pub(crate) fn tip_more(&self) -> &'static str {
        match self.language {
            Language::English => "More tools",
            Language::TraditionalChinese => "更多工具",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_languages_have_the_text() {
        let en = Language::English.strings().tip_more();
        let zh = Language::TraditionalChinese.strings().tip_more();
        assert!(!en.is_empty());
        assert!(
            zh.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
            "{zh}"
        );
    }
}
