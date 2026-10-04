//! Byte-level encoder and validating decoder for protocol frames.
//!
//! The decoder never trusts a length field: every count is checked against
//! the bytes that are actually left (times the smallest possible encoding of
//! one item) and against an explicit cap *before* anything is allocated, so a
//! hostile frame cannot make the reader allocate more than a small multiple
//! of its own size.

use std::fmt;

use super::PROTOCOL_VERSION;

/// Why a frame was rejected. Any of these means the peer is broken or
/// hostile; the connection is torn down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProtocolError {
    /// The frame ended in the middle of a field.
    Truncated,
    /// Bytes were left after the last field.
    TrailingBytes,
    /// The length prefix exceeds the frame cap for this direction.
    FrameTooLarge(u64),
    /// The frame was written by a different protocol version.
    Version(u16),
    /// Unknown message kind.
    UnknownKind(u8),
    /// Unknown tag of an enum-like field.
    BadTag(&'static str, u8),
    /// A string or collection is longer than its cap.
    TooLong(&'static str),
    /// A string is not valid UTF-8 (or UTF-16 for paths).
    BadText(&'static str),
    /// A field holds a value outside its domain (NaN, zero size, ...).
    BadValue(&'static str),
    /// The outline nests deeper than the protocol allows.
    TooDeep,
    /// A message that cannot be represented on the wire (sender side).
    Unencodable(&'static str),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => f.write_str("truncated frame"),
            Self::TrailingBytes => f.write_str("trailing bytes after the last field"),
            Self::FrameTooLarge(n) => write!(f, "frame of {n} bytes exceeds the cap"),
            Self::Version(v) => write!(f, "protocol version {v} (expected {PROTOCOL_VERSION})"),
            Self::UnknownKind(k) => write!(f, "unknown message kind {k:#04x}"),
            Self::BadTag(what, t) => write!(f, "invalid {what} tag {t}"),
            Self::TooLong(what) => write!(f, "{what} too long"),
            Self::BadText(what) => write!(f, "{what} is not valid text"),
            Self::BadValue(what) => write!(f, "invalid {what}"),
            Self::TooDeep => f.write_str("outline nested too deeply"),
            Self::Unencodable(what) => write!(f, "cannot encode {what}"),
        }
    }
}

impl std::error::Error for ProtocolError {}

pub(crate) type Result<T> = std::result::Result<T, ProtocolError>;

/// Builds one frame: `u32 length`, `u16 version`, `u8 kind`, fields.
pub(crate) struct Encoder {
    buf: Vec<u8>,
}

impl Encoder {
    pub(crate) fn new(kind: u8) -> Self {
        let mut buf = Vec::with_capacity(64);
        buf.extend_from_slice(&[0; 4]);
        buf.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
        buf.push(kind);
        Self { buf }
    }

    pub(crate) fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub(crate) fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub(crate) fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub(crate) fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub(crate) fn f32(&mut self, v: f32) {
        self.buf.extend_from_slice(&v.to_bits().to_le_bytes());
    }

    pub(crate) fn bool(&mut self, v: bool) {
        self.buf.push(u8::from(v));
    }

    /// A collection length. Callers cap collections far below `u32::MAX`.
    pub(crate) fn count(&mut self, n: usize) {
        self.u32(u32::try_from(n).unwrap_or(u32::MAX));
    }

    /// A length-prefixed UTF-8 string; callers clip it to its cap first.
    pub(crate) fn str(&mut self, s: &str) {
        self.count(s.len());
        self.buf.extend_from_slice(s.as_bytes());
    }

    pub(crate) fn opt_str(&mut self, s: Option<&str>) {
        match s {
            Some(s) => {
                self.u8(1);
                self.str(s);
            }
            None => self.u8(0),
        }
    }

    /// Reserves a `u32` to be filled in later with [`Self::patch_u32`].
    pub(crate) fn placeholder_u32(&mut self) -> usize {
        let at = self.buf.len();
        self.u32(0);
        at
    }

    pub(crate) fn patch_u32(&mut self, at: usize, v: u32) {
        if let Some(slot) = self.buf.get_mut(at..at + 4) {
            slot.copy_from_slice(&v.to_le_bytes());
        }
    }

    /// Payload bytes written so far (version and kind included).
    pub(crate) fn payload_len(&self) -> usize {
        self.buf.len() - 4
    }

    /// The finished frame, length prefix included.
    pub(crate) fn finish(mut self) -> Vec<u8> {
        let len = u32::try_from(self.buf.len() - 4).unwrap_or(u32::MAX);
        self.buf[..4].copy_from_slice(&len.to_le_bytes());
        self.buf
    }
}

/// Reads the fields of one frame payload (the bytes after the length prefix).
pub(crate) struct Decoder<'a> {
    data: &'a [u8],
}

impl<'a> Decoder<'a> {
    /// Checks the version and returns the decoder positioned after the kind.
    pub(crate) fn start(payload: &'a [u8]) -> Result<(Self, u8)> {
        let mut d = Self { data: payload };
        let version = d.u16()?;
        if version != PROTOCOL_VERSION {
            return Err(ProtocolError::Version(version));
        }
        let kind = d.u8()?;
        Ok((d, kind))
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.data.len() {
            return Err(ProtocolError::Truncated);
        }
        let (head, rest) = self.data.split_at(n);
        self.data = rest;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let bytes = self.take(N)?;
        let mut out = [0; N];
        out.copy_from_slice(bytes);
        Ok(out)
    }

    pub(crate) fn remaining(&self) -> usize {
        self.data.len()
    }

    pub(crate) fn u8(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    pub(crate) fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    pub(crate) fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    /// An `f32` that must be finite.
    pub(crate) fn f32(&mut self, what: &'static str) -> Result<f32> {
        let v = f32::from_bits(self.u32()?);
        if v.is_finite() {
            Ok(v)
        } else {
            Err(ProtocolError::BadValue(what))
        }
    }

    pub(crate) fn bool(&mut self, what: &'static str) -> Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            t => Err(ProtocolError::BadTag(what, t)),
        }
    }

    /// `0` = absent, `1` = present; anything else is invalid.
    pub(crate) fn present(&mut self, what: &'static str) -> Result<bool> {
        self.bool(what)
    }

    /// A collection length, checked against `max` and against the bytes
    /// left, given that every item takes at least `min_item_bytes`.
    pub(crate) fn count(
        &mut self,
        what: &'static str,
        max: usize,
        min_item_bytes: usize,
    ) -> Result<usize> {
        let n = usize::try_from(self.u32()?).map_err(|_| ProtocolError::TooLong(what))?;
        if n > max {
            return Err(ProtocolError::TooLong(what));
        }
        if n.saturating_mul(min_item_bytes.max(1)) > self.remaining() {
            return Err(ProtocolError::Truncated);
        }
        Ok(n)
    }

    pub(crate) fn str(&mut self, what: &'static str, max_bytes: usize) -> Result<String> {
        let n = self.count(what, max_bytes, 1)?;
        let bytes = self.take(n)?;
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| ProtocolError::BadText(what))
    }

    pub(crate) fn opt_str(
        &mut self,
        what: &'static str,
        max_bytes: usize,
    ) -> Result<Option<String>> {
        if self.present(what)? {
            self.str(what, max_bytes).map(Some)
        } else {
            Ok(None)
        }
    }

    /// Fails unless every byte was consumed.
    pub(crate) fn finish(self) -> Result<()> {
        if self.data.is_empty() {
            Ok(())
        } else {
            Err(ProtocolError::TrailingBytes)
        }
    }
}

