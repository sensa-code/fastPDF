//! Incremental full-text search (spec §22).
//!
//! * Nothing is indexed when a document opens; the subsystem starts on the
//!   first search.
//! * Pages are searched from the current page outward (current → near →
//!   rest), so nearby hits show up first.
//! * Hits are streamed as they are found ("3 results found so far…"), and a
//!   search can be cancelled at any time (a new query cancels the old one).
//! * A search keeps at most [`SearchQuery::max_hits`] hits (20,000 by
//!   default), the ones nearest the start page, and then stops: no page past
//!   the cap is extracted, and the result list stays bounded.
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
//! * Options: match case, and whole words (a word next to CJK text counts
//!   as whole). Full-width ASCII forms always match their ASCII
//!   counterparts (「ＰＤＦ」 and `PDF`), one character for one, so
//!   highlights cover the glyphs as drawn; full-width letters keep the CJK
//!   spacing rule, so 「ＡＢＣ　ＤＥＦ」 matches `ABCDEF`, not `ABC DEF`.
//! * Every hit carries a [`Snippet`]: the page's own characters around it,
//!   at most 80, made safe to show.

mod fold;
mod matcher;
mod order;
mod page_text;
mod session;
mod snippet;

pub use fold::is_word_char;
pub use matcher::{Match, Matcher};
pub use order::search_order;
pub use session::{SearchEvent, SearchHit, SearchQuery, SearchSession, TextCache};
pub use snippet::Snippet;
