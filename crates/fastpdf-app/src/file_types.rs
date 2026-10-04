//! `fastpdf --register-file-types` / `--unregister-file-types` (spec §36:
//! file association): the per-user `.pdf` registration that
//! `fastpdf_shell` plans. With `--dry-run` the plan is printed as a `.reg`
//! file and nothing changes. No window opens either way.
//!
//! Registration only adds FastPDF to "Open with" and to Settings > Apps >
//! Default apps; Windows lets only the user pick the default handler.
//!
//! An MSIX install (ADR 0010) declares the association in its manifest
//! instead; with package identity both commands leave the registry alone.

use std::path::Path;

use fastpdf_shell::{Plan, PlanError, Registration};

/// Which plan the command runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileTypes {
    Register,
    Unregister,
}

/// The plan for `exe` (normally `std::env::current_exe()`).
pub(crate) fn plan_for(action: FileTypes, exe: &Path) -> Result<Plan, PlanError> {
    let registration = Registration::new(exe)?;
    Ok(match action {
        FileTypes::Register => registration.register_plan(),
        FileTypes::Unregister => registration.unregister_plan(),
    })
}

/// Runs the command for this executable; returns the process exit code.
pub(crate) fn run(action: FileTypes, dry_run: bool) -> i32 {
    run_with(action, dry_run, package_full_name())
}

/// `run` with the package identity passed in. A packaged process returns
/// before any plan is built, so the registry is never read or written.
fn run_with(action: FileTypes, dry_run: bool, package: Option<String>) -> i32 {
    if let Some(package) = package {
        println!("{}", managed_by_package(&package));
        return 0;
    }
    let plan = match std::env::current_exe()
        .map_err(|e| format!("cannot find this program's path: {e}"))
        .and_then(|exe| plan_for(action, &exe).map_err(|e| e.to_string()))
    {
        Ok(plan) => plan,
        Err(message) => {
            eprintln!("fastpdf: {message}");
            return 1;
        }
    };
    if dry_run {
        // For review. regedit imports non-ASCII `.reg` files only as
        // UTF-16 LE with a BOM; this prints UTF-8.
        print!("{}", plan.to_reg_file());
        return 0;
    }
    apply(action, &plan)
}

#[cfg(windows)]
fn apply(action: FileTypes, plan: &Plan) -> i32 {
    match fastpdf_shell::apply(plan) {
        Ok(changes) => {
            match action {
                FileTypes::Register => println!(
                    "FastPDF is registered for .pdf files for this user ({changes} registry changes).\n\
                     To make it the default PDF reader, choose it in Settings > Apps > Default apps ({}).",
                    fastpdf_shell::default_apps_settings_uri()
                ),
                FileTypes::Unregister => println!(
                    "FastPDF's .pdf registration was removed for this user ({changes} registry changes)."
                ),
            }
            0
        }
        Err(e) => {
            eprintln!("fastpdf: {e}");
            if action == FileTypes::Register {
                eprintln!(
                    "fastpdf: run `fastpdf --unregister-file-types` to remove a partial registration"
                );
            }
            1
        }
    }
}

#[cfg(not(windows))]
fn apply(_: FileTypes, _: &Plan) -> i32 {
    eprintln!("fastpdf: file associations are only supported on Windows");
    1
}

/// What the commands print when FastPDF runs from an installed package.
fn managed_by_package(package: &str) -> String {
    format!(
        "FastPDF runs from the installed package {package}: its .pdf file association is \
         managed by the package manifest, so nothing was changed.\n\
         To make it the default PDF reader, choose it in Settings > Apps > Default apps ({}).",
        fastpdf_shell::DEFAULT_APPS_SETTINGS_URI
    )
}

/// Full name of the MSIX package this process runs in, or `None` without
/// package identity (zip / portable build, `cargo run`).
#[cfg(windows)]
#[allow(unsafe_code)]
pub(crate) fn package_full_name() -> Option<String> {
    use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS};
    use windows_sys::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;

    let mut len: u32 = 0;
    // SAFETY: a null buffer with length 0 only asks for the required length.
    // Without identity the call fails with APPMODEL_ERROR_NO_PACKAGE.
    let status = unsafe { GetCurrentPackageFullName(&mut len, std::ptr::null_mut()) };
    if status != ERROR_INSUFFICIENT_BUFFER {
        return None;
    }
    let mut buffer = vec![0u16; len as usize];
    // SAFETY: `buffer` holds `len` UTF-16 units, the length the first call asked for.
    let status = unsafe { GetCurrentPackageFullName(&mut len, buffer.as_mut_ptr()) };
    // The first call already proved package identity; keep it even if the
    // name cannot be read, so the registry stays untouched.
    let name = if status == ERROR_SUCCESS {
        let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        String::from_utf16_lossy(&buffer[..end])
    } else {
        String::from("(name unavailable)")
    };
    Some(name)
}

#[cfg(not(windows))]
pub(crate) fn package_full_name() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // Plans only: nothing here may call `apply`, or `run` / `run_with` without
    // a package identity and without `dry_run`: those change the current
    // user's registry.

    #[test]
    fn plans_target_the_given_executable() {
        let exe = Path::new(r"C:\Program Files\FastPDF 測試\fastpdf.exe");
        let register = plan_for(FileTypes::Register, exe)
            .expect("valid path")
            .to_reg_file();
        assert!(register.contains(r"[HKEY_CURRENT_USER\Software\Classes\FastPDF.Document]"));
        assert!(register.contains(r#"\"C:\\Program Files\\FastPDF 測試\\fastpdf.exe\" \"%1\""#));
        let unregister = plan_for(FileTypes::Unregister, exe)
            .expect("valid path")
            .to_reg_file();
        assert!(unregister.contains(r"[-HKEY_CURRENT_USER\Software\Classes\FastPDF.Document]"));
    }

    #[test]
    fn unusable_paths_are_reported() {
        assert!(plan_for(FileTypes::Register, Path::new("fastpdf.exe")).is_err());
        assert!(plan_for(FileTypes::Register, Path::new(r"C:\a%b\fastpdf.exe")).is_err());
    }

    #[test]
    fn test_process_has_no_package_identity() {
        // `cargo test` binaries are never installed from an MSIX package.
        assert_eq!(package_full_name(), None);
    }

    #[test]
    fn packaged_runs_change_nothing_and_succeed() {
        // Safe to call without `dry_run`: with a package identity `run_with`
        // returns before building a plan, so `apply` is unreachable.
        let package = "FastPDF_0.0.1.0_x64__0123456789abc";
        for action in [FileTypes::Register, FileTypes::Unregister] {
            for dry_run in [false, true] {
                assert_eq!(run_with(action, dry_run, Some(package.to_owned())), 0);
            }
        }
        let message = managed_by_package(package);
        assert!(message.contains(package));
        assert!(message.contains("managed by the package manifest"));
        assert!(message.contains("ms-settings:defaultapps"));
    }
}
