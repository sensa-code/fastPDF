//! What links show: a chip at the bottom left saying where the link under
//! the pointer goes (or why FastPDF does not follow it), the card that asks
//! before an external link opens (`crate::link_open`), an error chip when
//! one did not open, and, with the development overlay, an outline around
//! every link of the visible pages (blue: a page of the document, green:
//! an address that may be opened, red: not followed).

use fastpdf_core::FramePage;
use fastpdf_core::keymap::ReaderCommand;
use fastpdf_core::links::LinkAction;
use fastpdf_shell::{UriKind, check_external_uri};
use gpui::{
    AnyElement, BorderStyle, Bounds, ClickEvent, Context, Div, FontWeight, InteractiveElement,
    IntoElement, ParentElement, Pixels, Point, StatefulInteractiveElement, Styled, Window, div,
    outline, point, prelude::FluentBuilder, px, size,
};

use crate::link_open::LinkNotice;
use crate::links::HoverTarget;
use crate::prompt::prompt_card;
use crate::reader::ReaderView;
use crate::theme::MONO_FONT;
use crate::toolbar::styled_button;

/// Room for the write notice chip (and a gap) when it shows.
const NOTICE_HEIGHT: f32 = 48.0;

impl ReaderView {
    /// The chip for the link under the pointer.
    pub(crate) fn render_link_status(&self) -> Option<Div> {
        let hover = self.links.hover.as_ref()?;
        // The error chip takes the place while it shows.
        if self.selection.dragging || self.links.notice.is_some() {
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
        Some(
            self.link_chip()
                .px_2()
                .py_1()
                .text_color(color)
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(text),
        )
    }

    /// The chip's place and look: bottom left, above the write notice.
    fn link_chip(&self) -> Div {
        let lift = if self.tools.notice.is_some() {
            NOTICE_HEIGHT
        } else {
            0.0
        };
        div()
            .absolute()
            .bottom(px(8.0 + crate::scrollbar::THICKNESS + lift))
            .left(px(8.0))
            .max_w(px(560.0))
            .rounded(px(6.0))
            .bg(self.theme.toolbar_bg)
            .border_1()
            .border_color(self.theme.toolbar_border)
            .text_size(px(13.0))
    }

    /// A link that did not open, until closed (its button or Esc).
    pub(crate) fn render_link_notice(&self, cx: &mut Context<'_, Self>) -> Option<Div> {
        let LinkNotice::OpenFailed(why) = self.links.notice.as_ref()?;
        let strings = self.strings();
        let theme = self.theme;
        Some(
            self.link_chip()
                .p_2()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(theme.error_text)
                        .child(strings.link_open_failed(why)),
                )
                .child(
                    styled_button("link-notice-close", "\u{2715}", true, false, &theme).on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.links.notice = None;
                            cx.notify();
                        }),
                    ),
                ),
        )
    }

    /// The card that asks before an external link opens: what it opens
    /// with, the site, the whole address as it will be handed over, and a
    /// note. The card has the keyboard: Esc cancels, Enter does nothing.
    pub(crate) fn render_link_prompt(&self, cx: &mut Context<'_, Self>) -> Option<AnyElement> {
        let prompt = self.links.prompt.as_ref()?;
        let theme = self.theme;
        let strings = self.strings();
        let uri = &prompt.uri;
        let body = div()
            .flex()
            .flex_col()
            .gap_2()
            .when_some(uri.host(), |body, host| {
                body.child(
                    div()
                        .font_weight(FontWeight::BOLD)
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(strings.link_prompt_site(host)),
                )
            })
            .child(
                div()
                    .id("link-prompt-address")
                    .max_h(px(160.0))
                    .overflow_y_scroll()
                    .p_2()
                    .rounded(px(4.0))
                    .bg(theme.input_bg)
                    .border_1()
                    .border_color(theme.toolbar_border)
                    .font_family(MONO_FONT)
                    .text_size(px(12.0))
                    .child(uri.as_str().to_owned()),
            )
            .child(
                div()
                    .text_color(theme.text_muted)
                    .child(strings.link_prompt_note()),
            );
        let buttons = div()
            .flex()
            .flex_row()
            .gap_2()
            .child(
                styled_button(
                    "link-copy",
                    strings.link_copy_address(),
                    true,
                    false,
                    &theme,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.copy_link_address(window, cx);
                })),
            )
            .child(
                styled_button("link-cancel", strings.cancel, true, false, &theme).on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.run_command(ReaderCommand::Cancel, window, cx);
                    }),
                ),
            )
            .child(
                styled_button("link-open", strings.link_open(), true, false, &theme).on_click(
                    cx.listener(|this, event: &ClickEvent, window, cx| {
                        // Only a deliberate single click opens: not the
                        // second click of a double-click, not a key.
                        let single = match event {
                            ClickEvent::Mouse(click) => click.up.click_count <= 1,
                            ClickEvent::Touch(_) => true,
                            ClickEvent::Keyboard(_) => false,
                        };
                        if single {
                            this.answer_link_prompt(true, window, cx);
                        }
                    }),
                ),
            );
        let card = prompt_card(
            "link-prompt",
            theme,
            prompt.placement,
            Some(&prompt.focus),
            strings.link_prompt_title(uri.kind()).into(),
            body,
            None,
            buttons,
        );
        Some(card.into_any_element())
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
