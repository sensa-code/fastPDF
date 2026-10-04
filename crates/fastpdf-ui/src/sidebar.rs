//! The sidebar (F4; spec §20: closed by default). Two tabs:
//!
//! * **Outline** — bookmarks, loaded on a background thread the first time
//!   the sidebar opens for a document; rows are virtualized.
//! * **Pages** — thumbnails. Rendering starts only while this tab is shown
//!   (`DocumentSession::enable_thumbnails`) and stops when it is hidden;
//!   only the visible rows plus a margin are ever requested (spec §23).
//!   Thumbnails are painted on a canvas through the same texture tracking
//!   as tiles, so evicted ones leave the GPU atlas too.

use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use fastpdf_core::session::ThumbnailItem;
use fastpdf_engine_api::{
    Destination, DocumentId, EngineDocument, EngineError, OutlineItem, PageIndex,
};
use gpui::{
    App, Bounds, ClickEvent, ContentMask, Context, Entity, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, Pixels, ScrollWheelEvent, SharedString,
    StatefulInteractiveElement, Styled, Task, TextAlign, TextRun, Window, canvas, div, fill, font,
    point, prelude::FluentBuilder, px, size, uniform_list,
};

use crate::reader::{DocState, ReaderView};
use crate::textures::TileImage;
use crate::toolbar::styled_button;

/// Sidebar width in logical pixels.
pub(crate) const WIDTH: f32 = 220.0;
/// Thumbnail width in logical pixels.
pub(crate) const THUMB_WIDTH: f32 = 132.0;
/// Space above each thumbnail and below its page number.
pub(crate) const ROW_PAD: f32 = 10.0;
/// Height of the page-number label under each thumbnail.
pub(crate) const LABEL_HEIGHT: f32 = 18.0;
/// Byte budget of the thumbnail cache (spec §15: 32 MB).
pub(crate) const THUMB_BUDGET: usize = 32 * 1024 * 1024;
/// Rows rendered above and below the visible ones.
pub(crate) const THUMB_MARGIN: u32 = 2;
/// Outline rows kept; deeper or longer outlines are cut off.
const MAX_OUTLINE_ROWS: usize = 20_000;
/// Indentation per outline level, in logical pixels.
pub(crate) const INDENT: f32 = 12.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SidebarTab {
    #[default]
    Outline,
    Pages,
}

/// One visible outline line.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OutlineRow {
    pub title: SharedString,
    pub depth: usize,
    pub destination: Option<Destination>,
}

pub(crate) enum OutlineState {
    NotLoaded,
    Loading(#[allow(dead_code)] Task<()>),
    Ready(Rc<Vec<OutlineRow>>),
    /// No outline, or the engine cannot read one.
    Empty,
    Failed(String),
}

/// Scroll state of the thumbnail list.
#[derive(Debug, Clone, Default)]
pub(crate) struct ThumbList {
    pub scroll: f32,
    /// Page the list last scrolled into view; follows the main view.
    pub followed: Option<PageIndex>,
    pub row_height: f32,
    pub rows: u32,
    pub view_height: f32,
}

impl ThumbList {
    /// Rows intersecting the list's viewport.
    pub(crate) fn visible_rows(&self) -> Range<u32> {
        visible_rows(self.scroll, self.view_height, self.row_height, self.rows)
    }

    pub(crate) fn max_scroll(&self) -> f32 {
        (self.rows as f32 * self.row_height - self.view_height).max(0.0)
    }

    pub(crate) fn scroll_by(&mut self, dy: f32) {
        self.scroll = (self.scroll + dy).clamp(0.0, self.max_scroll());
    }

    /// The row under `y` (logical pixels from the list's top edge).
    pub(crate) fn row_at(&self, y: f32) -> Option<u32> {
        if self.row_height <= 0.0 || y < 0.0 {
            return None;
        }
        let row = ((self.scroll + y) / self.row_height) as u32;
        (row < self.rows).then_some(row)
    }

