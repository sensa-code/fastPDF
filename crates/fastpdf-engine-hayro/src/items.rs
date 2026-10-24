//! Array items, references unresolved, without hayro's panicking iterator.
//!
//! `Array::raw_iter` unwraps the parse of each item: an item the array's
//! skip accepted but the parse rejects panics. A dictionary followed by
//! `stream` inside an array is one: skipping treats `stream ... endstream`
//! as stray tokens, parsing tries a stream and rejects it (an `/F` key, for
//! example). [`items`] parses each item first with a plain context and
//! stops at the first one that does not parse or is a stream (streams
//! cannot be array items, and their parse is the only one whose extent
//! depends on the context). The document's context adds only string
//! decryption and reference resolution, which never make an otherwise
//! valid item fail, so `raw_iter` then only parses items it can read.

use hayro::hayro_syntax::object::{Array, MaybeRef, Object};
use hayro::hayro_syntax::reader::{Reader, ReaderContext, ReaderExt};

/// The items of `array` up to the first one that cannot be read.
pub(crate) fn items<'p>(array: &Array<'p>) -> impl Iterator<Item = MaybeRef<Object<'p>>> + 'p {
    let mut check = Reader::new(array.data());
    let context = ReaderContext::dummy();
    let mut raw = array.raw_iter();
    std::iter::from_fn(move || {
        check.skip_white_spaces_and_comments();
        if check.at_end() {
            return None;
        }
        match check.read::<MaybeRef<Object<'_>>>(&context)? {
            MaybeRef::NotRef(Object::Stream(_)) => None,
            _ => raw.next(),
        }
    })
}

#[cfg(test)]
mod tests {
    use hayro::hayro_syntax::object::FromBytes;

    use super::*;

    fn kinds(source: &[u8]) -> Vec<&'static str> {
        let Some(array) = Array::from_bytes(source) else {
            return vec!["unreadable"];
        };
        items(&array)
            .map(|item| match item {
                MaybeRef::Ref(_) => "ref",
                MaybeRef::NotRef(Object::Dict(_)) => "dict",
                MaybeRef::NotRef(Object::Number(_)) => "number",
                MaybeRef::NotRef(_) => "other",
            })
            .collect()
    }

    #[test]
    fn items_stop_before_what_cannot_be_read() {
        assert_eq!(
            kinds(b"[1 0 R 5 << /A 1 >> /N]"),
            ["ref", "number", "dict", "other"]
        );
        // hayro's raw_iter panics on this one: the stream does not parse.
        assert_eq!(
            kinds(b"[1 0 R << /F 1 >> stream\nabc\nendstream 2 0 R]"),
            ["ref"]
        );
        assert_eq!(kinds(b"[]"), Vec::<&str>::new());
    }
}
