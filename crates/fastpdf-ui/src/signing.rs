//! Digital signatures (ADR 0013 §5, `docs/digital-signatures.md`): the
//! signature panel, which lists the certificates the user can sign with
//! (read in the background when the panel opens), and signing the open
//! document, with any pending text edits and stamps, into a new file.
//!
//! A placed stamp can be the signature's appearance: the one selected (or
//! the last one placed) becomes the visible signature instead of being
//! drawn into the page. The card's PIN dialog belongs to the reader's
//! window; the writer's child process shows it.

use std::path::PathBuf;

use fastpdf_engine_api::PageIndex;
use fastpdf_engine_api::write::{
    OutputContent, OutputSpec, PagePick, SignatureAppearance, SignatureSpec, Stamp, TextEdit,
    WriteJob,
};
use fastpdf_sign::Certificate;
use gpui::{
    AppContext, ClickEvent, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::opening::display_name;
use crate::reader::{DocState, ReaderView};
use crate::text_input::TextInput;
use crate::toolbar::styled_button;

const FIELD_CONTEXT: &str = "TextInput SignPanel";
const PANEL_WIDTH: f32 = 460.0;

/// The signature panel.
pub(crate) struct SignaturePanel {
    pub open: bool,
    /// The certificates; `None` while they are read.
    pub certificates: Option<Vec<Certificate>>,
    /// The chosen certificate's thumbprint.
    pub chosen: Option<[u8; 20]>,
    /// Development scripts: sign with a key made for the job.
    pub test_signer: bool,
    /// A placed stamp is the signature's appearance.
    pub visible: bool,
    pub reason: Entity<TextInput>,
    pub location: Entity<TextInput>,
    pub problem: Option<String>,
}

impl std::fmt::Debug for SignaturePanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignaturePanel")
            .field("open", &self.open)
            .field("certificates", &self.certificates.as_ref().map(Vec::len))
            .field("visible", &self.visible)
            .finish_non_exhaustive()
    }
}

impl SignaturePanel {
    pub(crate) fn new(cx: &mut Context<'_, ReaderView>) -> Self {
        Self {
            open: false,
            certificates: None,
            chosen: None,
            test_signer: false,
            visible: false,
            reason: cx.new(|cx| TextInput::with_context("", FIELD_CONTEXT, cx)),
            location: cx.new(|cx| TextInput::with_context("", FIELD_CONTEXT, cx)),
            problem: None,
        }
    }
}

/// The raw handle of `window` (0 when there is none), for dialogs that
/// belong to it.
fn window_handle(window: &Window) -> u64 {
    // GPUI's own `window_handle` is another method: ask raw-window-handle.
    match HasWindowHandle::window_handle(window).map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(handle)) => handle.hwnd.get() as u64,
        _ => 0,
    }
}

impl ReaderView {
    pub(crate) fn toggle_sign_panel(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.signing.open {
            self.signing.open = false;
            window.focus(&self.focus, cx);
            cx.notify();
            return;
        }
        self.make_room_for(crate::panel_slot::CornerPanel::Sign, window, cx);
        self.signing.open = true;
        self.signing.problem = None;
        // The test signer (development scripts) replaces the store.
        if self.signing.certificates.is_none() && !self.signing.test_signer {
            self.refresh_certificates(cx);
        }
        cx.notify();
    }

    /// Reads the certificates again, in the background (a card may have
    /// been inserted).
    pub(crate) fn refresh_certificates(&mut self, cx: &mut Context<'_, Self>) {
        self.signing.certificates = None;
        let work = cx
            .background_executor()
            .spawn(async move { fastpdf_sign::certificates() });
        cx.spawn(async move |this, cx| {
            let certificates = work.await;
            let _ = this.update(cx, |this, cx| {
                let panel = &mut this.signing;
                // Keep the choice when it is still there; one certificate
                // is the choice.
                let still = panel
                    .chosen
                    .is_some_and(|c| certificates.iter().any(|x| x.thumbprint == c));
                if !still {
                    panel.chosen = match certificates.as_slice() {
                        [only] => Some(only.thumbprint),
                        _ => None,
                    };
                }
                panel.certificates = Some(certificates);
                cx.notify();
            });
        })
        .detach();
    }

    /// The placed stamp that is the signature's appearance: the selected
    /// one, or the last one placed.
    fn appearance_stamp(&self) -> Option<usize> {
        if !self.signing.visible || self.stamps.placed.is_empty() {
            return None;
        }
        Some(
            self.stamps
                .selected
                .filter(|&i| i < self.stamps.placed.len())
                .unwrap_or(self.stamps.placed.len() - 1),
        )
    }