/// Longest prefix of `s` that fits in `max` bytes without splitting a char.
pub(crate) fn clip(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_respects_char_boundaries() {
        assert_eq!(clip("abc", 5), "abc");
        assert_eq!(clip("abc", 2), "ab");
        // "測" is three bytes: clipping inside it drops the whole char.
        assert_eq!(clip("a測b", 2), "a");
        assert_eq!(clip("a測b", 4), "a測");
        assert_eq!(clip("測", 0), "");
    }

    #[test]
    fn counts_are_checked_before_allocation() {
        let mut enc = Encoder::new(1);
        enc.u32(u32::MAX);
        let frame = enc.finish();
        let (mut d, _) = Decoder::start(&frame[4..]).unwrap();
        assert_eq!(
            d.count("items", 10, 4),
            Err(ProtocolError::TooLong("items"))
        );

        let mut enc = Encoder::new(1);
        enc.u32(1000);
        let frame = enc.finish();
        let (mut d, _) = Decoder::start(&frame[4..]).unwrap();
        assert_eq!(d.count("items", 10_000, 4), Err(ProtocolError::Truncated));
    }

    #[test]
    fn wrong_version_is_rejected() {
        let mut frame = Encoder::new(1).finish();
        frame[4] ^= 0xFF;
        assert!(matches!(
            Decoder::start(&frame[4..]),
            Err(ProtocolError::Version(_))
        ));
    }

    #[test]
    fn floats_must_be_finite_and_bools_canonical() {
        let mut enc = Encoder::new(1);
        enc.f32(f32::NAN);
        enc.u8(2);
        let frame = enc.finish();
        let (mut d, _) = Decoder::start(&frame[4..]).unwrap();
        assert_eq!(d.f32("x"), Err(ProtocolError::BadValue("x")));
        assert_eq!(d.bool("flag"), Err(ProtocolError::BadTag("flag", 2)));
    }
}
