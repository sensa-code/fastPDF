//! The properties panel's look: a column at the top right of the document
//! area (where the settings panel goes), below the find bar when that is
//! open, scrolling when it is taller than the area. It takes every mouse
//! press over it (`occlude`), so nothing underneath reacts.

use gpui::{
    ClickEvent, Context, Div, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px,
};

use super::text::{self, PENDING};
use super::{Copied, Section};
use crate::i18n::Strings;
use crate::reader::ReaderView;
use crate::theme::Theme;
use crate::toolbar::styled_button;

/// Panel width, label column and margins, in logical pixels.
const WIDTH: f32 = 480.0;
const LABEL_WIDTH: f32 = 150.0;
const MARGIN: f32 = 8.0;
/// Room the find bar takes at the top right while it is open.
const FIND_BAR_ROOM: f32 = 44.0;

impl ReaderView {
    pub(crate) fn render_properties_panel(
        &self,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement + use<> {
        let theme = self.theme;
        let strings = self.strings();
        let top = MARGIN + if self.find.open { FIND_BAR_ROOM } else { 0.0 };
        let (room_w, room_h) = self.viewport_bounds.map_or((None, None), |b| {
            (
                Some((f32::from(b.size.width) - 28.0).max(120.0)),
                Some((f32::from(b.size.height) - top - MARGIN).max(80.0)),
            )
        });

        let mut body = div()
            .id("properties-body")
            .flex()
            .flex_col()
            .gap_3()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll();
        match self.property_sections() {
            None => {
                body = body.child(
                    div()
                        .text_color(theme.text_muted)
                        .child(strings.no_document_open()),
                );
            }
            Some(sections) => {
                for (i, section) in sections.into_iter().enumerate() {
                    body = body.child(self.render_section(i, section, &theme, strings, cx));
                }
            }
        }

        let status = match self.properties_problem() {
            Some(why) => Some(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(theme.error_text)
                            .child(strings.properties_unavailable(&why)),
                    )
                    .child(
                        styled_button("properties-retry", strings.retry(), true, false, &theme)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.retry_properties(cx);
                            })),
                    ),
            ),
            None if self.properties_loading() => Some(
                div()
                    .text_color(theme.text_muted)
                    .child(strings.reading_properties()),
            ),
            None => None,
        };

        let copied_all = self.properties.copied == Some(Copied::All);
        let footer = div()
            .flex()
            .flex_row()
            .justify_end()
            .gap_2()
            .when(self.session().is_some(), |footer| {
                footer.child(
                    styled_button(
                        "properties-copy-all",
                        if copied_all {
                            strings.copied()
                        } else {
                            strings.copy_all()
                        },
                        true,
                        copied_all,
                        &theme,
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.copy_all_properties(cx);
                    })),
                )
            })
            .child(
                styled_button("properties-close", strings.close, true, false, &theme).on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.close_properties_from_panel(window, cx);
                    }),
                ),
            );

        div()
            .id("properties-panel")
            .absolute()
            .top(px(top))
            .right(px(20.0))
            .occlude()
            .w(px(WIDTH))
            .when_some(room_w, |panel, w| panel.max_w(px(w)))
            .when_some(room_h, |panel, h| panel.max_h(px(h)))
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded(px(6.0))
            .bg(theme.toolbar_bg)
            .border_1()
            .border_color(theme.toolbar_border)
            .text_size(px(13.0))
            .child(
                div()
                    .flex_none()
                    .text_size(px(15.0))
                    .child(strings.properties_title()),
            )
            .child(body)
            .when_some(status, |panel, status| {
                panel.child(div().flex_none().child(status))
            })
            .child(div().flex_none().child(footer))
    }

    fn render_section(
        &self,
        index: usize,
        section: Section,
        theme: &Theme,
        strings: &'static Strings,
        cx: &mut Context<'_, Self>,
    ) -> Div {
        let mut rows = div().flex().flex_col().gap_1().child(
            div()
                .text_size(px(12.0))
                .text_color(theme.text_muted)
                .border_b_1()
                .border_color(theme.toolbar_border)
                .child(strings.property_label(section.title)),
        );
        if let Some(note) = section.note {
            rows = rows.child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.text_muted)
                    .child(note),
            );
        }
        for (i, row) in section.rows.into_iter().enumerate() {
            let label = row.label;
            let copied = self.properties.copied == Some(Copied::Row(label));
            let pending = row.value == PENDING;
            let shown: SharedString = text::display_value(&row.value).into();
            rows = rows.child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap_2()
                    .child(
                        div()
                            .w(px(LABEL_WIDTH))
                            .flex_none()
                            .pt(px(4.0))
                            .text_color(theme.text_muted)
                            .child(strings.property_label(label)),
                    )
                    .child(div().flex_1().min_w_0().pt(px(4.0)).child(shown))
                    .child(
                        styled_button(
                            SharedString::from(format!("properties-copy-{index}-{i}")),
                            if copied {
                                strings.copied()
                            } else {
                                strings.copy_value()
                            },
                            !pending,
                            copied,
                            theme,
                        )
                        .text_size(px(12.0))
                        .when(!pending, |button| {
                            button.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.copy_property(label, cx);
                            }))
                        }),
                    ),
            );
        }
        rows
    }

    /// The engine is still reading.
    fn properties_loading(&self) -> bool {
        self.doc_queries()
            .is_some_and(|q| matches!(q.properties, crate::doc_queries::Lazy::Loading(_)))
    }
}
