//! Changing the text of a line (ADR 0013; owner request 2026-10-11): the
//! text edit panel, picking a line on a page and editing it in place, the
//! preview of pending changes, and saving them (with any stamps placed) as
//! a new file through the writer (`crate::tools`).
//!
//! A line is a span of the page's text layer, the one text selection uses.
//! The writer keeps a line's glyphs up to the first character the new
//! text changes and draws the rest anew with the chosen font
//! (`fastpdf_engine_api::write::kept_characters`); the preview does the
//! same on screen: paper over the old rest of the line, the new rest drawn
//! in that font. GPUI cannot turn text, so on a page shown turned the
//! preview only covers the line.

use std::path::PathBuf;

use fastpdf_engine_api::write::{
    FontFace, OutputContent, OutputSpec, PagePick, Stamp, TextEdit, WriteJob, kept_characters,
    text_after,
};
use fastpdf_engine_api::{ColorMode, PageIndex, PageRect, Rotation, TextLayer, TextSpan};
use gpui::{
    App, AppContext, BorderStyle, Bounds, ClickEvent, Context, Entity, InteractiveElement,
    IntoElement, ParentElement, Pixels, Rgba, SharedString, StatefulInteractiveElement, Styled,
    Subscription, TextAlign, TextRun, Window, div, fill, font, outline, point,
    prelude::FluentBuilder, px, size,
};

use crate::reader::{DocState, ReaderView};
use crate::text_input::{TextChanged, TextInput};
use crate::toolbar::styled_button;

/// Key context of the line editor's field: the editing keys, plus Enter
/// (apply) and Esc (cancel) from the central keymap (`LINE_EDITOR`).
const FIELD_CONTEXT: &str = "TextInput LineEditor";
const PANEL_WIDTH: f32 = 420.0;
/// How far (points) outside a line's box a press still picks the line.
const PICK_SLOP: f32 = 1.5;

/// A font new text can be drawn with: a Windows font file, and the family
/// the preview draws with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EditFont {
    pub family: &'static str,
    file: &'static str,
    index: u32,
}

/// The fonts offered: 標楷體 first, the font of official documents in
/// Taiwan, then 新細明體 and 微軟正黑體. Their licenses allow embedding a
/// subset (OS/2 fsType 8, "editable").
pub(crate) const EDIT_FONTS: [EditFont; 3] = [
    EditFont {
        family: "DFKai-SB",
        file: "kaiu.ttf",
        index: 0,
    },
    EditFont {
        family: "PMingLiU",
        file: "mingliu.ttc",
        index: 1,
    },
    EditFont {
        family: "Microsoft JhengHei",
        file: "msjh.ttc",
        index: 0,
    },
];

fn fonts_dir() -> PathBuf {
    std::env::var_os("WINDIR")
        .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
        .join("Fonts")
}

impl EditFont {
    fn face(&self) -> FontFace {
        FontFace {
            path: fonts_dir().join(self.file),
            index: self.index,
        }
    }

    fn installed(&self) -> bool {
        fonts_dir().join(self.file).is_file()
    }
}

/// A line whose text is to change.
#[derive(Debug, Clone)]
pub(crate) struct PendingEdit {
    pub page: PageIndex,
    /// The line as the text layer has it.
    pub span: TextSpan,
    pub new: String,
}

impl PendingEdit {
    fn is_line(&self, page: PageIndex, span: &TextSpan) -> bool {
        self.page == page && self.span.bounds == span.bounds && self.span.text == span.text
    }
}

/// Where a line's new text starts (page-space x) and the text drawn there:
/// the line keeps its characters up to the first one that changes.
pub(crate) fn new_part(span: &TextSpan, new: &str) -> (f32, String) {
    let kept = kept_characters(&span.text, new);
    let mut x = span.bounds.x0;
    let mut seen = 0;
    if kept > 0 {
        for (i, c) in span.text.chars().enumerate() {
            if !c.is_whitespace() {
                seen += 1;
                if seen == kept {
                    x = span.char_rect(i).x1;
                    break;
                }
            }
        }
    }
    (x, text_after(new, kept).to_owned())
}

