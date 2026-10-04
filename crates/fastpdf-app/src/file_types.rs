//! `fastpdf --register-file-types` / `--unregister-file-types` (spec §36:
//! file association): the per-user `.pdf` registration that
//! `fastpdf_shell` plans. With `--dry-run` the plan is printed as a `.reg`
//! file and nothing changes. No window opens either way.
//!
//! Registration only adds FastPDF to "Open with" and to Settings > Apps >
//! Default apps; Windows lets only the user pick the default handler.

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

#[cfg(test)]
mod tests {
    use super::*;

    // Plans only: nothing here may call `run` without `dry_run` or `apply`,
    // which change the current user's registry.

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
}
