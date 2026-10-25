//! The reading positions file, version 1: UTF-8 text, newest first, one
//! document per line, fields separated by TAB (spaces below) and the path
//! last.
//!
//! ```text
//! # FastPDF reading positions v1, written by FastPDF. Newest first, at most 200 files.
//! # page offset x zoom rotation pages bytes modified path
//! 1500 0.2513 0 fit-width 0 2000 1234567 1760000000000000000 C:\docs\book.pdf
//! ```
//!
//! `page` is one-based (the first page of the row at the top of the view);
//! `offset` how far below that row's top the view starts, as a fraction of
//! the row height (`-1..=2`); `x` the horizontal position as a fraction of
//! the document width (`0..=1`); `zoom` is `fit-width`, `fit-page` or a
//! factor (`2.5` = 250%); `rotation` the view rotation in degrees; `pages`
//! the page count, `bytes` and `modified` (nanoseconds since 1970) the
//! file's size and time when the view was saved, both `-` when unknown.
//!
//! Reading never fails: a line that cannot be used is skipped and the
//! others still apply (a BOM and CRLF line ends are accepted). Paths that
//! would not read back the same (not UTF-8, control characters, empty,
//! longer than [`MAX_PATH_CHARS`]) are never stored (`writable`), and
//! writing stops before the text grows past the file size cap.

use std::path::{Path, PathBuf};

use fastpdf_engine_api::{PageIndex, Rotation};

use super::{FileIdentity, MAX_ENTRIES, MAX_FILE_BYTES, PositionRecord};
use crate::view_state::{SavedView, SavedZoom};

/// Longest path kept, in characters.
pub(super) const MAX_PATH_CHARS: usize = 2048;

/// One document's line: its path and record.
pub(super) type Entry = (PathBuf, PositionRecord);

/// The usable lines of `text`, in file order (newest first), at most
/// [`MAX_ENTRIES`] of them.
pub(super) fn parse(text: &str) -> Vec<Entry> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    text.lines()
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .filter_map(parse_line)
        .take(MAX_ENTRIES)
        .collect()
}

fn parse_line(line: &str) -> Option<Entry> {
    let fields: Vec<&str> = line.split('\t').collect();
    let [
        page,
        offset,
        x,
        zoom,
        rotation,
        pages,
        bytes,
        modified,
        path,
    ] = fields[..]
    else {
        return None;
    };
    let page: u32 = page.parse().ok().filter(|&p| p >= 1)?;
    let page_count: u32 = pages.parse().ok().filter(|&n| n >= page)?;
    let offset = number(offset).filter(|v| (-1.0..=2.0).contains(v))?;
    let x = number(x).filter(|v| (0.0..=1.0).contains(v))?;
    let zoom = match zoom {
        "fit-width" => SavedZoom::FitWidth,
        "fit-page" => SavedZoom::FitPage,
        factor => SavedZoom::Custom(number(factor).filter(|&z| z > 0.0)?),
    };
    let rotation = match rotation {
        "0" => Rotation::R0,
        "90" => Rotation::R90,
        "180" => Rotation::R180,
        "270" => Rotation::R270,
        _ => return None,
    };
    let identity = match (bytes, modified) {
        ("-", "-") => None,
        (len, modified) => Some(FileIdentity {
            len: len.parse().ok()?,
            modified: modified.parse().ok()?,
        }),
    };
    let path = writable(Path::new(path))?;
    let view = SavedView {
        page: PageIndex::new(page - 1),
        offset,
        x,
        zoom,
        rotation,
    }
    .sanitized();
    let record = PositionRecord {
        view,
        page_count,
        identity,
    };
    Some((PathBuf::from(path), record))
}

/// A finite number.
fn number(text: &str) -> Option<f32> {
    text.parse::<f32>().ok().filter(|v| v.is_finite())
}

/// `path` as text when it can be written and read back unchanged: valid
/// UTF-8 (`to_str` is exact, so a real U+FFFD in a name is kept), on one
/// line without TAB (no control characters), not empty, not too long.
pub(super) fn writable(path: &Path) -> Option<&str> {
    let text = path.to_str()?;
    let fits = !text.is_empty()
        && text.chars().count() <= MAX_PATH_CHARS
        && !text.chars().any(char::is_control);
    fits.then_some(text)
}

/// The file contents for `entries` (newest first), at most [`MAX_ENTRIES`]
/// lines and [`MAX_FILE_BYTES`] bytes, and how many entries it holds.
pub(super) fn to_text(entries: &[Entry]) -> (String, usize) {
    let mut out = format!(
        "# FastPDF reading positions v1, written by FastPDF. Newest first, at most {MAX_ENTRIES} files.\n\
         # page\toffset\tx\tzoom\trotation\tpages\tbytes\tmodified\tpath\n"
    );
    let mut written = 0;
    for (path, record) in entries {
        if written == MAX_ENTRIES {
            break;
        }
        let Some(path) = writable(path) else {
            continue;
        };
        let line = line(path, record);
        if (out.len() + line.len()) as u64 > MAX_FILE_BYTES {
            break;
        }
        out.push_str(&line);
        written += 1;
    }
    (out, written)
}

fn line(path: &str, record: &PositionRecord) -> String {
    let view = super::quantized(record.view);
    let zoom = match view.zoom {
        SavedZoom::FitWidth => "fit-width".to_string(),
        SavedZoom::FitPage => "fit-page".to_string(),
        SavedZoom::Custom(z) => decimal(z),
    };
    let (bytes, modified) = match record.identity {
        Some(id) => (id.len.to_string(), id.modified.to_string()),
        None => ("-".to_string(), "-".to_string()),
    };
    format!(
        "{}\t{}\t{}\t{zoom}\t{}\t{}\t{bytes}\t{modified}\t{path}\n",
        view.page.display_number(),
        decimal(view.offset),
        decimal(view.x),
        view.rotation.degrees(),
        // A record never names a page past its document's end.
        record.page_count.max(view.page.display_number()),
    )
}

/// Four decimals at most, without trailing zeros (`0.25`, `2`, `-0.0125`).
fn decimal(value: f32) -> String {
    let text = format!("{value:.4}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    match text {
        "-0" | "" => "0".to_string(),
        text => text.to_string(),
    }
}
