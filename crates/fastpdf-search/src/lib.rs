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

mod matcher;
mod order;
mod session;

pub use matcher::{Match, Matcher};
pub use order::search_order;
pub use session::{SearchEvent, SearchHit, SearchQuery, SearchSession, TextCache};
