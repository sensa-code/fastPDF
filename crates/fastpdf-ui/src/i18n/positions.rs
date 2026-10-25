//! Text of the reading positions setting (`crate::reading`).

use super::{Language, Strings};

impl Strings {
    /// The settings panel's switch.
    pub(crate) fn remember_position(&self) -> &'static str {
        match self.language {
            Language::English => "Remember position",
            Language::TraditionalChinese => "記住閱讀位置",
        }
    }

    /// Under the switch when there is nowhere to keep positions.
    pub(crate) fn positions_not_kept(&self) -> &'static str {
        match self.language {
            Language::English => "Reading positions are not kept: FASTPDF_RECENT_FILE is empty.",
            Language::TraditionalChinese => "不會記住閱讀位置：FASTPDF_RECENT_FILE 為空字串。",
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
            (en.remember_position(), zh.remember_position()),
            (en.positions_not_kept(), zh.positions_not_kept()),
        ] {
            assert!(!en.is_empty());
            assert!(
                zh.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "{zh}"
            );
        }
    }
}
