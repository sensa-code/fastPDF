//! The find options: match case (Alt+C) and whole words (Alt+W). They are
//! settings (`find_match_case`, `find_whole_word`), shown as two small
//! toggles inside the find field; changing one searches again. The
//! shortcuts act only while the find bar is open, so they never flip a
//! setting out of sight.

use fastpdf_core::keymap::{ReaderCommand, keystrokes_for};
use fastpdf_search::SearchQuery;
use gpui::{
    ClickEvent, Context, Div, InteractiveElement, ParentElement, Stateful,
    StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px,
};

use crate::reader::ReaderView;
use crate::toolbar::tooltip;

/// Side of an option toggle, inside the 28 px field.
const TOGGLE_SIZE: f32 = 22.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FindOption {
    MatchCase,
    WholeWord,
}

impl FindOption {
    fn command(self) -> ReaderCommand {
        match self {
            Self::MatchCase => ReaderCommand::ToggleMatchCase,
            Self::WholeWord => ReaderCommand::ToggleWholeWord,
        }
    }
}

/// How the find bar searches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct FindOptions {
    pub match_case: bool,
    pub whole_word: bool,
}

impl FindOptions {
    /// The search for `text` with these options.
    pub(crate) fn query(self, text: String) -> SearchQuery {
        SearchQuery {
            case_sensitive: self.match_case,
            whole_word: self.whole_word,
            ..SearchQuery::new(text)
        }
    }

    pub(crate) fn is_on(self, option: FindOption) -> bool {
        match option {
            FindOption::MatchCase => self.match_case,
            FindOption::WholeWord => self.whole_word,
        }
    }
}

impl ReaderView {
    /// The find options, as the settings keep them.
    pub(crate) fn find_options(&self) -> FindOptions {
        FindOptions {
            match_case: self.settings.find_match_case,
            whole_word: self.settings.find_whole_word,
        }
    }

    /// Alt+C / Alt+W and the toggles: flips `option` while the find bar is
    /// open; otherwise the keystroke does nothing.
    pub(crate) fn toggle_find_option(&mut self, option: FindOption, cx: &mut Context<'_, Self>) {
        if self.find.open {
            let on = !self.find_options().is_on(option);
            self.set_find_option(option, on, cx);
        }
    }

    /// Sets `option`, saves it and, while the find bar is open, searches
    /// again with it.
    pub(crate) fn set_find_option(
        &mut self,
        option: FindOption,
        on: bool,
        cx: &mut Context<'_, Self>,
    ) {
        if self.find_options().is_on(option) == on {
            return;
        }
        match option {
            FindOption::MatchCase => self.settings.find_match_case = on,
            FindOption::WholeWord => self.settings.find_whole_word = on,
        }
        self.save_settings(cx);
        log::info!("find option {option:?}: {}", if on { "on" } else { "off" });
        if self.find.open {
            let text = self.find.input.read(cx).text().to_string();
            self.on_find_query(text, cx);
        }
        cx.notify();
    }

    /// The two toggles shown inside the find field: "Aa" (match case) and
    /// an underlined "ab" (whole word).
    pub(crate) fn render_find_toggles(&self, cx: &mut Context<'_, Self>) -> Div {
        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(2.0))
            .child(self.option_toggle(FindOption::MatchCase, cx))
            .child(self.option_toggle(FindOption::WholeWord, cx))
    }

    fn option_toggle(&self, option: FindOption, cx: &mut Context<'_, Self>) -> Stateful<Div> {
        let theme = self.theme;
        let strings = self.strings();
        let on = self.find_options().is_on(option);
        let (id, label, tip) = match option {
            FindOption::MatchCase => ("find-case", "Aa", strings.tip_match_case()),
            FindOption::WholeWord => ("find-word", "ab", strings.tip_whole_word()),
        };
        let tip = strings.with_shortcut(tip, keystrokes_for(option.command()).next());
        let hover = theme.button_hover;
        div()
            .id(id)
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .size(px(TOGGLE_SIZE))
            .rounded(px(3.0))
            .border_1()
            .text_size(px(12.0))
            .cursor_pointer()
            .map(|toggle| {
                if on {
                    toggle
                        .bg(theme.button_active)
                        .border_color(theme.accent)
                        .text_color(theme.text)
                } else {
                    toggle
                        .border_color(gpui::transparent_black())
                        .text_color(theme.text_muted)
                }
            })
            .when(option == FindOption::WholeWord, |toggle| toggle.underline())
            .hover(move |style| style.bg(hover))
            .tooltip(tooltip(tip.into(), theme, self.language().ui_font()))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.toggle_find_option(option, cx);
                // The press focused the reader; typing goes on in the field.
                let input = this.find.input.clone();
                input.update(cx, |input, cx| window.focus(input.focus_handle(), cx));
            }))
            .child(label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_shape_the_query() {
        let plain = FindOptions::default().query("pdf".into());
        assert_eq!(plain, SearchQuery::new("pdf"));
        let both = FindOptions {
            match_case: true,
            whole_word: true,
        };
        let query = both.query("PDF".into());
        assert!(query.case_sensitive && query.whole_word);
        assert_eq!(query.max_hits, SearchQuery::DEFAULT_MAX_HITS);
        assert!(both.is_on(FindOption::MatchCase) && both.is_on(FindOption::WholeWord));
        assert!(!FindOptions::default().is_on(FindOption::WholeWord));
    }

    #[test]
    fn options_have_their_shortcuts() {
        assert_eq!(
            keystrokes_for(FindOption::MatchCase.command()).next(),
            Some("alt-c")
        );
        assert_eq!(
            keystrokes_for(FindOption::WholeWord.command()).next(),
            Some("alt-w")
        );
    }
}
