//! Reader core: everything between the UI and the engine API.
//!
//! Grows milestone by milestone (document sessions, navigation, search
//! orchestration, memory pressure). Document loading is shared with the
//! benchmark harness so both measure the same path; [`DocumentSession`] is
//! the toolkit-independent heart of the viewer.

pub mod keymap;
pub mod loader;
pub mod memory;
pub mod paths;
pub mod recent;
pub mod selection;
pub mod session;

pub use session::{
    DocumentSession, Frame, FramePage, FrameTile, SessionConfig, SessionStats, ZoomMode,
};
