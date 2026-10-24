//! Go to a page by number or label (Ctrl+G): the toolbar's page indicator
//! is a button that turns into a small field, with the page count inside
//! it ("/ 120"); while it is open it also takes the place of the previous
//! and next page buttons, so even a narrow toolbar has room for both the
//! text and the count. Enter goes there (a jump Back returns from), Esc or
//! leaving the field closes it, and text that names no page keeps the
//! field open with a red border and "No such page" below it. What the text
//! means, and what the field starts with, is `fastpdf_core::go_to`.

use fastpdf_core::Jump;
use fastpdf_core::go_to::{KnownLabels, PageInputError, prefill_text, resolve_page_input};
use fastpdf_core::keymap::{ReaderCommand, keystrokes_for};
use fastpdf_engine_api::PageIndex;
use gpui::{
    AnyElement, AppContext, ClickEvent, Context, Entity, InteractiveElement, IntoElement,
    ParentElement, StatefulInteractiveElement, Styled, Subscription, Window, anchored, deferred,
    div, prelude::FluentBuilder, px,
};

use crate::reader::ReaderView;
use crate::text_input::{TextChanged, TextInput};
use crate::toolbar::tooltip;

/// Key context of the field: the editing keys plus the page field's own
/// bindings from the central keymap (`PAGE_FIELD`: Enter, Esc).
const FIELD_CONTEXT: &str = "TextInput PageField";
/// Width of the field (the page count included) when the row has room; it
/// narrows to [`MIN_FIELD_WIDTH`] in a crowded toolbar rather than push
/// buttons out.
const FIELD_WIDTH: f32 = 104.0;
const MIN_FIELD_WIDTH: f32 = 64.0;

/// The page field's state.
pub(crate) struct GoToPage {
    /// The field replaces the page indicator.
    pub editing: bool,
    /// The text the last Enter rejected (it named no page); the mark goes
    /// once the field holds something else.
    pub invalid: Option<String>,
    pub field: Entity<TextInput>,
    _changes: Subscription,
    _blur: Subscription,
}

impl GoToPage {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<'_, ReaderView>) -> Self {
        let field = cx.new(|cx| TextInput::with_context("", FIELD_CONTEXT, cx));
        // Change events arrive after the update that caused them, so compare
        // what the field holds now with the rejected text.
        let changes = cx.subscribe(
            &field,
            |view: &mut ReaderView, field, _: &TextChanged, cx| {
                let now = field.read(cx).text();
                if view
                    .go_to
                    .invalid
                    .as_deref()
                    .is_some_and(|rejected| rejected != now)
                {
                    view.go_to.invalid = None;
                    cx.notify();
                }
            },
        );
        let focus = field.read(cx).focus_handle().clone();
        // Leaving the field (a click elsewhere) closes it; focus stays where
        // it went.
        let blur = cx.on_blur(&focus, window, |view: &mut ReaderView, window, cx| {
            view.end_go_to(false, window, cx);
        });
        Self {
            editing: false,
            invalid: None,
            field,
            _changes: changes,
            _blur: blur,
        }
    }
}

impl ReaderView {
    /// Ctrl+G or a click on the page indicator: the field opens with the
    /// current page's label (or number) selected, so typing replaces it.
    /// Enter on that text stays on the page (`go_to::prefill_text`: `#N`
    /// when the label or number could mean another page, and while the
    /// labels are still loading).
    pub(crate) fn start_go_to(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some((page, count)) = self.session().map(|s| (s.current_page(), s.page_count())) else {
            return;
        };
        self.ensure_page_labels(cx);
        let known = if self.page_labels_settled() {
            KnownLabels::Loaded(self.page_labels())
        } else {
            KnownLabels::Loading
        };
        let text = prefill_text(page, self.page_label(page).as_deref(), known, count);
        self.go_to.editing = true;
        self.go_to.invalid = None;
        let field = self.go_to.field.clone();
        field.update(cx, |input, cx| {
            input.set_text(&text, cx);
            input.select_everything(cx);
            window.focus(input.focus_handle(), cx);
        });
        cx.notify();
    }