    /// Signs the open document (with its pending changes) into a new file.
    /// `preset` skips the save dialog (development scripts).
    pub(crate) fn sign_document(
        &mut self,
        preset: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.apply_line_edit(cx);
        let strings = self.strings();
        let signer = if self.signing.test_signer {
            "test".to_owned()
        } else {
            match self.signing.chosen {
                Some(thumbprint) => thumbprint.iter().map(|b| format!("{b:02X}")).collect(),
                None => {
                    self.signing.problem = Some(strings.choose_certificate.into());
                    cx.notify();
                    return;
                }
            }
        };
        let font = match self.edit_font_face() {
            Ok(font) => font,
            Err(problem) => {
                self.signing.problem = Some(problem);
                cx.notify();
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
        let shown = self.appearance_stamp();
        let mut picks: Vec<PagePick> = (0..count)
            .map(|p| PagePick::new(0, PageIndex::new(p)))
            .collect();
        let mut changed = false;
        let mut appearance = None;
        for (i, stamp) in self.stamps.placed.iter().enumerate() {
            let placed = Stamp {
                image: stamp.image.path.clone(),
                rect: stamp.rect,
                rotate: stamp.turn,
            };
            if Some(i) == shown {
                appearance = Some(SignatureAppearance {
                    page: stamp.page,
                    stamp: placed,
                });
            } else if let Some(pick) = picks.get_mut(stamp.page.as_usize()) {
                pick.stamps.push(placed);
                changed = true;
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
                    changed = true;
                }
            }
        }
        // Unchanged documents are signed as they are, so the signatures
        // they already have stay valid.
        let base = if changed {
            OutputContent::Pages(picks)
        } else {
            OutputContent::Copy { source: 0 }
        };
        let signature = SignatureSpec {
            signer,
            window: window_handle(window),
            reason: self.signing.reason.read(cx).text().trim().to_owned(),
            location: self.signing.location.read(cx).text().trim().to_owned(),
            appearance,
        };
        let suggested = format!("{stem}{}.pdf", strings.sign_suffix);
        self.signing.problem = None;
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
                        content: OutputContent::Signed {
                            base: Box::new(base),
                            signature,
                        },
                    }],
                })
            },
        );
    }

    /// The signature panel, floating over the top of the document.
    pub(crate) fn render_sign_panel(&self, cx: &mut Context<'_, Self>) -> impl IntoElement + use<> {
        let theme = self.theme;
        let strings = self.strings();
        let panel = &self.signing;
        let has_doc = self.session().is_some();
        let mut list = div()
            .id("certificates")
            .flex()
            .flex_col()
            .gap_1()
            .max_h(px(150.0))
            .overflow_y_scroll()
            .p_1()
            .rounded(px(4.0))
            .border_1()
            .border_color(theme.toolbar_border)
            .bg(theme.input_bg);
        match &panel.certificates {
            _ if panel.test_signer => {
                list = list.child(
                    div()
                        .p_1()
                        .rounded(px(4.0))
                        .bg(theme.button_active)
                        .child(strings.test_signer),
                );
            }
            None => {
                list = list.child(
                    div()
                        .text_color(theme.text_muted)
                        .child(strings.reading_certificates),
                );
            }
            Some(certificates) if certificates.is_empty() => {
                list = list.child(
                    div()
                        .text_color(theme.text_muted)
                        .child(strings.no_certificates),
                );
            }
            Some(certificates) => {
                for (i, certificate) in certificates.iter().enumerate() {
                    let chosen = panel.chosen == Some(certificate.thumbprint);
                    let thumbprint = certificate.thumbprint;
                    list = list.child(
                        div()
                            .id(("certificate", i))
                            .flex()
                            .flex_col()
                            .p_1()
                            .rounded(px(4.0))
                            .cursor_pointer()
                            .when(chosen, |d| d.bg(theme.button_active))
                            .hover(move |s| s.bg(theme.button_hover))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.signing.chosen = Some(thumbprint);
                                this.signing.problem = None;
                                cx.notify();
                            }))
                            .child(div().child(certificate.subject.clone()))
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(theme.text_muted)
                                    .child(strings.certificate_detail(
                                        &certificate.issuer,
                                        &certificate.valid_until,
                                    )),
                            ),
                    );
                }
            }
        }
        let shown = self
            .stamps
            .placed
            .get(
                self.stamps
                    .selected
                    .unwrap_or(self.stamps.placed.len().saturating_sub(1)),
            )
            .map(|s| {
                strings.sign_visible_stamp(s.page.display_number(), &display_name(&s.image.path))
            });
        let has_stamp = shown.is_some();
        let pending = !self.text_edits.edits.is_empty()
            || self.stamps.placed.len() > usize::from(panel.visible && has_stamp);
        let field = |label: &'static str, input: Entity<TextInput>| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(div().min_w(px(48.0)).child(label))
                .child(
                    div()
                        .flex_1()
                        .border_1()
                        .rounded(px(4.0))
                        .border_color(theme.toolbar_border)
                        .bg(theme.input_bg)
                        .px_1()
                        .child(input),
                )
        };
        let can_sign =
            has_doc && !self.tools.writing() && (panel.chosen.is_some() || panel.test_signer);
        let mut body = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_size(px(15.0)).child(strings.sign_title))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.text_muted)
                    .child(strings.sign_hint),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .child(div().flex_1().child(strings.sign_certificate))
                    .child(
                        styled_button("sign-refresh", strings.refresh, true, false, &theme)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.refresh_certificates(cx);
                            })),
                    ),
            )
            .child(list)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        styled_button(
                            "sign-visible",
                            if panel.visible {
                                "\u{2611}"
                            } else {
                                "\u{2610}"
                            },
                            has_stamp,
                            false,
                            &theme,
                        )
                        .on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| {
                                this.signing.visible = !this.signing.visible;
                                cx.notify();
                            },
                        )),
                    )
                    .child(
                        div().flex_1().child(strings.sign_visible).child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme.text_muted)
                                .child(shown.unwrap_or_else(|| strings.sign_no_stamp.into())),
                        ),
                    ),
            )
            .child(field(strings.sign_reason, panel.reason.clone()))
            .child(field(strings.sign_location, panel.location.clone()));
        if pending {
            body = body.child(
                div()
                    .text_size(px(12.0))
                    .child(strings.sign_pending_changes),
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
        body = body.child(
            div()
                .flex()
                .flex_row()
                .justify_end()
                .gap_1()
                .child(
                    styled_button("sign-save", strings.sign_save, can_sign, false, &theme)
                        .when(can_sign, |b| b.bg(theme.button_active))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.sign_document(None, window, cx);
                        })),
                )
                .child(
                    styled_button("sign-close", strings.close, true, false, &theme).on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.toggle_sign_panel(window, cx);
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
                    .id("sign-panel")
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
}
