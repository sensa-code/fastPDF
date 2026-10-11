//! Seals on pages (ADR 0013, owner request 2026-10-11): the stamp panel,
//! placing, moving and resizing stamps over the pages, and saving them
//! drawn into a new file through the writer (`crate::tools`).
//!
//! Stamp images are files the user picks (a PNG with a transparent
//! background, or a JPEG); the list is remembered next to the settings
//! (`stamps.txt`). Placed stamps live in page space, so they follow zoom,
//! scrolling and layouts. Each stamp is turned so that it looks upright on
//! the page as it was shown when it was placed; the preview shows it the
//! way the saved file will. GPUI cannot turn images, so every stamp image
//! comes with its preview turned all four ways.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fastpdf_engine_api::{PageIndex, PageRect, PageSize, Rotation};
use gpui::{
    BorderStyle, Bounds, ClickEvent, Context, Corners, DispatchPhase, Entity, InteractiveElement,
    IntoElement, MouseMoveEvent, MouseUpEvent, ObjectFit, ParentElement, PathPromptOptions, Pixels,
    RenderImage, StatefulInteractiveElement, Styled, StyledImage, Window, div, fill, img, outline,
    point, prelude::FluentBuilder, px, size,
};
use image::{Frame, RgbaImage};

use crate::reader::{ReaderView, display_name};
use crate::toolbar::styled_button;

/// Stamp images remembered at most. With four previews of at most
/// `PREVIEW_SIDE` pixels each, the library holds at most about 19 MB, and
/// only once the stamp panel has been opened.
const MAX_LIBRARY: usize = 8;
/// Larger stamp files are refused before reading.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PIXELS: u64 = 64 * 1024 * 1024;
/// Longest side of a preview, in pixels.
const PREVIEW_SIDE: u32 = 384;
/// The resize handle at a selected stamp's bottom-right corner.
const HANDLE: f32 = 12.0;
pub(crate) const DEFAULT_WIDTH_MM: f32 = 30.0;
const MIN_WIDTH_MM: f32 = 5.0;
const MAX_WIDTH_MM: f32 = 200.0;
const POINTS_PER_MM: f32 = 72.0 / 25.4;
/// File next to the settings that lists the stamp images.
const LIBRARY_FILE: &str = "stamps.txt";
const PANEL_WIDTH: f32 = 420.0;

/// A stamp image, ready to show.
pub(crate) struct StampImage {
    pub path: PathBuf,
    /// The preview turned 0, 90, 180 and 270 degrees clockwise.
    pub previews: [Arc<RenderImage>; 4],
    /// Width / height of the image as it is.
    pub aspect: f32,
}

impl std::fmt::Debug for StampImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StampImage")
            .field("path", &self.path)
            .field("aspect", &self.aspect)
            .finish_non_exhaustive()
    }
}

/// A stamp on a page.
#[derive(Debug, Clone)]
pub(crate) struct PlacedStamp {
    pub page: PageIndex,
    /// Where the image goes, in page space (unrotated).
    pub rect: PageRect,
    pub image: Arc<StampImage>,
    /// How the image is turned in page space (clockwise).
    pub turn: Rotation,
}

/// A stamp being dragged.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum StampDrag {
    /// Moved; the pointer is `grab` view pixels from its top-left corner.
    Move { grab: (f32, f32) },
    /// Resized from its bottom-right corner (aspect kept).
    Resize,
}

/// The stamp panel and the stamps placed on the open document.
#[derive(Debug)]
pub(crate) struct StampPanel {
    pub open: bool,
    pub library: Vec<Arc<StampImage>>,
    /// The library file was read (it is read once, when the panel opens).
    loaded: bool,
    /// The library entry the next click on a page places.
    pub armed: Option<usize>,
    pub placed: Vec<PlacedStamp>,
    pub selected: Option<usize>,
    pub drag: Option<StampDrag>,
    /// Width of a newly placed stamp.
    pub width_mm: f32,
    pub problem: Option<String>,
    /// Images no longer shown, to release from the GPU atlas.
    retired: Vec<Arc<RenderImage>>,
}

