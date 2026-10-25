//! Reader core: everything between the UI and the engine API.
//!
//! Grows milestone by milestone (document sessions, navigation, search
//! orchestration, memory pressure). Document loading is shared with the
//! benchmark harness so both measure the same path; [`DocumentSession`] is
//! the toolkit-independent heart of the viewer. [`ViewState`] and
//! [`SavedView`] say where its view is; [`NavigationHistory`] keeps the
//! views Back and Forward return to; [`links`] checks and caches the links
//! of the pages the pointer visits; [`positions`] remembers where each
//! document was left.

pub mod go_to;
pub mod history;
pub mod keymap;
pub mod links;
pub mod loader;
pub mod memory;
pub mod paths;
pub mod positions;
pub mod recent;
mod retry;
pub mod selection;
pub mod session;
pub mod view_state;

pub use fastpdf_render::{LayoutMode, Spread};
pub use history::{HISTORY_CAPACITY, Jump, NavigationHistory};
pub use session::{
    DocumentSession, Frame, FramePage, FrameTile, ScrollAxis, ScrollBars, ScrollThumb,
    SessionConfig, SessionStats, ZoomMode,
};
pub use view_state::{RestoreScope, SavedView, SavedZoom, ViewState};
