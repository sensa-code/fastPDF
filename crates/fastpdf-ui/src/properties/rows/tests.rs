use fastpdf_engine_api::{
    DocumentMetadata, EncryptionMethod, FormSummary, PrintPermission, Security,
};

use super::*;
use crate::i18n::Language;

fn value(sections: &[Section], label: L) -> Option<&str> {
    sections
        .iter()
        .flat_map(|s| &s.rows)
        .find(|r| r.label == label)
        .map(|r| r.value.as_str())
}

fn facts(answer: Answer<'_>) -> Facts<'_> {
    Facts {
        path: Path::new(r"C:\docs\a.pdf"),
        file_bytes: 1_289_331,
        page_count: 3,
        page: PageIndex::new(1),
        page_size: Some(PageSize::new(595.28, 841.89)),
        password: false,
        answer,
    }
}

fn encrypted() -> DocumentProperties {
    DocumentProperties {
        metadata: DocumentMetadata {
            title: Some("加密測試文件 Encrypted fixture".into()),
            creation_date: Some("D:20261024153000+08'00'".into()),
            pdf_version: Some("1.6".into()),
            encrypted: true,
            ..DocumentMetadata::default()
        },
        security: Some(Security {
            handler: Some("Standard".into()),
            method: EncryptionMethod::Aes128,
            version: Some(4),
            revision: Some(4),
            permissions: Some(Permissions::from_p(0x804, 4)),
            encrypt_metadata: true,
            user_password: false,
        }),
        forms: Some(FormSummary {
            fields: 20_000,
            signature_fields: 3,
            signatures: 1,
            xfa: false,
            truncated: true,
        }),
        tagged: Some(true),
        linearized: Some(false),
    }
}

#[test]
fn rows_while_reading_and_without_an_answer() {
    let zh = Language::TraditionalChinese.strings();
    let pending = sections(&facts(Answer::Pending), zh);
    assert_eq!(value(&pending, L::FileName), Some("a.pdf"));
    assert_eq!(value(&pending, L::Location), Some(r"C:\docs"));
    assert_eq!(value(&pending, L::Title), Some(PENDING));
    assert_eq!(value(&pending, L::Encryption), Some(PENDING));
    assert_eq!(value(&pending, L::Pages), Some("3"));
    assert_eq!(
        value(&pending, L::PageSize),
        Some("210.0 \u{00D7} 297.0 mm（595.3 \u{00D7} 841.9 pt）・A4・第 2 頁")
    );
    assert_eq!(
        value(&pending, L::FileSize),
        Some("1.23 MB（1,289,331 位元組）")
    );
    assert_eq!(value(&pending, L::Printing), None);
    // An engine that knows only the metadata: what it cannot tell is "—",
    // not "No" or "0".
    let unknown = DocumentProperties::from_metadata(DocumentMetadata::default());
    let rows = sections(&facts(Answer::Ready(&unknown)), zh);
    for label in [
        L::Tagged,
        L::FastWebView,
        L::FormFields,
        L::SignatureFields,
        L::Xfa,
    ] {
        assert_eq!(value(&rows, label), Some(MISSING), "{label:?}");
    }
    assert_eq!(value(&rows, L::Encryption), Some("否"));
    let none = sections(&facts(Answer::None), zh);
    assert_eq!(value(&none, L::Title), Some(MISSING));
    assert_eq!(value(&none, L::FormFields), Some(MISSING));
}

#[test]
fn rows_of_an_encrypted_document() {
    let p = encrypted();
    let zh = Language::TraditionalChinese.strings();
    let rows = sections(&facts(Answer::Ready(&p)), zh);
    assert_eq!(
        value(&rows, L::Title),
        Some("加密測試文件 Encrypted fixture")
    );
    assert_eq!(value(&rows, L::Author), Some(MISSING));
    assert_eq!(
        value(&rows, L::Created),
        Some("2026/10/24 15:30:00（UTC+08:00）")
    );
    assert_eq!(
        value(&rows, L::Encryption),
        Some("AES 128 位元（Standard 安全性處理常式，修訂版 4）")
    );
    assert_eq!(value(&rows, L::OpenedWith), Some("不需密碼"));
    assert_eq!(
        value(&rows, L::Printing),
        Some(zh.print_permission(PrintPermission::HighResolution))
    );
    assert_eq!(value(&rows, L::Copying), Some("不允許"));
    assert_eq!(value(&rows, L::Tagged), Some("是"));
    assert_eq!(value(&rows, L::FastWebView), Some("否"));
    assert_eq!(value(&rows, L::FormFields), Some("\u{2265} 20000"));
    assert_eq!(
        value(&rows, L::SignatureFields),
        Some("\u{2265} 3（已簽署 \u{2265} 1）")
    );
    let security = rows.iter().find(|s| s.title == L::Security);
    assert!(security.is_some_and(|s| s.note.is_some()));

    let text = as_text(&rows, zh);
    assert!(text.contains("標題：加密測試文件 Encrypted fixture\n"));
    assert!(text.contains("內容複製：不允許\n"));
    assert!(text.starts_with("描述\n檔案名稱：a.pdf\n"));

    // In English, and opened with a password.
    let en = Language::English.strings();
    let mut f = facts(Answer::Ready(&p));
    f.password = true;
    let rows = sections(&f, en);
    assert_eq!(value(&rows, L::OpenedWith), Some("User password"));
    assert_eq!(
        value(&rows, L::Encryption),
        Some("128-bit AES (Standard security handler, revision 4)")
    );
    assert!(as_text(&rows, en).contains("Content copying: Not allowed\n"));
}

#[test]
fn plain_documents_have_no_security_rows() {
    let p = DocumentProperties::from_metadata(DocumentMetadata {
        title: Some("t\u{202E}x\n".into()),
        ..DocumentMetadata::default()
    });
    let en = Language::English.strings();
    let rows = sections(&facts(Answer::Ready(&p)), en);
    assert_eq!(value(&rows, L::Encryption), Some("No"));
    assert_eq!(value(&rows, L::OpenedWith), None);
    assert_eq!(value(&rows, L::Printing), None);
    // Raw in the row, cleaned when copied.
    assert_eq!(value(&rows, L::Title), Some("t\u{202E}x\n"));
    assert!(as_text(&rows, en).contains("Title: tx \n"));
}