    /// Scrolls just enough to show `row` completely.
    pub(crate) fn reveal(&mut self, row: u32) {
        let top = row as f32 * self.row_height;
        let bottom = top + self.row_height;
        if top < self.scroll {
            self.scroll = top;
        } else if bottom > self.scroll + self.view_height {
            self.scroll = bottom - self.view_height;
        }
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
    }
}

fn visible_rows(scroll: f32, height: f32, row_height: f32, rows: u32) -> Range<u32> {
    if row_height <= 0.0 || height <= 0.0 || rows == 0 {
        return 0..0;
    }
    let first = (scroll / row_height).floor().max(0.0) as u32;
    let last = ((scroll + height) / row_height).ceil() as u32;
    first.min(rows)..last.min(rows)
}

/// Sidebar state; per document except `open` and `tab`.
pub(crate) struct Sidebar {
    pub open: bool,
    pub tab: SidebarTab,
    pub outline: OutlineState,
    /// Document the outline belongs to.
    pub outline_doc: Option<DocumentId>,
    pub thumbs: ThumbList,
    /// Device-pixel width thumbnails are rendering at (`None`: disabled).
    pub thumbs_width_px: Option<u32>,
    /// Where the thumbnail list was laid out.
    pub thumbs_bounds: Option<Bounds<Pixels>>,
}

impl std::fmt::Debug for Sidebar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sidebar")
            .field("open", &self.open)
            .field("tab", &self.tab)
            .finish_non_exhaustive()
    }
}

impl Default for Sidebar {
    fn default() -> Self {
        Self {
            open: false,
            tab: SidebarTab::default(),
            outline: OutlineState::NotLoaded,
            outline_doc: None,
            thumbs: ThumbList::default(),
            thumbs_width_px: None,
            thumbs_bounds: None,
        }
    }
}

impl Sidebar {
    /// Forgets per-document state (another document was opened).
    pub(crate) fn reset_document(&mut self) {
        self.outline = OutlineState::NotLoaded;
        self.outline_doc = None;
        self.thumbs = ThumbList::default();
        self.thumbs_width_px = None;
    }

    /// Whether thumbnails should be rendering.
    pub(crate) fn wants_thumbnails(&self) -> bool {
        self.open && self.tab == SidebarTab::Pages
    }
}

/// Depth-first, fully expanded outline rows (titles cleaned of control
/// characters), at most [`MAX_OUTLINE_ROWS`].
pub(crate) fn flatten_outline(items: &[OutlineItem]) -> Vec<OutlineRow> {
    fn walk(items: &[OutlineItem], depth: usize, out: &mut Vec<OutlineRow>) {
        for item in items {
            if out.len() >= MAX_OUTLINE_ROWS {
                return;
            }
            let title: String = item
                .title
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            let title = title.trim();
            // Empty titles stay empty; they are shown as "(untitled)" in
            // the UI language.
            out.push(OutlineRow {
                title: SharedString::from(title.to_string()),
                depth,
                destination: item.destination.clone(),
            });
            walk(&item.children, (depth + 1).min(16), out);
        }
    }
    let mut out = Vec::new();
    walk(items, 0, &mut out);
    out
}

/// What the thumbnail canvas paints in one frame.
pub(crate) struct ThumbFrame {
    items: Vec<ThumbnailItem<TileImage>>,
    scale: f32,
    current: PageIndex,
}

