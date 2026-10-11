//! Writing PDFs for the `--write-job` child process (ADR 0013).
//!
//! [`execute`] runs a [`WriteJob`]: it reads the sources, builds every
//! output in memory (a byte copy, or a rewrite of picked pages with their
//! stamps and text edits, `rewrite`), opens each result again with hayro to
//! check it (and interprets every edited page again, `edit::compare`),
//! writes it to a temporary file next to its destination and, once every
//! output is ready, moves them into place. A file an output replaces is set
//! aside first and only deleted once the job's success is established
//! (`execute` deletes them right away; the `--write-job` process calls
//! [`execute_keeping_backups`] and deletes them after writing its report, so
//! a writer ended in between leaves the reader able to restore them). When
//! anything fails, the outputs already moved are taken back and the
//! replaced files restored: nothing is left behind.
//!
//! The temporary and set-aside names carry this process's id
//! ([`temp_path`], [`backup_path`]), so the reader can clean up after a
//! writer it had to end.
//!
//! The first version refuses encrypted sources (ADR 0013 §4): their strings
//! and streams would have to be decrypted and written again, and rewriting
//! raw bytes would otherwise produce garbage.

mod content;
mod edit;
mod font;
mod image;
mod rewrite;
mod syntax;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fastpdf_engine_api::PageIndex;
use fastpdf_engine_api::write::{
    EditProblem, OutputContent, PagePick, WriteError, WriteJob, WriteReport, WrittenOutput,
    backup_path, discard_backups, temp_path,
};
use hayro::hayro_syntax::Pdf;

use edit::Mismatch;
use rewrite::{PageCheck, Source};

/// Runs `job`; see the module documentation.
pub fn execute(job: &WriteJob) -> Result<WriteReport, WriteError> {
    let report = execute_keeping_backups(job)?;
    discard_backups(job, std::process::id());
    Ok(report)
}

/// Like [`execute`], but the files that outputs replaced stay set aside
/// (`backup_path` with this process's id) until the caller discards them.
pub fn execute_keeping_backups(job: &WriteJob) -> Result<WriteReport, WriteError> {
    job.check()?;
    if let Some(folder) = job.outputs.iter().find(|o| o.path.is_dir()) {
        return Err(WriteError::Output {
            path: folder.path.clone(),
            message: "is a folder".into(),
        });
    }
    let sources = job
        .sources
        .iter()
        .enumerate()
        .map(|(index, path)| load(index, path))
        .collect::<Result<Vec<_>, _>>()?;
    let mut staged: Vec<Staged> = Vec::with_capacity(job.outputs.len());
    let built = (|| {
        for output in &job.outputs {
            let (bytes, pages) = match &output.content {
                OutputContent::Copy { source } => {
                    let source = &sources[*source];
                    (Arc::clone(&source.bytes), source.page_count())
                }
                OutputContent::Pages(picks) => {
                    let (bytes, checks) = rewrite::pages(&sources, picks)?;
                    verify(&bytes, picks.len()).map_err(|message| WriteError::Verify {
                        path: output.path.clone(),
                        message,
                    })?;
                    verify_edits(&bytes, &checks, &output.path)?;
                    (Arc::new(bytes), picks.len())
                }
                OutputContent::Merge(list) => {
                    let picks: Vec<PagePick> = list
                        .iter()
                        .flat_map(|&s| {
                            (0..sources[s].page_count())
                                .map(move |p| PagePick::new(s, PageIndex::new(p as u32)))
                        })
                        .collect();
                    let (bytes, _) = rewrite::pages(&sources, &picks)?;
                    verify(&bytes, picks.len()).map_err(|message| WriteError::Verify {
                        path: output.path.clone(),
                        message,
                    })?;
                    (Arc::new(bytes), picks.len())
                }
            };
            let temp = temp_path(&output.path, std::process::id());
            std::fs::write(&temp, bytes.as_slice()).map_err(|e| WriteError::Output {
                path: output.path.clone(),
                message: e.to_string(),
            })?;
            staged.push(Staged {
                temp,
                path: output.path.clone(),
                pages: u32::try_from(pages).unwrap_or(u32::MAX),
                bytes: bytes.len() as u64,
            });
        }
        Ok(())
    })();
    if let Err(e) = built {
        discard(&staged);
        return Err(e);
    }
    commit(&staged)?;
    Ok(WriteReport {
        outputs: staged
            .iter()
            .map(|o| WrittenOutput {
                path: o.path.clone(),
                pages: o.pages,
                bytes: o.bytes,
            })
            .collect(),
    })
}

/// Moves every staged output into place, or none.
fn commit(staged: &[Staged]) -> Result<(), WriteError> {
    let pid = std::process::id();
    // Outputs in place, with the file each one replaced (set aside).
    let mut done: Vec<(&Staged, Option<PathBuf>)> = Vec::with_capacity(staged.len());
    for (i, output) in staged.iter().enumerate() {
        let failed = |e: std::io::Error, done: &[(&Staged, Option<PathBuf>)]| {
            undo(done);
            discard(&staged[i..]);
            WriteError::Output {
                path: output.path.clone(),
                message: e.to_string(),
            }
        };
        let backup = if output.path.exists() {
            let backup = backup_path(&output.path, pid);
            if let Err(e) = std::fs::rename(&output.path, &backup) {
                return Err(failed(e, &done));
            }
            Some(backup)
        } else {
            None
        };
        if let Err(e) = std::fs::rename(&output.temp, &output.path) {
            if let Some(backup) = &backup {
                let _ = std::fs::rename(backup, &output.path);
            }
            return Err(failed(e, &done));
        }
        done.push((output, backup));
    }
    Ok(())
}

