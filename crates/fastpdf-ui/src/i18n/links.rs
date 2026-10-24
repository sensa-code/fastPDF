//! Text about links: where the link under the pointer goes, why FastPDF
//! does not follow one, and the card that asks before an external link
//! opens.

use fastpdf_shell::{OpenError, UriKind, UriRejection};

use super::{Language, Strings};
use crate::links::LinkBlock;

impl Strings {
    /// The link shows page `page` (one-based) of the document.
    pub(crate) fn link_go_to_page(&self, page: u32) -> String {
        match self.language {
            Language::English => format!("Go to page {page}"),
            Language::TraditionalChinese => format!("前往第 {page} 頁"),
        }
    }

    /// The link opens the web page `url` (after a confirmation).
    pub(crate) fn link_open_web(&self, url: &str) -> String {
        match self.language {
            Language::English => format!("Open web page: {url}"),
            Language::TraditionalChinese => format!("開啟網頁：{url}"),
        }
    }

    /// The link writes an email (`url` is the mailto address).
    pub(crate) fn link_send_mail(&self, url: &str) -> String {
        match self.language {
            Language::English => format!("Send email: {url}"),
            Language::TraditionalChinese => format!("寄送郵件：{url}"),
        }
    }

    /// FastPDF does not follow the link, because `why`.
    pub(crate) fn link_not_followed(&self, why: &str) -> String {
        match self.language {
            Language::English => format!("FastPDF does not follow this link: {why}"),
            Language::TraditionalChinese => format!("FastPDF 不會開啟這個連結：{why}"),
        }
    }

    /// Why a link is not followed, to go into [`Self::link_not_followed`].
    pub(crate) fn link_block_reason(&self, why: &LinkBlock) -> String {
        let zh = self.language == Language::TraditionalChinese;
        let text = |en: &str, tw: &str| if zh { tw.to_owned() } else { en.to_owned() };
        match why {
            LinkBlock::Action => text(
                "it would run a program, open another file or run a script, or its destination is missing",
                "它會執行程式、開啟其他檔案或指令碼，或目的地不存在",
            ),
            LinkBlock::MissingPage => text("the target page does not exist", "目的頁不存在"),
            LinkBlock::Uri(UriRejection::Scheme(scheme)) => {
                if zh {
                    format!("不支援「{scheme}:」連結")
                } else {
                    format!("\u{201c}{scheme}:\u{201d} links are not supported")
                }
            }
            LinkBlock::Uri(UriRejection::Malformed) => {
                text("the address is malformed", "網址格式不正確")
            }
            LinkBlock::Uri(UriRejection::TooLong) => text("the address is too long", "網址太長"),
            LinkBlock::Uri(UriRejection::Credentials) => text(
                "the address contains a user name or password",
                "網址含有帳號或密碼",
            ),
            LinkBlock::Uri(UriRejection::UnsafeCharacters) => text(
                "the address contains unsafe characters",
                "網址含有不安全的字元",
            ),
            LinkBlock::Uri(UriRejection::Empty) => text("the address is empty", "網址是空的"),
            // A reason a later fastpdf-shell adds.
            LinkBlock::Uri(_) => text("the address is not supported", "不支援這個網址"),
        }
    }

