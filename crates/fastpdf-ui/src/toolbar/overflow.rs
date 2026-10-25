//! What the toolbar does when the window is too narrow for it: the
//! controls that do not fit move into a "⋯" menu, the least important
//! first ([`HIDE_TIERS`]), and come back when there is room again.
//!
//! The toolbar is one row of [`Slot`]s. Every frame it is drawn, the row
//! reports the width each slot was laid out at (`measure`); `plan` then
//! picks what to hide in the next frame so the predicted row, separators and
//! gaps included, fits the window. A hidden control keeps the width it was
//! last measured at (forgotten when the language changes); a control never
//! measured counts as narrow, shows, and is measured. The page indicator
//! counts at its narrowest, so a long page label wraps before any button
//! moves into the menu. All of this happens while a frame is drawn, so an
//! idle window does nothing.
//!
//! The menu (`controls::menu_rows`) lists the hidden controls in toolbar
//! order with their label, shortcut and state, and runs the same commands
//! through `run_command`.

use std::fmt;

use fastpdf_core::keymap::ReaderCommand;
use gpui::{App, Bounds, Entity, Pixels, Window};

use super::{BAR_PADDING, GAP};
use crate::i18n::Language;
use crate::reader::{Menu, ReaderView};

/// A control, or controls that go together, that may move into the menu.
/// In toolbar order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Group {
    Sidebar,
    Open,
    /// Back and forward.
    History,
    PrevPage,
    NextPage,
    /// Zoom out, the zoom, zoom in.
    Zoom,
    FitWidth,
    FitPage,
    Layout,
    /// Rotate left and right.
    Rotate,
    Find,
    Night,
    Edit,
    Properties,
}

impl Group {
    pub(crate) const ALL: [Self; 14] = [
        Self::Sidebar,
        Self::Open,
        Self::History,
        Self::PrevPage,
        Self::NextPage,
        Self::Zoom,
        Self::FitWidth,
        Self::FitPage,
        Self::Layout,
        Self::Rotate,
        Self::Find,
        Self::Night,
        Self::Edit,
        Self::Properties,
    ];

    fn bit(self) -> u16 {
        1 << self as u16
    }

    /// The name logs and development scripts use.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Sidebar => "sidebar",
            Self::Open => "open",
            Self::History => "history",
            Self::PrevPage => "prev",
            Self::NextPage => "next",
            Self::Zoom => "zoom",
            Self::FitWidth => "fit-width",
            Self::FitPage => "fit-page",
            Self::Layout => "layout",
            Self::Rotate => "rotate",
            Self::Find => "find",
            Self::Night => "night",
            Self::Edit => "edit",
            Self::Properties => "properties",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|g| g.name() == name)
    }

    /// The commands its menu rows run.
    pub(crate) fn commands(self) -> &'static [ReaderCommand] {
        use ReaderCommand as C;
        match self {
            Self::Sidebar => &[C::ToggleSidebar],
            Self::Open => &[C::OpenFile],
            Self::History => &[C::NavigateBack, C::NavigateForward],
            Self::PrevPage => &[C::PreviousPage],
            Self::NextPage => &[C::NextPage],
            Self::Zoom => &[C::ZoomOut, C::ZoomIn],
            Self::FitWidth => &[C::FitWidth],
            Self::FitPage => &[C::FitPage],
            Self::Layout => &[
                C::LayoutSinglePage,
                C::LayoutContinuous,
                C::LayoutTwoPage,
                C::LayoutTwoPageContinuous,
                C::ToggleCoverPage,
            ],
            Self::Rotate => &[C::RotateCounterClockwise, C::RotateClockwise],
            Self::Find => &[C::Find],
            Self::Night => &[C::ToggleNightMode],
            Self::Edit => &[
                C::SaveCopy,
                C::PageTools,
                C::StampTool,
                C::EditText,
                C::Sign,
            ],
            Self::Properties => &[C::DocumentProperties],
        }
    }
}

/// The order controls move into the menu: the rotation buttons first, the
/// Open button last. A tier goes at once. The page indicator, the menu
/// button and the settings button always stay.
const HIDE_TIERS: [&[Group]; 13] = [
    &[Group::Rotate],
    &[Group::Night],
    &[Group::Layout],
    &[Group::FitPage],
    &[Group::FitWidth],
    &[Group::Sidebar],
    &[Group::PrevPage, Group::NextPage],
    &[Group::Properties],
    &[Group::Edit],
    &[Group::Zoom],
    &[Group::History],
    &[Group::Find],
    &[Group::Open],
];

/// The groups in the menu instead of the toolbar.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Hidden(u16);

