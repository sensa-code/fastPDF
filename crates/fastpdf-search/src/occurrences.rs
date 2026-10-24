//! Where a query may match, in one pass over a page: every occurrence of
//! the needle, overlapping ones included (Knuth–Morris–Pratt over UTF-8
//! bytes; a valid UTF-8 needle only matches at character boundaries). Whole
//! words need them: a candidate rejected for a letter next to it may hide
//! a valid one inside it ("ab-ab" in "xab-ab-ab"), and searching again from
//! each rejected candidate would cost the needle's length every time.

use std::ops::Range;
use std::str::MatchIndices;

/// For each prefix of the needle, the length of its longest proper prefix
/// that is also its suffix: where matching goes on after a mismatch.
pub(crate) fn borders(needle: &[u8]) -> Vec<u32> {
    let mut borders = vec![0u32; needle.len()];
    let mut k = 0usize;
    for (i, &byte) in needle.iter().enumerate().skip(1) {
        while k > 0 && needle.get(k) != Some(&byte) {
            k = borders.get(k - 1).map_or(0, |&b| b as usize);
        }
        if needle.get(k) == Some(&byte) {
            k += 1;
        }
        if let Some(slot) = borders.get_mut(i) {
            *slot = u32::try_from(k).unwrap_or(0);
        }
    }
    borders
}

/// Every occurrence of `needle` in `hay`, overlapping ones included, as
/// start offsets in increasing order.
pub(crate) struct Overlapping<'a> {
    hay: &'a [u8],
    needle: &'a [u8],
    /// [`borders`] of the needle.
    borders: &'a [u32],
    at: usize,
    matched: usize,
}

impl<'a> Overlapping<'a> {
    pub(crate) fn new(hay: &'a str, needle: &'a str, borders: &'a [u32]) -> Self {
        Self {
            hay: hay.as_bytes(),
            needle: needle.as_bytes(),
            borders,
            at: 0,
            matched: 0,
        }
    }

    fn border(&self, len: usize) -> usize {
        len.checked_sub(1)
            .and_then(|i| self.borders.get(i))
            .map_or(0, |&b| b as usize)
    }
}

impl Iterator for Overlapping<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        let len = self.needle.len();
        if len == 0 || self.borders.len() != len {
            return None;
        }
        while let Some(&byte) = self.hay.get(self.at) {
            self.at += 1;
            while self.matched > 0 && self.needle.get(self.matched) != Some(&byte) {
                self.matched = self.border(self.matched);
            }
            if self.needle.get(self.matched) == Some(&byte) {
                self.matched += 1;
            }
            if self.matched == len {
                self.matched = self.border(len);
                return Some(self.at - len);
            }
        }
        None
    }
}

/// Where a needle occurs: apart (non-overlapping, the standard library's
/// search) or every occurrence (whole words).
pub(crate) enum Starts<'a> {
    Apart(MatchIndices<'a, &'a str>),
    All(Overlapping<'a>),
}

impl Iterator for Starts<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        match self {
            Self::Apart(found) => found.next().map(|(start, _)| start),
            Self::All(found) => found.next(),
        }
    }
}

/// The occurrences of `needle` at `starts` that `keep` accepts, from left
/// to right, none overlapping one accepted before it.
pub(crate) fn accepted<'a>(
    starts: Starts<'a>,
    needle_len: usize,
    mut keep: impl FnMut(Range<usize>) -> bool + 'a,
) -> impl Iterator<Item = Range<usize>> + 'a {
    let mut from = 0;
    starts
        .map(move |start| start..start + needle_len)
        .filter(move |range| {
            let accept = range.start >= from && keep(range.clone());
            if accept {
                from = range.end;
            }
            accept
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(hay: &str, needle: &str) -> Vec<usize> {
        let table = borders(needle.as_bytes());
        Overlapping::new(hay, needle, &table).collect()
    }

    #[test]
    fn borders_of_periodic_needles() {
        assert_eq!(borders(b"abab"), vec![0, 0, 1, 2]);
        assert_eq!(borders(b"aaa"), vec![0, 1, 2]);
        assert_eq!(borders(b"abc"), vec![0, 0, 0]);
        assert!(borders(b"").is_empty());
    }

    #[test]
    fn every_occurrence_overlapping_ones_too() {
        assert_eq!(all("aaaa", "aa"), vec![0, 1, 2]);
        assert_eq!(all("abababa", "aba"), vec![0, 2, 4]);
        assert_eq!(all("xab-ab-ab", "ab-ab"), vec![1, 4]);
        assert_eq!(all("公公公", "公公"), vec![0, 3]);
        assert!(all("ab", "abc").is_empty());
        assert!(all("", "a").is_empty());
        // A table that does not belong to the needle finds nothing.
        assert_eq!(Overlapping::new("aa", "a", &[]).count(), 0);
    }

    #[test]
    fn accepted_occurrences_do_not_overlap() {
        let table = borders(b"aa");
        let found: Vec<_> = accepted(
            Starts::All(Overlapping::new("aaaaa", "aa", &table)),
            2,
            |_| true,
        )
        .collect();
        assert_eq!(found, vec![0..2, 2..4]);
        let found: Vec<_> = accepted(
            Starts::All(Overlapping::new("aaaaa", "aa", &table)),
            2,
            |r| r.start != 0,
        )
        .collect();
        assert_eq!(found, vec![1..3, 3..5]);
    }
}
