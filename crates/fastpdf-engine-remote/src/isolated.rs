//! Helper processes under the render host's isolation (ADR 0008 §3): created
//! inside a fresh job object with a commit limit, `KILL_ON_JOB_CLOSE`, no
//! inherited handles, no child processes of their own and UI restrictions.
//! The writer's `--write-job` processes run this way (ADR 0013).
//!
//! Other platforms: a plain child process without limits.

use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::time::Duration;

/// How an isolated process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolatedExit {
    /// It exited with this code.
    Exited(u32),
    /// It reached its memory limit and was ended.
    MemoryLimit,
}

/// A running isolated process. Dropping it ends the process.
#[derive(Debug)]
pub struct IsolatedProcess {
    #[cfg(windows)]
    child: crate::win::process::Child,
    #[cfg(not(windows))]
    child: std::sync::Mutex<std::process::Child>,
}

impl IsolatedProcess {
    /// Starts `program args...` with a commit limit of `memory_limit`
    /// bytes.
    pub fn spawn(program: &Path, args: &[OsString], memory_limit: u64) -> io::Result<Self> {
        #[cfg(windows)]
        {
            let limits = crate::win::process::JobLimits {
                memory: Some(memory_limit),
            };
            crate::win::process::spawn(program, args, limits).map(|child| Self { child })
        }
        #[cfg(not(windows))]
        {
            let _ = memory_limit;
            let child = std::process::Command::new(program)
                .args(args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()?;
            Ok(Self {
                child: std::sync::Mutex::new(child),
            })
        }
    }

    /// Waits up to `timeout`; `None` while it is still running.
    pub fn wait(&self, timeout: Duration) -> Option<IsolatedExit> {
        #[cfg(windows)]
        {
            let code = self.child.wait(timeout)?;
            Some(if self.child.hit_memory_limit() {
                IsolatedExit::MemoryLimit
            } else {
                IsolatedExit::Exited(code)
            })
        }
        #[cfg(not(windows))]
        {
            let deadline = std::time::Instant::now() + timeout;
            loop {
                let status = self.child.lock().ok()?.try_wait().ok()?;
                if let Some(status) = status {
                    return Some(IsolatedExit::Exited(status.code().unwrap_or(1) as u32));
                }
                if std::time::Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    /// The process id.
    pub fn pid(&self) -> u32 {
        #[cfg(windows)]
        {
            self.child.pid()
        }
        #[cfg(not(windows))]
        {
            self.child.lock().map_or(0, |c| c.id())
        }
    }

    /// Ends the process now.
    pub fn kill(&self) {
        #[cfg(windows)]
        self.child.kill();
        #[cfg(not(windows))]
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
        }
    }

    /// A readable cause for an exit code (crash codes on Windows).
    pub fn describe(code: u32) -> String {
        #[cfg(windows)]
        {
            crate::win::process::describe_exit(code)
        }
        #[cfg(not(windows))]
        {
            format!("exit code {code}")
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_and_timeouts_are_reported() {
        let cmd = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
        let quick = IsolatedProcess::spawn(
            Path::new(&cmd),
            &["/C".into(), "exit".into(), "3".into()],
            256 << 20,
        )
        .expect("cmd starts");
        assert_eq!(
            quick.wait(Duration::from_secs(20)),
            Some(IsolatedExit::Exited(3))
        );
        let slow = IsolatedProcess::spawn(
            Path::new(&cmd),
            &[
                "/C".into(),
                "ping".into(),
                "-n".into(),
                "30".into(),
                "127.0.0.1".into(),
            ],
            256 << 20,
        );
        // ping is a child of cmd, which the job forbids; either way the
        // process must be killable while it runs.
        if let Ok(slow) = slow
            && slow.wait(Duration::from_millis(200)).is_none()
        {
            slow.kill();
            assert!(slow.wait(Duration::from_secs(10)).is_some());
        }
    }
}
