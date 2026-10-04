use crate::{PageIndex, PageRect};

/// Text of one page with geometry, used by selection, copy and search.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TextLayer {
    pub page: PageIndex,
    /// Spans in content-stream order (roughly reading order for most files).
    pub spans: Vec<TextSpan>,
}

/// A run of text sharing one baseline and font.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TextSpan {
    pub text: String,
    pub bounds: PageRect,
    /// One rectangle per `char` of `text`, in order. Empty when the engine
    /// only knows span-level geometry.
    pub char_bounds: Vec<PageRect>,
}

impl TextSpan {
    pub fn char_count(&self) -> usize {
        self.text.chars().count()
    }

    /// Box of the `index`-th `char`: its entry in `char_bounds`, or — when
    /// the engine only provides span geometry (or omitted evenly spaced
    /// boxes to save memory) — an even split of the span box. Selection,
    /// search highlights and copy all use this, so they always agree.
    pub fn char_rect(&self, index: usize) -> PageRect {
        if let Some(r) = self.char_bounds.get(index) {
            return *r;
        }
        let n = self.char_count().max(1) as f32;
        let w = self.bounds.width() / n;
        let x0 = self.bounds.x0 + w * index as f32;
        PageRect::new(x0, self.bounds.y0, x0 + w, self.bounds.y1)
    }
}

impl TextLayer {
    pub fn new(page: PageIndex) -> Self {
        Self {
            page,
            spans: Vec::new(),
        }
    }

    /// Plain text with one line per span; good enough for search and copy
    /// until layout analysis lands.
    pub fn plain_text(&self) -> String {
        let mut out = String::with_capacity(self.spans.iter().map(|s| s.text.len() + 1).sum());
        for span in &self.spans {
            out.push_str(&span.text);
            out.push('\n');
        }
        out
    }

    /// Approximate heap footprint, used to weigh entries in the text cache.
    pub fn heap_bytes(&self) -> usize {
        self.spans
            .iter()
            .map(|s| {
                std::mem::size_of::<TextSpan>()
                    + s.text.capacity()
                    + s.char_bounds.capacity() * std::mem::size_of::<PageRect>()
            })
            .sum()
    }
}
