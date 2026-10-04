//! Incremental full-text search (spec §22).
//!
//! * Nothing is indexed when a document opens; the subsystem starts on the
//!   first search.
//! * Pages are searched from the current page outward (current → near →
//!   rest), so nearby hits show up first.
//! * Hits are streamed as they are found ("3 results found so far…"), and a
//!   search can be cancelled at any time (a new query cancels the old one).
//! * Extracted text layers live in a byte-budgeted cache registered with the
//!   memory budget manager (retention TEXT: dropped early under pressure).
//! * A page is matched in reading order, across span and line boundaries:
//!   its text is laid out with the lines, order and spaces copy writes
//!   (`TextLayer::lay_out_lines`), then normalized like the query
//!   ([`Matcher`]): a line break or space next to a CJK character is nothing
//!   (「公」 / 「文」 matches 「公文」), between other characters one space.
//!   A hit that wraps has a highlight rectangle on each line. Pages lacking
//!   one of the query's characters are skipped without layout, and the
//!   normalized text of searched pages is cached with their text layer, so
//!   the next query only scans it.

mod matcher;
mod order;
mod page_text;
mod session;

pub use matcher::{Match, Matcher};
pub use order::search_order;
pub use session::{SearchEvent, SearchHit, SearchQuery, SearchSession, TextCache};