/// The span of `layer` under the page point `(x, y)`: the smallest whose
/// box (a little larger) holds it.
fn span_at(layer: &TextLayer, x: f32, y: f32) -> Option<&TextSpan> {
    layer
        .spans
        .iter()
        .filter(|s| {
            let b = s.bounds;
            x >= b.x0 - PICK_SLOP
                && x <= b.x1 + PICK_SLOP
                && y >= b.y0 - PICK_SLOP
                && y <= b.y1 + PICK_SLOP
        })
        .min_by(|a, b| {
            let area = |s: &TextSpan| s.bounds.width() * s.bounds.height();
            area(a).total_cmp(&area(b))
        })
}

/// The text edit panel and the changes made to the open document's text.
pub(crate) struct TextEditPanel {
    pub open: bool,
    pub edits: Vec<PendingEdit>,
    /// The line being edited in place.
    pub editing: Option<(PageIndex, TextSpan)>,
    /// The line under the pointer.
    pub hover: Option<(PageIndex, PageRect)>,
    /// Index into [`EDIT_FONTS`].
    pub font: usize,
    /// Which of [`EDIT_FONTS`] are installed; looked up when the panel
    /// first opens (no file system access while only reading).
    installed: Option<Vec<bool>>,
    pub problem: Option<String>,
    pub field: Entity<TextInput>,
    _changes: Subscription,
}

impl std::fmt::Debug for TextEditPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextEditPanel")
            .field("open", &self.open)
            .field("edits", &self.edits.len())
            .field("editing", &self.editing.is_some())
            .field("font", &self.font)
            .finish_non_exhaustive()
    }
}

impl TextEditPanel {
    pub(crate) fn new(cx: &mut Context<'_, ReaderView>) -> Self {
        let field = cx.new(|cx| TextInput::with_context("", FIELD_CONTEXT, cx));
        let changes = cx.subscribe(&field, |view: &mut ReaderView, _, _: &TextChanged, cx| {
            view.text_edits.problem = None;
            cx.notify();
        });
        Self {
            open: false,
            edits: Vec::new(),
            editing: None,
            hover: None,
            font: 0,
            installed: None,
            problem: None,
            field,
            _changes: changes,
        }
    }

    /// The open document closed: its changes go.
    pub(crate) fn reset_document(&mut self) {
        self.edits.clear();
        self.editing = None;
        self.hover = None;
        self.problem = None;
    }

    fn is_installed(&self, font: usize) -> bool {
        self.installed
            .as_ref()
            .and_then(|i| i.get(font).copied())
            .unwrap_or(false)
    }

    /// The font chosen, if it is installed.
    fn chosen_font(&self) -> Option<&'static EditFont> {
        self.is_installed(self.font)
            .then(|| EDIT_FONTS.get(self.font))
            .flatten()
    }
}