impl Default for StampPanel {
    fn default() -> Self {
        Self {
            open: false,
            library: Vec::new(),
            loaded: false,
            armed: None,
            placed: Vec::new(),
            selected: None,
            drag: None,
            width_mm: DEFAULT_WIDTH_MM,
            problem: None,
            retired: Vec::new(),
        }
    }
}

impl StampPanel {
    /// The open document closed: its stamps go.
    pub(crate) fn reset_document(&mut self) {
        self.placed.clear();
        self.selected = None;
        self.drag = None;
    }

    /// Images to release (`Window::drop_image`), taken.
    pub(crate) fn take_retired(&mut self) -> Vec<Arc<RenderImage>> {
        std::mem::take(&mut self.retired)
    }
}

/// Reads a stamp image and makes its previews. Runs on a background thread.
pub(crate) fn load_stamp_image(path: &Path) -> Result<StampImage, String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > MAX_FILE_BYTES {
        return Err(format!("larger than {} MB", MAX_FILE_BYTES >> 20));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let reader = image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let (width, height) = reader.into_dimensions().map_err(|e| e.to_string())?;
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(format!("{width} x {height} pixels is not usable"));
    }
    let image = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
    // Only ever smaller: `thumbnail` would also enlarge a small image.
    let preview = if width > PREVIEW_SIDE || height > PREVIEW_SIDE {
        image.thumbnail(PREVIEW_SIDE, PREVIEW_SIDE).to_rgba8()
    } else {
        image.to_rgba8()
    };
    let turned = [
        preview.clone(),
        image::imageops::rotate90(&preview),
        image::imageops::rotate180(&preview),
        image::imageops::rotate270(&preview),
    ];
    Ok(StampImage {
        path: path.to_path_buf(),
        previews: turned.map(render_image),
        aspect: width as f32 / height as f32,
    })
}

/// GPUI wants BGRA with straight alpha.
fn render_image(mut rgba: RgbaImage) -> Arc<RenderImage> {
    for px in rgba.pixels_mut() {
        px.0.swap(0, 2);
    }
    Arc::new(RenderImage::new([Frame::new(rgba)]))
}

/// `stamps.txt` next to the settings file.
fn library_file(settings: Option<&Path>) -> Option<PathBuf> {
    Some(settings?.parent()?.join(LIBRARY_FILE))
}

fn read_library(file: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .take(MAX_LIBRARY)
        .collect()
}

fn write_library(file: &Path, paths: &[PathBuf]) {
    let text: String = paths
        .iter()
        .filter_map(|p| p.to_str())
        .map(|p| format!("{p}\n"))
        .collect();
    let mut temp = file.as_os_str().to_owned();
    temp.push(format!(".{}.tmp", std::process::id()));
    let temp = PathBuf::from(temp);
    let written = std::fs::write(&temp, text).and_then(|()| std::fs::rename(&temp, file));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&temp);
        log::warn!("cannot save the stamp list {}: {e}", file.display());
    }
}

/// The turn that makes an image look upright on a page shown turned by
/// `display`.
pub(crate) fn upright_turn(display: Rotation) -> Rotation {
    Rotation::from_degrees(-i64::from(display.degrees())).unwrap_or_default()
}

/// Which preview shows a stamp turned by `turn` on a page shown turned by
/// `display`.
pub(crate) fn preview_index(turn: Rotation, display: Rotation) -> usize {
    ((turn.degrees() + display.degrees()) / 90 % 4) as usize
}

