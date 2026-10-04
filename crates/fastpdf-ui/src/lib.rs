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
//! * Print (Ctrl+P) opens a small panel; jobs run on their own thread
//!   through `fastpdf_print` (Win32 GDI, banded) and report progress
//!   through the same wake-up channel.
//! * Start-up (`startup.rs`): the command-line document opens, and its
//!   first page renders, on a thread while GPUI starts, so the first frame
//!   can show exact tiles.
//! * Text is English or Traditional Chinese (`i18n.rs`), after the Windows
//!   UI language unless the settings choose; mouse-wheel notches scroll
//!   with a short ease-out (`smooth_scroll.rs`).
//! * Chrome follows the Windows light/dark app mode unless the user picks
//!   one; night mode inverts the pages (`ColorMode::Inverted`). Both, the
//!   sidebar, the default zoom and the window placement persist in a small
//!   `key = value` settings file (`settings.rs`, std only).

mod actions;
mod bench;
mod devscript;
mod document;
mod find;
mod i18n;
mod overlay;
mod print;
mod reader;
mod select;
mod settings;
mod settings_panel;
mod sidebar;
mod smooth_scroll;
mod startup;
mod text_input;
mod textures;
mod theme;
mod toolbar;
mod viewport;

pub use actions::{KEY_CONTEXT, bind_keys};
pub use bench::{BenchEvent, BenchHook};
pub use document::{OpenFailure, OpenedDocument, PendingOpen, open_document_blocking};
pub use reader::{ReaderOptions, ReaderView, open_reader_window};
pub use startup::{ScreenGuess, Startup};
pub use textures::DEFAULT_UPLOAD_BUDGET;
