//! What the chip and the card of an external link name as its target, and
//! which end of it stays in view when it does not fit: the end of a host
//! (`login.example.com.evil.example` belongs to evil.example), every
//! recipient of a mail link.

use fastpdf_shell::{ExternalUri, UriKind};
use gpui::{IntoElement, ParentElement, Styled, div, prelude::FluentBuilder};

/// Which end of a value stays in view when it does not fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Keep {
    Start,
    End,
}

/// What a chip names for an external link: the host of a web address, or
/// every recipient of a mail link, from before `?` and from the query alike
/// (`mailto:a@x?bcc=b@y` must not read as mail to a@x only), as the address
/// spells them; both keep their end in view. A mail link that names nobody
/// shows its address from the start (`mailto:?subject=...` says so).
pub(crate) fn external_target(uri: &ExternalUri) -> (String, Keep) {
    match uri.kind() {
        UriKind::Web => (uri.host().unwrap_or(uri.as_str()).to_owned(), Keep::End),
        UriKind::Mail => match uri.mail_recipients() {
            Some(recipients) if !recipients.is_empty() => (recipients.join(", "), Keep::End),
            _ => (uri.as_str().to_owned(), Keep::Start),
        },
    }
}

/// A label and a value on one line; a value too long for it loses the end
/// that `keep` does not name.
pub(crate) fn label_and_value(label: &'static str, value: String, keep: Keep) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .gap_1()
        .min_w_0()
        .child(div().flex_none().whitespace_nowrap().child(label))
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .map(|value| match keep {
                    Keep::End => value.text_ellipsis_start(),
                    Keep::Start => value.text_ellipsis(),
                })
                .child(value),
        )
}

#[cfg(test)]
mod tests {
    use fastpdf_shell::check_external_uri;

    use super::*;

    fn target(raw: &str) -> (String, Keep) {
        match check_external_uri(raw) {
            Ok(uri) => external_target(&uri),
            Err(why) => panic!("{raw}: {why:?}"),
        }
    }

    fn end(text: &str) -> (String, Keep) {
        (text.to_owned(), Keep::End)
    }

    #[test]
    fn chips_name_the_host_or_every_recipient() {
        assert_eq!(
            target("https://login.microsoftonline.com.oauth2-authorize-session.evil.example/a?b"),
            end("login.microsoftonline.com.oauth2-authorize-session.evil.example")
        );
        assert_eq!(
            target("https://b\u{fc}cher.example/"),
            end("xn--bcher-kva.example")
        );
        assert_eq!(
            target("mailto:a@example.invalid,b@example.invalid?subject=x"),
            end("a@example.invalid,b@example.invalid")
        );
        assert_eq!(
            target("mailto:?to=a@example.invalid"),
            end("a@example.invalid")
        );
        assert_eq!(
            target("mailto:a@example.invalid?Subject=x&body=y"),
            end("a@example.invalid")
        );
        let long_body = "y".repeat(500);
        for (raw, shown) in [
            (
                "mailto:a@example.invalid?bcc=b@example.invalid".to_owned(),
                "a@example.invalid, b@example.invalid",
            ),
            (
                format!("mailto:a@example.invalid?CC=b@example.invalid&body={long_body}"),
                "a@example.invalid, b@example.invalid",
            ),
            (
                "mailto:a@example.invalid?%74o=b%40example.invalid&=c@example.invalid".to_owned(),
                "a@example.invalid, b%40example.invalid, c@example.invalid",
            ),
        ] {
            assert_eq!(target(&raw), end(shown), "{raw}");
        }
        assert_eq!(
            target("mailto:?subject=x&body=y"),
            ("mailto:?subject=x&body=y".to_owned(), Keep::Start)
        );
    }
}
