//! Text of the document properties panel (Ctrl+D).

use fastpdf_engine_api::{EncryptionMethod, PrintPermission};

use super::{Language, Strings};

mod labels;

pub(crate) use labels::PropertyLabel;

impl Strings {
    pub(crate) fn properties_title(&self) -> &'static str {
        match self.language {
            Language::English => "Document properties",
            Language::TraditionalChinese => "文件內容",
        }
    }

    pub(crate) fn tip_properties(&self) -> &'static str {
        match self.language {
            Language::English => "Document properties",
            Language::TraditionalChinese => "文件內容",
        }
    }

    pub(crate) fn yes_no(&self, yes: bool) -> &'static str {
        match (self.language, yes) {
            (Language::English, true) => "Yes",
            (Language::English, false) => "No",
            (Language::TraditionalChinese, true) => "是",
            (Language::TraditionalChinese, false) => "否",
        }
    }

    pub(crate) fn allowed(&self, allowed: bool) -> &'static str {
        match (self.language, allowed) {
            (Language::English, true) => "Allowed",
            (Language::English, false) => "Not allowed",
            (Language::TraditionalChinese, true) => "允許",
            (Language::TraditionalChinese, false) => "不允許",
        }
    }

    pub(crate) fn print_permission(&self, printing: PrintPermission) -> &'static str {
        match (self.language, printing) {
            (Language::English, PrintPermission::HighResolution) => "High resolution",
            (Language::English, PrintPermission::LowResolution) => "Low resolution (150 dpi)",
            (Language::English, PrintPermission::NotAllowed) => "Not allowed",
            (Language::TraditionalChinese, PrintPermission::HighResolution) => "高解析度",
            (Language::TraditionalChinese, PrintPermission::LowResolution) => "低解析度（150 dpi）",
            (Language::TraditionalChinese, PrintPermission::NotAllowed) => "不允許",
        }
    }

    /// How the document was opened: with a password the reader typed, or
    /// without one.
    pub(crate) fn opened_with(&self, password: bool) -> &'static str {
        match (self.language, password) {
            (Language::English, true) => "User password",
            (Language::English, false) => "No password",
            (Language::TraditionalChinese, true) => "使用者密碼",
            (Language::TraditionalChinese, false) => "不需密碼",
        }
    }

    /// Above the permission rows.
    pub(crate) fn permissions_note(&self) -> &'static str {
        match self.language {
            Language::English => {
                "The document's owner password sets these permissions. FastPDF shows them and does not apply them."
            }
            Language::TraditionalChinese => {
                "文件的擁有者密碼設定了下列權限。FastPDF 只顯示，不會套用。"
            }
        }
    }

    pub(crate) fn encryption_none(&self) -> &'static str {
        self.yes_no(false)
    }

    pub(crate) fn encryption_details_unknown(&self) -> &'static str {
        match self.language {
            Language::English => "Encrypted (details unavailable)",
            Language::TraditionalChinese => "已加密（無法讀取詳細資訊）",
        }
    }

    pub(crate) fn encryption_method(&self, method: EncryptionMethod) -> String {
        let zh = matches!(self.language, Language::TraditionalChinese);
        match method {
            EncryptionMethod::Rc4 { key_bits } if zh => format!("RC4 {key_bits} 位元"),
            EncryptionMethod::Rc4 { key_bits } => format!("{key_bits}-bit RC4"),
            EncryptionMethod::Aes128 if zh => "AES 128 位元".into(),
            EncryptionMethod::Aes128 => "128-bit AES".into(),
            EncryptionMethod::Aes256 if zh => "AES 256 位元".into(),
            EncryptionMethod::Aes256 => "256-bit AES".into(),
            EncryptionMethod::Identity if zh => "不加密內容（Identity）".into(),
            EncryptionMethod::Identity => "Content not encrypted (Identity)".into(),
            EncryptionMethod::Unknown if zh => "不明的加密方式".into(),
            EncryptionMethod::Unknown => "Unknown method".into(),
        }
    }

    /// The handler and revision after the method: "（Standard 安全性處理常式，
    /// 修訂版 4）"; empty when neither is known. `handler` is display-safe.
    pub(crate) fn encryption_details(&self, handler: Option<&str>, revision: Option<u8>) -> String {
        let zh = matches!(self.language, Language::TraditionalChinese);
        let mut parts = Vec::new();
        if let Some(handler) = handler {
            parts.push(if zh {
                format!("{handler} 安全性處理常式")
            } else {
                format!("{handler} security handler")
            });
        }
        if let Some(revision) = revision {
            parts.push(if zh {
                format!("修訂版 {revision}")
            } else {
                format!("revision {revision}")
            });
        }
        match (parts.is_empty(), zh) {
            (true, _) => String::new(),
            (false, true) => format!("（{}）", parts.join("，")),
            (false, false) => format!(" ({})", parts.join(", ")),
        }
    }

    /// `count` (already formatted with separators) bytes on their own.
    pub(crate) fn bytes(&self, count: &str) -> String {
        match self.language {
            Language::English if count == "1" => "1 byte".into(),
            Language::English => format!("{count} bytes"),
            Language::TraditionalChinese => format!("{count} 位元組"),
        }
    }

    /// A rounded size with the exact count: "1.23 MB (1,289,331 bytes)".
    pub(crate) fn size_with_bytes(&self, rounded: &str, count: &str) -> String {
        match self.language {
            Language::English => format!("{rounded} ({})", self.bytes(count)),
            Language::TraditionalChinese => format!("{rounded}（{}）", self.bytes(count)),
        }
    }

    /// The current page's size: "210.0 × 297.0 mm (595.3 × 841.9 pt) · A4 ·
    /// page 3"; `paper` when it is a known size.
    pub(crate) fn page_size_text(
        &self,
        mm: &str,
        pt: &str,
        paper: Option<&str>,
        page: u32,
    ) -> String {
        match self.language {
            Language::English => {
                let paper = paper.map(|p| format!(" · {p}")).unwrap_or_default();
                format!("{mm} ({pt}){paper} · page {page}")
            }
            Language::TraditionalChinese => {
                let paper = paper.map(|p| format!("・{p}")).unwrap_or_default();
                format!("{mm}（{pt}）{paper}・第 {page} 頁")
            }
        }
    }

    /// A date with its time and, when the file names one, its offset from
    /// UTC ("UTC", "UTC+08:00").
    pub(crate) fn date_text(
        &self,
        (year, month, day): (u16, u8, u8),
        (hour, minute, second): (u8, u8, u8),
        offset: Option<&str>,
    ) -> String {
        match self.language {
            Language::English => {
                let offset = offset.map(|o| format!(" ({o})")).unwrap_or_default();
                format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}{offset}")
            }
            Language::TraditionalChinese => {
                let offset = offset.map(|o| format!("（{o}）")).unwrap_or_default();
                format!("{year:04}/{month:02}/{day:02} {hour:02}:{minute:02}:{second:02}{offset}")
            }
        }
    }

    /// Signature fields and how many are signed: "2 (1 signed)".
    pub(crate) fn signature_count(&self, fields: &str, signed: &str) -> String {
        match self.language {
            Language::English => format!("{fields} ({signed} signed)"),
            Language::TraditionalChinese => format!("{fields}（已簽署 {signed}）"),
        }
    }

    pub(crate) fn no_document_open(&self) -> &'static str {
        match self.language {
            Language::English => "No document is open.",
            Language::TraditionalChinese => "沒有開啟的文件。",
        }
    }

    pub(crate) fn reading_properties(&self) -> &'static str {
        match self.language {
            Language::English => "Reading document properties\u{2026}",
            Language::TraditionalChinese => "正在讀取文件內容\u{2026}",
        }
    }

    /// The engine could not answer; `why` is display-safe.
    pub(crate) fn properties_unavailable(&self, why: &str) -> String {
        match self.language {
            Language::English => format!("Document properties unavailable: {why}"),
            Language::TraditionalChinese => format!("無法讀取文件內容：{why}"),
        }
    }

    pub(crate) fn retry(&self) -> &'static str {
        match self.language {
            Language::English => "Retry",
            Language::TraditionalChinese => "重試",
        }
    }

    pub(crate) fn copy_value(&self) -> &'static str {
        match self.language {
            Language::English => "Copy",
            Language::TraditionalChinese => "複製",
        }
    }

    pub(crate) fn copied(&self) -> &'static str {
        match self.language {
            Language::English => "Copied",
            Language::TraditionalChinese => "已複製",
        }
    }

    pub(crate) fn copy_all(&self) -> &'static str {
        match self.language {
            Language::English => "Copy all",
            Language::TraditionalChinese => "全部複製",
        }
    }
}

#[cfg(test)]
mod tests;
