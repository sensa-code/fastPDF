//! GPUI views for FastPDF; the only library crate that knows GPUI.
//!
//! # Data flow
//!
//! ```text
//! render workers ── Pixmap ─► convert (worker thread) ─► Arc<RenderImage>
//!        │                                                   │
//!        └─ wake() ─► futures channel ─► UI task ─► cx.notify()
//!                                                            ▼
//! ReaderView::render ─► toolbar + document area ─► canvas prepaint:
//!     DocumentSession::resize / frame()  (drains finished tiles)
//!                                      ─► canvas paint:
//!     TileTextures::begin_frame  (drop_image for evicted tiles)
//!     page backgrounds, stand-in tiles, exact tiles (upload budget)
//! ```
//!
//! * [`DocumentSession`](fastpdf_core::DocumentSession) owns layout,
//!   navigation and the tile pipeline; this crate only feeds it input and
//!   paints its [`Frame`](fastpdf_core::Frame).
//! * GPU texture lifetime is explicit (docs/audit/gpui.md): every tile the
//!   tile cache evicts reaches the UI thread through a retire queue and is
//!   released with `Window::drop_image`; closing a document releases all of
//!   them. GPUI never frees atlas space on its own.
//! * Nothing runs on a timer: frames happen on input, on a worker wake-up,
//!   or while a bounded amount of texture upload work is pending.
//! * Find (Ctrl+F), text selection and the sidebar (outline, thumbnails)
//!   start their background work only when used: text is extracted through
//!   one byte-budgeted `TextCache`, thumbnails render only while visible.
//!   Match case (Alt+C) and whole word (Alt+W) are find-bar toggles
//!   (`find_options.rs`); Ctrl+Shift+F lists every hit in the sidebar
//!   (`search_results.rs`).
//! * Back and Forward (Alt+Left / Alt+Right, the mouse's side buttons, the
//!   toolbar's arrows) return between views left by a jump: go to page,
//!   first or last page, the outline, thumbnails, links, search hits
//!   (`navigation.rs`, a bounded `fastpdf_core::NavigationHistory`).
//! * Page labels (`page_labels.rs`) are asked for after the first page is
//!   exact and shown in the toolbar, thumbnails and scroll bar; Ctrl+G goes
//!   to a page by number or label (`go_to_page.rs`).
//! * Documents that need a password ask for it in a prompt card in the
//!   document area (`password.rs`, `prompt.rs`, ADR 0014); they are never
//!   written.
//! * Print (Ctrl+P) opens a small panel; jobs run on their own thread
//!   through `fastpdf_print` (Win32 GDI, banded) and report progress
//!   through the same wake-up channel.
//! * Start-up (`startup.rs`): the command-line document opens, and its
//!   first page renders, on a thread while GPUI starts, so the first frame
//!   can show exact tiles.
//! * Documents reopen where they were left (`reading.rs`): page, offset,
//!   zoom and rotation, kept next to the recent-files list; the restored
//!   page is the first one rendered.
//! * Text is English or Traditional Chinese (`i18n.rs`), after the Windows
//!   UI language unless the settings choose; mouse-wheel notches scroll
//!   with a short ease-out (`smooth_scroll.rs`).
//! * Ctrl+D (or the toolbar's ⓘ) shows the document's properties
//!   (`properties.rs`); the engine is asked only then. Toolbar controls a
//!   narrow window has no room for move into a "⋯" menu
//!   (`toolbar/overflow.rs`).
//! * Links on the pages are read when the pointer first moves over a page
//!   (`links.rs`); a click follows an internal link, a drag still selects;
//!   http, https and mailto links open after a confirmation (`link_open.rs`,
//!   ADR 0015), nothing else does.
//! * Chrome follows the Windows light/dark app mode unless the user picks
//!   one; night mode inverts the pages (`ColorMode::Inverted`). Both, the
//!   sidebar, the default zoom and the window placement persist in a small
//!   `key = value` settings file (`settings.rs`, std only).

mod actions;
mod bench;
mod devscript;
mod doc_queries;
mod document;
mod empty_state;
mod find;
mod find_options;
mod go_to_page;
mod hit_list;
mod i18n;
mod input;
mod link_open;
mod link_prompt;
mod link_target;
mod links;
mod navigation;
mod opening;
mod overlay;
mod page_labels;
mod panel_slot;
mod password;
mod print;
mod prompt;
mod properties;
mod reader;
mod reading;
mod scrollbar;
mod search_results;
mod select;
mod settings;
mod settings_panel;
mod sidebar;
mod signing;
mod smooth_scroll;
mod stamps;
mod startup;
mod text_edits;
mod text_input;
mod text_mask;
mod textures;
mod theme;
mod toolbar;
mod tools;
mod viewport;
mod window_placement;

pub use actions::{KEY_CONTEXT, bind_keys};
pub use bench::{BenchEvent, BenchHook};
pub use document::{
    LockedSource, OpenFailure, OpenedDocument, PendingOpen, open_document_blocking,
};
pub use reader::{ReaderOptions, ReaderView, WriteRunner, open_reader_window};
pub use startup::{ScreenGuess, Startup};
pub use textures::DEFAULT_UPLOAD_BUDGET;
