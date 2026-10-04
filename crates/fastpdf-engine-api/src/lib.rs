//! FastPDF domain model and the engine-neutral `PdfEngine` abstraction.
//!
//! Everything above this crate (reader core, renderer, UI) speaks only these
//! types. Concrete PDF engines (Hayro, zpdf, ...) live in adapter crates that
//! translate their own types into this model, so engines stay replaceable
//! (spec §4, §6, §7; ADR 0002).
//!
//! Coordinate conventions used throughout:
//!
//! * **Page space** ([`PageRect`]): PDF points (1/72 in), origin at the
//!   top-left corner of the page's visible box (CropBox), y pointing down,
//!   *before* any rotation. Text boxes, link rectangles and destinations use
//!   this space so they are independent of zoom and rotation.
//! * **Pixel space** ([`PixelRect`]): device pixels of a page rendered at a
//!   given [`RenderScale`] *after* rotation, origin top-left. Render regions
//!   (tiles) are expressed in this space.

mod cancel;
mod engine;
mod error;
mod geometry;
mod guard;
mod ids;
mod limits;
mod metadata;
mod nav;
mod pixmap;
mod render;
mod source;
mod text;

pub use cancel::CancelToken;
pub use engine::{
    EngineCapabilities, EngineDocument, EngineInfo, MemoryPressure, OpenOptions, PageInfo,
    PdfEngine,
};
pub use error::{EngineError, LimitKind};
pub use geometry::{PageRect, PageSize, PixelRect, PixelSize, RenderScale, Rotation};
pub use guard::{GuardedDocument, open_guarded};
pub use ids::{DocumentId, PageId, PageIndex};
pub use limits::ResourceLimits;
pub use metadata::DocumentMetadata;
pub use nav::{Destination, DestinationView, Link, LinkTarget, OutlineItem};
pub use pixmap::{PixelFormat, Pixmap, PixmapMut, Rgba8};
pub use render::{ColorMode, RenderOutcome, RenderRequest};
pub use source::{DocumentSource, SharedBytes};
pub use text::{TextLayer, TextSpan};