impl ReaderView {
    pub(crate) fn toggle_text_edit_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.text_edits.open {
            self.close_text_edit_panel(window, cx);
            return;
        }
        if self.stamps.open {
            self.toggle_stamp_panel(cx);
        }
        self.signing.open = false;
        let panel = &mut self.text_edits;
        panel.open = true;
        panel.problem = None;
        if panel.installed.is_none() {
            let installed: Vec<bool> = EDIT_FONTS.iter().map(EditFont::installed).collect();
            panel.font = installed.iter().position(|&i| i).unwrap_or(0);
            panel.installed = Some(installed);
        }
        self.selection.clear();
        cx.notify();
    }

    /// Hides the panel; the line being edited keeps what was typed.
    pub(crate) fn hide_text_edit_panel(&mut self, cx: &mut Context<'_, Self>) {
        self.apply_line_edit(cx);
        self.text_edits.open = false;
        self.text_edits.hover = None;
        cx.notify();
    }

    pub(crate) fn close_text_edit_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.hide_text_edit_panel(cx);
        window.focus(&self.focus, cx);
    }

    /// The line under a window position; asks for the page's text when it
    /// is not here yet.
    fn line_at(
        &mut self,
        position: gpui::Point<Pixels>,
        cx: &mut Context<'_, Self>,
    ) -> Option<(PageIndex, TextSpan)> {
        let point = self.page_point(position)?;
        match self.selection.layer(point.page) {
            Some(layer) => span_at(layer, point.x, point.y).map(|s| (point.page, s.clone())),
            None => {
                let missing: Vec<PageIndex> =
                    self.selection.want_layer(point.page).into_iter().collect();
                self.fetch_text_layers(missing, cx);
                None
            }
        }
    }

    /// Pointer moves over the document: the line under it is marked.
    pub(crate) fn text_edit_hover(
        &mut self,
        position: gpui::Point<Pixels>,
        cx: &mut Context<'_, Self>,
    ) {
        if !self.text_edits.open {
            return;
        }
        let hover = self
            .line_at(position, cx)
            .map(|(page, span)| (page, span.bounds));
        if hover != self.text_edits.hover {
            self.text_edits.hover = hover;
            cx.notify();
        }
    }

    /// A press on the document while the panel is open: applies the line
    /// being edited and starts editing the line pressed. True when the
    /// press was for text editing.
    pub(crate) fn text_edit_mouse_down(
        &mut self,
        position: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        if !self.text_edits.open {
            return false;
        }
        self.apply_line_edit(cx);
        match self.line_at(position, cx) {
            Some((page, span)) => self.begin_line_edit(page, span, window, cx),
            None => window.focus(&self.focus, cx),
        }
        true
    }

    /// Shows the field over `span`, holding the line's pending text.
    pub(crate) fn begin_line_edit(
        &mut self,
        page: PageIndex,
        span: TextSpan,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let text = self
            .text_edits
            .edits
            .iter()
            .find(|e| e.is_line(page, &span))
            .map_or_else(|| span.text.clone(), |e| e.new.clone());
        self.text_edits.editing = Some((page, span));
        self.text_edits.problem = None;
        let field = self.text_edits.field.clone();
        field.update(cx, |input, cx| {
            input.set_text(&text, cx);
            window.focus(input.focus_handle(), cx);
        });
        cx.notify();
    }

    /// The field's text becomes the line's pending change (none when it is
    /// the line's text again).
    pub(crate) fn apply_line_edit(&mut self, cx: &mut Context<'_, Self>) {
        let Some((page, span)) = self.text_edits.editing.take() else {
            return;
        };
        let new = self.text_edits.field.read(cx).text().to_owned();
        let edits = &mut self.text_edits.edits;
        edits.retain(|e| !e.is_line(page, &span));
        if new != span.text {
            edits.push(PendingEdit { page, span, new });
        }
        cx.notify();
    }

    /// Esc in the field: the line keeps its pending text. True when a line
    /// was being edited.
    pub(crate) fn cancel_line_edit(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        if self.text_edits.editing.take().is_none() {
            return false;
        }
        window.focus(&self.focus, cx);
        cx.notify();
        true
    }

    /// Saves the open document with its pending changes (text edits and
    /// stamps) drawn in, as a new file.
    pub(crate) fn save_changes(
        &mut self,
        preset: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.apply_line_edit(cx);
        let strings = self.strings();
        if !matches!(self.doc, DocState::Open(_)) {
            return;
        }
        if self.text_edits.edits.is_empty() && self.stamps.placed.is_empty() {
            self.report_change_problem(strings.nothing_changed.into(), cx);
            return;
        }
        let font = match self.edit_font_face() {
            Ok(font) => font,
            Err(problem) => {
                self.report_change_problem(problem, cx);
                return;
            }
        };
        let DocState::Open(open) = &self.doc else {
            return;
        };
        let source = open.path.clone();
        let count = open.session.page_count();
        let stem = source
            .file_stem()
            .map_or_else(|| "document".into(), |s| s.to_string_lossy().into_owned());
        let mut picks: Vec<PagePick> = (0..count)
            .map(|p| PagePick::new(0, PageIndex::new(p)))
            .collect();
        for stamp in &self.stamps.placed {
            if let Some(pick) = picks.get_mut(stamp.page.as_usize()) {
                pick.stamps.push(Stamp {
                    image: stamp.image.path.clone(),
                    rect: stamp.rect,
                    rotate: stamp.turn,
                });
            }
        }
        if let Some(font) = font {
            for edit in &self.text_edits.edits {
                if let Some(pick) = picks.get_mut(edit.page.as_usize()) {
                    pick.edits.push(TextEdit {
                        rect: edit.span.bounds,
                        old: edit.span.text.clone(),
                        new: edit.new.clone(),
                        font: font.clone(),
                    });
                }
            }
        }
        let suffix = if self.text_edits.edits.is_empty() {
            strings.stamp_suffix
        } else {
            strings.edit_suffix
        };
        let suggested = format!("{stem}{suffix}.pdf");
        self.text_edits.problem = None;
        self.stamps.problem = None;
        self.ask_where_for(
            source,
            suggested,
            preset,
            window,
            cx,
            move |source, path| {
                Ok(WriteJob {
                    sources: vec![source],
                    outputs: vec![OutputSpec {
                        path,
                        content: OutputContent::Pages(picks),
                    }],
                })
            },
        );
    }

    /// The font new text is drawn with: `None` without text edits; the
    /// message to show when no font for new text is installed.
    pub(crate) fn edit_font_face(&mut self) -> Result<Option<FontFace>, String> {
        if self.text_edits.edits.is_empty() {
            return Ok(None);
        }
        if self.text_edits.installed.is_none() {
            let installed: Vec<bool> = EDIT_FONTS.iter().map(EditFont::installed).collect();
            self.text_edits.installed = Some(installed);
        }
        match self.text_edits.chosen_font() {
            Some(font) => Ok(Some(font.face())),
            None => Err(self.strings().no_edit_font.into()),
        }
    }

    /// Shows why the changes cannot be saved, in the panel that is open.
    fn report_change_problem(&mut self, problem: String, cx: &mut Context<'_, Self>) {
        if self.text_edits.open {
            self.text_edits.problem = Some(problem);
        } else {
            self.stamps.problem = Some(problem);
        }
        cx.notify();
    }

    /// Paints the pending changes over the pages (the new rest of each
    /// line over paper) and, while the panel is open, marks the edited
    /// lines and the line under the pointer.
    pub(crate) fn paint_text_edits(
        &self,
        origin: gpui::Point<Pixels>,
        paper: Rgba,
        mode: ColorMode,
        window: &mut Window,
        cx: &mut App,
    ) {
        let panel = &self.text_edits;
        if panel.edits.is_empty() && !panel.open {
            return;
        }
        let Some(session) = self.session() else {
            return;
        };
        let accent = self.theme.accent;
        let ink = if mode == ColorMode::Inverted {
            gpui::white()
        } else {
            gpui::black()
        };
        let family = EDIT_FONTS.get(panel.font).map_or("DFKai-SB", |f| f.family);
        let bounds = |r: [f32; 4]| {
            Bounds::new(
                point(origin.x + px(r[0]), origin.y + px(r[1])),
                size(px(r[2]), px(r[3])),
            )
        };
        for edit in &panel.edits {
            if panel
                .editing
                .as_ref()
                .is_some_and(|(page, span)| edit.is_line(*page, span))
            {
                continue;
            }
            let b = edit.span.bounds;
            let (x, rest) = new_part(&edit.span, &edit.new);
            let cover = PageRect::new(x, b.y0, b.x1.max(x), b.y1);
            if let Some(r) = session.page_to_view(edit.page, cover) {
                window.paint_quad(fill(bounds(r), paper));
            }
            let upright = session.display_rotation(edit.page) == Some(Rotation::R0);
            if upright
                && !rest.is_empty()
                && let Some(r) = session.page_to_view(edit.page, PageRect::new(x, b.y0, x, b.y1))
            {
                // A text layer box runs from the descender to the ascender
                // line: it is one em high.
                let em = r[3].max(1.0);
                let run = TextRun {
                    len: rest.len(),
                    font: font(family),
                    color: ink,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let line =
                    window
                        .text_system()
                        .shape_line(SharedString::from(rest), px(em), &[run], None);
                let at = point(origin.x + px(r[0]), origin.y + px(r[1]));
                if let Err(e) = line.paint(at, px(em), TextAlign::Left, None, window, cx) {
                    log::debug!("cannot paint an edited line: {e}");
                }
            }
            if panel.open
                && let Some(r) = session.page_to_view(edit.page, b)
            {
                window.paint_quad(outline(bounds(r), accent, BorderStyle::Dashed));
            }
        }
        if let Some((page, rect)) = panel.hover.filter(|_| panel.open)
            && let Some(r) = session.page_to_view(page, rect)
        {
            window.paint_quad(outline(
                bounds(r).dilate(px(2.0)),
                accent,
                BorderStyle::Solid,
            ));
        }
    }

    /// The field over the line being edited.
    pub(crate) fn render_line_editor(
        &self,
        cx: &mut Context<'_, Self>,
    ) -> Option<impl IntoElement + use<>> {
        let (page, span) = self.text_edits.editing.as_ref()?;
        let r = self.session()?.page_to_view(*page, span.bounds)?;
        let theme = self.theme;
        let strings = self.strings();
        let width = (r[2] + 80.0).max(280.0);
        let top = r[1] + r[3] / 2.0 - 17.0;
        Some(
            div()
                .absolute()
                .left(px(r[0] - 6.0))
                .top(px(top))
                .w(px(width))
                .child(
                    div()
                        .id("line-editor")
                        .occlude()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .p_1()
                        .rounded(px(4.0))
                        .bg(theme.toolbar_bg)
                        .border_1()
                        .border_color(theme.accent)
                        .text_size(px(14.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(self.text_edits.field.clone()),
                        )
                        .child(
                            styled_button("line-apply", strings.apply, true, false, &theme)
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.apply_line_edit(cx);
                                    window.focus(&this.focus, cx);
                                })),
                        )
                        .child(
                            styled_button("line-cancel", "\u{2715}", true, false, &theme).on_click(
                                cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.cancel_line_edit(window, cx);
                                }),
                            ),
                        ),
                ),
        )
    }

    /// The text edit panel, floating over the top of the document.
    pub(crate) fn render_text_edit_panel(
        &self,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement + use<> {
        let theme = self.theme;
        let strings = self.strings();
        let panel = &self.text_edits;
        let has_doc = self.session().is_some();
        let fonts = EDIT_FONTS.iter().enumerate().map(|(i, _)| {
            let installed = panel.is_installed(i);
            styled_button(
                format!("edit-font-{i}"),
                strings.edit_font_name(i),
                installed,
                panel.font == i,
                &theme,
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                if this.text_edits.is_installed(i) {
                    this.text_edits.font = i;
                    this.text_edits.problem = None;
                    cx.notify();
                }
            }))
        });
        let mut body = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_size(px(15.0)).child(strings.edit_text_title))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.text_muted)
                    .child(strings.edit_text_hint),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .child(div().mr_1().child(strings.edit_font))
                    .children(fonts),
            );
        if !panel.edits.is_empty() {
            let rows = panel.edits.iter().enumerate().map(|(i, edit)| {
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(strings.edit_on_page(edit.page.display_number(), &edit.new)),
                    )
                    .child(
                        styled_button(format!("edit-remove-{i}"), "\u{2715}", true, false, &theme)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                if i < this.text_edits.edits.len() {
                                    this.text_edits.edits.remove(i);
                                    cx.notify();
                                }
                            })),
                    )
            });
            body = body.child(
                div()
                    .id("pending-edits")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .max_h(px(160.0))
                    .overflow_y_scroll()
                    .p_1()
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(theme.toolbar_border)
                    .bg(theme.input_bg)
                    .children(rows),
            );
        }
        if let Some(problem) = &panel.problem {
            body = body.child(div().text_color(theme.error_text).child(problem.clone()));
        }
        body = body.child(
            div()
                .text_size(px(12.0))
                .text_color(theme.text_muted)
                .child(strings.original_untouched),
        );
        let pending = !panel.edits.is_empty() || !self.stamps.placed.is_empty();
        let can_save = has_doc && pending && !self.tools.writing();
        body = body.child(
            div()
                .flex()
                .flex_row()
                .justify_end()
                .gap_1()
                .child(
                    styled_button("edit-save", strings.save_as, can_save, false, &theme)
                        .when(can_save, |b| b.bg(theme.button_active))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.save_changes(None, window, cx);
                        })),
                )
                .child(
                    styled_button(
                        "edit-clear",
                        strings.clear_edits,
                        !panel.edits.is_empty(),
                        false,
                        &theme,
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.text_edits.edits.clear();
                        this.text_edits.editing = None;
                        cx.notify();
                    })),
                )
                .child(
                    styled_button("edit-close", strings.close, true, false, &theme).on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.close_text_edit_panel(window, cx);
                        }),
                    ),
                ),
        );
        div()
            .absolute()
            .top(px(8.0))
            .right(px(8.0 + crate::scrollbar::THICKNESS))
            .child(
                div()
                    .id("text-edit-panel")
                    .occlude()
                    .w(px(PANEL_WIDTH))
                    .p_3()
                    .rounded(px(6.0))
                    .bg(theme.toolbar_bg)
                    .border_1()
                    .border_color(theme.toolbar_border)
                    .text_size(px(13.0))
                    .child(body),
            )
    }

    /// Development scripts: edits the line of page `page` (one-based) whose
    /// text contains `needle` to read `new`. `None` while the page's text
    /// is still coming.
    pub(crate) fn dev_text_edit(
        &mut self,
        page: u32,
        needle: &str,
        new: &str,
        cx: &mut Context<'_, Self>,
    ) -> Option<Result<(), String>> {
        let page = PageIndex::new(page.saturating_sub(1));
        let Some(layer) = self.selection.layer(page) else {
            let missing: Vec<PageIndex> = self.selection.want_layer(page).into_iter().collect();
            self.fetch_text_layers(missing, cx);
            return None;
        };
        let Some(span) = layer
            .spans
            .iter()
            .find(|s| s.text.contains(needle))
            .cloned()
        else {
            return Some(Err(format!(
                "no line with {needle:?} on page {}",
                page.display_number()
            )));
        };
        let edits = &mut self.text_edits.edits;
        edits.retain(|e| !e.is_line(page, &span));
        let new = span.text.replacen(needle, new, 1);
        edits.push(PendingEdit { page, span, new });
        cx.notify();
        Some(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(text: &str) -> TextSpan {
        // Ten points per character, starting at x = 100.
        let n = text.chars().count() as f32;
        TextSpan {
            text: text.into(),
            bounds: PageRect::new(100.0, 50.0, 100.0 + 10.0 * n, 62.0),
            char_bounds: Vec::new(),
        }
    }

    #[test]
    fn previews_keep_the_unchanged_start() {
        let line = span("原告 張三");
        assert_eq!(new_part(&line, "原告 李四"), (120.0, " 李四".into()));
        assert_eq!(new_part(&line, "被告 張三"), (100.0, "被告 張三".into()));
        assert_eq!(new_part(&line, "原告 張三丰"), (150.0, "丰".into()));
        assert_eq!(new_part(&line, ""), (100.0, String::new()));
    }

    #[test]
    fn presses_pick_the_smallest_line_under_them() {
        let layer = TextLayer {
            page: PageIndex::FIRST,
            spans: vec![span("a long line here"), {
                let mut s = span("ab");
                s.bounds = PageRect::new(100.0, 50.0, 120.0, 62.0);
                s
            }],
        };
        assert_eq!(
            span_at(&layer, 110.0, 55.0).map(|s| s.text.as_str()),
            Some("ab")
        );
        assert_eq!(
            span_at(&layer, 200.0, 61.0).map(|s| s.text.as_str()),
            Some("a long line here")
        );
        assert!(span_at(&layer, 200.0, 70.0).is_none());
    }
}
