//! Opening an external link (ADR 0015): a click on an http, https or
//! mailto link that passed `fastpdf_shell::check_external_uri` asks first,
//! every time, on a prompt card that shows the site and the full address
//! (`crate::link_prompt` draws it). Only a single click on its Open button
//! hands the address to Windows (`fastpdf_shell::open_external`, off the UI
//! thread); Enter does nothing, Esc and Cancel close it, Copy address puts
//! the address on the clipboard instead. FastPDF itself never connects:
//! the browser or mail app does, after the user said so. Development
//! scripts never launch anything (`dry_run`): they log what would open.

use fastpdf_shell::{ExternalUri, OpenError};
use gpui::{Bounds, ClipboardItem, Context, FocusHandle, Pixels, Point, Subscription, Window};

use crate::links::HoverTarget;
use crate::prompt::CardPlacement;
use crate::reader::ReaderView;

/// The question whether to open `uri`.
pub(crate) struct LinkPrompt {
    pub uri: ExternalUri,
    /// The card's own keyboard focus (it has no text field).
    pub focus: FocusHandle,
    pub placement: CardPlacement,
    /// Hands the keyboard back to the card while it shows.
    _refocus: Subscription,
}

/// A link that did not open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LinkNotice {
    OpenFailed(OpenError),
}

/// The card goes to the half of the document area the click was not in.
pub(crate) fn placement_away_from(
    click: Point<Pixels>,
    area: Option<Bounds<Pixels>>,
) -> CardPlacement {
    match area {
        Some(area) if click.y < area.center().y => CardPlacement::Bottom,
        Some(_) => CardPlacement::Top,
        None => CardPlacement::Center,
    }
}

impl ReaderView {
    /// A click on an external link: ask whether to open it.
    pub(crate) fn open_link_prompt(
        &mut self,
        uri: ExternalUri,
        click: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        log::info!(
            "link prompt: {}",
            HoverTarget::External(uri.clone()).log_text()
        );
        let focus = cx.focus_handle();
        // GPUI gives the keyboard to the reader on a press outside the card
        // (the toolbar): hand it back, or Esc would go nowhere.
        let refocus = cx.on_focus(&self.focus, window, |view: &mut Self, window, cx| {
            view.refocus_prompt(window, cx);
        });
        window.focus(&focus, cx);
        self.menus.open = None;
        self.links.hover = None;
        self.links.prompt = Some(LinkPrompt {
            uri,
            focus,
            placement: placement_away_from(click, self.viewport_bounds),
            _refocus: refocus,
        });
        cx.notify();
    }

    /// Open (`true`) or Cancel; the card closes either way.
    pub(crate) fn answer_link_prompt(
        &mut self,
        open: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(prompt) = self.links.prompt.take() else {
            return;
        };
        window.focus(&self.focus, cx);
        cx.notify();
        if !open {
            return;
        }
        let uri = prompt.uri;
        if self.links.dry_run {
            log::info!("dev script: would open {}", uri.as_str());
            return;
        }
        log::info!(
            "link opened: {}",
            HoverTarget::External(uri.clone()).log_text()
        );
        log::debug!("opening {}", uri.as_str());
        let work = cx
            .background_executor()
            .spawn(async move { fastpdf_shell::open_external(&uri) });
        cx.spawn(async move |this, cx| match work.await {
            Ok(()) => {}
            // The user cancelled in a dialog of the shell: nothing to say.
            Err(OpenError::Cancelled) => log::info!("opening the link was cancelled"),
            Err(e) => {
                log::warn!("the link did not open: {e}");
                let _ = this.update(cx, |this, cx| {
                    this.links.notice = Some(LinkNotice::OpenFailed(e));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Copy address: the address goes to the clipboard and the card closes.
    pub(crate) fn copy_link_address(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(prompt) = self.links.prompt.take() else {
            return;
        };
        window.focus(&self.focus, cx);
        if self.links.dry_run {
            // The clipboard belongs to the person at the machine.
            log::info!("dev script: would copy {}", prompt.uri.as_str());
        } else {
            log::info!("link address copied");
            cx.write_to_clipboard(ClipboardItem::new_string(prompt.uri.as_str().to_owned()));
        }
        cx.notify();
    }

    pub(crate) fn refocus_link_prompt(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(prompt) = &self.links.prompt {
            window.focus(&prompt.focus, cx);
        }
    }

    pub(crate) fn link_prompt_focused(&self, window: &Window) -> bool {
        self.links
            .prompt
            .as_ref()
            .is_some_and(|p| p.focus.is_focused(window))
    }
}

#[cfg(test)]
mod tests {
    use fastpdf_core::links::{LinkAction, NotFollowed};
    use gpui::{point, px, size};

    use super::*;
    use crate::links::LinkBlock;

    #[test]
    fn only_allowed_addresses_ask_to_open() {
        let of = |uri: &str| HoverTarget::of(&LinkAction::Uri(uri.into()));
        assert!(matches!(
            of("https://example.invalid/a?b#c"),
            HoverTarget::External(_)
        ));
        assert!(matches!(
            of("mailto:a@example.invalid"),
            HoverTarget::External(_)
        ));
        for refused in [
            "file:///C:/Windows/System32/calc.exe",
            "javascript:alert(1)",
            "www.example.invalid",
            "https://user:pw@example.invalid/",
        ] {
            assert!(
                matches!(of(refused), HoverTarget::Blocked(LinkBlock::Uri(_))),
                "{refused}"
            );
        }
        assert_eq!(
            HoverTarget::of(&LinkAction::NotFollowed(NotFollowed::Action)),
            HoverTarget::Blocked(LinkBlock::Action)
        );
    }

    #[test]
    fn the_card_avoids_the_clicked_half() {
        let area = Some(Bounds::new(
            point(px(0.0), px(40.0)),
            size(px(800.0), px(600.0)),
        ));
        assert_eq!(
            placement_away_from(point(px(100.0), px(100.0)), area),
            CardPlacement::Bottom
        );
        assert_eq!(
            placement_away_from(point(px(100.0), px(500.0)), area),
            CardPlacement::Top
        );
        assert_eq!(
            placement_away_from(point(px(100.0), px(100.0)), None),
            CardPlacement::Center
        );
    }
}