impl Hidden {
    pub(crate) fn hides(self, group: Group) -> bool {
        self.0 & group.bit() != 0
    }

    pub(crate) fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// `self` with `group` hidden too.
    pub(crate) fn with(self, group: Group) -> Self {
        Self(self.0 | group.bit())
    }

    /// The groups of the first `tiers` hide tiers.
    fn tiers(tiers: usize) -> Self {
        let bits = HIDE_TIERS
            .iter()
            .take(tiers)
            .flat_map(|tier| tier.iter())
            .fold(0, |bits, group| bits | group.bit());
        Self(bits)
    }
}

impl fmt::Debug for Hidden {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(
                Group::ALL
                    .iter()
                    .filter(|g| self.hides(**g))
                    .map(|g| g.name()),
            )
            .finish()
    }
}

/// A child of the toolbar row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Slot {
    Group(Group),
    Separator,
    /// The page indicator, or the page field while it is open.
    Page,
    /// The document's name, which takes what room is left.
    Title,
    /// The "⋯" menu button.
    More,
    Settings,
}

/// The row's clusters; a separator goes between two that show something.
/// The title sits between the last two.
const CLUSTERS: [&[Slot]; 6] = [
    &[Slot::Group(Group::Sidebar), Slot::Group(Group::Open)],
    &[Slot::Group(Group::History)],
    &[
        Slot::Group(Group::PrevPage),
        Slot::Page,
        Slot::Group(Group::NextPage),
    ],
    &[
        Slot::Group(Group::Zoom),
        Slot::Group(Group::FitWidth),
        Slot::Group(Group::FitPage),
        Slot::Group(Group::Layout),
    ],
    &[Slot::Group(Group::Rotate)],
    &[
        Slot::Group(Group::Find),
        Slot::Group(Group::Night),
        Slot::Group(Group::Edit),
        Slot::Group(Group::Properties),
        Slot::More,
        Slot::Settings,
    ],
];
pub(super) const MAX_SLOTS: usize = 26;
/// Laid-out width of a separator.
const SEPARATOR_WIDTH: f32 = 1.0;
/// The menu button's width before it was ever shown (a glyph button).
const MORE_WIDTH: f32 = 28.0;

/// The row's children with `hidden` in the menu. While the page field is
/// open (`editing`) it takes the place of the page buttons.
pub(super) fn slots(hidden: Hidden, editing: bool) -> ([Slot; MAX_SLOTS], usize) {
    let shows = |slot: &Slot| match *slot {
        Slot::Group(Group::PrevPage | Group::NextPage) if editing => false,
        Slot::Group(group) => !hidden.hides(group),
        Slot::More => !hidden.is_empty(),
        Slot::Separator | Slot::Page | Slot::Title | Slot::Settings => true,
    };
    let mut out = [Slot::Title; MAX_SLOTS];
    let mut len = 0;
    let mut push = |slot| {
        out[len] = slot;
        len += 1;
    };
    let mut before = false;
    for (index, cluster) in CLUSTERS.iter().enumerate() {
        if index == CLUSTERS.len() - 1 {
            push(Slot::Title);
        }
        if !cluster.iter().any(shows) {
            continue;
        }
        if before {
            push(Slot::Separator);
        }
        for slot in cluster.iter().filter(|s| shows(s)) {
            push(*slot);
        }
        before = true;
    }
    (out, len)
}

/// Widths of the row's children (logical pixels), as last laid out.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Widths {
    groups: [Option<f32>; Group::ALL.len()],
    page: f32,
    more: Option<f32>,
    settings: f32,
}

impl Default for Widths {
    fn default() -> Self {
        Self {
            groups: [None; Group::ALL.len()],
            page: 0.0,
            more: None,
            settings: 0.0,
        }
    }
}

impl Widths {
    fn of(&self, slot: Slot) -> f32 {
        match slot {
            Slot::Group(group) => self.groups[group as usize].unwrap_or(0.0),
            Slot::Separator => SEPARATOR_WIDTH,
            Slot::Page => self.page,
            Slot::Title => 0.0,
            Slot::More => self.more.unwrap_or(MORE_WIDTH),
            Slot::Settings => self.settings,
        }
    }

    /// The row's width with `hidden` in the menu and the title empty.
    fn row(&self, hidden: Hidden, editing: bool) -> f32 {
        let (slots, len) = slots(hidden, editing);
        let content: f32 = slots[..len].iter().map(|s| self.of(*s)).sum();
        content + GAP * len.saturating_sub(1) as f32
    }
}

