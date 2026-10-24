//! What links show: a chip at the bottom left saying where the link under
//! the pointer goes (or why FastPDF does not follow it), the card that asks
//! before an external link opens (`crate::link_open`), an error chip when
//! one did not open, and, with the development overlay, an outline around
//! every link of the visible pages (blue: a page of the document, green:
//! an address that may be opened, red: not followed).

use fastpdf_core::FramePage;
use fastpdf_core::keymap::ReaderCommand;
use fastpdf_core::links::LinkAction;
use fastpdf_shell::{ExternalUri, UriKind, check_external_uri};
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
/// Widest a chip gets.
const MAX_CHIP_WIDTH: f32 = 560.0;
/// The address box shows at least two lines of the address.
const ADDRESS_MIN_HEIGHT: f32 = 52.0;

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
        let chip = self.link_chip().px_2().py_1();
        let (text, color) = match &hover.target {
            HoverTarget::Page(destination) => (
                strings.link_go_to_page(destination.page.display_number()),
                theme.text,
            ),
            // Where it goes, with the end kept: a host is read from its end
            // (`login.example.com.evil.example` belongs to evil.example).
            HoverTarget::External(uri) => {
                let label = match uri.kind() {
                    UriKind::Web => strings.link_open_web(),
                    UriKind::Mail => strings.link_send_mail(),
                };
                return Some(
                    chip.flex()
                        .flex_row()
                        .gap_1()
                        .text_color(theme.text)
                        .child(label_and_value(label, external_target(uri).to_owned())),
                );
            }
            HoverTarget::Blocked(why) => (
                strings.link_not_followed(&strings.link_block_reason(why)),
                theme.error_text,
            ),
        };
        Some(
            chip.text_color(color)
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
        // Never wider than the document area: its right side, where a
        // host's end is shown, must not be clipped in a narrow window.
        let room = self.viewport_bounds.map_or(MAX_CHIP_WIDTH, |b| {
            f32::from(b.size.width) - 16.0 - crate::scrollbar::THICKNESS
        });
        div()
            .absolute()
            .bottom(px(8.0 + crate::scrollbar::THICKNESS + lift))
            .left(px(8.0))
            .max_w(px(room.clamp(48.0, MAX_CHIP_WIDTH)))
            .rounded(px(6.0))
            .bg(self.theme.toolbar_bg)
            .border_1()
            .border_color(self.theme.toolbar_border)
            .text_size(px(13.0))
    }

    /// A link that did not open, until closed (its button or Esc).
    pub(crate) fn render_link_notice(
        &self,
        cx: &mut Context<'_, Self>,
    ) -> Option<gpui::Stateful<Div>> {
        let LinkNotice::OpenFailed(why) = self.links.notice.as_ref()?;
        let strings = self.strings();
        let theme = self.theme;
        Some(
            self.link_chip()
                .id("link-notice")
                // Presses on it (its close button) must not reach the page.
                .occlude()
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
        // The note may scroll out of a small window; the site and the
        // address stay with the buttons (and the card scrolls when even
        // they do not fit), so Open is never in view without the address.
        let body = div()
            .text_color(theme.text_muted)
            .child(strings.link_prompt_note());
        let field = div()
            .flex()
            .flex_col()
            .gap_2()
            .when_some(uri.host(), |field, host| {
                field.child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_1()
                        .font_weight(FontWeight::BOLD)
                        .child(label_and_value(strings.link_prompt_site(), host.to_owned())),
                )
            })
            .child(
                div()
                    .id("link-prompt-address")
                    .min_h(px(ADDRESS_MIN_HEIGHT))
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
            Some(field.into_any_element()),
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

/// What a chip names for an external link: the host of a web address, the
/// recipients of a mail link (the whole address when it has them only in
/// its query).
fn external_target(uri: &ExternalUri) -> &str {
    match uri.kind() {
        UriKind::Web => uri.host().unwrap_or(uri.as_str()),
        UriKind::Mail => {
            let recipients = uri
                .as_str()
                .strip_prefix("mailto:")
                .and_then(|rest| rest.split('?').next())
                .unwrap_or_default();
            if recipients.is_empty() {
                uri.as_str()
            } else {
                recipients
            }
        }
    }
}

/// A label and a value on one line; a value too long for it loses its
/// start, never its end (where a host says whose it is).
fn label_and_value(label: &'static str, value: String) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .gap_1()
        .min_w_0()
        .child(div().flex_none().whitespace_nowrap().child(label))
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis_start()
                .child(value),
        )
}

#[cfg(test)]
mod tests {
    use fastpdf_shell::check_external_uri;

    use super::*;

    fn target(raw: &str) -> String {
        match check_external_uri(raw) {
            Ok(uri) => external_target(&uri).to_owned(),
            Err(why) => panic!("{raw}: {why:?}"),
        }
    }

    #[test]
    fn chips_name_the_host_or_the_recipients() {
        assert_eq!(
            target("https://login.microsoftonline.com.oauth2-authorize-session.evil.example/a?b"),
            "login.microsoftonline.com.oauth2-authorize-session.evil.example"
        );
        assert_eq!(
            target("https://b\u{fc}cher.example/"),
            "xn--bcher-kva.example"
        );
        assert_eq!(
            target("mailto:a@example.invalid,b@example.invalid?subject=x"),
            "a@example.invalid,b@example.invalid"
        );
        assert_eq!(
            target("mailto:?to=a@example.invalid"),
            "mailto:?to=a@example.invalid"
        );
    }
}