impl ReaderView {
    pub(crate) fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.set_sidebar_open(!self.sidebar.open, window, cx);
    }

    pub(crate) fn set_sidebar_open(
        &mut self,
        open: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.sidebar.open = open;
        if open {
            self.ensure_outline(cx);
        }
        self.sync_thumbnails(window);
        self.settings.sidebar_open = open;
        self.save_settings(cx);
        cx.notify();
    }

    pub(crate) fn set_sidebar_tab(
        &mut self,
        tab: SidebarTab,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.sidebar.tab = tab;
        // Show the current page when the list appears.
        self.sidebar.thumbs.followed = None;
        self.sync_thumbnails(window);
        self.settings.sidebar_tab = tab;
        self.save_settings(cx);
        cx.notify();
    }

    /// Loads the outline on a background thread, once per document.
    pub(crate) fn ensure_outline(&mut self, cx: &mut Context<'_, Self>) {
        let Some(session) = self.session() else {
            return;
        };
        let document = session.id();
        if self.sidebar.outline_doc == Some(document)
            && !matches!(self.sidebar.outline, OutlineState::NotLoaded)
        {
            return;
        }
        let doc = Arc::clone(session.document());
        let work = cx.background_executor().spawn(async move { doc.outline() });
        let task = cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if this.sidebar.outline_doc != Some(document) {
                    return;
                }
                this.sidebar.outline = match result {
                    Ok(items) => {
                        let rows = flatten_outline(&items);
                        if rows.is_empty() {
                            OutlineState::Empty
                        } else {
                            OutlineState::Ready(Rc::new(rows))
                        }
                    }
                    Err(EngineError::Unsupported(_)) => OutlineState::Empty,
                    Err(e) => OutlineState::Failed(e.to_string()),
                };
                cx.notify();
            });
        });
        self.sidebar.outline_doc = Some(document);
        self.sidebar.outline = OutlineState::Loading(task);
    }

    /// Starts or stops thumbnail rendering to match the sidebar (spec §23:
    /// thumbnails cost work, so they exist only while the tab is shown).
    pub(crate) fn sync_thumbnails(&mut self, window: &mut Window) {
        let want = self.sidebar.wants_thumbnails();
        let width_px = thumb_width_px(window.scale_factor());
        let retire = self.textures.retire_queue();
        let waker = self.waker.clone();
        let enabled = self.sidebar.thumbs_width_px;
        let Some(session) = self.session_mut() else {
            self.sidebar.thumbs_width_px = None;
            return;
        };
        if want && enabled != Some(width_px) {
            let cache = session.enable_thumbnails(width_px, THUMB_BUDGET, move |evicted| {
                // Same path as tiles: only the UI thread may drop_image.
                if retire.retire(evicted.into_iter().map(|(_, image)| image)) {
                    waker.wake();
                }
            });
            self.memory.manager().register(cache);
            self.sidebar.thumbs_width_px = Some(width_px);
        } else if !want && enabled.is_some() {
            session.disable_thumbnails();
            self.sidebar.thumbs_width_px = None;
        }
    }

    fn activate_outline(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        let OutlineState::Ready(rows) = &self.sidebar.outline else {
            return;
        };
        let Some(destination) = rows.get(index).and_then(|r| r.destination.clone()) else {
            return;
        };
        if let Some(session) = self.session_mut() {
            session.go_to_destination(&destination);
        }
        cx.notify();
    }

    pub(crate) fn render_sidebar(
        &self,
        view: Entity<Self>,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement + use<> {
        let theme = self.theme;
        let strings = self.strings();
        let tab = self.sidebar.tab;
        let tabs = div()
            .flex()
            .flex_row()
            .flex_none()
            .gap_1()
            .p_1()
            .border_b_1()
            .border_color(theme.toolbar_border)
            .text_size(px(13.0))
            .child(
                styled_button(
                    "tab-outline",
                    strings.outline,
                    true,
                    tab == SidebarTab::Outline,
                    &theme,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.set_sidebar_tab(SidebarTab::Outline, window, cx);
                })),
            )
            .child(
                styled_button(
                    "tab-pages",
                    strings.pages,
                    true,
                    tab == SidebarTab::Pages,
                    &theme,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.set_sidebar_tab(SidebarTab::Pages, window, cx);
                })),
            );
        let body = match tab {
            SidebarTab::Outline => self.render_outline(view).into_any_element(),
            SidebarTab::Pages => self.render_thumbnails(view, cx).into_any_element(),
        };
        div()
            .id("sidebar")
            .w(px(WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(theme.sidebar_bg)
            .border_r_1()
            .border_color(theme.toolbar_border)
            .child(tabs)
            .child(div().flex_1().min_h_0().relative().child(body))
    }

    fn render_outline(&self, view: Entity<Self>) -> gpui::AnyElement {
        let theme = self.theme;
        let strings = self.strings();
        let message = |text: String| {
            div()
                .p_3()
                .text_size(px(13.0))
                .text_color(theme.text_muted)
                .child(text)
                .into_any_element()
        };
        if self.session().is_none() {
            return message(strings.no_document.into());
        }
        let rows = match &self.sidebar.outline {
            OutlineState::Ready(rows) => Rc::clone(rows),
            OutlineState::NotLoaded | OutlineState::Loading(_) => {
                return message(strings.loading.into());
            }
            OutlineState::Empty => return message(strings.no_outline.into()),
            OutlineState::Failed(e) => return message(strings.outline_unavailable(e)),
        };
        let untitled = SharedString::from(strings.untitled);
        let count = rows.len();
        uniform_list("outline", count, move |range, _window, _cx| {
            range
                .filter_map(|ix| {
                    let row = rows.get(ix)?;
                    let view = view.clone();
                    let title = if row.title.is_empty() {
                        untitled.clone()
                    } else {
                        row.title.clone()
                    };
                    let has_target = row.destination.is_some();
                    Some(
                        div()
                            .id(("outline-row", ix))
                            .h(px(26.0))
                            .pl(px(10.0 + INDENT * row.depth as f32))
                            .pr_2()
                            .flex()
                            .items_center()
                            .text_size(px(13.0))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .when(!has_target, |d| d.text_color(theme.text_muted))
                            .when(has_target, |d| {
                                d.cursor_pointer()
                                    .hover(move |s| s.bg(theme.button_hover))
                                    .on_click(move |_: &ClickEvent, _window, cx| {
                                        view.update(cx, |this, cx| this.activate_outline(ix, cx));
                                    })
                            })
                            .child(title),
                    )
                })
                .collect::<Vec<_>>()
        })
        .size_full()
        .into_any_element()
    }

    fn render_thumbnails(
        &self,
        view: Entity<Self>,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement + use<> {
        let prepaint_view = view.clone();
        div()
            .id("thumbnails")
            .size_full()
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                let dy = f32::from(event.delta.pixel_delta(px(33.0)).y);
                this.sidebar.thumbs.scroll_by(-dy);
                cx.stop_propagation();
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.on_thumbnail_click(event.position, cx);
                }),
            )
            .child(
                canvas(
                    move |bounds, window, cx| {
                        prepaint_view.update(cx, |this, _| this.prepare_thumbnails(bounds, window))
                    },
                    move |bounds, frame, window, cx| {
                        view.update(cx, |this, cx| {
                            this.paint_thumbnails(bounds, frame, window, cx);
                        });
                    },
                )
                .size_full(),
            )
    }

    fn on_thumbnail_click(&mut self, position: gpui::Point<Pixels>, cx: &mut Context<'_, Self>) {
        let Some(bounds) = self.sidebar.thumbs_bounds else {
            return;
        };
        let Some(row) = self
            .sidebar
            .thumbs
            .row_at(f32::from(position.y - bounds.top()))
        else {
            return;
        };
        if let Some(session) = self.session_mut() {
            session.go_to_page(PageIndex::new(row));
        }
        cx.notify();
    }

    /// Thumbnail canvas prepaint: lays out rows and asks the session for the
    /// visible ones (it schedules the missing ones at P4).
    fn prepare_thumbnails(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
    ) -> Option<ThumbFrame> {
        self.textures.begin_frame(self.frame_seq, window);
        self.sidebar.thumbs_bounds = Some(bounds);
        let scale = window.scale_factor();
        // The window moved to a display with another scale.
        if self
            .sidebar
            .thumbs_width_px
            .is_some_and(|w| w != thumb_width_px(scale))
        {
            self.sync_thumbnails(window);
        }
        let retire = self.textures.retire_queue();
        let DocState::Open(open) = &mut self.doc else {
            return None;
        };
        let session = &mut open.session;
        let aspect = session
            .layout()
            .page_size(PageIndex::FIRST)
            .map_or(1.3, |s| s.height / s.width.max(1.0))
            .clamp(0.4, 2.5);
        let current = session.current_page();
        let list = &mut self.sidebar.thumbs;
        list.rows = session.page_count();
        list.row_height = ROW_PAD + THUMB_WIDTH * aspect + LABEL_HEIGHT + ROW_PAD / 2.0;
        list.view_height = f32::from(bounds.size.height);
        if list.followed != Some(current) {
            list.followed = Some(current);
            list.reveal(current.get());
        }
        list.scroll = list.scroll.clamp(0.0, list.max_scroll());
        let visible = list.visible_rows();
        let items = retire.in_frame(|| session.thumbnails(visible, THUMB_MARGIN));
        Some(ThumbFrame {
            items,
            scale,
            current,
        })
    }

    fn paint_thumbnails(
        &mut self,
        bounds: Bounds<Pixels>,
        frame: Option<ThumbFrame>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(frame) = frame else {
            return;
        };
        let theme = self.theme;
        // Thumbnails are rendered in the session's color mode (their keys
        // include it); unrendered ones show matching paper.
        let mode = self.session().map(|s| s.color_mode()).unwrap_or_default();
        let paper = crate::theme::paper(self.options.session.paper, mode);
        let list = self.sidebar.thumbs.clone();
        let box_height = list.row_height - ROW_PAD - LABEL_HEIGHT - ROW_PAD / 2.0;
        let label_font = font(self.language().ui_font());
        let strings = self.strings();
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for item in &frame.items {
                let row = item.page.get() as f32;
                let top = bounds.top() + px(row * list.row_height - list.scroll);
                // Device pixels to logical, fitted into the row's box.
                let mut w = item.size[0] as f32 / frame.scale;
                let mut h = item.size[1] as f32 / frame.scale;
                if h > box_height {
                    w *= box_height / h;
                    h = box_height;
                }
                let x = bounds.left() + (bounds.size.width - px(w)) / 2.0;
                let rect = Bounds::new(point(x, top + px(ROW_PAD)), size(px(w), px(h)));
                let (border, width) = if item.page == frame.current {
                    (theme.accent, 3.0)
                } else {
                    (theme.page_border, 1.0)
                };
                window.paint_quad(fill(rect.dilate(px(width)), border));
                window.paint_quad(fill(rect, paper));
                if let Some(image) = &item.image {
                    self.textures.paint(image, rect, None, window);
                }
                let label = if item.failed {
                    strings.thumbnail_failed(item.page.display_number())
                } else {
                    item.page.display_number().to_string()
                };
                let color = if item.failed {
                    theme.error_text
                } else {
                    theme.text_muted
                };
                let run = TextRun {
                    len: label.len(),
                    font: label_font.clone(),
                    color: color.into(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let line = window
                    .text_system()
                    .shape_line(label.into(), px(12.0), &[run], None);
                let origin = point(bounds.left(), rect.bottom() + px(3.0));
                let painted = line.paint(
                    origin,
                    px(LABEL_HEIGHT - 4.0),
                    TextAlign::Center,
                    Some(bounds.size.width),
                    window,
                    cx,
                );
                if let Err(e) = painted {
                    log::debug!("thumbnail label: {e}");
                }
            }
        });
    }
}

