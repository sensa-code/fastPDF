//! Dev-script steps for links (`crate::links`). Points are given like
//! `stamp-place`: a one-based page and page-space points from its top-left
//! corner (unrotated, as in fixtures/README.md). The page is brought into
//! view first if the point is not visible (not a jump: Back does not
//! return from it). The steps drive the same handlers as the mouse and
//! wait while the page's links are read. `link-prompt` answers the card of
//! an external link like its buttons; scripts run dry (`LinkLayer::dry_run`):
//! Open logs what would open, Copy what would be copied.

use fastpdf_core::keymap::ReaderCommand;
use fastpdf_engine_api::{PageIndex, PageRect};
use gpui::{Context, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point, Window, point, px};

use super::StepState;
use crate::reader::ReaderView;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum LinkStep {
    /// `link-hover=P,X,Y`: the pointer moves there.
    Hover { page: u32, x: f32, y: f32 },
    /// `link-click=P,X,Y`: a click there; stops the script unless a link
    /// is there.
    Click { page: u32, x: f32, y: f32 },
    /// `link-drag=P,X0,Y0,X1,Y1`: a press there and a drag to the second
    /// point, on the same page (selects text, follows nothing);
    /// `link-drag=P,X0,Y0,X1,Y1,X2,Y2` drags on to a third point and is
    /// released there (back near the start: still a drag).
    Drag {
        page: u32,
        from: (f32, f32),
        to: (f32, f32),
        back: Option<(f32, f32)>,
    },
    /// `link-prompt=open|copy|cancel`: answers the card of an external
    /// link; stops the script unless it shows.
    Prompt(PromptAnswer),
}

/// A button of the link card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptAnswer {
    Open,
    Copy,
    Cancel,
}

impl LinkStep {
    /// The step written `name=value`; `None` for another step or a bad value.
    pub(super) fn parse(name: &str, value: Option<&str>) -> Option<Self> {
        if name == "link-prompt" {
            return Some(Self::Prompt(match value? {
                "open" => PromptAnswer::Open,
                "copy" => PromptAnswer::Copy,
                "cancel" => PromptAnswer::Cancel,
                _ => return None,
            }));
        }
        if !matches!(name, "link-hover" | "link-click" | "link-drag") {
            return None;
        }
        let mut parts = value?.split(',').map(str::trim);
        let page = parts.next()?.parse().ok().filter(|&p: &u32| p >= 1)?;
        let numbers: Vec<f32> = parts
            .map(|p| p.parse().ok().filter(|v: &f32| v.is_finite()))
            .collect::<Option<_>>()?;
        Some(match (name, numbers.as_slice()) {
            ("link-hover", &[x, y]) => Self::Hover { page, x, y },
            ("link-click", &[x, y]) => Self::Click { page, x, y },
            ("link-drag", &[x0, y0, x1, y1]) => Self::Drag {
                page,
                from: (x0, y0),
                to: (x1, y1),
                back: None,
            },
            ("link-drag", &[x0, y0, x1, y1, x2, y2]) => Self::Drag {
                page,
                from: (x0, y0),
                to: (x1, y1),
                back: Some((x2, y2)),
            },
            _ => return None,
        })
    }

    fn page(&self) -> Option<u32> {
        match *self {
            Self::Hover { page, .. } | Self::Click { page, .. } | Self::Drag { page, .. } => {
                Some(page)
            }
            Self::Prompt(_) => None,
        }
    }
}