/// Takes outputs back out of place and restores what they replaced.
fn undo(done: &[(&Staged, Option<PathBuf>)]) {
    for (output, backup) in done.iter().rev() {
        let _ = std::fs::remove_file(&output.path);
        if let Some(backup) = backup {
            let _ = std::fs::rename(backup, &output.path);
        }
    }
}

/// An output written to its temporary file.
struct Staged {
    temp: PathBuf,
    path: PathBuf,
    pages: u32,
    bytes: u64,
}

fn discard(staged: &[Staged]) {
    for output in staged {
        let _ = std::fs::remove_file(&output.temp);
    }
}

fn load(index: usize, path: &Path) -> Result<Source, WriteError> {
    let bytes = std::fs::read(path).map_err(|e| WriteError::Source {
        index,
        message: e.to_string(),
    })?;
    if is_encrypted(&bytes) {
        return Err(WriteError::Encrypted { index });
    }
    Source::open(bytes).map_err(|message| WriteError::Source { index, message })
}

/// Whether any name in the file is `/Encrypt`, `#xx` escapes decoded. The
/// key that makes a document encrypted lives in a trailer or in a
/// cross-reference stream's dictionary, and neither can sit inside a
/// compressed object stream, so it always appears among these bytes in
/// some spelling. A false positive (the name in some other place) only
/// refuses the job; a missed encryption would rewrite encrypted bytes as
/// plain ones.
fn is_encrypted(bytes: &[u8]) -> bool {
    let mut names = bytes.split(|&b| b == b'/').skip(1);
    names.any(|segment| {
        let end = segment
            .iter()
            .position(|&b| syntax::is_whitespace(b) || syntax::is_delimiter(b))
            .unwrap_or(segment.len());
        end <= 3 * b"Encrypt".len() && syntax::decode_name(&segment[..end]) == b"Encrypt"
    })
}

/// Opens a written document again: it must parse and have `pages` pages.
fn verify(bytes: &[u8], pages: usize) -> Result<(), String> {
    let pdf = Pdf::new(bytes.to_vec()).map_err(|e| format!("does not parse: {e:?}"))?;
    let found = pdf.pages().len();
    if found != pages {
        return Err(format!("{found} pages instead of {pages}"));
    }
    Ok(())
}

/// Interprets the edited pages of a written document again: each must draw
/// what its check expects. Glyphs that should have stayed but did not mean
/// the text cannot be edited reliably; anything else is a failed
/// verification.
fn verify_edits(bytes: &[u8], checks: &[PageCheck], path: &Path) -> Result<(), WriteError> {
    if checks.is_empty() {
        return Ok(());
    }
    let failed = |message: String| WriteError::Verify {
        path: path.to_path_buf(),
        message,
    };
    let pdf = Pdf::new(bytes.to_vec()).map_err(|e| failed(format!("does not parse: {e:?}")))?;
    let pages = pdf.pages();
    for check in checks {
        if check.page >= pages.len() {
            return Err(failed(format!("page {} is missing", check.page + 1)));
        }
        let analysis = content::analyze(&pages[check.page])
            .ok_or_else(|| failed(format!("page {} cannot be read", check.page + 1)))?;
        match edit::compare(&analysis.glyphs, &check.expected) {
            Ok(()) => {}
            Err(Mismatch::Kept(_)) => {
                return Err(WriteError::Edit {
                    source: check.source,
                    page: check.source_page,
                    problem: EditProblem::Unsupported,
                });
            }
            Err(Mismatch::New(message)) => {
                return Err(failed(format!("page {}: {message}", check.page + 1)));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_commit_restores_everything() {
        let dir = std::env::temp_dir().join(format!("fastpdf-commit-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pid = std::process::id();
        // Output 1 replaces an existing file; output 2's staged file is
        // missing, so moving it into place fails.
        let first = dir.join("first.pdf");
        std::fs::write(&first, b"the user's file").unwrap();
        std::fs::write(temp_path(&first, pid), b"new").unwrap();
        let second = dir.join("second.pdf");
        let staged = [
            Staged {
                temp: temp_path(&first, pid),
                path: first.clone(),
                pages: 1,
                bytes: 3,
            },
            Staged {
                temp: temp_path(&second, pid),
                path: second.clone(),
                pages: 1,
                bytes: 3,
            },
        ];
        assert!(matches!(commit(&staged), Err(WriteError::Output { .. })));
        assert_eq!(std::fs::read(&first).unwrap(), b"the user's file");
        assert!(!second.exists());
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
        assert_eq!(left.len(), 1, "no staged or set-aside file is left");
        // A good commit replaces the file and keeps the user's set aside
        // until the success is established.
        std::fs::write(temp_path(&first, pid), b"new").unwrap();
        commit(&staged[..1]).unwrap();
        assert_eq!(std::fs::read(&first).unwrap(), b"new");
        assert_eq!(
            std::fs::read(backup_path(&first, pid)).unwrap(),
            b"the user's file"
        );
        let job = WriteJob {
            sources: Vec::new(),
            outputs: vec![fastpdf_engine_api::write::OutputSpec {
                path: first.clone(),
                content: OutputContent::Copy { source: 0 },
            }],
        };
        discard_backups(&job, pid);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn encryption_is_found_anywhere() {
        assert!(is_encrypted(b"trailer << /Encrypt 5 0 R >>"));
        assert!(is_encrypted(b"<</Encrypt<</Filter/Standard>>>>"));
        assert!(is_encrypted(b"<</Encr#79pt 5 0 R>>"), "an escaped key");
        assert!(is_encrypted(b"<</#45#6E#63#72#79#70#74 5 0 R>>"));
        assert!(!is_encrypted(b"/EncryptMetadata false /Encrypted"));
        assert!(!is_encrypted(b"no slash Encrypt here"));
    }
}
