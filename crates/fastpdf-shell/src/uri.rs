//! Which external links FastPDF hands to Windows (ADR 0015).
//!
//! A link's address comes from the document, so it is hostile input. Only
//! `http`, `https` and `mailto` are accepted, normalized by the WHATWG URL
//! parser (`url`): host names in punycode, unsafe characters
//! percent-encoded. What [`check_external_uri`] returns is exactly the
//! string the confirmation shows and the shell receives, and it is
//! printable ASCII without `"`, so it cannot carry an argument into a
//! handler's `"%1"` command line. Addresses with credentials are refused
//! (`https://bank.example@evil.example/` reads like the bank), as are
//! control, bidi and zero-width characters anywhere in the raw text: the
//! parser would silently drop tabs and line breaks (`java\nscript:`) and
//! the others hide what the address really says.

mod mailto;

use std::fmt;

use url::Url;

/// Longest address accepted, before and after normalization (Internet
/// Explorer's `INTERNET_MAX_URL_LENGTH` is 2083).
pub const MAX_URI_LEN: usize = 2048;

/// Longest scheme kept in [`UriRejection::Scheme`].
const MAX_SCHEME_LEN: usize = 32;

/// What an accepted address opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UriKind {
    /// `http` or `https`: the default browser.
    Web,
    /// `mailto`: the default mail app.
    Mail,
}

/// An address that may be opened after the user confirmed it. Only
/// [`check_external_uri`] makes one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalUri {
    text: String,
    kind: UriKind,
    host: Option<String>,
}

impl ExternalUri {
    /// The normalized address: printable ASCII, no `"`, at most
    /// [`MAX_URI_LEN`] bytes.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn kind(&self) -> UriKind {
        self.kind
    }

    /// The host of a web address (punycode for international names, IPv6
    /// in brackets); `None` for `mailto`.
    pub fn host(&self) -> Option<&str> {
        self.host.as_deref()
    }

    /// The scheme: `http`, `https` or `mailto`.
    pub fn scheme(&self) -> &str {
        self.text.split(':').next().unwrap_or_default()
    }

    /// Who a mail link writes to, as the address spells it (percent
    /// encoding kept, so nothing decoded can hide or reorder text): the
    /// recipients before `?`, then the value of each query field that does
    /// more than write the text (`to`, `cc`, `bcc`, one without a name).
    /// Empty when it names nobody; `None` for a web address.
    pub fn mail_recipients(&self) -> Option<Vec<&str>> {
        let rest = self.text.strip_prefix("mailto:")?;
        let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
        let path = Some(path).filter(|path| !path.is_empty());
        Some(
            path.into_iter()
                .chain(mailto::recipient_values(query))
                .collect(),
        )
    }
}

/// Why an address is not opened.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum UriRejection {
    /// Nothing but white space.
    Empty,
    /// Longer than [`MAX_URI_LEN`] bytes.
    TooLong,
    /// Not an absolute URL (`www.example.com`, a file path, ...).
    Malformed,
    /// Another scheme (`file`, `javascript`, `data`, `ms-settings`, ...),
    /// lowercase and at most 32 characters.
    Scheme(String),
    /// A user name or password before the host.
    Credentials,
    /// Control, bidi or zero-width characters, or characters that would
    /// leave the normalized address unsafe to pass on.
    UnsafeCharacters,
    /// A mail link sets a field other than to, cc, bcc, subject, body and
    /// in-reply-to (`attach` can attach local files in some mail apps):
    /// its name, lowercase, at most 32 ASCII letters, digits, `-` or `_`.
    MailField(String),
}

impl fmt::Display for UriRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("the address is empty"),
            Self::TooLong => write!(f, "the address is longer than {MAX_URI_LEN} bytes"),
            Self::Malformed => f.write_str("the address is not an absolute URL"),
            Self::Scheme(s) => write!(f, "\"{s}:\" links are not opened"),
            Self::Credentials => f.write_str("the address contains a user name or password"),
            Self::UnsafeCharacters => f.write_str("the address contains unsafe characters"),
            Self::MailField(name) => write!(f, "the mail link sets the field \"{name}\""),
        }
    }
}

impl std::error::Error for UriRejection {}

