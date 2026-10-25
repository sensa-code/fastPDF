use super::*;

fn cjk(text: &str) -> bool {
    text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

#[test]
fn both_languages_have_the_text() {
    let en = Language::English.strings();
    let zh = Language::TraditionalChinese.strings();
    for (en, zh) in [
        (en.properties_title(), zh.properties_title()),
        (en.tip_properties(), zh.tip_properties()),
        (en.permissions_note(), zh.permissions_note()),
        (
            en.encryption_details_unknown(),
            zh.encryption_details_unknown(),
        ),
        (en.no_document_open(), zh.no_document_open()),
        (en.reading_properties(), zh.reading_properties()),
        (en.retry(), zh.retry()),
        (en.copy_value(), zh.copy_value()),
        (en.copied(), zh.copied()),
        (en.copy_all(), zh.copy_all()),
        (en.yes_no(true), zh.yes_no(true)),
        (en.allowed(false), zh.allowed(false)),
        (en.opened_with(true), zh.opened_with(true)),
        (
            en.print_permission(PrintPermission::LowResolution),
            zh.print_permission(PrintPermission::LowResolution),
        ),
    ] {
        assert!(!en.is_empty());
        assert!(cjk(zh), "{zh}");
    }
    assert_eq!(
        zh.with_shortcut(zh.tip_properties(), Some("ctrl-d")),
        "文件內容（Ctrl+D）"
    );
    assert_eq!(
        en.with_shortcut(en.tip_properties(), Some("ctrl-d")),
        "Document properties (Ctrl+D)"
    );
}

#[test]
fn composed_text_reads_in_each_language() {
    let en = Language::English.strings();
    let zh = Language::TraditionalChinese.strings();
    assert_eq!(
        zh.encryption_details(Some("Standard"), Some(4)),
        "（Standard 安全性處理常式，修訂版 4）"
    );
    assert_eq!(
        en.encryption_details(Some("Standard"), Some(4)),
        " (Standard security handler, revision 4)"
    );
    assert_eq!(en.encryption_details(None, None), "");
    assert_eq!(zh.encryption_details(None, Some(2)), "（修訂版 2）");
    assert_eq!(
        zh.encryption_method(EncryptionMethod::Aes128),
        "AES 128 位元"
    );
    assert_eq!(
        en.encryption_method(EncryptionMethod::Rc4 { key_bits: 40 }),
        "40-bit RC4"
    );
    assert_eq!(en.bytes("1"), "1 byte");
    assert_eq!(zh.signature_count("2", "1"), "2（已簽署 1）");
    assert_eq!(en.signature_count("≥ 2", "1"), "≥ 2 (1 signed)");
}
