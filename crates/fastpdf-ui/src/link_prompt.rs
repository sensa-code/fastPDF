//! What links show: a chip at the bottom left saying where the link under
//! the pointer goes (or why FastPDF does not follow it), and, with the
//! development overlay, an outline around every link of the visible pages
//! (blue: a page of the document, green: an address that may be opened,
//! red: not followed).

use fastpdf_core::FramePage;
use fastpdf_core::links::LinkAction;
use fastpdf_shell::{UriKind, check_external_uri};
use gpui::{
    BorderStyle, Bounds, Div, ParentElement, Pixels, Point, Styled, Window, div, outline, point,
    px, size,
};

use crate::links::HoverTarget;
use crate::reader::ReaderView;

/// Room for the write notice chip (and a gap) when it shows.
const NOTICE_HEIGHT: f32 = 48.0;

impl ReaderView {
    /// The chip for the link under the pointer.
    pub(crate) fn render_link_status(&self) -> Option<Div> {
        let hover = self.links.hover.as_ref()?;
        if self.selection.dragging {
            return None;
        }
        let strings = self.strings();
        let theme = self.theme;
        let (text, color) = match &hover.target {
            HoverTarget::Page(destination) => (
                strings.link_go_to_page(destination.page.display_number()),
                theme.text,
            ),
            HoverTarget::External(uri) => (
                match uri.kind() {
                    UriKind::Web => strings.link_open_web(uri.as_str()),
                    UriKind::Mail => strings.link_send_mail(uri.as_str()),
                },
                theme.text,
            ),
            HoverTarget::Blocked(why) => (
                strings.link_not_followed(&strings.link_block_reason(why)),
                theme.error_text,
            ),
        };
        let lift = if self.tools.notice.is_some() {
            NOTICE_HEIGHT
        } else {
            0.0
        };
        Some(
            div()
                .absolute()
                .bottom(px(8.0 + crate::scrollbar::THICKNESS + lift))
                .left(px(8.0))
                .max_w(px(560.0))
                .px_2()
                .py_1()
                .rounded(px(6.0))
                .bg(theme.toolbar_bg)
                .border_1()
                .border_color(theme.toolbar_border)
                .text_size(px(13.0))
                .text_color(color)
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(text),
        )
    }

    /// Development overlay: outlines the links of `pages` that are read.
    pub(crate) fn paint_link_outlines(
        &self,
        origin: Point<Pixels>,
        pages: &[FramePage],
        window: &mut Window,
    ) {
        let Some(session) = self.session() else {
            return;
        };
        let theme = self.theme;
        for frame_page in pages {
            let Some(links) = self.cached_links(frame_page.page) else {
                continue;
            };
            for link in links.iter() {
                let Some([x, y, w, h]) = session.page_to_view(frame_page.page, link.bounds) else {
                    continue;
                };
                let color = match &link.action {
                    LinkAction::GoTo(_) => theme.accent,
                    LinkAction::Uri(uri) if check_external_uri(uri).is_ok() => theme.overlay_text,
                    _ => theme.error_text,
                };
                let bounds = Bounds::new(
                    point(origin.x + px(x), origin.y + px(y)),
                    size(px(w), px(h)),
                );
                window.paint_quad(outline(bounds, color, BorderStyle::Solid));
            }
        }
    }
}
