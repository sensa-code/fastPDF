/// Document-level information shown in the title bar and properties dialog.
///
/// Dates are kept as the raw PDF date strings (`D:YYYYMMDDHHmmSS...`);
/// formatting is a UI concern.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DocumentMetadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
    pub creation_date: Option<String>,
    pub modification_date: Option<String>,
    /// Header version, e.g. `"1.7"`.
    pub pdf_version: Option<String>,
    pub encrypted: bool,
}