    /// The question on the card.
    pub(crate) fn link_prompt_title(&self, kind: UriKind) -> &'static str {
        match (self.language, kind) {
            (Language::English, UriKind::Web) => "Open this link in your browser?",
            (Language::English, UriKind::Mail) => "Open this link in your email app?",
            (Language::TraditionalChinese, UriKind::Web) => "要在瀏覽器開啟這個連結嗎？",
            (Language::TraditionalChinese, UriKind::Mail) => "要用郵件程式開啟這個連結嗎？",
        }
    }

    /// The web site the link goes to (`host`: punycode for international
    /// names).
    pub(crate) fn link_prompt_site(&self, host: &str) -> String {
        match self.language {
            Language::English => format!("Site: {host}"),
            Language::TraditionalChinese => format!("網站：{host}"),
        }
    }

    pub(crate) fn link_prompt_note(&self) -> &'static str {
        match self.language {
            Language::English => {
                "This link comes from the PDF document. Open it only if you trust the address."
            }
            Language::TraditionalChinese => "這個連結來自 PDF 文件，請確認網址可信再開啟。",
        }
    }

    pub(crate) fn link_open(&self) -> &'static str {
        match self.language {
            Language::English => "Open",
            Language::TraditionalChinese => "開啟",
        }
    }

    pub(crate) fn link_copy_address(&self) -> &'static str {
        match self.language {
            Language::English => "Copy address",
            Language::TraditionalChinese => "複製網址",
        }
    }

    /// The link did not open.
    pub(crate) fn link_open_failed(&self, why: &OpenError) -> String {
        let zh = self.language == Language::TraditionalChinese;
        let why = match (why, zh) {
            (OpenError::NoHandler, false) => "no app is set up to open this kind of link".into(),
            (OpenError::NoHandler, true) => "沒有可以開啟這種連結的程式".into(),
            (OpenError::Cancelled, false) => "it was cancelled".into(),
            (OpenError::Cancelled, true) => "已取消".into(),
            (OpenError::Win32(code), false) => format!("Windows error {code}"),
            (OpenError::Win32(code), true) => format!("Windows 錯誤 {code}"),
            (OpenError::Unsupported, false) => "opening links is not supported here".into(),
            (OpenError::Unsupported, true) => "這裡無法開啟連結".into(),
        };
        if zh {
            format!("無法開啟連結：{why}")
        } else {
            format!("Cannot open the link: {why}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn han(text: &str) -> bool {
        text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
    }

    #[test]
    fn every_reason_has_both_languages() {
        let en = Language::English.strings();
        let zh = Language::TraditionalChinese.strings();
        let reasons = [
            LinkBlock::Action,
            LinkBlock::MissingPage,
            LinkBlock::Uri(UriRejection::Empty),
            LinkBlock::Uri(UriRejection::TooLong),
            LinkBlock::Uri(UriRejection::Malformed),
            LinkBlock::Uri(UriRejection::Scheme("file".into())),
            LinkBlock::Uri(UriRejection::Credentials),
            LinkBlock::Uri(UriRejection::UnsafeCharacters),
        ];
        for why in &reasons {
            let (e, z) = (en.link_block_reason(why), zh.link_block_reason(why));
            assert!(!e.is_empty() && !han(&e), "{why:?}: {e}");
            assert!(han(&z), "{why:?}: {z}");
        }
        assert_eq!(
            en.link_not_followed(&en.link_block_reason(&reasons[5])),
            "FastPDF does not follow this link: \u{201c}file:\u{201d} links are not supported"
        );
        assert_eq!(zh.link_go_to_page(3), "前往第 3 頁");
        assert_eq!(en.link_go_to_page(3), "Go to page 3");
        for text in [
            zh.link_open_web("https://example.invalid/"),
            zh.link_send_mail("mailto:a@example.invalid"),
            zh.link_not_followed("x"),
        ] {
            assert!(han(&text), "{text}");
        }
    }

    #[test]
    fn the_card_and_failures_have_both_languages() {
        let en = Language::English.strings();
        let zh = Language::TraditionalChinese.strings();
        for kind in [UriKind::Web, UriKind::Mail] {
            assert!(!han(en.link_prompt_title(kind)));
            assert!(han(zh.link_prompt_title(kind)));
        }
        for (e, z) in [
            (en.link_prompt_note(), zh.link_prompt_note()),
            (en.link_open(), zh.link_open()),
            (en.link_copy_address(), zh.link_copy_address()),
        ] {
            assert!(!e.is_empty() && !han(e), "{e}");
            assert!(han(z), "{z}");
        }
        assert_eq!(
            zh.link_prompt_site("example.invalid"),
            "網站：example.invalid"
        );
        for why in [
            OpenError::NoHandler,
            OpenError::Cancelled,
            OpenError::Win32(5),
            OpenError::Unsupported,
        ] {
            let (e, z) = (en.link_open_failed(&why), zh.link_open_failed(&why));
            assert!(
                e.starts_with("Cannot open the link: ") && e.len() > 22,
                "{e}"
            );
            assert!(
                z.starts_with("無法開啟連結：") && han(&z[21..]) || z.contains("Windows"),
                "{z}"
            );
        }
    }
}
