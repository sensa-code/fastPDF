//! The writer's child process (ADR 0013).
//!
//! The reader never writes PDFs itself. A write job travels in its text
//! form (`fastpdf_engine_api::write`) to this same executable, started as
//! `fastpdf --write-job <job file> <result file>` under the render host's
//! isolation (`IsolatedProcess`: a memory limit, killed with the reader).
//! The child runs the hayro writer and leaves its report in the result
//! file; nothing else passes between the two processes. Like the render
//! host, the child is dispatched before settings, logging or GPUI.
//!
//! Signatures are made in the child too (`fastpdf-sign`): the card's PIN
//! dialog belongs to the reader's window, which lets the child bring it to
//! the front.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use fastpdf_engine_api::CancelToken;
use fastpdf_engine_api::write::{
    self, OutputContent, SignProblem, SignatureSpec, WriteError, WriteJob, WriteReport,
    backup_path, discard_backups, temp_path,
};
use fastpdf_engine_remote::{IsolatedExit, IsolatedProcess};
use fastpdf_sign::{SignError, Signer};

/// First argument of a writer process.
const FLAG: &str = "--write-job";
/// A job still running after this long is ended.
const TIME_LIMIT: Duration = Duration::from_secs(30 * 60);

/// Runs a write job when this process was started as one. Must be the
/// first thing `main` does after the render host check.
pub(crate) fn dispatch() -> Option<ExitCode> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(OsStr::new(FLAG)) {
        return None;
    }
    let (Some(job), Some(result)) = (args.next(), args.next()) else {
        return Some(ExitCode::from(2));
    };
    let job = read_job(Path::new(&job));
    let outcome = job.as_ref().map_err(Clone::clone).and_then(execute);
    if std::fs::write(&result, write::result_to_text(&outcome)).is_err() {
        // The reader sees no report and undoes the job.
        return Some(ExitCode::from(3));
    }
    // The report stands: the files the outputs replaced can go.
    if let (Ok(job), Ok(_)) = (&job, &outcome) {
        discard_backups(job, std::process::id());
    }
    Some(if outcome.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn read_job(job: &Path) -> Result<WriteJob, WriteError> {
    let text = std::fs::read_to_string(job)
        .map_err(|e| WriteError::Job(format!("cannot read the job: {e}")))?;
    WriteJob::from_text(&text)
}

#[cfg(feature = "engine-hayro")]
fn execute(job: &WriteJob) -> Result<WriteReport, WriteError> {
    // Sources are hostile input: a panic still ends with a report.
    std::panic::catch_unwind(|| {
        fastpdf_engine_hayro::write::execute_keeping_backups_with(job, &sign)
    })
    .unwrap_or_else(|_| Err(WriteError::Process("the writer stopped on an error".into())))
}

/// Signs for the writer: with a certificate of the user's store or, with
/// the development overlay on (`FASTPDF_DEV_OVERLAY`, which development
/// scripts need too), with a key made for the job (`test`).
fn sign(spec: &SignatureSpec, parts: &[&[u8]]) -> Result<Vec<u8>, SignProblem> {
    let signer = if spec.signer == "test" {
        let development = std::env::var("FASTPDF_DEV_OVERLAY").is_ok_and(|v| v.trim() != "0");
        if !development {
            return Err(SignProblem::Failed(
                "the test signer needs FASTPDF_DEV_OVERLAY=1".into(),
            ));
        }
        Signer::Ephemeral
    } else {
        let thumbprint =
            fastpdf_sign::parse_thumbprint(&spec.signer).ok_or(SignProblem::CertificateMissing)?;
        Signer::Store {
            thumbprint,
            window: spec.window as isize,
        }
    };
    fastpdf_sign::sign(&signer, parts).map_err(|e| match e {
        SignError::CertificateMissing => SignProblem::CertificateMissing,
        SignError::CardMissing => SignProblem::CardMissing,
        SignError::Cancelled => SignProblem::Cancelled,
        SignError::WrongPin => SignProblem::WrongPin,
        SignError::Unavailable => SignProblem::Unavailable,
        SignError::Failed { what, code } => SignProblem::Failed(format!("{what} (0x{code:08X})")),
    })
}

#[cfg(not(feature = "engine-hayro"))]
fn execute(_: &WriteJob) -> Result<WriteReport, WriteError> {
    Err(WriteError::Process(
        "this build has no PDF writer (engine-hayro)".into(),
    ))
}

/// Runs `job` in an isolated child process. Blocks the calling thread (the
/// reader calls it from a background task); `cancel` ends the child.
pub(crate) fn run(job: &WriteJob, cancel: &CancelToken) -> Result<WriteReport, WriteError> {
    let exe = std::env::current_exe()
        .map_err(|e| WriteError::Process(format!("no executable path: {e}")))?;
    let stem = format!("fastpdf-write-{}-{}", std::process::id(), unique());
    let dir = std::env::temp_dir();
    let job_path = dir.join(format!("{stem}.job"));
    let result_path = dir.join(format!("{stem}.result"));
    let _cleanup = Cleanup(vec![job_path.clone(), result_path.clone()]);
    std::fs::write(&job_path, job.to_text()?)
        .map_err(|e| WriteError::Process(format!("cannot write the job file: {e}")))?;
    let args: Vec<OsString> = vec![
        FLAG.into(),
        job_path.into_os_string(),
        result_path.clone().into_os_string(),
    ];
    // What is there before the writer starts: after a writer that had to
    // be ended, files that were not there are its outputs and go again.
    let existed: Vec<bool> = job.outputs.iter().map(|o| o.path.exists()).collect();
    let child = IsolatedProcess::spawn(&exe, &args, memory_limit(job))
        .map_err(|e| WriteError::Process(format!("cannot start the writer: {e}")))?;
    if job
        .outputs
        .iter()
        .any(|o| matches!(o.content, OutputContent::Signed { .. }))
    {
        // The card's PIN dialog comes from the child: let it take the front.
        allow_foreground(child.pid());
    }
    let started = Instant::now();
    let exit = loop {
        if let Some(exit) = child.wait(Duration::from_millis(100)) {
            break exit;
        }
        let stop = if cancel.is_cancelled() {
            Some("cancelled")
        } else if started.elapsed() > TIME_LIMIT {
            Some("took longer than 30 minutes")
        } else {
            None
        };
        if let Some(why) = stop {
            child.kill();
            let _ = child.wait(Duration::from_secs(10));
            // It may have finished just before: then its report stands.
            if let Ok(Ok(report)) =
                std::fs::read_to_string(&result_path).map(|text| write::result_from_text(&text))
            {
                discard_backups(job, child.pid());
                return Ok(report);
            }
            clean_up(job, child.pid(), &existed);
            return Err(WriteError::Process(why.into()));
        }
    };
    // The report decides, whatever the exit: a writer may crash after it
    // finished the job and wrote its report. A success must list every
    // output.
    let reported = std::fs::read_to_string(&result_path)
        .ok()
        .map(|text| write::result_from_text(&text))
        .map(|result| {
            result.and_then(|report| {
                if report.outputs.len() == job.outputs.len() {
                    Ok(report)
                } else {
                    Err(WriteError::Process("an incomplete report".into()))
                }
            })
        });
    if let Some(Ok(report)) = reported {
        // The writer may have ended before deleting them.
        discard_backups(job, child.pid());
        return Ok(report);
    }
    // No success report: undo whatever the writer left.
    clean_up(job, child.pid(), &existed);
    match (reported, exit) {
        (Some(Err(e)), _) => Err(e),
        (_, IsolatedExit::MemoryLimit) => Err(WriteError::Process(
            "needed more memory than allowed".into(),
        )),
        (_, IsolatedExit::Exited(code)) => {
            Err(WriteError::Process(IsolatedProcess::describe(code)))
        }
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn allow_foreground(pid: u32) {
    // SAFETY: lets process `pid` set the foreground window; no pointers.
    unsafe { windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(pid) };
}

#[cfg(not(windows))]
fn allow_foreground(_: u32) {}

/// Undoes what writer `pid` may have left after it ended abnormally: its
/// staging files go, a file it set aside comes back (the user's file it
/// was replacing), and an output that did not exist before (`existed`) is
/// removed, so the job leaves nothing behind.
fn clean_up(job: &WriteJob, pid: u32, existed: &[bool]) {
    for (i, output) in job.outputs.iter().enumerate() {
        let _ = std::fs::remove_file(temp_path(&output.path, pid));
        let backup = backup_path(&output.path, pid);
        if backup.exists() {
            let _ = std::fs::remove_file(&output.path);
            let _ = std::fs::rename(&backup, &output.path);
        } else if !existed.get(i).copied().unwrap_or(true) {
            let _ = std::fs::remove_file(&output.path);
        }
    }
}

/// The writer reads its sources whole and builds outputs in memory: room
/// for four times the sources plus a base, between 1 and 16 GiB.
fn memory_limit(job: &WriteJob) -> u64 {
    let sources: u64 = job
        .sources
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();
    (1u64 << 30)
        .saturating_add(sources.saturating_mul(4))
        .min(16 << 30)
}

fn unique() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Removes the job's temporary files however `run` ends.
struct Cleanup(Vec<PathBuf>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_up_restores_and_removes() {
        let dir = std::env::temp_dir().join(format!("fastpdf-cleanup-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pid = 4242;
        let replaced = dir.join("replaced.pdf");
        let created = dir.join("created.pdf");
        let staged = dir.join("staged.pdf");
        // The writer set aside the user's file, put its own in place, made
        // a new output and was staging a third when it was ended.
        std::fs::write(backup_path(&replaced, pid), b"user").unwrap();
        std::fs::write(&replaced, b"new").unwrap();
        std::fs::write(&created, b"new").unwrap();
        std::fs::write(temp_path(&staged, pid), b"half").unwrap();
        let output = |path: &PathBuf| write::OutputSpec {
            path: path.clone(),
            content: write::OutputContent::Copy { source: 0 },
        };
        let job = WriteJob {
            sources: vec![dir.join("in.pdf")],
            outputs: vec![output(&replaced), output(&created), output(&staged)],
        };
        clean_up(&job, pid, &[true, false, false]);
        assert_eq!(std::fs::read(&replaced).unwrap(), b"user");
        assert!(!created.exists());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_memory_limit_grows_with_the_sources() {
        let job = WriteJob::default();
        assert_eq!(memory_limit(&job), 1 << 30);
        let exe = std::env::current_exe().unwrap_or_default();
        let job = WriteJob {
            sources: vec![exe.clone(), PathBuf::from("does-not-exist.pdf")],
            outputs: Vec::new(),
        };
        let size = std::fs::metadata(&exe).map_or(0, |m| m.len());
        assert_eq!(memory_limit(&job), (1 << 30) + 4 * size);
    }
}
