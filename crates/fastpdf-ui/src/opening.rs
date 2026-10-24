//! Opening and closing documents in the reader window: a document opens
//! off the UI thread (`crate::document`) and replaces the current one when
//! it is ready; the recent-files list follows (spec §8, §10).

use std::future::Future;
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use fastpdf_core::recent::RecentFiles;
use fastpdf_engine_api::EngineDocument;
use gpui::{Context, PathPromptOptions, SharedString, Window};

use crate::bench::BenchEvent;
use crate::document::{OpenFailure, Opened, open_document_blocking};
use crate::navigation::apply_default_zoom;
use crate::reader::{DocState, OpenDoc, ReaderView};
use crate::smooth_scroll::PageTurn;
use crate::toolbar;

/// Recent files listed in the empty window.
const RECENT_SHOWN: usize = 10;
pub(crate) const APP_TITLE: &str = "FastPDF";

impl ReaderView {
    /// Opens `path`, replacing the current document.
    pub fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<'_, Self>) {
        let path = std::path::absolute(&path).unwrap_or(path);
        let engine = Arc::clone(&self.options.engine);
        let open_path = path.clone();
        let task = cx.background_executor().spawn(async move {
            open_document_blocking(engine.as_ref(), &open_path).map(Opened::from)
        });
        self.begin_open(path, task, window, cx);
    }

    pub(crate) fn begin_open(
        &mut self,
        path: PathBuf,
        result: impl Future<Output = Result<Opened, OpenFailure>> + 'static,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.close_document(window, cx);
        self.doc = DocState::Loading { path: path.clone() };
        // Replacing the task drops (cancels) any open still in progress; its
        // background work finishes and the result is discarded.
        self.open_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = result.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.finish_open(path, result, window, cx);
            });
        }));
        cx.notify();
    }

    pub(crate) fn finish_open(
        &mut self,
        path: PathBuf,
        result: Result<Opened, OpenFailure>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        // A newer open replaced this one.
        if !matches!(&self.doc, DocState::Loading { path: p } if *p == path) {
            return;
        }
        let Opened {
            doc: opened,
            session,
        } = match result {
            Ok(opened) => opened,
            Err(OpenFailure::Locked(locked)) => {
                self.lock_document(locked, window, cx);
                return;
            }
            Err(failure) => {
                log::warn!("cannot open {}: {failure}", path.display());
                self.doc = DocState::Failed { failure, path };
                window.set_window_title(APP_TITLE);
                cx.notify();
                return;
            }
        };

        let session = match session {
            // Started while GPUI was starting; the first frame resizes it to
            // the real document area if the guess was off. Night mode or the
            // default zoom may have changed while the document was opening:
            // nothing of it has been shown yet, so apply the current ones.
            Some(mut session) => {
                session.set_color_mode(self.color_mode());
                session.set_layout_mode(self.settings.layout_mode());
                apply_default_zoom(&mut session, self.settings.default_zoom);
                session
            }
            None => {
                let (view_size, scale) = self.viewport_estimate(window);
                self.session_parts().session(&opened, view_size, scale)
            }
        };
        self.memory
            .manager()
            .register(session.tile_cache().budgeted());
        self.memory.watch(&opened.doc);

        let name: SharedString = display_name(&opened.path).into();
        window.set_window_title(&format!("{name} - {APP_TITLE}"));
        log::info!(
            "session for {} ({} pages, engine {})",
            opened.path.display(),
            opened.doc.page_count(),
            opened.doc.engine().name
        );
        self.remember_recent(opened.path.clone(), cx);
        self.doc = DocState::Open(Box::new(OpenDoc {
            session,
            path: opened.path,
            name,
            opened_at: Instant::now(),
            exact_reported: false,
            protected: opened.password_protected,
            queries: Default::default(),
        }));
        self.report(BenchEvent::DocumentOpened);
        // Panels that were open carry over to the new document.
        if self.sidebar.open {
            self.ensure_outline(cx);
            self.sync_thumbnails(window);
        }
        if self.find.open {
            let query = self.find.input.read(cx).text().to_string();
            self.on_find_query(query, cx);
        }
        cx.notify();
    }

    /// Closes the current document and frees its GPU textures, its cached
    /// text and (once a running print job has stopped) the engine document.
    pub(crate) fn close_document(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.end_go_to(false, window, cx);
        self.find.reset(cx);
        self.selection.reset();
        self.reset_links(window, cx);
        self.sidebar.reset_document();
        self.smooth_scroll.stop();
        self.page_turn = PageTurn::default();
        self.scrollbars.clear();
        self.stamps.reset_document();
        self.text_edits.reset_document();
        // A print job keeps the document alive until it stops.
        self.print.cancel_job();
        let previous = mem::replace(&mut self.doc, DocState::Empty);
        if let DocState::Open(open) = previous {
            let OpenDoc {
                mut session, path, ..
            } = *open;
            log::info!("closing {}", path.display());
            // The text cache is shared by all documents (spec §15).
            self.texts.remove_document(session.id());
            // Cancel rendering and evict every tile and thumbnail (they
            // reach the retire queue), then release every texture.
            session.close();
            self.textures.release_all(window);
            // Dropping the session joins its render workers, which may wait
            // for a page that is still rasterizing: keep that off the UI thread.
            // A text extraction that was running when the document closed
            // (search thread, selection) may still have added its page;
            // sweep once more after the workers are gone.
            let document = session.id();
            let texts = Arc::clone(&self.texts);
            cx.background_executor()
                .spawn(async move {
                    drop(session);
                    texts.remove_document(document);
                })
                .detach();
            window.set_window_title(APP_TITLE);
            self.load_recent(cx);
        } else if let DocState::Locked(_) = previous {
            // The prompt and its field (wiped on drop) are gone: back to the
            // empty window.
            window.focus(&self.focus, cx);
            window.set_window_title(APP_TITLE);
            self.load_recent(cx);
        }
        cx.notify();
    }

    fn remember_recent(&mut self, path: PathBuf, cx: &mut Context<'_, Self>) {
        let Some(storage) = self.options.recent_files.clone() else {
            return;
        };
        self.recent.retain(|p| !same_file(p, &path));
        self.recent.insert(0, path.clone());
        self.recent.truncate(RECENT_SHOWN);
        cx.background_executor()
            .spawn(async move {
                let mut recent = RecentFiles::load(storage, RecentFiles::DEFAULT_MAX);
                recent.add(&path);
                if let Err(e) = recent.save() {
                    log::warn!("cannot save the recent files list: {e}");
                }
            })
            .detach();
    }

    /// Reads the recent-files list in the background (empty window).
    pub(crate) fn load_recent(&mut self, cx: &mut Context<'_, Self>) {
        let Some(storage) = self.options.recent_files.clone() else {
            return;
        };
        let read = cx.background_executor().spawn(async move {
            RecentFiles::load(storage, RecentFiles::DEFAULT_MAX)
                .entries()
                .iter()
                .take(RECENT_SHOWN)
                .cloned()
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let recent = read.await;
            let _ = this.update(cx, |this, cx| {
                this.recent = recent;
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn prompt_open(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| this.open_path(path, window, cx));
        })
        .detach();
    }

    /// Size and scale for a new session before its first layout.
    fn viewport_estimate(&self, window: &Window) -> ((f32, f32), f32) {
        let scale = window.scale_factor();
        match self.viewport_bounds {
            Some(b) => ((f32::from(b.size.width), f32::from(b.size.height)), scale),
            None => {
                let s = window.viewport_size();
                (
                    (
                        f32::from(s.width),
                        (f32::from(s.height) - toolbar::HEIGHT).max(1.0),
                    ),
                    scale,
                )
            }
        }
    }
}

pub(crate) fn is_pdf(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// Windows paths are case-insensitive.
fn same_file(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

pub(crate) fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}