impl ReaderView {
    pub(super) fn run_link_step(
        &mut self,
        step: &LinkStep,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> StepState {
        let Some(number) = step.page() else {
            return self.answer_link_step(step, window, cx);
        };
        let Some(page_count) = self.session().map(|s| s.page_count()) else {
            return StepState::Abort("no document".into());
        };
        if number > page_count {
            return StepState::Abort(format!("the document has {page_count} pages"));
        }
        let page = PageIndex::new(number - 1);
        let (x, y) = match *step {
            LinkStep::Hover { x, y, .. } | LinkStep::Click { x, y, .. } => (x, y),
            LinkStep::Drag { from, .. } => from,
            LinkStep::Prompt(_) => return StepState::Abort("not a point".into()),
        };
        let Some(position) = self.window_point(page, x, y) else {
            return StepState::Pending;
        };
        // Moving the pointer there reads the page's links.
        self.link_hover(position, cx);
        if !self.links_loaded_for(page) {
            return StepState::Pending;
        }
        let under = self
            .links
            .hover
            .as_ref()
            .filter(|h| h.page == page)
            .map(|h| (h.index, h.target.log_text()));
        let label = match &under {
            Some((index, target)) => format!("#{index} -> {target}"),
            None => "no link".to_owned(),
        };
        let at = format!("page {} ({x}, {y})", page.display_number());
        match *step {
            LinkStep::Hover { .. } => {
                log::info!("link hover: {at}: {label}");
                StepState::Done
            }
            LinkStep::Click { .. } => {
                if under.is_none() {
                    return StepState::Abort(format!("no link at {at}"));
                }
                log::info!("link click: {at}: {label}");
                self.dev_press(position, window, cx);
                self.on_selection_end(&release(position), window, cx);
                StepState::Done
            }
            LinkStep::Drag { to, back, .. } => {
                let mut points = vec![to];
                points.extend(back);
                let mut path = Vec::new();
                for (x, y) in points {
                    let Some(end) = self.window_point(page, x, y) else {
                        return StepState::Abort(format!("{at}: ({x}, {y}) is not shown"));
                    };
                    path.push(end);
                }
                log::info!("link drag: {at} via {:?}: starts on {label}", to);
                self.dev_press(position, window, cx);
                let mut end = position;
                for point in path {
                    // The pointer moves with the drag (no hover while dragging).
                    self.on_selection_drag(point, cx);
                    self.link_hover(point, cx);
                    end = point;
                }
                self.on_selection_end(&release(end), window, cx);
                StepState::Done
            }
            LinkStep::Prompt(_) => StepState::Abort("not a point".into()),
        }
    }

    /// `link-prompt=...`: the card's buttons.
    fn answer_link_step(
        &mut self,
        step: &LinkStep,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> StepState {
        let LinkStep::Prompt(answer) = *step else {
            return StepState::Abort("not a prompt answer".into());
        };
        if self.links.prompt.is_none() {
            return StepState::Abort("no link prompt".into());
        }
        log::info!("link prompt: {answer:?}");
        match answer {
            PromptAnswer::Open => self.answer_link_prompt(true, window, cx),
            PromptAnswer::Copy => self.copy_link_address(window, cx),
            // Like Esc or the Cancel button.
            PromptAnswer::Cancel => self.run_command(ReaderCommand::Cancel, window, cx),
        }
        StepState::Done
    }

    /// A left-button press at `position`, as the mouse makes it.
    fn dev_press(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let press = MouseDownEvent {
            button: MouseButton::Left,
            position,
            click_count: 1,
            ..MouseDownEvent::default()
        };
        self.on_document_mouse_down(&press, window, cx);
    }

    /// Where page-space point (x, y) of `page` is in the window; shows the
    /// page first when the point is not in view.
    fn window_point(&mut self, page: PageIndex, x: f32, y: f32) -> Option<Point<Pixels>> {
        let bounds = self.viewport_bounds?;
        let spot = PageRect::new(x, y, x, y);
        let session = self.session_mut()?;
        let (width, height) = (session.viewport().width, session.viewport().height);
        let shown = |r: [f32; 4]| r[0] >= 0.0 && r[1] >= 0.0 && r[0] < width && r[1] < height;
        if !session.page_to_view(page, spot).is_some_and(shown) {
            session.go_to_page(page);
        }
        let r = session.page_to_view(page, spot)?;
        Some(point(
            bounds.origin.x + px(r[0]),
            bounds.origin.y + px(r[1]),
        ))
    }
}

fn release(position: Point<Pixels>) -> MouseUpEvent {
    MouseUpEvent {
        button: MouseButton::Left,
        position,
        click_count: 1,
        ..MouseUpEvent::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devscript::{Step, parse};

    #[test]
    fn link_steps_parse() {
        assert_eq!(
            parse("link-hover=1,100,100; link-click= 3 , 72.5 , 700 ; link-drag=1,100,100,300,130")
                .unwrap(),
            vec![
                Step::Link(LinkStep::Hover {
                    page: 1,
                    x: 100.0,
                    y: 100.0
                }),
                Step::Link(LinkStep::Click {
                    page: 3,
                    x: 72.5,
                    y: 700.0
                }),
                Step::Link(LinkStep::Drag {
                    page: 1,
                    from: (100.0, 100.0),
                    to: (300.0, 130.0),
                    back: None,
                }),
            ]
        );
        assert_eq!(
            parse("link-drag=1,100,100,200,100,101,100").unwrap(),
            vec![Step::Link(LinkStep::Drag {
                page: 1,
                from: (100.0, 100.0),
                to: (200.0, 100.0),
                back: Some((101.0, 100.0)),
            })]
        );
        assert_eq!(
            parse("link-prompt=open; link-prompt=copy; link-prompt=cancel").unwrap(),
            vec![
                Step::Link(LinkStep::Prompt(PromptAnswer::Open)),
                Step::Link(LinkStep::Prompt(PromptAnswer::Copy)),
                Step::Link(LinkStep::Prompt(PromptAnswer::Cancel)),
            ]
        );
        for bad in [
            "link-prompt",
            "link-prompt=yes",
            "link-prompt=Open",
            "link-hover",
            "link-hover=1,100",
            "link-hover=0,1,1",
            "link-hover=1,x,1",
            "link-hover=1,1,NaN",
            "link-click=1,1,1,1",
            "link-drag=1,1,1",
            "link-drag=1,1,1,1,1,1",
            "link-press=1,1,1",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