/// Thumbnail width in device pixels at `scale`.
fn thumb_width_px(scale: f32) -> u32 {
    (THUMB_WIDTH * scale).round().max(16.0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastpdf_engine_api::DestinationView;

    fn item(title: &str, page: u32, children: Vec<OutlineItem>) -> OutlineItem {
        OutlineItem {
            title: title.into(),
            destination: Some(Destination {
                page: PageIndex::new(page),
                view: DestinationView::Fit,
            }),
            uri: None,
            open: true,
            children,
        }
    }

    #[test]
    fn outline_is_flattened_depth_first_with_clean_titles() {
        let outline = vec![
            item(
                "第一章\n",
                0,
                vec![item("1.1", 1, vec![]), item("", 2, vec![])],
            ),
            item("Chapter 2", 5, vec![]),
        ];
        let rows = flatten_outline(&outline);
        let summary: Vec<(&str, usize)> =
            rows.iter().map(|r| (r.title.as_ref(), r.depth)).collect();
        assert_eq!(
            summary,
            vec![("第一章", 0), ("1.1", 1), ("", 1), ("Chapter 2", 0)]
        );
        assert_eq!(rows[3].destination.as_ref().map(|d| d.page.get()), Some(5));
    }

    #[test]
    fn thumbnail_rows_are_virtualized() {
        let mut list = ThumbList {
            scroll: 0.0,
            followed: None,
            row_height: 200.0,
            rows: 2000,
            view_height: 650.0,
        };
        assert_eq!(list.visible_rows(), 0..4);
        list.scroll_by(1_000.0);
        assert_eq!(list.visible_rows(), 5..9);
        assert_eq!(list.row_at(10.0), Some(5));
        list.scroll_by(-1e9);
        assert_eq!(list.scroll, 0.0);
        list.scroll_by(1e12);
        assert_eq!(list.scroll, list.max_scroll());
        assert_eq!(list.visible_rows().end, 2000);
        assert_eq!(list.row_at(-1.0), None);
    }

    #[test]
    fn reveal_scrolls_the_least_needed() {
        let mut list = ThumbList {
            row_height: 100.0,
            rows: 50,
            view_height: 350.0,
            ..ThumbList::default()
        };
        list.reveal(2);
        assert_eq!(list.scroll, 0.0, "already visible");
        list.reveal(10);
        assert_eq!(list.scroll, 1100.0 - 350.0);
        list.reveal(1);
        assert_eq!(list.scroll, 100.0);
        assert_eq!(visible_rows(0.0, 0.0, 100.0, 50), 0..0);
    }
}
