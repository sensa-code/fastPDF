//! Writing PDFs (ADR 0013): engine-neutral descriptions of write jobs, their
//! results, and the text form both travel in between the reader and the
//! `--write-job` child process.
//!
//! A job reads `sources` and writes every output as a new file; the reader
//! never modifies a file in place. An output is a byte copy of a source, a
//! new document made of picked pages (from any source, in any order,
//! turned, with images stamped on them), or every page of some sources one
//! after the other (merging files whose page count the reader does not
//! know).
//!
//! The text form is one item per line, paths last so they may contain
//! spaces:
//!
//! ```text
//! fastpdf-write-job 1
//! source D:\in\a.pdf
//! output D:\out\a-pages.pdf
//! page 0 2 90
//! stamp 100 120 180 200 0 D:\seals\big.png
//! output D:\out\a-copy.pdf
//! copy 0
//! output D:\out\both.pdf
//! merge 0 1
//! ```

use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};

use crate::{PageIndex, PageRect, Rotation};

const JOB_HEADER: &str = "fastpdf-write-job 1";
const RESULT_HEADER: &str = "fastpdf-write-result 1";

/// Read `sources`, write each of `outputs`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WriteJob {
    pub sources: Vec<PathBuf>,
    pub outputs: Vec<OutputSpec>,
}

/// One file to write.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputSpec {
    pub path: PathBuf,
    pub content: OutputContent,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OutputContent {
    /// The bytes of `sources[source]`, unchanged.
    Copy { source: usize },
    /// A new document of these pages, in this order.
    Pages(Vec<PagePick>),
    /// A new document of every page of these sources, in this order.
    Merge(Vec<usize>),
}

/// A page of a new document.
#[derive(Debug, Clone, PartialEq)]
pub struct PagePick {
    /// Index into [`WriteJob::sources`].
    pub source: usize,
    pub page: PageIndex,
    /// Turned clockwise, on top of the page's own rotation.
    pub rotate: Rotation,
    /// Images drawn over the page's content (seals, signatures).
    pub stamps: Vec<Stamp>,
}

impl PagePick {
    pub fn new(source: usize, page: PageIndex) -> Self {
        Self {
            source,
            page,
            rotate: Rotation::R0,
            stamps: Vec::new(),
        }
    }
}

/// An image placed on a page.
#[derive(Debug, Clone, PartialEq)]
pub struct Stamp {
    /// A PNG or JPEG file.
    pub image: PathBuf,
    /// Where the image goes, in page space (points, CropBox top-left
    /// origin, before rotation); it is stretched to fill the rectangle.
    pub rect: PageRect,
    /// The image turned clockwise in page space, so it appears upright on
    /// a page that is shown rotated (with 90 or 270, the image's width
    /// runs along the rectangle's height).
    pub rotate: Rotation,
}

/// What a finished job wrote.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WriteReport {
    pub outputs: Vec<WrittenOutput>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WrittenOutput {
    pub path: PathBuf,
    pub pages: u32,
    pub bytes: u64,
}

/// Why a job wrote nothing. A job is all or nothing: when any output fails,
/// no output is left behind.
#[derive(Debug, Clone, PartialEq)]
pub enum WriteError {
    /// The job description itself is unusable.
    Job(String),
    /// A source cannot be read or is not a PDF the writer can parse.
    Source { index: usize, message: String },
    /// A source is encrypted; writing encrypted documents is not supported
    /// yet (ADR 0013 §4).
    Encrypted { index: usize },
    /// A page that the source does not have.
    NoSuchPage { source: usize, page: PageIndex },
    /// A stamp image cannot be read or decoded.
    Image { path: PathBuf, message: String },
    /// An output cannot be written or moved into place.
    Output { path: PathBuf, message: String },
    /// A written file did not open again as the expected document.
    Verify { path: PathBuf, message: String },
    /// The writer process ended abnormally (crash, memory limit, time
    /// limit) or could not be started.
    Process(String),
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Job(m) => write!(f, "bad write job: {m}"),
            Self::Source { index, message } => write!(f, "source {index}: {message}"),
            Self::Encrypted { index } => write!(f, "source {index} is encrypted"),
            Self::NoSuchPage { source, page } => {
                write!(f, "source {source} has no page {}", page.display_number())
            }
            Self::Image { path, message } => write!(f, "{}: {message}", path.display()),
            Self::Output { path, message } => write!(f, "{}: {message}", path.display()),
            Self::Verify { path, message } => {
                write!(f, "{}: verification failed: {message}", path.display())
            }
            Self::Process(m) => write!(f, "writer process: {m}"),
        }
    }
}