/// `rect` moved (not resized) to lie on a page of `size` as far as it fits.
pub(crate) fn keep_on_page(rect: PageRect, size: PageSize) -> PageRect {
    let shift = |lo: f32, hi: f32, limit: f32| {
        if lo < 0.0 {
            -lo
        } else if hi > limit {
            (limit - hi).max(-lo)
        } else {
            0.0
        }
    };
    let dx = shift(rect.x0, rect.x1, size.width);
    let dy = shift(rect.y0, rect.y1, size.height);
    PageRect::new(rect.x0 + dx, rect.y0 + dy, rect.x1 + dx, rect.y1 + dy)
}

impl ReaderView {
    pub(crate) fn toggle_stamp_panel(&mut self, cx: &mut Context<'_, Self>) {
        self.stamps.open = !self.stamps.open;
        if self.stamps.open && self.text_edits.open {
            self.hide_text_edit_panel(cx);
        }
        if !self.stamps.open {
            self.stamps.armed = None;
            // Nothing shows the previews any more: free their GPU memory
            // (they are uploaded again when the panel opens).
            if self.stamps.placed.is_empty() {
                let shown: Vec<Arc<RenderImage>> = self
                    .stamps
                    .library
                    .iter()
                    .flat_map(|s| s.previews.iter().cloned())
                    .collect();
                self.stamps.retired.extend(shown);
            }
        }
        if self.stamps.open && !self.stamps.loaded {
            self.stamps.loaded = true;
            if let Some(file) = library_file(self.settings_path()) {
                let paths = read_library(&file);
                self.load_stamp_images(paths, false, cx);
            }
        }
        cx.notify();
    }