/// Checks a link's address and returns the normalized form to show and
/// open, or why it must not be opened.
pub fn check_external_uri(raw: &str) -> Result<ExternalUri, UriRejection> {
    let raw = raw.trim_matches(|c: char| c.is_ascii_whitespace() || c == '\0');
    if raw.is_empty() {
        return Err(UriRejection::Empty);
    }
    if raw.len() > MAX_URI_LEN {
        return Err(UriRejection::TooLong);
    }
    if raw.chars().any(|c| c.is_control() || hides_text(c)) {
        return Err(UriRejection::UnsafeCharacters);
    }
    let url = Url::parse(raw).map_err(|_| UriRejection::Malformed)?;
    let (kind, host) = match url.scheme() {
        "http" | "https" => {
            if !url.username().is_empty() || url.password().is_some() {
                return Err(UriRejection::Credentials);
            }
            let host = url.host_str().filter(|h| !h.is_empty());
            (
                UriKind::Web,
                Some(host.ok_or(UriRejection::Malformed)?.to_owned()),
            )
        }
        "mailto" => {
            // RFC 6068: the recipients may be in the path or only in the
            // query (`mailto:?to=...`); a bare `mailto:` says nothing.
            if url.path().is_empty() && url.query().is_none_or(str::is_empty) {
                return Err(UriRejection::Malformed);
            }
            // Mail apps parse the address themselves, some only by `?` and
            // `&`: a fragment (`?subject=x#&attach=...`) or a second `?`
            // would hide fields from the check below while they still
            // reach the app. Neither means anything in a mail link.
            if url.fragment().is_some() || url.query().is_some_and(|q| q.contains('?')) {
                return Err(UriRejection::Malformed);
            }
            if let Some(field) = url.query().and_then(mailto::refused_field) {
                return Err(UriRejection::MailField(field));
            }
            (UriKind::Mail, None)
        }
        other => {
            let scheme: String = other.chars().take(MAX_SCHEME_LEN).collect();
            return Err(UriRejection::Scheme(scheme.to_ascii_lowercase()));
        }
    };
    let text = String::from(url);
    if text.len() > MAX_URI_LEN {
        return Err(UriRejection::TooLong);
    }
    if !text
        .bytes()
        .all(|b| (0x21..=0x7E).contains(&b) && b != b'"')
    {
        return Err(UriRejection::UnsafeCharacters);
    }
    Ok(ExternalUri { text, kind, host })
}