impl std::error::Error for WriteError {}

/// A path as it appears in the text form (the rest of a line).
fn path_text(path: &Path) -> Result<&str, WriteError> {
    let text = path
        .to_str()
        .ok_or_else(|| WriteError::Job(format!("{} is not valid Unicode", path.display())))?;
    if text.is_empty() || text.contains(['\n', '\r']) || text.trim() != text {
        return Err(WriteError::Job(format!("unusable path {text:?}")));
    }
    Ok(text)
}

/// One line of free text: no line breaks, no surrounding spaces.
fn one_line(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    flat.trim().to_owned()
}

impl WriteJob {
    /// The job's text form.
    pub fn to_text(&self) -> Result<String, WriteError> {
        let mut out = format!("{JOB_HEADER}\n");
        for source in &self.sources {
            let _ = writeln!(out, "source {}", path_text(source)?);
        }
        for output in &self.outputs {
            let _ = writeln!(out, "output {}", path_text(&output.path)?);
            match &output.content {
                OutputContent::Copy { source } => {
                    let _ = writeln!(out, "copy {source}");
                }
                OutputContent::Merge(sources) => {
                    let list: Vec<String> = sources.iter().map(usize::to_string).collect();
                    let _ = writeln!(out, "merge {}", list.join(" "));
                }
                OutputContent::Pages(pages) => {
                    for pick in pages {
                        let _ = writeln!(
                            out,
                            "page {} {} {}",
                            pick.source,
                            pick.page.get(),
                            pick.rotate.degrees()
                        );
                        for stamp in &pick.stamps {
                            let r = stamp.rect;
                            let _ = writeln!(
                                out,
                                "stamp {} {} {} {} {} {}",
                                r.x0,
                                r.y0,
                                r.x1,
                                r.y1,
                                stamp.rotate.degrees(),
                                path_text(&stamp.image)?
                            );
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// Parses the text form and checks the job is consistent (indices in
    /// range, every output has content).
    pub fn from_text(text: &str) -> Result<Self, WriteError> {
        let bad = |line: usize, what: &str| WriteError::Job(format!("line {line}: {what}"));
        let mut lines = text.lines().enumerate().map(|(i, l)| (i + 1, l));
        match lines.next() {
            Some((_, header)) if header.trim_start_matches('\u{feff}') == JOB_HEADER => {}
            _ => return Err(WriteError::Job("not a write job".into())),
        }
        let mut job = Self::default();
        for (number, line) in lines {
            if line.trim().is_empty() {
                continue;
            }
            let (keyword, rest) = line.split_once(' ').unwrap_or((line, ""));
            match keyword {
                "source" => job.sources.push(PathBuf::from(rest)),
                "output" => job.outputs.push(OutputSpec {
                    path: PathBuf::from(rest),
                    content: OutputContent::Pages(Vec::new()),
                }),
                "copy" => {
                    let source = rest.parse().map_err(|_| bad(number, "bad source index"))?;
                    let output = job
                        .outputs
                        .last_mut()
                        .ok_or_else(|| bad(number, "copy before output"))?;
                    match &output.content {
                        OutputContent::Pages(pages) if pages.is_empty() => {
                            output.content = OutputContent::Copy { source };
                        }
                        _ => return Err(bad(number, "an output has one kind of content")),
                    }
                }
                "merge" => {
                    let sources = rest
                        .split(' ')
                        .map(str::parse)
                        .collect::<Result<Vec<usize>, _>>()
                        .map_err(|_| bad(number, "bad source list"))?;
                    let output = job
                        .outputs
                        .last_mut()
                        .ok_or_else(|| bad(number, "merge before output"))?;
                    match &output.content {
                        OutputContent::Pages(pages) if pages.is_empty() => {
                            output.content = OutputContent::Merge(sources);
                        }
                        _ => return Err(bad(number, "an output has one kind of content")),
                    }
                }
                "page" => {
                    let fields: Vec<&str> = rest.split(' ').collect();
                    let [source, page, degrees] = fields[..] else {
                        return Err(bad(number, "page needs source, page and rotation"));
                    };
                    let source = source.parse().map_err(|_| bad(number, "bad source"))?;
                    let page = page.parse().map_err(|_| bad(number, "bad page"))?;
                    let rotate = degrees
                        .parse()
                        .ok()
                        .and_then(Rotation::from_degrees)
                        .ok_or_else(|| bad(number, "bad rotation"))?;
                    match job.outputs.last_mut().map(|o| &mut o.content) {
                        Some(OutputContent::Pages(pages)) => pages.push(PagePick {
                            source,
                            page: PageIndex::new(page),
                            rotate,
                            stamps: Vec::new(),
                        }),
                        _ => return Err(bad(number, "page outside a page output")),
                    }
                }
                "stamp" => {
                    let mut fields = rest.splitn(6, ' ');
                    let mut number_field = || -> Result<f32, WriteError> {
                        fields
                            .next()
                            .and_then(|f| f.parse::<f32>().ok())
                            .filter(|v| v.is_finite())
                            .ok_or_else(|| bad(number, "bad stamp rectangle"))
                    };
                    let rect = PageRect::new(
                        number_field()?,
                        number_field()?,
                        number_field()?,
                        number_field()?,
                    );
                    let rotate = fields
                        .next()
                        .and_then(|f| f.parse().ok())
                        .and_then(Rotation::from_degrees)
                        .ok_or_else(|| bad(number, "bad stamp rotation"))?;
                    let image = fields
                        .next()
                        .filter(|p| !p.is_empty())
                        .ok_or_else(|| bad(number, "stamp without an image"))?;
                    let pick = match job.outputs.last_mut().map(|o| &mut o.content) {
                        Some(OutputContent::Pages(pages)) => pages.last_mut(),
                        _ => None,
                    }
                    .ok_or_else(|| bad(number, "stamp before a page"))?;
                    pick.stamps.push(Stamp {
                        image: PathBuf::from(image),
                        rect,
                        rotate,
                    });
                }
                other => return Err(bad(number, &format!("unknown item {other:?}"))),
            }
        }
        job.check()?;
        Ok(job)
    }

    /// Indices in range, at least one output, no output without content.
    pub fn check(&self) -> Result<(), WriteError> {
        if self.outputs.is_empty() {
            return Err(WriteError::Job("nothing to write".into()));
        }
        let source_ok = |s: usize| s < self.sources.len();
        for output in &self.outputs {
            let ok = match &output.content {
                OutputContent::Copy { source } => source_ok(*source),
                OutputContent::Pages(pages) => {
                    !pages.is_empty() && pages.iter().all(|p| source_ok(p.source))
                }
                OutputContent::Merge(sources) => {
                    !sources.is_empty() && sources.iter().all(|&s| source_ok(s))
                }
            };
            if !ok {
                return Err(WriteError::Job(format!(
                    "{}: no pages or a bad source",
                    output.path.display()
                )));
            }
            if self.sources.iter().any(|s| same_path(s, &output.path)) {
                return Err(WriteError::Job(format!(
                    "{} would overwrite a source",
                    output.path.display()
                )));
            }
        }
        for (i, output) in self.outputs.iter().enumerate() {
            if self.outputs[..i]
                .iter()
                .any(|o| same_path(&o.path, &output.path))
            {
                return Err(WriteError::Job(format!(
                    "{} is written twice",
                    output.path.display()
                )));
            }
        }
        Ok(())
    }
}

/// Where the writer process `pid` stages `destination` before moving it
/// into place (next to it, so the move stays on one volume). The reader
/// uses the same name to clean up after a writer it had to end.
pub fn temp_path(destination: &Path, pid: u32) -> PathBuf {
    with_suffix(destination, &format!(".{pid}.fastpdf-tmp"))
}

/// Where the writer process `pid` keeps a file it replaces until every
/// output is in place.
pub fn backup_path(destination: &Path, pid: u32) -> PathBuf {
    with_suffix(destination, &format!(".{pid}.fastpdf-old"))
}

/// Deletes the files writer `pid` set aside for `job`'s outputs. Called
/// once the job's success is established (its report is written): until
/// then they are what restores the user's files if the writer is ended.
pub fn discard_backups(job: &WriteJob, pid: u32) {
    for output in &job.outputs {
        let _ = std::fs::remove_file(backup_path(&output.path, pid));
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Whether `a` and `b` name the same file: both resolved (links, `..`,
/// short names) as far as they exist, without the trailing dots and spaces
/// Windows drops from file names, compared without case.
fn same_path(a: &Path, b: &Path) -> bool {
    resolved(a) == resolved(b)
}

fn resolved(path: &Path) -> String {
    let full = std::fs::canonicalize(path)
        .ok()
        .or_else(|| {
            // Not there yet: its folder resolved, plus its name.
            let name = path.file_name()?;
            let parent = match path.parent() {
                Some(p) if !p.as_os_str().is_empty() => std::fs::canonicalize(p).ok()?,
                _ => std::fs::canonicalize(".").ok()?,
            };
            Some(parent.join(name))
        })
        .or_else(|| std::path::absolute(path).ok())
        .unwrap_or_else(|| path.to_path_buf());
    let text = full.to_string_lossy().to_lowercase();
    text.trim_end_matches(['.', ' ']).to_owned()
}

/// The result of a job: what was written, or why nothing was.
pub fn result_to_text(result: &Result<WriteReport, WriteError>) -> String {
    let mut out = format!("{RESULT_HEADER}\n");
    match result {
        Ok(report) => {
            for o in &report.outputs {
                let path = o.path.to_string_lossy();
                let _ = writeln!(out, "ok {} {} {}", o.pages, o.bytes, one_line(&path));
            }
        }
        Err(e) => {
            let line = match e {
                WriteError::Job(m) => format!("job {}", one_line(m)),
                WriteError::Source { index, message } => {
                    format!("source {index} {}", one_line(message))
                }
                WriteError::Encrypted { index } => format!("encrypted {index}"),
                WriteError::NoSuchPage { source, page } => {
                    format!("no-page {source} {}", page.get())
                }
                WriteError::Image { path, message } => format!(
                    "image {}\t{}",
                    one_line(&path.to_string_lossy()),
                    one_line(message)
                ),
                WriteError::Output { path, message } => format!(
                    "output {}\t{}",
                    one_line(&path.to_string_lossy()),
                    one_line(message)
                ),
                WriteError::Verify { path, message } => format!(
                    "verify {}\t{}",
                    one_line(&path.to_string_lossy()),
                    one_line(message)
                ),
                WriteError::Process(m) => format!("process {}", one_line(m)),
            };
            let _ = writeln!(out, "error {line}");
        }
    }
    // A report cut short (the writer ended while writing it) lacks this.
    out.push_str("end\n");
    out
}

/// Parses [`result_to_text`]'s form; anything unreadable is a
/// [`WriteError::Process`] (the writer did not finish its report).
pub fn result_from_text(text: &str) -> Result<WriteReport, WriteError> {
    let broken = || WriteError::Process("no readable result".into());
    let mut lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.first() != Some(&RESULT_HEADER) || lines.last() != Some(&"end") {
        return Err(broken());
    }
    lines.pop();
    let mut report = WriteReport::default();
    for line in lines.into_iter().skip(1) {
        if let Some(rest) = line.strip_prefix("ok ") {
            let mut fields = rest.splitn(3, ' ');
            let pages = fields.next().and_then(|f| f.parse().ok());
            let bytes = fields.next().and_then(|f| f.parse().ok());
            let path = fields.next().filter(|p| !p.is_empty());
            let (Some(pages), Some(bytes), Some(path)) = (pages, bytes, path) else {
                return Err(broken());
            };
            report.outputs.push(WrittenOutput {
                path: PathBuf::from(path),
                pages,
                bytes,
            });
        } else if let Some(rest) = line.strip_prefix("error ") {
            return Err(parse_error(rest).unwrap_or_else(broken));
        } else {
            return Err(broken());
        }
    }
    Ok(report)
}

fn parse_error(text: &str) -> Option<WriteError> {
    let (kind, rest) = text.split_once(' ').unwrap_or((text, ""));
    let path_and_message = |rest: &str| {
        let (path, message) = rest.split_once('\t')?;
        Some((PathBuf::from(path), message.to_owned()))
    };
    Some(match kind {
        "job" => WriteError::Job(rest.to_owned()),
        "source" => {
            let (index, message) = rest.split_once(' ').unwrap_or((rest, ""));
            WriteError::Source {
                index: index.parse().ok()?,
                message: message.to_owned(),
            }
        }
        "encrypted" => WriteError::Encrypted {
            index: rest.parse().ok()?,
        },
        "no-page" => {
            let (source, page) = rest.split_once(' ')?;
            WriteError::NoSuchPage {
                source: source.parse().ok()?,
                page: PageIndex::new(page.parse().ok()?),
            }
        }
        "image" => {
            let (path, message) = path_and_message(rest)?;
            WriteError::Image { path, message }
        }
        "output" => {
            let (path, message) = path_and_message(rest)?;
            WriteError::Output { path, message }
        }
        "verify" => {
            let (path, message) = path_and_message(rest)?;
            WriteError::Verify { path, message }
        }
        "process" => WriteError::Process(rest.to_owned()),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job() -> WriteJob {
        WriteJob {
            sources: vec![
                PathBuf::from(r"D:\in\a b.pdf"),
                PathBuf::from(r"D:\in\公文.pdf"),
            ],
            outputs: vec![
                OutputSpec {
                    path: PathBuf::from(r"D:\out\merged.pdf"),
                    content: OutputContent::Pages(vec![
                        PagePick {
                            source: 0,
                            page: PageIndex::new(2),
                            rotate: Rotation::R90,
                            stamps: vec![Stamp {
                                image: PathBuf::from(r"C:\seals\大章 v2.png"),
                                rect: PageRect::new(100.5, 120.0, 180.25, 200.0),
                                rotate: Rotation::R270,
                            }],
                        },
                        PagePick::new(1, PageIndex::FIRST),
                    ]),
                },
                OutputSpec {
                    path: PathBuf::from(r"D:\out\copy.pdf"),
                    content: OutputContent::Copy { source: 1 },
                },
                OutputSpec {
                    path: PathBuf::from(r"D:\out\both.pdf"),
                    content: OutputContent::Merge(vec![1, 0, 1]),
                },
            ],
        }
    }

    #[test]
    fn jobs_round_trip_through_text() {
        let job = job();
        let text = job.to_text().unwrap();
        assert!(text.starts_with("fastpdf-write-job 1\nsource D:\\in\\a b.pdf\n"));
        assert_eq!(WriteJob::from_text(&text).unwrap(), job);
        assert_eq!(
            WriteJob::from_text(&text.replace('\n', "\r\n")).unwrap(),
            job
        );
    }

    #[test]
    fn inconsistent_jobs_are_rejected() {
        let parse = |body: &str| WriteJob::from_text(&format!("{JOB_HEADER}\n{body}"));
        assert!(parse("").is_err(), "nothing to write");
        assert!(parse("source a.pdf\noutput b.pdf\n").is_err(), "no pages");
        assert!(parse("source a.pdf\noutput b.pdf\npage 1 0 0\n").is_err());
        assert!(parse("source a.pdf\noutput b.pdf\npage 0 0 45\n").is_err());
        assert!(parse("source a.pdf\noutput b.pdf\ncopy 0\npage 0 0 0\n").is_err());
        assert!(parse("source a.pdf\noutput b.pdf\nstamp 0 0 1 1 s.png\n").is_err());
        assert!(parse("source a.pdf\noutput b.pdf\npage 0 0 0\nstamp 0 0 x 1 0 s.png\n").is_err());
        assert!(parse("source a.pdf\noutput b.pdf\npage 0 0 0\nstamp 0 0 1 1 45 s.png\n").is_err());
        assert!(
            parse("source a.pdf\noutput b.pdf\npage 0 0 0\nstamp 0 0 1 1 90 s s.png\n").is_ok()
        );
        assert!(
            parse("source a.pdf\noutput A.PDF\ncopy 0\n").is_err(),
            "overwrite"
        );
        assert!(parse("source a.pdf\noutput .\\x\\..\\a.pdf. \ncopy 0\n").is_err());
        assert!(parse("source a.pdf\noutput b.pdf\ncopy 0\noutput B.pdf\ncopy 0\n").is_err());
        assert!(parse("source a.pdf\nfrobnicate\n").is_err());
        assert!(WriteJob::from_text("garbage").is_err());
        assert!(parse("source a.pdf\noutput b.pdf\npage 0 7 -90\n").is_ok());
        assert!(parse("source a.pdf\noutput b.pdf\nmerge 0 0\n").is_ok());
        assert!(parse("source a.pdf\noutput b.pdf\nmerge 0 1\n").is_err());
        assert!(parse("source a.pdf\noutput b.pdf\nmerge\n").is_err());
        assert!(parse("source a.pdf\noutput b.pdf\nmerge 0\ncopy 0\n").is_err());
        let unusable = WriteJob {
            sources: vec![PathBuf::from("a.pdf")],
            outputs: vec![OutputSpec {
                path: PathBuf::from("b.pdf\nc.pdf"),
                content: OutputContent::Copy { source: 0 },
            }],
        };
        assert!(unusable.to_text().is_err());
    }

    #[test]
    fn paths_are_compared_resolved() {
        let dir = std::env::temp_dir().join(format!("fastpdf-write-api-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let file = dir.join("in.pdf");
        std::fs::write(&file, b"x").unwrap();
        assert!(same_path(&file, &dir.join("sub").join("..").join("IN.PDF")));
        assert!(same_path(&file, &dir.join("in.pdf. ")));
        assert!(!same_path(&file, &dir.join("in2.pdf")));
        assert!(same_path(
            &dir.join("new.pdf"),
            &dir.join("sub\\..\\new.pdf")
        ));
        assert_eq!(
            temp_path(Path::new(r"C:\a\b.pdf"), 7),
            PathBuf::from(r"C:\a\b.pdf.7.fastpdf-tmp")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn results_round_trip_through_text() {
        let ok: Result<WriteReport, WriteError> = Ok(WriteReport {
            outputs: vec![WrittenOutput {
                path: PathBuf::from(r"D:\out\a b.pdf"),
                pages: 3,
                bytes: 12345,
            }],
        });
        assert_eq!(result_from_text(&result_to_text(&ok)), ok);
        let errors = [
            WriteError::Job("x".into()),
            WriteError::Source {
                index: 1,
                message: "bad xref".into(),
            },
            WriteError::Encrypted { index: 0 },
            WriteError::NoSuchPage {
                source: 0,
                page: PageIndex::new(9),
            },
            WriteError::Image {
                path: PathBuf::from(r"C:\s s.png"),
                message: "not a PNG".into(),
            },
            WriteError::Output {
                path: PathBuf::from(r"D:\o.pdf"),
                message: "access denied".into(),
            },
            WriteError::Verify {
                path: PathBuf::from(r"D:\o.pdf"),
                message: "2 pages instead of 3".into(),
            },
            WriteError::Process("memory limit".into()),
        ];
        for e in errors {
            let text = result_to_text(&Err(e.clone()));
            assert_eq!(result_from_text(&text), Err(e));
        }
        // Line breaks in messages cannot break the format.
        let text = result_to_text(&Err(WriteError::Job("a\nb".into())));
        assert_eq!(result_from_text(&text), Err(WriteError::Job("a b".into())));
        assert!(matches!(
            result_from_text("nonsense"),
            Err(WriteError::Process(_))
        ));
        // A report cut short is no success.
        let full = result_to_text(&ok);
        let cut = full.trim_end().trim_end_matches("end");
        assert!(matches!(result_from_text(cut), Err(WriteError::Process(_))));
    }
}
