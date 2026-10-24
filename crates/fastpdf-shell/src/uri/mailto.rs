//! Which fields a mail link may fill in. RFC 6068 lets a `mailto` query
//! set any header field, and some mail apps honour more: `attach` or
//! `attachment` can attach a local file, or reach a UNC path and leak the
//! user's NTLM credentials. A link from a document may only address the
//! mail and write its subject and text.

use url::form_urlencoded;

/// The fields a mail link may set (compared without regard to case).
const ALLOWED: [&str; 6] = ["to", "cc", "bcc", "subject", "body", "in-reply-to"];
/// Longest field name kept in the rejection (it is shown to the user).
const MAX_NAME_LEN: usize = 32;

/// The first field of `query` a mail link may not set, if any: its name,
/// lowercase, reduced to ASCII letters, digits, `-` and `_` (`?` when
/// nothing is left).
pub(super) fn refused_field(query: &str) -> Option<String> {
    form_urlencoded::parse(query.as_bytes())
        .map(|(name, _)| name)
        .find(|name| !name.is_empty() && !ALLOWED.iter().any(|ok| name.eq_ignore_ascii_case(ok)))
        .map(|name| {
            let shown: String = name
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
                .take(MAX_NAME_LEN)
                .collect();
            if shown.is_empty() {
                "?".to_owned()
            } else {
                shown.to_ascii_lowercase()
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_addressing_and_text_fields_pass() {
        for ok in [
            "",
            "subject=Hi",
            "to=a@example.invalid&CC=b@example.invalid&Bcc=c@example.invalid",
            "subject=x&body=line%0D%0Aline&In-Reply-To=%3Cid%40example.invalid%3E",
            "&&subject=x",
        ] {
            assert_eq!(refused_field(ok), None, "{ok}");
        }
        for (query, name) in [
            ("attach=C:%5Cx.txt", "attach"),
            (
                "subject=x&ATTACHMENT=%5C%5Cserver%5Cshare%5Cf",
                "attachment",
            ),
            ("%61ttach=x", "attach"),
            ("attach", "attach"),
            ("subject=x&x-mailer=y", "x-mailer"),
            ("from=a@example.invalid", "from"),
            ("%E2%80%AEhcatta=x", "hcatta"),
            ("%E2%80%AE=x", "?"),
        ] {
            assert_eq!(refused_field(query).as_deref(), Some(name), "{query}");
        }
    }
}
