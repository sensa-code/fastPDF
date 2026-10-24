//! Page labels on the wire (`Payload::PageLabels`).
//!
//! Per range: `u32 first page`, `u8 style`, `u32 start`, `str prefix`. The
//! encoder makes engine output representable (ranges in strictly
//! increasing page order, start values of at least 1, prefixes clipped to
//! the cap); the decoder rejects anything else outright, since a host that
//! sends it is broken or compromised. The parent normalizes the result
//! again with its own page count (`PageLabels::new`).

use fastpdf_engine_api::{PageIndex, PageLabelRange, PageLabelStyle, PageLabels};

use super::wire::{Decoder, Encoder, ProtocolError, Result, clip};

/// Ranges in one reply at most.
pub(crate) const MAX_LABEL_RANGES: usize = PageLabels::MAX_RANGES;
/// Bytes of one prefix at most.
pub(crate) const MAX_LABEL_PREFIX: usize = PageLabels::MAX_PREFIX_BYTES;

/// first page (4) + style (1) + start (4) + prefix length (4)
const MIN_RANGE_BYTES: usize = 13;

pub(super) fn put_page_labels(e: &mut Encoder, ranges: &[PageLabelRange]) {
    let mut kept = Vec::with_capacity(ranges.len().min(MAX_LABEL_RANGES));
    let mut last: Option<PageIndex> = None;
    for range in ranges {
        if kept.len() == MAX_LABEL_RANGES {
            break;
        }
        if last.is_some_and(|last| range.first_page <= last) {
            continue;
        }
        last = Some(range.first_page);
        kept.push(range);
    }
    e.count(kept.len());
    for range in kept {
        e.u32(range.first_page.get());
        e.u8(style_tag(range.style));
        e.u32(range.start.max(1));
        e.str(clip(&range.prefix, MAX_LABEL_PREFIX));
    }
}

pub(super) fn get_page_labels(d: &mut Decoder<'_>) -> Result<Vec<PageLabelRange>> {
    let n = d.count("page labels", MAX_LABEL_RANGES, MIN_RANGE_BYTES)?;
    let mut ranges: Vec<PageLabelRange> = Vec::with_capacity(n);
    for _ in 0..n {
        let first_page = PageIndex::new(d.u32()?);
        if ranges.last().is_some_and(|r| first_page <= r.first_page) {
            return Err(ProtocolError::BadValue("page label order"));
        }
        let style = style_from_tag(d.u8()?)?;
        let start = d.u32()?;
        if start == 0 {
            return Err(ProtocolError::BadValue("page label start"));
        }
        let prefix = d.str("page label prefix", MAX_LABEL_PREFIX)?;
        ranges.push(PageLabelRange {
            first_page,
            style,
            prefix,
            start,
        });
    }
    Ok(ranges)
}

fn style_tag(style: PageLabelStyle) -> u8 {
    match style {
        PageLabelStyle::None => 0,
        PageLabelStyle::Decimal => 1,
        PageLabelStyle::UpperRoman => 2,
        PageLabelStyle::LowerRoman => 3,
        PageLabelStyle::UpperLetters => 4,
        PageLabelStyle::LowerLetters => 5,
    }
}

fn style_from_tag(tag: u8) -> Result<PageLabelStyle> {
    Ok(match tag {
        0 => PageLabelStyle::None,
        1 => PageLabelStyle::Decimal,
        2 => PageLabelStyle::UpperRoman,
        3 => PageLabelStyle::LowerRoman,
        4 => PageLabelStyle::UpperLetters,
        5 => PageLabelStyle::LowerLetters,
        t => return Err(ProtocolError::BadTag("page label style", t)),
    })
}

#[cfg(test)]
mod tests {
    use super::super::{
        CMD_PAGE_LABELS, Command, Payload, REPLY_DONE, Reply, decode_command, decode_reply,
        encode_command, encode_reply,
    };
    use super::*;

    fn range(first: u32, style: PageLabelStyle, prefix: &str, start: u32) -> PageLabelRange {
        PageLabelRange {
            first_page: PageIndex::new(first),
            style,
            prefix: prefix.to_owned(),
            start,
        }
    }

    /// The reply payload (without the frame length) carrying `ranges`.
    fn reply_bytes(ranges: Vec<PageLabelRange>) -> Vec<u8> {
        encode_reply(&Reply::Done {
            id: 3,
            result: Ok(Payload::PageLabels(ranges)),
        })[4..]
            .to_vec()
    }

    fn decoded(bytes: &[u8]) -> Result<Vec<PageLabelRange>> {
        match decode_reply(bytes)? {
            Reply::Done {
                result: Ok(Payload::PageLabels(ranges)),
                ..
            } => Ok(ranges),
            other => panic!("not page labels: {other:?}"),
        }
    }

    // version (2) + kind (1) + id (8) + result tag (1) + payload tag (1)
    const COUNT_AT: usize = 13;
    const FIRST_RANGE_AT: usize = COUNT_AT + 4;

