//! Text of the password prompt for encrypted documents.

use super::{Language, Strings};

impl Strings {
    pub(crate) fn password_required(&self) -> &'static str {
        match self.language {
            Language::English => "Password required",
            Language::TraditionalChinese => "需要密碼",
        }
    }

    pub(crate) fn password_hint(&self) -> &'static str {
        match self.language {
            Language::English => "Enter the password to open this document.",
            Language::TraditionalChinese => "請輸入密碼以開啟這份文件。",
        }
    }

    pub(crate) fn password_placeholder(&self) -> &'static str {
        match self.language {
            Language::English => "Password",
            Language::TraditionalChinese => "密碼",
        }
    }

    pub(crate) fn password_wrong(&self) -> &'static str {
        match self.language {
            Language::English => "The password is incorrect. Try again.",
            Language::TraditionalChinese => "密碼不正確，請再試一次。",
        }
    }

    pub(crate) fn password_checking(&self) -> &'static str {
        match self.language {
            Language::English => "Checking the password\u{2026}",
            Language::TraditionalChinese => "正在確認密碼\u{2026}",
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
            (en.password_required(), zh.password_required()),
            (en.password_hint(), zh.password_hint()),
            (en.password_placeholder(), zh.password_placeholder()),
            (en.password_wrong(), zh.password_wrong()),
            (en.password_checking(), zh.password_checking()),
        ] {
            assert!(!en.is_empty());
            assert!(
                zh.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "{zh}"
            );
        }
    }
}