    /// Asks for stamp images to add.
    pub(crate) fn add_stamp_images(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let answer = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = answer.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| this.load_stamp_images(paths, true, cx));
        })
        .detach();
    }

    /// Decodes `paths` in the background and adds them to the library;
    /// `report` shows images that cannot be used (the remembered list
    /// skips them quietly).
    pub(crate) fn load_stamp_images(
        &mut self,
        paths: Vec<PathBuf>,
        report: bool,
        cx: &mut Context<'_, Self>,
    ) {
        let work = cx.background_executor().spawn(async move {
            paths
                .into_iter()
                .map(|p| {
                    let loaded = load_stamp_image(&p);
                    (p, loaded)
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let results = work.await;
            let _ = this.update(cx, |this, cx| {
                let strings = this.strings();
                for (path, result) in results {
                    match result {
                        Ok(image) => this.add_to_library(Arc::new(image)),
                        Err(e) => {
                            log::warn!("stamp image {}: {e}", path.display());
                            if report {
                                this.stamps.problem =
                                    Some(strings.write_image_failed(&display_name(&path), &e));
                            }
                        }
                    }
                }
                if report {
                    this.save_library();
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn add_to_library(&mut self, image: Arc<StampImage>) {
        let library = &mut self.stamps.library;
        if let Some(i) = library.iter().position(|s| s.path == image.path) {
            let old = library.remove(i);
            self.stamps.retired.extend(old.previews.iter().cloned());
        }
        library.insert(0, image);
        while library.len() > MAX_LIBRARY {
            if let Some(old) = library.pop() {
                self.stamps.retired.extend(old.previews.iter().cloned());
            }
        }
        self.stamps.armed = Some(0);
    }

    /// Development scripts: adds an image read in advance.
    pub(crate) fn dev_add_stamp(&mut self, image: StampImage) {
        self.add_to_library(Arc::new(image));
    }

    fn save_library(&self) {
        let Some(file) = library_file(self.settings_path()) else {
            return;
        };
        let paths: Vec<PathBuf> = self.stamps.library.iter().map(|s| s.path.clone()).collect();
        std::thread::spawn(move || write_library(&file, &paths));
    }

    pub(crate) fn remove_from_library(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        if index >= self.stamps.library.len() {
            return;
        }
        let old = self.stamps.library.remove(index);
        // Stamps already placed keep their image until they are removed.
        if !self
            .stamps
            .placed
            .iter()
            .any(|p| Arc::ptr_eq(&p.image, &old))
        {
            self.stamps.retired.extend(old.previews.iter().cloned());
        }
        self.stamps.armed = None;
        self.save_library();
        cx.notify();
    }

    /// Places library entry `index` centred on `(x, y)` (view pixels) on the
    /// page there. False when there is no page there.
    pub(crate) fn place_stamp(&mut self, index: usize, x: f32, y: f32) -> bool {
        let Some(image) = self.stamps.library.get(index).cloned() else {
            return false;
        };
        let width_pt = self.stamps.width_mm * POINTS_PER_MM;
        let Some(session) = self.session() else {
            return false;
        };
        let Some((page, _, _)) = session.view_to_page(x, y) else {
            return false;
        };
        let (Some(display), Some(page_size)) = (
            session.display_rotation(page),
            session.page_size_known(page),
        ) else {
            return false;
        };
        let ppp = session.viewport().px_per_point() as f32;
        // On screen the stamp stands upright: its width runs across.
        let (w, h) = (width_pt * ppp, width_pt / image.aspect * ppp);
        let Some(rect) = view_rect_on(session, page, [x - w / 2.0, y - h / 2.0, w, h]) else {
            return false;
        };
        self.stamps.placed.push(PlacedStamp {
            page,
            rect: keep_on_page(rect, page_size),
            image,
            turn: upright_turn(display),
        });
        self.stamps.selected = Some(self.stamps.placed.len() - 1);
        self.stamps.armed = None;
        true
    }

    /// A press on the document: places the armed stamp, or grabs a placed
    /// one (its body moves it, the corner handle resizes it). True when the
    /// press was for the stamps.
    pub(crate) fn stamp_mouse_down(&mut self, position: gpui::Point<Pixels>) -> bool {
        let Some(b) = self.viewport_bounds else {
            return false;
        };
        let (x, y) = (
            f32::from(position.x - b.origin.x),
            f32::from(position.y - b.origin.y),
        );
        // Topmost first: the last placed is drawn last.
        let hit = self.session().and_then(|session| {
            self.stamps
                .placed
                .iter()
                .enumerate()
                .rev()
                .find_map(|(i, stamp)| {
                    let r = session.page_to_view(stamp.page, stamp.rect)?;
                    let handle = self.stamps.selected == Some(i)
                        && x >= r[0] + r[2] - HANDLE
                        && x <= r[0] + r[2] + HANDLE / 2.0
                        && y >= r[1] + r[3] - HANDLE
                        && y <= r[1] + r[3] + HANDLE / 2.0;
                    let inside = x >= r[0] && x <= r[0] + r[2] && y >= r[1] && y <= r[1] + r[3];
                    if handle {
                        Some((i, StampDrag::Resize))
                    } else if inside {
                        Some((
                            i,
                            StampDrag::Move {
                                grab: (x - r[0], y - r[1]),
                            },
                        ))
                    } else {
                        None
                    }
                })
        });
        if let Some((i, drag)) = hit {
            self.stamps.selected = Some(i);
            self.stamps.drag = Some(drag);
            return true;
        }
        if let Some(index) = self.stamps.armed {
            return self.place_stamp(index, x, y);
        }
        if self.stamps.selected.take().is_some() {
            return self.stamps.open;
        }
        false
    }

    /// Drag update from the canvas' window-wide listener.
    pub(crate) fn on_stamp_drag(
        &mut self,
        position: gpui::Point<Pixels>,
        cx: &mut Context<'_, Self>,
    ) {
        let (Some(drag), Some(i), Some(b)) =
            (self.stamps.drag, self.stamps.selected, self.viewport_bounds)
        else {
            return;
        };
        let (x, y) = (
            f32::from(position.x - b.origin.x),
            f32::from(position.y - b.origin.y),
        );
        let Some(stamp) = self.stamps.placed.get(i) else {
            return;
        };
        let Some(session) = self.session() else {
            return;
        };
        let Some(view) = session.page_to_view(stamp.page, stamp.rect) else {
            return;
        };
        let display = session.display_rotation(stamp.page).unwrap_or_default();
        let quarter = preview_index(stamp.turn, display);
        // The aspect the stamp has on screen now.
        let aspect = if quarter % 2 == 1 {
            1.0 / stamp.image.aspect
        } else {
            stamp.image.aspect
        };
        let ppp = session.viewport().px_per_point() as f32;
        let target = match drag {
            StampDrag::Move { grab } => [x - grab.0, y - grab.1, view[2], view[3]],
            StampDrag::Resize => {
                let min = MIN_WIDTH_MM * POINTS_PER_MM * ppp;
                let max = MAX_WIDTH_MM * POINTS_PER_MM * ppp;
                let w = (x - view[0]).clamp(min, max);
                [view[0], view[1], w, w / aspect]
            }
        };
        let page_size = session.page_size_known(stamp.page);
        let Some(rect) = view_rect_on(session, stamp.page, target) else {
            return;
        };
        let rect = match page_size {
            Some(size) => keep_on_page(rect, size),
            None => rect,
        };
        if let Some(stamp) = self.stamps.placed.get_mut(i) {
            stamp.rect = rect;
        }
        cx.notify();
    }

    pub(crate) fn end_stamp_drag(&mut self, cx: &mut Context<'_, Self>) {
        if self.stamps.drag.take().is_some() {
            cx.notify();
        }
    }

    /// Delete: removes the selected stamp. True when there was one.
    pub(crate) fn remove_selected_stamp(&mut self) -> bool {
        let Some(i) = self.stamps.selected.take() else {
            return false;
        };
        if i < self.stamps.placed.len() {
            self.stamps.placed.remove(i);
            return true;
        }
        false
    }

    /// Paints the placed stamps over the pages (inside the document area's
    /// content mask) and the selected one's frame and handle.
    pub(crate) fn paint_stamps(&mut self, origin: gpui::Point<Pixels>, window: &mut Window) {
        for image in self.stamps.take_retired() {
            let _ = window.drop_image(image);
        }
        let Some(session) = self.session() else {
            return;
        };
        let accent = self.theme.accent;
        for (i, stamp) in self.stamps.placed.iter().enumerate() {
            let Some(r) = session.page_to_view(stamp.page, stamp.rect) else {
                continue;
            };
            let display = session.display_rotation(stamp.page).unwrap_or_default();
            let preview = &stamp.image.previews[preview_index(stamp.turn, display)];
            let bounds = Bounds::new(
                point(origin.x + px(r[0]), origin.y + px(r[1])),
                size(px(r[2]), px(r[3])),
            );
            if let Err(e) = window.paint_image(
                bounds,
                bounds,
                Corners::default(),
                Arc::clone(preview),
                0,
                false,
            ) {
                log::debug!("cannot paint a stamp: {e}");
            }
            if self.stamps.selected == Some(i) {
                window.paint_quad(outline(bounds, accent, BorderStyle::Dashed));
                let corner = point(
                    bounds.origin.x + bounds.size.width - px(HANDLE / 2.0),
                    bounds.origin.y + bounds.size.height - px(HANDLE / 2.0),
                );
                window.paint_quad(fill(
                    Bounds::new(corner, size(px(HANDLE), px(HANDLE))),
                    accent,
                ));
            }
        }
    }

    /// The stamp panel, floating over the top of the document.
    pub(crate) fn render_stamp_panel(
        &self,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement + use<> {
        let theme = self.theme;
        let strings = self.strings();
        let panel = &self.stamps;
        let has_doc = self.session().is_some();
        let mut library = div().flex().flex_row().flex_wrap().gap_2();
        for (i, image) in panel.library.iter().enumerate() {
            let armed = panel.armed == Some(i);
            library = library.child(
                div()
                    .id(("stamp-image", i))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .p_1()
                    .w(px(84.0))
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(if armed {
                        theme.accent
                    } else {
                        theme.toolbar_border
                    })
                    .when(armed, |d| d.bg(theme.button_active))
                    .cursor_pointer()
                    .hover(move |s| s.bg(theme.button_hover))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.stamps.armed = (this.stamps.armed != Some(i)).then_some(i);
                        cx.notify();
                    }))
                    .child(
                        img(Arc::clone(&image.previews[0]))
                            .w(px(64.0))
                            .h(px(64.0))
                            .object_fit(ObjectFit::Contain),
                    )
                    .child(
                        div()
                            .w_full()
                            .text_size(px(11.0))
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(display_name(&image.path)),
                    )
                    .child(
                        styled_button(format!("stamp-forget-{i}"), "\u{2715}", true, false, &theme)
                            .h(px(20.0))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                this.remove_from_library(i, cx);
                            })),
                    ),
            );
        }
        let mut body = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_size(px(15.0)).child(strings.stamps_title))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.text_muted)
                    .child(strings.stamp_hint),
            );
        if panel.library.is_empty() {
            body = body.child(
                div()
                    .text_color(theme.text_muted)
                    .child(strings.no_stamp_images),
            );
        } else {
            body = body.child(library);
        }
        body = body.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .child(
                    styled_button("stamp-add", strings.add_stamp_images, true, false, &theme)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.add_stamp_images(window, cx);
                        })),
                )
                .child(div().flex_1())
                .child(div().child(strings.stamp_width))
                .child(
                    styled_button("stamp-smaller", "\u{2212}", true, false, &theme).on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.stamps.width_mm = (this.stamps.width_mm - 5.0).max(MIN_WIDTH_MM);
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    div()
                        .min_w(px(48.0))
                        .flex()
                        .justify_center()
                        .child(format!("{:.0} mm", panel.width_mm)),
                )
                .child(
                    styled_button("stamp-larger", "+", true, false, &theme).on_click(cx.listener(
                        |this, _: &ClickEvent, _, cx| {
                            this.stamps.width_mm = (this.stamps.width_mm + 5.0).min(MAX_WIDTH_MM);
                            cx.notify();
                        },
                    )),
                ),
        );
        if !panel.placed.is_empty() {
            let rows = panel.placed.iter().enumerate().map(|(i, stamp)| {
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
                            .when(panel.selected == Some(i), |d| d.text_color(theme.accent))
                            .child(strings.stamp_on_page(
                                stamp.page.display_number(),
                                &display_name(&stamp.image.path),
                            )),
                    )
                    .child(
                        styled_button(format!("stamp-remove-{i}"), "\u{2715}", true, false, &theme)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                if i < this.stamps.placed.len() {
                                    this.stamps.placed.remove(i);
                                    this.stamps.selected = None;
                                    cx.notify();
                                }
                            })),
                    )
            });
            body = body.child(
                div()
                    .id("placed-stamps")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .max_h(px(140.0))
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
        let pending = !panel.placed.is_empty() || !self.text_edits.edits.is_empty();
        let can_save = has_doc && pending && !self.tools.writing();
        body = body.child(
            div()
                .flex()
                .flex_row()
                .justify_end()
                .gap_1()
                .child(
                    styled_button("stamp-save", strings.save_as, can_save, false, &theme)
                        .when(can_save, |b| b.bg(theme.button_active))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.save_changes(None, window, cx);
                        })),
                )
                .child(
                    styled_button(
                        "stamp-clear",
                        strings.clear_stamps,
                        !panel.placed.is_empty(),
                        false,
                        &theme,
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.stamps.reset_document();
                        cx.notify();
                    })),
                )
                .child(
                    styled_button("stamp-close", strings.close, true, false, &theme).on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.toggle_stamp_panel(cx);
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
                    .id("stamp-panel")
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

/// A view rectangle `[x, y, w, h]` (logical pixels) in the space of `page`.
fn view_rect_on(
    session: &fastpdf_core::DocumentSession<crate::textures::TileImage>,
    page: PageIndex,
    r: [f32; 4],
) -> Option<PageRect> {
    let (ax, ay) = session.view_to_page_on(page, r[0], r[1])?;
    let (bx, by) = session.view_to_page_on(page, r[0] + r[2], r[1] + r[3])?;
    Some(PageRect::new(ax, ay, bx, by))
}

/// While a stamp is dragged, follows the mouse everywhere in the window
/// until the button is released.
pub(crate) fn track_stamp_drag(view: Entity<ReaderView>, window: &mut Window) {
    let move_view = view.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, cx| {
        if phase == DispatchPhase::Bubble {
            move_view.update(cx, |this, cx| this.on_stamp_drag(event.position, cx));
        }
    });
    window.on_mouse_event(move |_: &MouseUpEvent, phase, _window, cx| {
        if phase == DispatchPhase::Bubble {
            view.update(cx, |this, cx| this.end_stamp_drag(cx));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_are_turned_to_look_upright() {
        assert_eq!(upright_turn(Rotation::R0), Rotation::R0);
        assert_eq!(upright_turn(Rotation::R90), Rotation::R270);
        assert_eq!(upright_turn(Rotation::R270), Rotation::R90);
        // Placed upright, shown upright: the unturned preview.
        for display in [Rotation::R0, Rotation::R90, Rotation::R180, Rotation::R270] {
            assert_eq!(preview_index(upright_turn(display), display), 0);
        }
        // Placed on an upright page, then the view turned right.
        assert_eq!(preview_index(Rotation::R0, Rotation::R90), 1);
    }

    #[test]
    fn stamps_stay_on_their_page() {
        let page = PageSize::new(600.0, 800.0);
        let r = keep_on_page(PageRect::new(-10.0, 790.0, 40.0, 840.0), page);
        assert_eq!((r.x0, r.y0, r.x1, r.y1), (0.0, 750.0, 50.0, 800.0));
        let inside = PageRect::new(10.0, 10.0, 20.0, 20.0);
        assert_eq!(keep_on_page(inside, page), inside);
        // Larger than the page: its top-left corner stays on it.
        let big = keep_on_page(PageRect::new(-5.0, 0.0, 700.0, 10.0), page);
        assert_eq!((big.x0, big.x1), (0.0, 705.0));
    }

    #[test]
    fn images_get_four_previews() {
        let dir = std::env::temp_dir().join(format!("fastpdf-stamps-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("seal.png");
        image::RgbaImage::from_pixel(40, 20, image::Rgba([200, 0, 0, 128]))
            .save(&path)
            .unwrap();
        let stamp = load_stamp_image(&path).unwrap();
        assert!((stamp.aspect - 2.0).abs() < 1e-6);
        let size = |i: usize| {
            let s = stamp.previews[i].size(0);
            (u32::from(s.width), u32::from(s.height))
        };
        assert_eq!([size(0), size(1)], [(40, 20), (20, 40)]);
        // BGRA for GPUI.
        let bgra = stamp.previews[0].as_bytes(0).unwrap();
        assert_eq!(&bgra[..4], &[0, 0, 200, 128]);
        assert!(load_stamp_image(&dir.join("missing.png")).is_err());
        std::fs::write(dir.join("junk.png"), b"no image").unwrap();
        assert!(load_stamp_image(&dir.join("junk.png")).is_err());

        // The remembered list.
        let file = dir.join(LIBRARY_FILE);
        write_library(&file, &[path.clone(), dir.join("b c.png")]);
        assert_eq!(read_library(&file), vec![path, dir.join("b c.png")]);
        assert_eq!(
            library_file(Some(&dir.join("settings.toml"))),
            Some(dir.join(LIBRARY_FILE))
        );
        assert_eq!(library_file(None), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