    #[test]
    fn page_labels_round_trip() {
        let ranges = vec![
            range(0, PageLabelStyle::None, "Cover", 1),
            range(1, PageLabelStyle::LowerRoman, "", 1),
            range(4, PageLabelStyle::Decimal, "附錄-", 5),
            range(9, PageLabelStyle::UpperLetters, "App-", u32::MAX),
            range(10, PageLabelStyle::UpperRoman, "", 2),
            range(u32::MAX, PageLabelStyle::LowerLetters, "z", 26),
        ];
        assert_eq!(decoded(&reply_bytes(ranges.clone())), Ok(ranges));
        assert_eq!(decoded(&reply_bytes(Vec::new())), Ok(Vec::new()));

        let cmd = Command::PageLabels { id: 42 };
        let frame = encode_command(&cmd).unwrap();
        assert_eq!(decode_command(&frame[4..]), Ok(cmd));
    }

    #[test]
    fn page_labels_are_rejected_in_the_wrong_direction() {
        let frame = encode_command(&Command::PageLabels { id: 1 }).unwrap();
        assert_eq!(
            decode_reply(&frame[4..]),
            Err(ProtocolError::UnknownKind(CMD_PAGE_LABELS))
        );
        assert_eq!(
            decode_command(&reply_bytes(vec![range(0, PageLabelStyle::Decimal, "", 1)])),
            Err(ProtocolError::UnknownKind(REPLY_DONE))
        );
    }

    #[test]
    fn invalid_page_labels_are_rejected() {
        let one = reply_bytes(vec![range(5, PageLabelStyle::Decimal, "p", 1)]);
        let style_at = FIRST_RANGE_AT + 4;
        let start_at = style_at + 1;

        let mut bad = one.clone();
        bad[style_at] = 6;
        assert_eq!(
            decoded(&bad),
            Err(ProtocolError::BadTag("page label style", 6))
        );

        let mut bad = one.clone();
        bad[start_at..start_at + 4].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(
            decoded(&bad),
            Err(ProtocolError::BadValue("page label start"))
        );

        // A second range that does not start after the first.
        let two = reply_bytes(vec![
            range(5, PageLabelStyle::Decimal, "p", 1),
            range(7, PageLabelStyle::Decimal, "q", 1),
        ]);
        let second_at = FIRST_RANGE_AT + MIN_RANGE_BYTES + 1;
        for page in [5u32, 4] {
            let mut bad = two.clone();
            bad[second_at..second_at + 4].copy_from_slice(&page.to_le_bytes());
            assert_eq!(
                decoded(&bad),
                Err(ProtocolError::BadValue("page label order"))
            );
        }

        // A prefix one byte over the cap.
        let mut bad = reply_bytes(vec![range(
            0,
            PageLabelStyle::None,
            &"a".repeat(MAX_LABEL_PREFIX),
            1,
        )]);
        let len_at = FIRST_RANGE_AT + 9;
        let over = MAX_LABEL_PREFIX as u32 + 1;
        bad[len_at..len_at + 4].copy_from_slice(&over.to_le_bytes());
        bad.push(b'a');
        assert_eq!(
            decoded(&bad),
            Err(ProtocolError::TooLong("page label prefix"))
        );

        // Invalid UTF-8 in a prefix.
        let mut bad = one.clone();
        let last = bad.len() - 1;
        bad[last] = 0xFF;
        assert_eq!(
            decoded(&bad),
            Err(ProtocolError::BadText("page label prefix"))
        );

        // Counts over the cap fail before anything is allocated.
        let mut bad = one;
        let over = MAX_LABEL_RANGES as u32 + 1;
        bad[COUNT_AT..COUNT_AT + 4].copy_from_slice(&over.to_le_bytes());
        assert_eq!(decoded(&bad), Err(ProtocolError::TooLong("page labels")));
        bad[COUNT_AT..COUNT_AT + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(decoded(&bad), Err(ProtocolError::TooLong("page labels")));
        bad[COUNT_AT..COUNT_AT + 4].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(decoded(&bad), Err(ProtocolError::Truncated));
    }

    #[test]
    fn engine_labels_are_sanitized_not_rejected() {
        let long = "測".repeat(100); // 300 bytes
        let back = decoded(&reply_bytes(vec![
            range(3, PageLabelStyle::Decimal, "a", 0),
            range(3, PageLabelStyle::Decimal, "duplicate", 1),
            range(1, PageLabelStyle::Decimal, "earlier", 1),
            range(8, PageLabelStyle::LowerRoman, &long, 2),
        ]))
        .unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0], range(3, PageLabelStyle::Decimal, "a", 1));
        assert!(back[1].prefix.len() <= MAX_LABEL_PREFIX);
        assert!(back[1].prefix.chars().all(|c| c == '測'));

        let many: Vec<_> = (0..MAX_LABEL_RANGES as u32 + 10)
            .map(|p| range(p, PageLabelStyle::Decimal, "", 1))
            .collect();
        assert_eq!(
            decoded(&reply_bytes(many)).map(|r| r.len()),
            Ok(MAX_LABEL_RANGES)
        );
    }
}
