//! PDF dates (ISO 32000-2 §7.9.4): `D:YYYYMMDDHHmmSSOHH'mm'`, read
//! leniently but within bounds.
//!
//! Real files leave out the `D:` prefix, the trailing apostrophe, the
//! minutes of the offset or every field after the year; all of that is
//! accepted. Anything else (a month 13, letters, a second date glued to the
//! first, text longer than any date) is not a date: the caller shows the
//! text as it was written instead. Parsing never panics.

/// A date as written in a PDF, in the offset it was written with (no
/// time-zone conversion). Fields left out of the text have their ISO 32000
/// defaults (month and day 1, time 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdfDate {
    pub year: u16,
    /// 1 to 12.
    pub month: u8,
    /// 1 to 31.
    pub day: u8,
    /// 0 to 23.
    pub hour: u8,
    /// 0 to 59.
    pub minute: u8,
    /// 0 to 59.
    pub second: u8,
    /// Offset from UTC in minutes as written: `Z` is 0, `-00'30'` is -30.
    /// `None` when the date names no offset.
    pub utc_offset_minutes: Option<i16>,
}

impl PdfDate {
    /// Longest text looked at; longer text is not a date.
    pub const MAX_LEN: usize = 64;

    /// The date `text` writes, if it is one.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim_matches(|c: char| c.is_ascii_whitespace() || c == '\0');
        if text.len() > Self::MAX_LEN || !text.is_ascii() {
            return None;
        }
        let bytes = text.as_bytes();
        let mut r = Reader {
            bytes: bytes.strip_prefix(b"D:").unwrap_or(bytes),
        };
        let year = u16::try_from(r.digits(4)?).ok()?;
        // Each field is optional, but only after the ones before it.
        let mut fields = [1u8, 1, 0, 0, 0];
        let ranges = [1..=12, 1..=31, 0..=23, 0..=59, 0..=59];
        for (field, range) in fields.iter_mut().zip(ranges) {
            if !r.next_is_digit() {
                break;
            }
            let value = u8::try_from(r.digits(2)?).ok()?;
            if !range.contains(&value) {
                return None;
            }
            *field = value;
        }
        let [month, day, hour, minute, second] = fields;
        let utc_offset_minutes = r.offset()?;
        r.bytes.is_empty().then_some(Self {
            year,
            month,
            day,
            hour,
            minute,
            second,
            utc_offset_minutes,
        })
    }
}

/// What is left of the date text.
struct Reader<'a> {
    bytes: &'a [u8],
}

impl Reader<'_> {
    fn next_is_digit(&self) -> bool {
        self.bytes.first().is_some_and(u8::is_ascii_digit)
    }

    /// Exactly `n` digits.
    fn digits(&mut self, n: usize) -> Option<u32> {
        let (head, rest) = self.bytes.split_at_checked(n)?;
        if !head.iter().all(u8::is_ascii_digit) {
            return None;
        }
        self.bytes = rest;
        Some(head.iter().fold(0u32, |v, d| v * 10 + u32::from(d - b'0')))
    }

    fn skip(&mut self, byte: u8) {
        if let Some(rest) = self.bytes.strip_prefix(&[byte]) {
            self.bytes = rest;
        }
    }

    /// The offset part: nothing (`Some(None)`), `Z`, or a sign, hours and
    /// optional minutes, apostrophes optional. `None` when it is malformed.
    fn offset(&mut self) -> Option<Option<i16>> {
        let Some((&first, rest)) = self.bytes.split_first() else {
            return Some(None);
        };
        self.bytes = rest;
        let sign = match first {
            // Some writers add a zero offset after the Z; it says nothing more.
            b'Z' => {
                if !self.bytes.is_empty() {
                    self.hours_minutes()?;
                }
                return Some(Some(0));
            }
            b'+' => 1,
            b'-' => -1,
            _ => return None,
        };
        let (hours, minutes) = self.hours_minutes()?;
        let total = i16::try_from(hours * 60 + minutes).ok()?;
        Some(Some(sign * total))
    }

    /// `HH`, then optionally `'`, `mm` and `'`.
    fn hours_minutes(&mut self) -> Option<(u32, u32)> {
        let hours = self.digits(2).filter(|h| *h <= 23)?;
        self.skip(b'\'');
        let minutes = if self.next_is_digit() {
            self.digits(2).filter(|m| *m <= 59)?
        } else {
            0
        };
        self.skip(b'\'');
        Some((hours, minutes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(
        (year, month, day): (u16, u8, u8),
        (hour, minute, second): (u8, u8, u8),
        offset: Option<i16>,
    ) -> Option<PdfDate> {
        Some(PdfDate {
            year,
            month,
            day,
            hour,
            minute,
            second,
            utc_offset_minutes: offset,
        })
    }

    #[test]
    fn full_dates_keep_their_offset() {
        assert_eq!(
            PdfDate::parse("D:20261024153000+08'00'"),
            date((2026, 10, 24), (15, 30, 0), Some(480))
        );
        assert_eq!(
            PdfDate::parse("D:20261024153000Z"),
            date((2026, 10, 24), (15, 30, 0), Some(0))
        );
        assert_eq!(
            PdfDate::parse("D:20261024153000Z00'00'"),
            date((2026, 10, 24), (15, 30, 0), Some(0))
        );
        // The sign of an offset under an hour is kept.
        assert_eq!(
            PdfDate::parse("D:20261024153000-00'30'"),
            date((2026, 10, 24), (15, 30, 0), Some(-30))
        );
        assert_eq!(
            PdfDate::parse("D:19991231235959-08'00"),
            date((1999, 12, 31), (23, 59, 59), Some(-480))
        );
        assert_eq!(
            PdfDate::parse("D:20261024153000+0530"),
            date((2026, 10, 24), (15, 30, 0), Some(330))
        );
        assert_eq!(
            PdfDate::parse("D:20261024153000+08"),
            date((2026, 10, 24), (15, 30, 0), Some(480))
        );
    }

    #[test]
    fn partial_dates_and_missing_prefix() {
        assert_eq!(
            PdfDate::parse("D:2026102415"),
            date((2026, 10, 24), (15, 0, 0), None)
        );
        assert_eq!(
            PdfDate::parse("D:2026"),
            date((2026, 1, 1), (0, 0, 0), None)
        );
        assert_eq!(
            PdfDate::parse("20200101"),
            date((2020, 1, 1), (0, 0, 0), None)
        );
        assert_eq!(
            PdfDate::parse(" D:20200101120000Z\0"),
            date((2020, 1, 1), (12, 0, 0), Some(0))
        );
    }

    #[test]
    fn anything_else_is_not_a_date() {
        for text in [
            "",
            "D:",
            "D:20",
            "D:2026-13-45",
            "D:20261324",
            "D:20261000",
            "D:2026102424",
            "D:202610241560",
            "D:20261024153000+24'00'",
            "D:20261024153000+08'60'",
            "D:20261024153000+8",
            "D:2026102415300",
            "D:20261024153000 garbage",
            "D:20261024153000+08'00'D:20261024153000",
            "yesterday",
            "D:２０２６",
            "D:2026年",
        ] {
            assert_eq!(PdfDate::parse(text), None, "{text:?}");
        }
        // Longer than any date, even when it starts like one.
        let long = format!("D:20261024153000Z{}", "0".repeat(10_000));
        assert_eq!(PdfDate::parse(&long), None);
        assert_eq!(PdfDate::parse(&"9".repeat(10_000)), None);
    }
}