/// The fewest hide tiers with which the row fits in `available`.
fn plan(widths: &Widths, editing: bool, available: f32) -> Hidden {
    (0..=HIDE_TIERS.len())
        .map(Hidden::tiers)
        .find(|hidden| widths.row(*hidden, editing) <= available + 0.5)
        .unwrap_or(Hidden::tiers(HIDE_TIERS.len()))
}

/// The toolbar's fit to the window.
#[derive(Debug, Default)]
pub(crate) struct ToolbarFit {
    widths: Widths,
    hidden: Hidden,
    /// The language the widths were measured in.
    language: Option<Language>,
    /// The last frame drawn hid what its measurements plan.
    settled: bool,
}

impl ToolbarFit {
    /// The groups the next frame shows in the menu.
    pub(crate) fn hidden(&self) -> Hidden {
        self.hidden
    }

    /// Whether the last frame drawn hid what its widths ask for (no other
    /// frame is needed for the toolbar).
    pub(crate) fn settled(&self) -> bool {
        self.settled
    }

    /// Takes the widths a frame laid out (`bounds`, one per slot of
    /// `rendered`) and plans the next frame; true when that hides other
    /// groups. `page_min`: how narrow the page indicator may get.
    fn measured(
        &mut self,
        rendered: Hidden,
        editing: bool,
        bounds: &[Bounds<Pixels>],
        page_min: Option<f32>,
        available: f32,
        language: Language,
    ) -> bool {
        if self.language != Some(language) {
            self.widths = Widths::default();
            self.language = Some(language);
        }
        let (slots, len) = slots(rendered, editing);
        if bounds.len() != len {
            return false;
        }
        for (slot, bounds) in slots[..len].iter().zip(bounds) {
            let width = f32::from(bounds.size.width);
            match *slot {
                Slot::Group(group) => self.widths.groups[group as usize] = Some(width),
                Slot::Page => self.widths.page = page_min.map_or(width, |min| min.min(width)),
                Slot::More => self.widths.more = Some(width),
                Slot::Settings => self.widths.settings = width,
                Slot::Separator | Slot::Title => {}
            }
        }
        let next = plan(&self.widths, editing, available);
        self.settled = next == rendered;
        let changed = next != self.hidden;
        self.hidden = next;
        changed
    }
}