/// Characters that change or hide how the text around them reads: bidi
/// controls, zero-width characters, the soft hyphen and the BOM.
fn hides_text(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accepted(raw: &str) -> ExternalUri {
        match check_external_uri(raw) {
            Ok(uri) => uri,
            Err(why) => panic!("{raw:?} rejected: {why:?}"),
        }
    }

    #[test]
    fn web_and_mail_addresses_are_normalized() {
        let cases = [
            (
                "https://example.invalid/path?q=1#frag",
                "https://example.invalid/path?q=1#frag",
                UriKind::Web,
                Some("example.invalid"),
            ),
            (
                "  HTTP://Example.Invalid  ",
                "http://example.invalid/",
                UriKind::Web,
                Some("example.invalid"),
            ),
            (
                "https://b\u{fc}cher.example/",
                "https://xn--bcher-kva.example/",
                UriKind::Web,
                Some("xn--bcher-kva.example"),
            ),
            (
                "http:\\\\example.invalid\\a\\b",
                "http://example.invalid/a/b",
                UriKind::Web,
                Some("example.invalid"),
            ),
            (
                "https://example.invalid/a\"b?c=\"d\"",
                "https://example.invalid/a%22b?c=%22d%22",
                UriKind::Web,
                Some("example.invalid"),
            ),
            (
                "https://example.invalid/caf\u{e9} x",
                "https://example.invalid/caf%C3%A9%20x",
                UriKind::Web,
                Some("example.invalid"),
            ),
            (
                "https://[::1]:8080/",
                "https://[::1]:8080/",
                UriKind::Web,
                Some("[::1]"),
            ),
            (
                "MAILTO:someone@example.invalid?subject=FastPDF%20test",
                "mailto:someone@example.invalid?subject=FastPDF%20test",
                UriKind::Mail,
                None,
            ),
            (
                "mailto:?to=help@example.invalid&subject=Question",
                "mailto:?to=help@example.invalid&subject=Question",
                UriKind::Mail,
                None,
            ),
            (
                "mailto:?subject=Feedback",
                "mailto:?subject=Feedback",
                UriKind::Mail,
                None,
            ),
        ];
        for (raw, text, kind, host) in cases {
            let uri = accepted(raw);
            assert_eq!(uri.as_str(), text, "{raw:?}");
            assert_eq!(uri.kind(), kind, "{raw:?}");
            assert_eq!(uri.host(), host, "{raw:?}");
        }
        assert_eq!(accepted("HTTPS://example.invalid").scheme(), "https");
        assert_eq!(accepted("mailto:a@example.invalid").scheme(), "mailto");
    }

    #[test]
    fn everything_else_is_refused_with_its_reason() {
        use UriRejection as R;
        let scheme = |s: &str| R::Scheme(s.into());
        let long = format!("https://example.invalid/{}", "a".repeat(MAX_URI_LEN));
        // Percent-encoding makes it longer than the limit.
        let grows = format!("https://example.invalid/{}", "\u{e9}".repeat(400));
        let cases: Vec<(&str, UriRejection)> = vec![
            ("", R::Empty),
            (" \t\r\n\0 ", R::Empty),
            ("file:///C:/Windows/System32/calc.exe", scheme("file")),
            ("javascript:alert(1)", scheme("javascript")),
            ("JavaScript:alert(1)", scheme("javascript")),
            ("data:text/html,<script>alert(1)</script>", scheme("data")),
            ("vbscript:msgbox(1)", scheme("vbscript")),
            ("ms-settings:defaultapps", scheme("ms-settings")),
            ("ms-msdt:/id PCWDiagnostic", scheme("ms-msdt")),
            ("search-ms:query=x", scheme("search-ms")),
            ("ftp://example.invalid/", scheme("ftp")),
            ("C:\\Windows\\calc.exe", scheme("c")),
            ("www.example.invalid", R::Malformed),
            ("\\\\server\\share\\x.exe", R::Malformed),
            ("/relative/path", R::Malformed),
            ("https://", R::Malformed),
            ("mailto:", R::Malformed),
            ("mailto:?", R::Malformed),
            ("java\nscript:alert(1)", R::UnsafeCharacters),
            ("https://example.invalid/\ta", R::UnsafeCharacters),
            ("https://exa\0mple.invalid/", R::UnsafeCharacters),
            (
                "https://example.invalid/\u{202e}gnp.exe",
                R::UnsafeCharacters,
            ),
            ("https://exam\u{200b}ple.invalid/", R::UnsafeCharacters),
            ("https://user:secret@example.invalid/", R::Credentials),
            ("https://bank.example@evil.example/", R::Credentials),
            ("mailto:some one@example.invalid", R::UnsafeCharacters),
            ("mailto:a\"b@example.invalid", R::UnsafeCharacters),
            (
                "mailto:a@example.invalid?subject=x&attach=C:%5Cx.txt",
                R::MailField("attach".into()),
            ),
            (
                "mailto:?Attachment=%5C%5Cserver%5Cshare%5Cf",
                R::MailField("attachment".into()),
            ),
            (
                "mailto:a@example.invalid?subject=x#&attach=C:%5Cwin.ini",
                R::Malformed,
            ),
            ("mailto:a@example.invalid#x", R::Malformed),
            (
                "mailto:a@example.invalid?subject=x?attach=C:%5Cwin.ini",
                R::Malformed,
            ),
            (long.as_str(), R::TooLong),
            (grows.as_str(), R::TooLong),
        ];
        for (raw, why) in cases {
            assert_eq!(check_external_uri(raw), Err(why), "{raw:?}");
        }
        let scheme = format!("{}:x", "a".repeat(100));
        assert_eq!(
            check_external_uri(&scheme),
            Err(R::Scheme("a".repeat(MAX_SCHEME_LEN)))
        );
    }

    #[test]
    fn rejections_explain_themselves() {
        assert_eq!(
            UriRejection::Scheme("file".into()).to_string(),
            "\"file:\" links are not opened"
        );
        assert!(UriRejection::TooLong.to_string().contains("2048"));
    }
}
