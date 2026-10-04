//! A minimal single-line text field with IME support (the find box).
//!
//! Portions Copyright Zed Industries, Inc., licensed under the Apache
//! License, Version 2.0 (`licenses/Apache-2.0.txt`); modified for FastPDF.
//! See THIRD_PARTY_LICENSES.md, "Ported source".
//!
//! Adapted from GPUI's `examples/input.rs` (zed `a84689073`),
//! reduced to what a search box needs: typing and IME composition (marked
//! text, candidate window placement), caret movement and selection by
//! keyboard and mouse, clipboard. Offsets are byte offsets into UTF-8; the
//! platform input API speaks UTF-16, converted at the trait boundary.
//! Movement is per `char` (no grapheme segmentation dependency), which is
//! exact for CJK and Latin text.

use std::ops::Range;

use gpui::{
    Action, App, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, GlobalElementId,
    InteractiveElement, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, NoAction, PaintQuad, ParentElement, Pixels, Point, Render, ShapedLine,
    SharedString, Style, Styled, TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window,
    actions, div, fill, hsla, point, px, relative, size,
};

/// Key context shared by every text field; the editing keys below are bound
/// in it and take precedence over the reader's bindings.
pub(crate) const EDIT_CONTEXT: &str = "TextInput";
/// Full key context of the find bar's field: the editing keys plus the find
/// bar's own bindings from the central keymap (`FIND_BAR`).
const FIND_FIELD_CONTEXT: &str = "TextInput FindBar";

actions!(
    fastpdf_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        Home,
        End,
        SelectToHome,
        SelectToEnd,
        SelectAll,
        Copy,
        Cut,
        Paste,
    ]
);

/// Editing keys of the field. `space` / `shift-space` are unbound here so
/// they type instead of paging the document (the reader binds them).
pub(crate) fn bindings() -> Vec<(&'static str, Box<dyn Action>)> {
    vec![
        ("backspace", Box::new(Backspace)),
        ("delete", Box::new(Delete)),
        ("left", Box::new(Left)),
        ("right", Box::new(Right)),
        ("shift-left", Box::new(SelectLeft)),
        ("shift-right", Box::new(SelectRight)),
        ("home", Box::new(Home)),
        ("end", Box::new(End)),
        ("shift-home", Box::new(SelectToHome)),
        ("shift-end", Box::new(SelectToEnd)),
        ("ctrl-a", Box::new(SelectAll)),
        ("ctrl-c", Box::new(Copy)),
        ("ctrl-x", Box::new(Cut)),
        ("ctrl-v", Box::new(Paste)),
        ("space", Box::new(NoAction)),
        ("shift-space", Box::new(NoAction)),
    ]
}

/// Emitted when the committed text changes (not while an IME composes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TextChanged(pub String);

/// The field's state; render it as a child view.
pub(crate) struct TextInput {
    focus: FocusHandle,
    /// Key context of the field: [`EDIT_CONTEXT`] plus its role.
    key_context: &'static str,
    text: String,
    placeholder: SharedString,
    selected: Range<usize>,
    reversed: bool,
    /// IME composition in progress.
    marked: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    selecting: bool,
}

impl std::fmt::Debug for TextInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextInput")
            .field("text", &self.text)
            .field("selected", &self.selected)
            .field("marked", &self.marked)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<TextChanged> for TextInput {}

impl TextInput {
    /// The find bar's field.
    pub(crate) fn new(placeholder: impl Into<SharedString>, cx: &mut Context<'_, Self>) -> Self {
        Self::with_context(placeholder, FIND_FIELD_CONTEXT, cx)
    }

