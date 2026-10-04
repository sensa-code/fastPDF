use crate::{PageIndex, PageRect};

/// Where a link or bookmark points inside the document.
#[derive(Debug, Clone, PartialEq)]
pub struct Destination {
    pub page: PageIndex,
    pub view: DestinationView,
}

/// How the target page should be shown. Coordinates are in page space.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum DestinationView {
    /// Keep the current zoom; scroll to the given point when present.
    Xyz {
        left: Option<f32>,
        top: Option<f32>,
        zoom: Option<f32>,
    },
    /// Fit the whole page.
    #[default]
    Fit,
    /// Fit the page width, scrolled to `top`.
    FitWidth { top: Option<f32> },
    /// Fit the page height, scrolled to `left`.
    FitHeight { left: Option<f32> },
    /// Fit the given rectangle.
    FitRect(PageRect),
}

/// One bookmark of the document outline.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OutlineItem {
    pub title: String,
    pub destination: Option<Destination>,
    /// External target, when the bookmark is a URI action.
    pub uri: Option<String>,
    /// Whether the item is expanded by default.
    pub open: bool,
    pub children: Vec<OutlineItem>,
}

/// A clickable area on a page.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub bounds: PageRect,
    pub target: LinkTarget,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum LinkTarget {
    Internal(Destination),
    /// External URI. The reader never follows these without user action.
    Uri(String),
    /// An action FastPDF does not execute (JavaScript, launch, ...).
    Unsupported,
}