    /// Enter in the field: goes to the page it names and closes the field;
    /// text that names no page keeps it open, marked. `None` when the field
    /// is not open. Before the labels have loaded only numbers resolve.
    pub(crate) fn confirm_go_to(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<Result<PageIndex, PageInputError>> {
        if !self.go_to.editing {
            return None;
        }
        let page_count = self.session()?.page_count();
        let text = self.go_to.field.read(cx).text().to_owned();
        let result = resolve_page_input(&text, self.page_labels(), page_count);
        match result {
            Ok(page) => {
                self.end_go_to(true, window, cx);
                self.jump(Jump::Direct, |s| s.go_to_page(page));
            }
            Err(PageInputError::NoSuchPage) => {
                self.go_to.invalid = Some(text);
                let field = self.go_to.field.clone();
                field.update(cx, |input, cx| input.select_everything(cx));
            }
            Err(PageInputError::Empty) => {
                self.end_go_to(true, window, cx);
            }
        }
        cx.notify();
        Some(result)
    }

    /// Closes the field (Esc, Enter, leaving it, closing the document);
    /// `refocus` gives the keyboard back to the document. True when it was
    /// open. Closing twice is harmless.
    pub(crate) fn end_go_to(
        &mut self,
        refocus: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        if !self.go_to.editing {
            return false;
        }
        self.go_to.editing = false;
        self.go_to.invalid = None;
        // A field that goes away must not keep the keyboard (the reader's
        // shortcuts would stop working); focus that moved elsewhere stays.
        if refocus || self.go_to.field.read(cx).focus_handle().is_focused(window) {
            window.focus(&self.focus, cx);
        }
        cx.notify();
        true
    }
}

/// The toolbar's page control: the page indicator as a button, or the
/// field while it is open. `blocked`: a prompt card blocks the window.
pub(crate) fn page_control(
    view: &ReaderView,
    blocked: bool,
    window: &Window,
    cx: &mut Context<'_, ReaderView>,
) -> AnyElement {
    let theme = view.theme;
    let strings = view.strings();
    let font = view.language().ui_font();
    let has_doc = view.session().is_some();
    if view.go_to.editing && has_doc {
        let count = view.session().map_or(0, |s| s.page_count());
        let invalid = view.go_to.invalid.is_some();
        let border = if invalid {
            theme.error_text
        } else {
            theme.accent
        };
        // The count sits inside the field, which has the page buttons'
        // room as well: in a narrow window both the text and the count show.
        let field = div()
            .w(px(FIELD_WIDTH))
            .flex_shrink(1.0)
            .min_w(px(MIN_FIELD_WIDTH))
            .h(px(28.0))
            .px_1()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .rounded(px(4.0))
            .bg(theme.input_bg)
            .border_1()
            .border_color(border)
            .child(
                div()
                    .flex_1()
                    .min_w(px(24.0))
                    .child(view.go_to.field.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .text_size(px(12.0))
                    .text_color(theme.text_muted)
                    .child(format!("/ {count}")),
            );
        let control = div()
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .min_w(px(MIN_FIELD_WIDTH))
            .child(field);
        if !invalid {
            return control.into_any_element();
        }
        // Below the field, outside the row: the message takes no room.
        let message = div()
            .px_2()
            .py_1()
            .rounded(px(4.0))
            .bg(theme.toolbar_bg)
            .border_1()
            .border_color(theme.error_text)
            .text_color(theme.error_text)
            .text_size(px(13.0))
            .font_family(font)
            .whitespace_nowrap()
            .child(strings.go_to_no_such_page());
        return control
            .child(
                div().absolute().top(px(32.0)).left(px(0.0)).child(
                    deferred(
                        anchored()
                            .snap_to_window_with_margin(px(8.0))
                            .child(message),
                    )
                    .with_priority(1),
                ),
            )
            .into_any_element();
    }
    let enabled = has_doc && !blocked;
    let tip = strings.with_shortcut(
        strings.tip_go_to_page(),
        keystrokes_for(ReaderCommand::GoToPage).next(),
    );
    let (indicator, narrowest) = crate::page_labels::indicator(view, window);
    // The button narrows exactly as far as its indicator may, so a label
    // wraps and gives way before any button of the row does.
    let pad = px(4.0);
    div()
        .id("go-to-page")
        .flex()
        .items_center()
        .h(px(28.0))
        .px(pad)
        .rounded(px(4.0))
        .map(|button| match narrowest {
            Some(narrowest) => button.min_w(narrowest + pad * 2.0),
            None => button.flex_none(),
        })
        .child(indicator)
        .tooltip(tooltip(tip.into(), theme, font))
        .when(enabled, |button| {
            let hover = theme.button_hover;
            button
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.run_command(ReaderCommand::GoToPage, window, cx);
                }))
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use fastpdf_core::keymap::PAGE_FIELD;
    use gpui::KeyContext;

    use super::FIELD_CONTEXT;
    use crate::text_input::EDIT_CONTEXT;

    #[test]
    fn page_field_context_has_editing_keys_and_page_field_bindings() {
        let context = KeyContext::parse(FIELD_CONTEXT).unwrap();
        assert!(context.contains(EDIT_CONTEXT));
        assert!(context.contains(PAGE_FIELD));
    }
}
