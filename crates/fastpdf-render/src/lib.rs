//! Tile-based rendering pipeline (spec §12–§14, §17–§18; ADR 0003).
//!
//! * [`ScaleBucket`] quantizes arbitrary zoom levels into a small set of
//!   render resolutions so the tile cache does not fragment.
//! * [`TileGrid`] splits a rendered page into fixed-size tiles; only tiles
//!   that intersect the viewport are rendered.
//! * [`DocumentLayout`] + [`Viewport`] describe the continuous-scroll view;
//!   [`plan_tiles`] turns them into prioritized tile requests.
//! * [`RenderScheduler`] runs those requests on a bounded worker pool,
//!   cancelling work the user has scrolled away from.
//! * [`TileCache`] stores finished tiles under a byte budget and finds
//!   lower/higher-resolution stand-ins while a zoom re-render is pending.

mod layout;
mod plan;
mod scale;
mod scheduler;
mod tile;
mod tile_cache;
mod viewport;

pub use layout::{DocumentLayout, LayoutRect, ScrollAnchor};
pub use plan::{PlanConfig, PlannedTile, Priority, RenderJob, plan_tiles};
pub use scale::{ScaleBucket, ZoomLevel};
pub use scheduler::{Lane, RenderScheduler, SchedulerConfig, SchedulerStats, TileResult};
pub use tile::{DEFAULT_TILE_SIZE, TileCoord, TileGrid, TileKey};
pub use tile_cache::{Fallback, TileCache};
pub use viewport::{POINTS_TO_PX, Viewport};