    /// A field whose key context is `key_context`, which must contain
    /// [`EDIT_CONTEXT`] for the editing keys to work.
    pub(crate) fn with_context(
        placeholder: impl Into<SharedString>,
        key_context: &'static str,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        Self {
            focus: cx.focus_handle(),
            key_context,
            text: String::new(),
            placeholder: placeholder.into(),
            selected: 0..0,
            reversed: false,
            marked: None,
            last_layout: None,
            last_bounds: None,
            selecting: false,
        }
    }

    pub(crate) fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Replaces the whole text (not an edit by the user, e.g. a scripted or
    /// restored value) and reports it like typing would.
    pub(crate) fn set_text(&mut self, text: &str, cx: &mut Context<'_, Self>) {
        let line = text.replace(['\r', '\n'], " ");
        self.text = line;
        self.selected = self.text.len()..self.text.len();
        self.reversed = false;
        self.marked = None;
        cx.emit(TextChanged(self.text.clone()));
        cx.notify();
    }

    /// Selects everything, so typing replaces the previous query.
    pub(crate) fn select_everything(&mut self, cx: &mut Context<'_, Self>) {
        self.selected = 0..self.text.len();
        self.reversed = false;
        cx.notify();
    }

    fn cursor(&self) -> usize {
        if self.reversed {
            self.selected.start
        } else {
            self.selected.end
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<'_, Self>) {
        self.selected = offset..offset;
        self.reversed = false;
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<'_, Self>) {
        if self.reversed {
            self.selected.start = offset;
        } else {
            self.selected.end = offset;
        }
        if self.selected.end < self.selected.start {
            self.reversed = !self.reversed;
            self.selected = self.selected.end..self.selected.start;
        }
        cx.notify();
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.text[..offset]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.text[offset..]
            .chars()
            .next()
            .map_or(self.text.len(), |c| offset + c.len_utf8())
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<'_, Self>) {
        if self.selected.is_empty() {
            self.move_to(self.previous_boundary(self.cursor()), cx);
        } else {
            self.move_to(self.selected.start, cx);
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<'_, Self>) {
        if self.selected.is_empty() {
            self.move_to(self.next_boundary(self.cursor()), cx);
        } else {
            self.move_to(self.selected.end, cx);
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<'_, Self>) {
        self.select_to(self.previous_boundary(self.cursor()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<'_, Self>) {
        self.select_to(self.next_boundary(self.cursor()), cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<'_, Self>) {
        self.move_to(0, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<'_, Self>) {
        self.move_to(self.text.len(), cx);
    }

    fn select_to_home(&mut self, _: &SelectToHome, _: &mut Window, cx: &mut Context<'_, Self>) {
        self.select_to(0, cx);
    }

    fn select_to_end(&mut self, _: &SelectToEnd, _: &mut Window, cx: &mut Context<'_, Self>) {
        self.select_to(self.text.len(), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<'_, Self>) {
        self.select_everything(cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.selected.is_empty() {
            let prev = self.previous_boundary(self.cursor());
            self.select_to(prev, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.selected.is_empty() {
            let next = self.next_boundary(self.cursor());
            self.select_to(next, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<'_, Self>) {
        if !self.selected.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.text[self.selected.clone()].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<'_, Self>) {
        if !self.selected.is_empty() {
            self.copy(&Copy, window, cx);
            self.replace_text_in_range(None, "", window, cx);
        }
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            // Single line: line breaks become spaces.
            let line = text.replace(['\r', '\n'], " ");
            self.replace_text_in_range(None, &line, window, cx);
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        window.focus(&self.focus, cx);
        self.selecting = true;
        let index = self.index_for_position(event.position);
        if event.modifiers.shift {
            self.select_to(index, cx);
        } else {
            self.move_to(index, cx);
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<'_, Self>) {
        self.selecting = false;
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.selecting {
            self.select_to(self.index_for_position(event.position), cx);
        }
    }

    fn index_for_position(&self, position: Point<Pixels>) -> usize {
        let (Some(bounds), Some(line)) = (self.last_bounds, self.last_layout.as_ref()) else {
            return 0;
        };
        if self.text.is_empty() || position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.text.len();
        }
        line.closest_index_for_x(position.x - bounds.left())
            .min(self.text.len())
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        offset_from_utf16(&self.text, offset)
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        offset_to_utf16(&self.text, offset)
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    /// Replaces `range` with `new_text`, returning the start of the inserted
    /// text. Pure text bookkeeping shared by the IME entry points.
    fn splice(&mut self, range: Range<usize>, new_text: &str) -> usize {
        let start = range.start.min(self.text.len());
        let end = range.end.clamp(start, self.text.len());
        self.text.replace_range(start..end, new_text);
        start
    }
}

/// Byte offset in `text` of UTF-16 offset `offset` (clamped).
fn offset_from_utf16(text: &str, offset: usize) -> usize {
    let mut utf8 = 0;
    let mut utf16 = 0;
    for ch in text.chars() {
        if utf16 >= offset {
            break;
        }
        utf16 += ch.len_utf16();
        utf8 += ch.len_utf8();
    }
    utf8
}

/// UTF-16 offset of byte offset `offset` in `text` (clamped).
fn offset_to_utf16(text: &str, offset: usize) -> usize {
    let mut utf8 = 0;
    let mut utf16 = 0;
    for ch in text.chars() {
        if utf8 >= offset {
            break;
        }
        utf8 += ch.len_utf8();
        utf16 += ch.len_utf16();
    }
    utf16
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<'_, Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        self.text.get(range).map(str::to_owned)
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _: &mut Window,
        _: &mut Context<'_, Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected),
            reversed: self.reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<'_, Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|r| self.range_to_utf16(r))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) {
        if self.marked.take().is_some() {
            cx.emit(TextChanged(self.text.clone()));
        }
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked.clone())
            .unwrap_or(self.selected.clone());
        let start = self.splice(range, new_text);
        let caret = start + new_text.len();
        self.selected = caret..caret;
        self.reversed = false;
        self.marked = None;
        cx.emit(TextChanged(self.text.clone()));
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked.clone())
            .unwrap_or(self.selected.clone());
        let start = self.splice(range, new_text);
        self.marked = (!new_text.is_empty()).then(|| start..start + new_text.len());
        // The IME's selection is relative to the composition.
        self.selected = new_selected_range_utf16
            .map(|r| {
                let r = offset_from_utf16(new_text, r.start)..offset_from_utf16(new_text, r.end);
                start + r.start..start + r.end
            })
            .unwrap_or(start + new_text.len()..start + new_text.len());
        self.reversed = false;
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<'_, Self>,
    ) -> Option<Bounds<Pixels>> {
        // Places the IME candidate window under the composition.
        let layout = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        Some(Bounds::from_corners(
            point(
                bounds.left() + layout.x_for_index(range.start),
                bounds.top(),
            ),
            point(
                bounds.left() + layout.x_for_index(range.end),
                bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<'_, Self>,
    ) -> Option<usize> {
        let local = self.last_bounds?.localize(&point)?;
        let layout = self.last_layout.as_ref()?;
        let index = layout.index_for_x(local.x)?;
        Some(self.offset_to_utf16(index))
    }
}

impl Render for TextInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        div()
            .key_context(self.key_context)
            .track_focus(&self.focus)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_to_home))
            .on_action(cx.listener(Self::select_to_end))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .size_full()
            .flex()
            .items_center()
            .overflow_hidden()
            .child(TextField { input: cx.entity() })
    }
}

/// Paints the text, selection, composition underline and caret, and
/// registers the field as the window's text input target.
struct TextField {
    input: Entity<TextInput>,
}

struct TextFieldState {
    line: Option<ShapedLine>,
    caret: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}

impl IntoElement for TextField {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextField {
    type RequestLayoutState = ();
    type PrepaintState = TextFieldState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let style = window.text_style();
        let (display, color) = if input.text.is_empty() {
            (input.placeholder.clone(), style.color.opacity(0.45))
        } else {
            (SharedString::from(input.text.clone()), style.color)
        };
        let run = TextRun {
            len: display.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = match input.marked.as_ref() {
            Some(marked) if !input.text.is_empty() => vec![
                TextRun {
                    len: marked.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked.end - marked.start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: display.len() - marked.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|r| r.len > 0)
            .collect(),
            _ => vec![run],
        };
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(display, font_size, &runs, None);
        let accent = hsla(0.58, 0.9, 0.5, 1.0);
        let (selection, caret) = if input.selected.is_empty() {
            let x = if input.text.is_empty() {
                px(0.0)
            } else {
                line.x_for_index(input.cursor())
            };
            (
                None,
                Some(fill(
                    Bounds::new(
                        point(bounds.left() + x, bounds.top()),
                        size(px(1.5), bounds.size.height),
                    ),
                    accent,
                )),
            )
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(
                            bounds.left() + line.x_for_index(input.selected.start),
                            bounds.top(),
                        ),
                        point(
                            bounds.left() + line.x_for_index(input.selected.end),
                            bounds.bottom(),
                        ),
                    ),
                    accent.opacity(0.3),
                )),
                None,
            )
        };
        TextFieldState {
            line: Some(line),
            caret,
            selection,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        state: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.input.read(cx).focus.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        if let Some(selection) = state.selection.take() {
            window.paint_quad(selection);
        }
        let Some(line) = state.line.take() else {
            return;
        };
        if let Err(e) = line.paint(
            bounds.origin,
            window.line_height(),
            TextAlign::Left,
            None,
            window,
            cx,
        ) {
            log::debug!("text field paint: {e}");
        }
        if focus.is_focused(window)
            && let Some(caret) = state.caret.take()
        {
            window.paint_quad(caret);
        }
        self.input.update(cx, |input, _| {
            input.last_layout = Some(line);
            input.last_bounds = Some(bounds);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::KeyContext;

    #[test]
    fn find_field_context_has_editing_keys_and_find_bar_bindings() {
        let context = KeyContext::parse(FIND_FIELD_CONTEXT).unwrap();
        assert!(context.contains(EDIT_CONTEXT));
        assert!(context.contains(fastpdf_core::keymap::FIND_BAR));
    }

    #[test]
    fn utf16_offsets_round_trip_through_cjk_and_astral_text() {
        let text = "改善a😀b";
        // 改(3 bytes,1 unit) 善(3,1) a(1,1) 😀(4,2) b(1,1)
        assert_eq!(offset_to_utf16(text, 0), 0);
        assert_eq!(offset_to_utf16(text, 3), 1);
        assert_eq!(offset_to_utf16(text, 7), 3);
        assert_eq!(offset_to_utf16(text, 11), 5);
        assert_eq!(offset_to_utf16(text, text.len()), 6);
        for units in [0, 1, 2, 3, 5, 6] {
            assert_eq!(offset_to_utf16(text, offset_from_utf16(text, units)), units);
        }
        assert_eq!(offset_from_utf16(text, 99), text.len(), "clamped");
    }

    #[test]
    fn every_binding_names_a_key() {
        let keys: Vec<&str> = bindings().into_iter().map(|(k, _)| k).collect();
        assert!(keys.contains(&"space"), "space must type in the field");
        assert!(keys.contains(&"ctrl-v"));
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), keys.len(), "duplicate keystroke");
    }
}
