//! Names of the properties panel's sections and rows.

use super::super::{Language, Strings};

/// A section or row of the properties panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PropertyLabel {
    Description,
    Advanced,
    Security,
    Forms,
    FileName,
    Location,
    Title,
    Author,
    Subject,
    Keywords,
    Created,
    Modified,
    Creator,
    Producer,
    PdfVersion,
    Pages,
    PageSize,
    FileSize,
    Tagged,
    FastWebView,
    Encryption,
    OpenedWith,
    Printing,
    Changing,
    Assembly,
    Copying,
    AccessibilityCopying,
    Commenting,
    FillingForms,
    Signing,
    FormFields,
    SignatureFields,
    Xfa,
}

impl PropertyLabel {
    /// Every label, for the language test.
    #[cfg(test)]
    pub(crate) const ALL: [Self; 33] = [
        Self::Description,
        Self::Advanced,
        Self::Security,
        Self::Forms,
        Self::FileName,
        Self::Location,
        Self::Title,
        Self::Author,
        Self::Subject,
        Self::Keywords,
        Self::Created,
        Self::Modified,
        Self::Creator,
        Self::Producer,
        Self::PdfVersion,
        Self::Pages,
        Self::PageSize,
        Self::FileSize,
        Self::Tagged,
        Self::FastWebView,
        Self::Encryption,
        Self::OpenedWith,
        Self::Printing,
        Self::Changing,
        Self::Assembly,
        Self::Copying,
        Self::AccessibilityCopying,
        Self::Commenting,
        Self::FillingForms,
        Self::Signing,
        Self::FormFields,
        Self::SignatureFields,
        Self::Xfa,
    ];
}

impl Strings {
    pub(crate) fn property_label(&self, label: PropertyLabel) -> &'static str {
        use PropertyLabel as L;
        let zh = matches!(self.language, Language::TraditionalChinese);
        let (en, zh_tw) = match label {
            L::Description => ("Description", "描述"),
            L::Advanced => ("Advanced", "進階"),
            L::Security => ("Security", "安全性"),
            L::Forms => ("Forms and signatures", "表單與簽章"),
            L::FileName => ("File name", "檔案名稱"),
            L::Location => ("Location", "位置"),
            L::Title => ("Title", "標題"),
            L::Author => ("Author", "作者"),
            L::Subject => ("Subject", "主旨"),
            L::Keywords => ("Keywords", "關鍵字"),
            L::Created => ("Created", "建立日期"),
            L::Modified => ("Modified", "修改日期"),
            L::Creator => ("Application", "應用程式"),
            L::Producer => ("PDF producer", "PDF 產生器"),
            L::PdfVersion => ("PDF version", "PDF 版本"),
            L::Pages => ("Number of pages", "頁數"),
            L::PageSize => ("Page size", "頁面大小"),
            L::FileSize => ("File size", "檔案大小"),
            L::Tagged => ("Tagged PDF", "標記 PDF"),
            L::FastWebView => ("Fast Web View", "快速 Web 檢視"),
            L::Encryption => ("Encryption", "加密"),
            L::OpenedWith => ("Opened with", "開啟方式"),
            L::Printing => ("Printing", "列印"),
            L::Changing => ("Changing the document", "變更文件"),
            L::Assembly => ("Document assembly", "頁面組合"),
            L::Copying => ("Content copying", "內容複製"),
            L::AccessibilityCopying => ("Content copying for accessibility", "協助工具的內容複製"),
            L::Commenting => ("Commenting", "註解"),
            L::FillingForms => ("Filling of form fields", "填寫表單欄位"),
            L::Signing => ("Signing", "簽署"),
            L::FormFields => ("Form fields", "表單欄位"),
            L::SignatureFields => ("Signature fields", "簽章欄位"),
            L::Xfa => ("XFA form", "XFA 表單"),
        };
        if zh { zh_tw } else { en }
    }

    /// One line of the copied text: "Title: value".
    pub(crate) fn property_line(&self, label: PropertyLabel, value: &str) -> String {
        let label = self.property_label(label);
        match self.language {
            Language::English => format!("{label}: {value}"),
            Language::TraditionalChinese => format!("{label}：{value}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_label_exists_in_both_languages() {
        let en = Language::English.strings();
        let zh = Language::TraditionalChinese.strings();
        for label in PropertyLabel::ALL {
            let (en, zh) = (en.property_label(label), zh.property_label(label));
            assert!(!en.is_empty(), "{label:?}");
            assert!(
                zh.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "{label:?}: {zh}"
            );
        }
        assert_eq!(zh.property_line(PropertyLabel::Title, "x"), "標題：x");
        assert_eq!(en.property_line(PropertyLabel::Title, "x"), "Title: x");
    }
}
