//! The numeric part of a page label: decimal, roman and letter numbering
//! (ISO 32000-2 §12.4.2), with caps so a hostile start value can never
//! produce a long string.

use super::PageLabelStyle;

/// Longest numeric part written in roman or letter form; numbers that would
/// need more fall back to decimal.
pub(super) const MAX_NUMERIC_CHARS: usize = 32;

/// Thousands written as repeated `M` at most: 16 `M` plus at most 12
/// characters for the rest (`DCCCLXXXVIII`) stays within
/// [`MAX_NUMERIC_CHARS`].
const MAX_ROMAN_THOUSANDS: u64 = 16;

const ROMAN: [(u64, &str); 13] = [
    (1000, "M"),
    (900, "CM"),
    (500, "D"),
    (400, "CD"),
    (100, "C"),
    (90, "XC"),
    (50, "L"),
    (40, "XL"),
    (10, "X"),
    (9, "IX"),
    (5, "V"),
    (4, "IV"),
    (1, "I"),
];

/// Appends `n` in `style` to `out`. `PageLabelStyle::None` appends nothing.
pub(super) fn format(style: PageLabelStyle, n: u64, out: &mut String) {
    match style {
        PageLabelStyle::None => {}
        PageLabelStyle::Decimal => push_decimal(n, out),
        PageLabelStyle::UpperRoman => push_roman(n, false, out),
        PageLabelStyle::LowerRoman => push_roman(n, true, out),
        PageLabelStyle::UpperLetters => push_letters(n, false, out),
        PageLabelStyle::LowerLetters => push_letters(n, true, out),
    }
}

fn push_decimal(n: u64, out: &mut String) {
    use std::fmt::Write;
    // Writing into a String cannot fail.
    let _ = write!(out, "{n}");
}

fn push_roman(n: u64, lower: bool, out: &mut String) {
    if n == 0 || n / 1000 > MAX_ROMAN_THOUSANDS {
        return push_decimal(n, out);
    }
    let mut rest = n;
    for (value, digits) in ROMAN {
        while rest >= value {
            rest -= value;
            if lower {
                out.extend(digits.chars().map(|c| c.to_ascii_lowercase()));
            } else {
                out.push_str(digits);
            }
        }
    }
}

/// A, B, ..., Z, AA, BB, ..., ZZ, AAA, ...
fn push_letters(n: u64, lower: bool, out: &mut String) {
    let Some(index) = n.checked_sub(1) else {
        return push_decimal(n, out);
    };
    let repeats = index / 26 + 1;
    if repeats > MAX_NUMERIC_CHARS as u64 {
        return push_decimal(n, out);
    }
    let base = if lower { b'a' } else { b'A' };
    let letter = char::from(base + (index % 26) as u8);
    out.extend(std::iter::repeat_n(letter, repeats as usize));
}

/// The number `text` would be in `style`, ignoring ASCII case, or `None`
/// when it cannot be one. Callers check that formatting the number gives
/// `text` back, which rejects non-canonical forms such as `IIII`.
pub(super) fn parse(style: PageLabelStyle, text: &str) -> Option<u64> {
    if text.is_empty() || text.len() > MAX_NUMERIC_CHARS {
        return None;
    }
    // Every numbered style falls back to decimal for large numbers.
    if text.bytes().all(|b| b.is_ascii_digit()) {
        return text.parse().ok();
    }
    match style {
        PageLabelStyle::None | PageLabelStyle::Decimal => None,
        PageLabelStyle::UpperRoman | PageLabelStyle::LowerRoman => parse_roman(text),
        PageLabelStyle::UpperLetters | PageLabelStyle::LowerLetters => parse_letters(text),
    }
}

fn roman_digit(b: u8) -> Option<i64> {
    Some(match b.to_ascii_uppercase() {
        b'I' => 1,
        b'V' => 5,
        b'X' => 10,
        b'L' => 50,
        b'C' => 100,
        b'D' => 500,
        b'M' => 1000,
        _ => return None,
    })
}

/// A digit smaller than the one after it is subtracted (`IX` = 9). At most
/// [`MAX_NUMERIC_CHARS`] digits of at most 1000, so the sum cannot overflow.
fn parse_roman(text: &str) -> Option<u64> {
    let digits: Vec<i64> = text.bytes().map(roman_digit).collect::<Option<_>>()?;
    let mut total = 0i64;
    for (i, value) in digits.iter().enumerate() {
        match digits.get(i + 1) {
            Some(next) if next > value => total -= value,
            _ => total += value,
        }
    }
    u64::try_from(total).ok().filter(|t| *t > 0)
}