/// The row's `on_children_prepainted` listener: measures the frame drawn
/// with `rendered` hidden and, when the plan changes, asks for one more
/// frame (a notification while a frame is drawn would be dropped, so it is
/// sent when the next frame starts).
pub(super) fn measure(
    view: Entity<ReaderView>,
    rendered: Hidden,
    editing: bool,
    page_min: Option<f32>,
    language: Language,
) -> impl Fn(Vec<Bounds<Pixels>>, &mut Window, &mut App) + 'static {
    move |bounds, window, cx| {
        let available = f32::from(window.viewport_size().width) - 2.0 * BAR_PADDING;
        let changed = view.update(cx, |this, _| {
            let fit = &mut this.toolbar_fit;
            if !fit.measured(rendered, editing, &bounds, page_min, available, language) {
                return false;
            }
            let hidden = fit.hidden();
            log::debug!("toolbar: {hidden:?} in the overflow menu");
            // A menu whose button just went (or the overflow menu when
            // everything fits again) closes rather than come back later.
            let gone = match this.menus.open {
                Some(Menu::Overflow) => hidden.is_empty(),
                Some(Menu::Layout) => hidden.hides(Group::Layout),
                Some(Menu::Edit) => hidden.hides(Group::Edit),
                None => false,
            };
            if gone {
                this.menus.open = None;
            }
            true
        });
        if changed {
            let id = view.entity_id();
            window.on_next_frame(move |_, cx| cx.notify(id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Widths of the toolbar in English at 100% (measured in a GUI run).
    fn english() -> Widths {
        let mut widths = Widths::default();
        for (group, width) in [
            (Group::Sidebar, 62.0),
            (Group::Open, 46.0),
            (Group::History, 58.0),
            (Group::PrevPage, 28.0),
            (Group::NextPage, 28.0),
            (Group::Zoom, 100.0),
            (Group::FitWidth, 72.0),
            (Group::FitPage, 66.0),
            (Group::Layout, 62.0),
            (Group::Rotate, 58.0),
            (Group::Find, 44.0),
            (Group::Night, 50.0),
            (Group::Edit, 50.0),
            (Group::Properties, 28.0),
        ] {
            widths.groups[group as usize] = Some(width);
        }
        widths.page = 60.0;
        widths.more = Some(28.0);
        widths.settings = 28.0;
        widths
    }

    fn names(hidden: Hidden) -> String {
        format!("{hidden:?}")
    }

    #[test]
    fn slots_put_separators_between_clusters_that_show() {
        let (slots, len) = slots(Hidden::default(), false);
        let slots = &slots[..len];
        use Slot::*;
        assert_eq!(
            slots[..4],
            [
                Group(super::Group::Sidebar),
                Group(super::Group::Open),
                Separator,
                Group(super::Group::History)
            ]
        );
        assert!(!slots.contains(&More));
        assert_eq!(slots.iter().filter(|s| **s == Separator).count(), 5);
        assert_eq!(slots.last(), Some(&Settings));
        // Everything hidden: the page indicator, the title, ⋯ and settings.
        let (slots, len) = super::slots(Hidden::tiers(HIDE_TIERS.len()), false);
        assert_eq!(slots[..len], [Page, Title, Separator, More, Settings]);
        // The page field takes the page buttons' place.
        let (slots, len) = super::slots(Hidden::default(), true);
        assert!(!slots[..len].contains(&Group(super::Group::PrevPage)));
        assert!(slots[..len].contains(&Page));
    }

    #[test]
    fn the_least_important_controls_go_first() {
        let widths = english();
        let full = widths.row(Hidden::default(), false);
        assert_eq!(plan(&widths, false, full), Hidden::default());
        assert_eq!(plan(&widths, false, full + 500.0), Hidden::default());
        // A little short: the rotation buttons go (and ⋯ comes).
        assert_eq!(names(plan(&widths, false, full - 10.0)), r#"["rotate"]"#);
        // The menu button needs room too: hiding rotation (58 + 2 + 1 + 2
        // for its separator) but adding ⋯ (28 + 2) saves 33.
        assert_eq!(names(plan(&widths, false, full - 33.0)), r#"["rotate"]"#);
        assert_eq!(
            names(plan(&widths, false, full - 34.0)),
            r#"["rotate", "night"]"#
        );
        // Very narrow: only the page indicator, ⋯ and settings stay.
        let all = Hidden::tiers(HIDE_TIERS.len());
        assert_eq!(plan(&widths, false, 100.0), all);
        // Every plan fits, except the last resort.
        for available in (100..900).step_by(7) {
            let available = available as f32;
            let hidden = plan(&widths, false, available);
            assert!(hidden == all || widths.row(hidden, false) <= available + 0.5);
        }
    }

    #[test]
    fn plans_are_stable_once_measured() {
        // Frame after frame with the same widths, the plan does not change,
        // so a still window asks for no more frames.
        let widths = english();
        for available in [360.0, 661.0, 826.0, 1400.0] {
            let first = plan(&widths, false, available - 8.0);
            let mut fit = ToolbarFit {
                widths,
                hidden: first,
                language: Some(Language::English),
                settled: false,
            };
            let (slots, len) = slots(first, false);
            let bounds: Vec<Bounds<Pixels>> = slots[..len]
                .iter()
                .map(|s| Bounds {
                    origin: Default::default(),
                    size: gpui::size(gpui::px(widths.of(*s)), gpui::px(28.0)),
                })
                .collect();
            let changed = fit.measured(
                first,
                false,
                &bounds,
                Some(60.0),
                available - 8.0,
                Language::English,
            );
            assert!(!changed, "{available}: {first:?} -> {:?}", fit.hidden());
            assert!(fit.settled());
        }
    }

    #[test]
    fn unknown_widths_show_and_a_new_language_measures_again() {
        let mut fit = ToolbarFit::default();
        // Nothing measured yet: everything shows, to be measured.
        assert!(plan(&fit.widths, false, 300.0).is_empty());
        fit.widths = english();
        fit.language = Some(Language::English);
        let len = slots(Hidden::default(), false).1;
        let bounds = vec![Bounds::<Pixels>::default(); len];
        fit.measured(
            Hidden::default(),
            false,
            &bounds,
            None,
            300.0,
            Language::TraditionalChinese,
        );
        assert_eq!(fit.language, Some(Language::TraditionalChinese));
        // Zero-width slots: everything fits now.
        assert!(fit.hidden().is_empty());
        // Bounds that do not match the slots are ignored.
        assert!(!fit.measured(
            Hidden::default(),
            false,
            &bounds[1..],
            None,
            10.0,
            Language::TraditionalChinese
        ));
    }

    #[test]
    fn groups_have_names_and_commands() {
        for group in Group::ALL {
            assert_eq!(Group::from_name(group.name()), Some(group));
            assert!(!group.commands().is_empty());
        }
        assert_eq!(Group::from_name("nothing"), None);
        let mut seen = std::collections::HashSet::new();
        for tier in HIDE_TIERS {
            for group in tier {
                assert!(seen.insert(*group), "{group:?} twice");
            }
        }
        assert_eq!(seen.len(), Group::ALL.len());
    }
}
