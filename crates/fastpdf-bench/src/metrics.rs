//! Process-level measurements: peak memory and CPU time (spec §26).

use std::time::Duration;

/// Memory counters of the current process, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct MemoryCounters {
    pub(crate) working_set: u64,
    pub(crate) peak_working_set: u64,
    /// Committed private bytes (what the OS must back with RAM or pagefile).
    pub(crate) private: u64,
    pub(crate) peak_private: u64,
}

pub(crate) fn memory() -> Option<MemoryCounters> {
    imp::memory()
}

/// User + kernel CPU time consumed by this process so far.
pub(crate) fn cpu_time() -> Option<Duration> {
    imp::cpu_time()
}

/// Static description of the machine, recorded with every corpus run so
/// numbers are never compared across different hardware by accident.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub(crate) struct MachineInfo {
    pub(crate) os: String,
    pub(crate) cpu: String,
    pub(crate) threads: usize,
    pub(crate) ram_gb: Option<u64>,
}

pub(crate) fn machine() -> MachineInfo {
    MachineInfo {
        os: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        cpu: imp::cpu_name().unwrap_or_else(|| "unknown".into()),
        threads: std::thread::available_parallelism().map_or(1, |n| n.get()),
        ram_gb: imp::total_ram().map(|b| (b + (1 << 29)) >> 30),
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod imp {
    use super::MemoryCounters;
    use std::time::Duration;
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    pub(super) fn memory() -> Option<MemoryCounters> {
        let mut c = PROCESS_MEMORY_COUNTERS_EX {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..Default::default()
        };
        // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no
        // cleanup; `c` is a correctly sized, writable PROCESS_MEMORY_COUNTERS_EX
        // whose `cb` field states its size, as the API requires.
        let ok = unsafe {
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                (&raw mut c).cast::<PROCESS_MEMORY_COUNTERS>(),
                c.cb,
            )
        };
        (ok != 0).then_some(MemoryCounters {
            working_set: c.WorkingSetSize as u64,
            peak_working_set: c.PeakWorkingSetSize as u64,
            private: c.PrivateUsage as u64,
            peak_private: c.PeakPagefileUsage as u64,
        })
    }

    pub(super) fn cpu_time() -> Option<Duration> {
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: pseudo-handle of the current process; all four out-params
        // point to valid FILETIME values.
        let ok = unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut creation,
                &mut exit,
                &mut kernel,
                &mut user,
            )
        };
        let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
        // FILETIME durations are in 100 ns units.
        (ok != 0).then(|| Duration::from_nanos((ticks(kernel) + ticks(user)) * 100))
    }

    pub(super) fn total_ram() -> Option<u64> {
        let mut status = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        // SAFETY: `status` is a writable MEMORYSTATUSEX with dwLength set.
        let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
        (ok != 0).then_some(status.ullTotalPhys)
    }

    pub(super) fn cpu_name() -> Option<String> {
        let key: Vec<u16> = "HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0\0"
            .encode_utf16()
            .collect();
        let value: Vec<u16> = "ProcessorNameString\0".encode_utf16().collect();
        let mut buf = [0u16; 256];
        let mut len = std::mem::size_of_val(&buf) as u32;
        // SAFETY: key/value are NUL-terminated UTF-16 strings; `buf` is a
        // writable buffer whose size in bytes is passed in `len`.
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut len,
            )
        };
        if status != 0 {
            return None;
        }
        let chars = (len as usize / 2).min(buf.len());
        let s = String::from_utf16_lossy(&buf[..chars]);
        Some(s.trim_end_matches('\0').trim().to_owned())
    }
}

#[cfg(not(windows))]
mod imp {
    use super::MemoryCounters;
    use std::time::Duration;

    pub(super) fn memory() -> Option<MemoryCounters> {
        // Linux: VmHWM / VmRSS from /proc; other platforms report nothing.
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let field = |name: &str| -> Option<u64> {
            let line = status.lines().find(|l| l.starts_with(name))?;
            let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
            Some(kb * 1024)
        };
        Some(MemoryCounters {
            working_set: field("VmRSS:")?,
            peak_working_set: field("VmHWM:")?,
            private: field("VmData:").unwrap_or(0),
            peak_private: field("VmPeak:").unwrap_or(0),
        })
    }

    pub(super) fn cpu_time() -> Option<Duration> {
        None
    }

    pub(super) fn total_ram() -> Option<u64> {
        None
    }

    pub(super) fn cpu_name() -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_counters_are_plausible() {
        let m = memory().unwrap();
        // The kernel updates the peak lazily, so it can briefly trail the
        // current working set under load; only check that both are present.
        assert!(m.working_set > 0 && m.peak_working_set > 0);
        assert!(cpu_time().is_some());
        let info = machine();
        assert!(info.threads >= 1);
        assert!(info.ram_gb.unwrap_or(0) >= 1);
        assert_ne!(info.cpu, "unknown");
    }
}
