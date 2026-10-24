//! Array items and dictionary entries, references unresolved, without
//! hayro's panicking iterators.
//!
//! `Array::raw_iter` and `Dict::entries` unwrap the parse of each item: an
//! item the skip accepted (when the array or dictionary was read) but the
//! parse rejects panics. A dictionary followed by `stream` inside an array
//! or a dictionary is one: skipping treats `stream ... endstream` as stray
//! tokens, parsing tries a stream and rejects it (an `/F` key, for example).
//!
//! [`items`] parses each item first with a plain context and stops at the
//! first one that does not parse, so `raw_iter` only parses items it can
//! read: the document's context adds only string decryption and reference
//! resolution, which never make an otherwise valid item fail. A stream
//! whose `/Length` is a reference also stops it: with the document's
//! context its extent may differ from the plain parse, and the two readers
//! would part ways. Streams cannot be array items in a valid file. Callers
//! that must see every item ask [`Items::stopped`].
//!
//! [`entries`] reads each value of a dictionary by key, so one value that
//! does not parse is left out instead of panicking.

use hayro::hayro_syntax::object::array::ArrayIter;
use hayro::hayro_syntax::object::{Array, Dict, MaybeRef, Name, Object};
use hayro::hayro_syntax::reader::{Reader, ReaderContext, ReaderExt};

/// The items of an array up to the first one that cannot be read.
pub(crate) struct Items<'p> {
    check: Reader<'p>,
    context: ReaderContext<'static>,
    raw: ArrayIter<'p>,
    stopped: bool,
}

/// The items of `array` up to the first one that cannot be read.
pub(crate) fn items<'p>(array: &Array<'p>) -> Items<'p> {
    Items {
        check: Reader::new(array.data()),
        context: ReaderContext::dummy(),
        raw: array.raw_iter(),
        stopped: false,
    }
}

impl Items<'_> {
    /// An item that could not be read ended the iteration before the end
    /// of the array.
    pub(crate) fn stopped(&self) -> bool {
        self.stopped
    }
}

impl<'p> Iterator for Items<'p> {
    type Item = MaybeRef<Object<'p>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.stopped {
            return None;
        }
        self.check.skip_white_spaces_and_comments();
        if self.check.at_end() {
            return None;
        }
        let readable = match self.check.read::<MaybeRef<Object<'_>>>(&self.context) {
            Some(MaybeRef::NotRef(Object::Stream(s))) => !matches!(
                s.dict().get_raw::<Object<'_>>(b"Length"),
                Some(MaybeRef::Ref(_))
            ),
            Some(_) => true,
            None => false,
        };
        if !readable {
            self.stopped = true;
            return None;
        }
        self.raw.next()
    }
}

/// The entries of `dict` sorted by key, like `Dict::entries`; a value that
/// does not parse as an object is taken as the dictionary hayro would read
/// for that key, or left out.
pub(crate) fn entries<'p>(dict: &Dict<'p>) -> Vec<(Name<'p>, MaybeRef<Object<'p>>)> {
    let mut keys: Vec<Name<'p>> = dict.keys().collect();
    keys.sort_by(|a, b| a.as_ref().cmp(b.as_ref()));
    keys.into_iter()
        .filter_map(|key| {
            let value = match dict.get_raw::<Object<'p>>(key.as_ref()) {
                Some(value) => value,
                None => match dict.get_raw::<Dict<'p>>(key.as_ref())? {
                    MaybeRef::Ref(r) => MaybeRef::Ref(r),
                    MaybeRef::NotRef(d) => MaybeRef::NotRef(Object::Dict(d)),
                },
            };
            Some((key, value))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use hayro::hayro_syntax::object::FromBytes;

    use super::*;

    fn kinds(source: &[u8]) -> (Vec<&'static str>, bool) {
        let Some(array) = Array::from_bytes(source) else {
            return (vec!["unreadable"], false);
        };
        let mut it = items(&array);
        let kinds = it
            .by_ref()
            .map(|item| match item {
                MaybeRef::Ref(_) => "ref",
                MaybeRef::NotRef(Object::Dict(_)) => "dict",
                MaybeRef::NotRef(Object::Number(_)) => "number",
                MaybeRef::NotRef(Object::Stream(_)) => "stream",
                MaybeRef::NotRef(_) => "other",
            })
            .collect();
        (kinds, it.stopped())
    }

    #[test]
    fn items_stop_before_what_cannot_be_read() {
        assert_eq!(
            kinds(b"[1 0 R 5 << /A 1 >> /N]"),
            (vec!["ref", "number", "dict", "other"], false)
        );
        // hayro's raw_iter panics on this one: the stream does not parse.
        assert_eq!(
            kinds(b"[1 0 R << /F 1 >> stream\nabc\nendstream 2 0 R]"),
            (vec!["ref"], true)
        );
        // A stream with a direct /Length reads the same either way.
        assert_eq!(
            kinds(b"[<< /Length 3 >> stream\nabc\nendstream 2 0 R]"),
            (vec!["stream", "ref"], false)
        );
        // With a reference as /Length the readers could part ways.
        assert_eq!(
            kinds(b"[1 0 R << /Length 9 0 R >> stream\nabc\nendstream 2 0 R]"),
            (vec!["ref"], true)
        );
        assert_eq!(kinds(b"[]"), (Vec::new(), false));
    }

    #[test]
    fn entries_leave_out_what_cannot_be_read() {
        let Some(dict) =
            Dict::from_bytes(b"<< /B 2 0 R /A << /F 1 >> stream\nabc\nendstream /C [1 2] >>")
        else {
            panic!("the dictionary reads");
        };
        let entries = entries(&dict);
        let keys: Vec<&[u8]> = entries.iter().map(|(k, _)| k.as_ref()).collect();
        assert_eq!(keys, [b"A".as_slice(), b"B", b"C"]);
        // /A does not parse as an object; hayro would read it as the
        // dictionary in front of `stream`.
        assert!(matches!(entries[0].1, MaybeRef::NotRef(Object::Dict(_))));
        assert!(matches!(entries[1].1, MaybeRef::Ref(_)));
    }
}
