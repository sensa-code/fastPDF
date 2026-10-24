//! Messages in the document area: the empty window with the recent files,
//! a document that is opening or failed to open, and the notice shown over
//! the pages once their render host stopped restarting.

use fastpdf_engine_api::EngineDocument;
use gpui::{
    ClickEvent, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px,
};

use crate::opening::{APP_TITLE, display_name};
use crate::reader::{DocState, ReaderView};

impl ReaderView {
    /// Shown over the document once its render host stopped restarting
    /// after a crash storm (ADR 0008 §2): every page fails from then on,
    /// and the pages alone would not say why.
    pub(crate) fn render_host_notice(&self) -> Option<gpui::Div> {
        let DocState::Open(doc) = &self.doc else {
            return None;
        };
        let status = doc.session.document().host_status()?;
        if !status.restarts_disabled {
            return None;
        }
        let theme = self.theme;
        Some(
            div()
                .absolute()
                .bottom(px(8.0))
                .left(px(8.0))
                .max_w(px(520.0))
                .p_2()
                .rounded(px(6.0))
                .bg(theme.toolbar_bg)
                .border_1()
                .border_color(theme.toolbar_border)
                .text_size(px(13.0))
                .text_color(theme.error_text)
                .child(self.strings().rendering_stopped(status.crashes)),
        )
    }

    /// Text shown instead of a document: the empty state with recent files,
    /// progress, errors.
    pub(crate) fn placeholder(
        &self,
        cx: &mut Context<'_, Self>,
    ) -> Option<impl IntoElement + use<>> {
        let theme = self.theme;
        let strings = self.strings();
        let (title, detail, is_error): (String, String, bool) = match &self.doc {
            DocState::Open(_) => return None,
            DocState::Empty => (APP_TITLE.into(), strings.empty_hint.into(), false),
            DocState::Loading { path } => {
                (strings.opening(&display_name(path)), String::new(), false)
            }
            DocState::Failed { path, failure } => (
                strings.cannot_open(&display_name(path)),
                strings.open_failure(failure),
                true,
            ),
        };
        let recent = if matches!(self.doc, DocState::Empty) {
            self.render_recent(cx)
        } else {
            None
        };
        Some(
            div()
                .absolute()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .p_4()
                .child(div().text_size(px(22.0)).child(title))
                .when(!detail.is_empty(), |d| {
                    d.child(
                        div()
                            .max_w(px(640.0))
                            .text_size(px(14.0))
                            .text_color(if is_error {
                                theme.error_text
                            } else {
                                theme.text_muted
                            })
                            .child(detail),
                    )
                })
                .when_some(recent, |d, recent| d.child(recent)),
        )
    }

    /// Clickable recent files (spec §8 "Open Recent").
    fn render_recent(&self, cx: &mut Context<'_, Self>) -> Option<gpui::Div> {
        if self.recent.is_empty() {
            return None;
        }
        let theme = self.theme;
        let rows: Vec<_> = self
            .recent
            .iter()
            .enumerate()
            .map(|(i, path)| {
                let name = display_name(path);
                let dir = path
                    .parent()
                    .map(|d| d.display().to_string())
                    .unwrap_or_default();
                let open = path.clone();
                div()
                    .id(("recent", i))
                    .flex()
                    .flex_col()
                    .px_3()
                    .py_1()
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .hover(move |s| s.bg(theme.button_hover))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.open_path(open.clone(), window, cx);
                    }))
                    .child(div().text_size(px(14.0)).child(name))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme.text_muted)
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(dir),
                    )
            })
            .collect();
        Some(
            div()
                .mt_4()
                .w(px(480.0))
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .px_3()
                        .text_size(px(13.0))
                        .text_color(theme.text_muted)
                        .child(self.strings().recent),
                )
                .children(rows),
        )
    }
}