fn parse_letters(text: &str) -> Option<u64> {
    let first = text.bytes().next()?.to_ascii_uppercase();
    if !first.is_ascii_uppercase() || !text.bytes().all(|b| b.to_ascii_uppercase() == first) {
        return None;
    }
    let repeats = text.len() as u64;
    Some((repeats - 1) * 26 + u64::from(first - b'A') + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn formatted(style: PageLabelStyle, n: u64) -> String {
        let mut s = String::new();
        format(style, n, &mut s);
        s
    }

    fn round_trips(style: PageLabelStyle, n: u64) -> bool {
        let text = formatted(style, n);
        parse(style, &text) == Some(n)
    }

    #[test]
    fn roman_numerals_format_and_parse() {
        for n in 1..=3999 {
            assert!(round_trips(PageLabelStyle::UpperRoman, n), "{n}");
            assert!(round_trips(PageLabelStyle::LowerRoman, n), "{n}");
        }
        assert_eq!(formatted(PageLabelStyle::LowerRoman, 4), "iv");
        assert_eq!(formatted(PageLabelStyle::UpperRoman, 1994), "MCMXCIV");
        assert_eq!(formatted(PageLabelStyle::UpperRoman, 4000), "MMMM");
        let big = formatted(PageLabelStyle::UpperRoman, 16_888);
        assert!(big.starts_with(&"M".repeat(16)) && big.len() <= MAX_NUMERIC_CHARS);
        assert!(round_trips(PageLabelStyle::UpperRoman, 16_999));
        assert_eq!(formatted(PageLabelStyle::UpperRoman, 17_000), "17000");
        assert!(round_trips(PageLabelStyle::UpperRoman, 17_000));
        assert_eq!(
            formatted(PageLabelStyle::LowerRoman, u64::from(u32::MAX) + 7),
            "4294967302"
        );
        // Parsing is lenient; the caller's canonical check is not.
        assert_eq!(parse(PageLabelStyle::UpperRoman, "IIII"), Some(4));
        assert_ne!(formatted(PageLabelStyle::UpperRoman, 4), "IIII");
        assert_eq!(parse(PageLabelStyle::UpperRoman, "VX"), Some(5));
        assert_ne!(formatted(PageLabelStyle::UpperRoman, 5), "VX");
        assert_eq!(parse(PageLabelStyle::UpperRoman, "IQ"), None);
        assert_eq!(parse(PageLabelStyle::UpperRoman, ""), None);
    }

    #[test]
    fn letters_follow_the_spec() {
        let upper = PageLabelStyle::UpperLetters;
        assert_eq!(formatted(upper, 1), "A");
        assert_eq!(formatted(upper, 26), "Z");
        assert_eq!(formatted(upper, 27), "AA");
        assert_eq!(formatted(upper, 52), "ZZ");
        assert_eq!(formatted(upper, 53), "AAA");
        assert_eq!(formatted(PageLabelStyle::LowerLetters, 28), "bb");
        for n in 1..=26 * 32 {
            assert!(round_trips(upper, n), "{n}");
            assert!(round_trips(PageLabelStyle::LowerLetters, n), "{n}");
        }
        assert_eq!(formatted(upper, 26 * 32), "Z".repeat(32));
        assert_eq!(formatted(upper, 26 * 32 + 1), "833");
        assert_eq!(parse(upper, "AB"), None);
        assert_eq!(parse(upper, "aA"), Some(27));
    }

    #[test]
    fn decimal_and_none() {
        assert_eq!(formatted(PageLabelStyle::Decimal, 42), "42");
        assert_eq!(formatted(PageLabelStyle::None, 42), "");
        assert_eq!(parse(PageLabelStyle::Decimal, "042"), Some(42));
        assert_eq!(parse(PageLabelStyle::Decimal, "4a"), None);
        assert_eq!(parse(PageLabelStyle::None, "4a"), None);
        assert_eq!(parse(PageLabelStyle::Decimal, &"9".repeat(40)), None);
        assert_eq!(parse(PageLabelStyle::Decimal, "99999999999999999999"), None);
    }
}
