//! Windows shell integration: per-user `.pdf` file association plans
//! (spec §33: file association, Explorer integration).
//!
//! A [`Plan`] is plain data: the registry writes (or deletions) that make
//! FastPDF appear in Explorer's "Open with" menu and in Settings > Apps >
//! Default apps. Building a plan has no side effects and is unit tested;
//! only `apply` (Windows only) touches the registry, and tests never call it.
//!
//! Everything lives under `HKEY_CURRENT_USER`, so no elevation is needed and
//! other users are unaffected:
//!
//! | Key (under `HKCU`) | Purpose |
//! |---|---|
//! | `Software\Classes\FastPDF.Document` | ProgID: type name, `DefaultIcon`, `shell\open\command`, `Application` |
//! | `Software\Classes\.pdf\OpenWithProgids`, value `FastPDF.Document` | "Open with" entry for `.pdf` without taking the default |
//! | `Software\Classes\Applications\fastpdf.exe` | application entry: `FriendlyAppName`, `SupportedTypes`, `DefaultIcon`, `shell\open\command` |
//! | `Software\FastPDF\Capabilities` | `ApplicationName`, `ApplicationDescription`, `ApplicationIcon`, `FileAssociations` |
//! | `Software\RegisteredApplications`, value `FastPDF` | lists the capabilities in Settings > Default apps |
//!
//! Windows 10/11 do not let programs choose the default handler: the
//! per-extension choice (`...\Explorer\FileExts\.pdf\UserChoice`) is
//! hash-protected and set only by the user. Plans never write it; the app
//! opens [`default_apps_settings_uri`] so the user can pick FastPDF there.

use std::fmt;
use std::path::Path;

#[cfg(windows)]
mod apply;
#[cfg(windows)]
pub use apply::{ApplyError, apply};

/// ProgID for PDF documents opened by FastPDF.
pub const PROG_ID: &str = "FastPDF.Document";
/// Display name in "Open with" and Default apps.
pub const APP_NAME: &str = "FastPDF";
/// Description shown in Default apps.
pub const APP_DESCRIPTION: &str = "Fast, lightweight PDF reader";
/// Type name of `FastPDF.Document`.
pub const DOCUMENT_TYPE_NAME: &str = "PDF Document";
/// Value name under `Software\RegisteredApplications`.
pub const REGISTERED_APP_NAME: &str = "FastPDF";
/// Capabilities key (relative to `HKEY_CURRENT_USER`).
pub const CAPABILITIES_KEY: &str = r"Software\FastPDF\Capabilities";
/// File extensions FastPDF registers for.
pub const EXTENSIONS: &[&str] = &[".pdf"];
/// Settings > Apps > Default apps.
pub const DEFAULT_APPS_SETTINGS_URI: &str = "ms-settings:defaultapps";

const CLASSES: &str = r"Software\Classes";
const VENDOR_KEY: &str = r"Software\FastPDF";
const REGISTERED_APPLICATIONS_KEY: &str = r"Software\RegisteredApplications";

/// Deep link to FastPDF's own page in Default apps (Windows 11; older builds
/// open the general Default apps page, which [`DEFAULT_APPS_SETTINGS_URI`] is).
pub fn default_apps_settings_uri() -> String {
    format!("{DEFAULT_APPS_SETTINGS_URI}?registeredAppUser={REGISTERED_APP_NAME}")
}

/// Registry hive of a plan. Plans are per-user only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hive {
    CurrentUser,
}

impl Hive {
    pub fn name(self) -> &'static str {
        match self {
            Self::CurrentUser => "HKEY_CURRENT_USER",
        }
    }
}

/// Registry value data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegData {
    /// `REG_SZ`.
    String(String),
    /// `REG_NONE` without data: the documented form of `OpenWithProgids` entries.
    None,
}

/// One registry operation; keys are relative to [`Plan::hive`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegOp {
    /// Create `key` if needed and set a value (`name: None` is the default value).
    SetValue {
        key: String,
        name: Option<String>,
        data: RegData,
    },
    /// Delete one value. A missing key or value is not an error.
    DeleteValue { key: String, name: String },
    /// Delete a key with all subkeys and values. A missing key is not an error.
    DeleteTree { key: String },
    /// Delete a key only when no subkeys and no values are left in it.
    DeleteKeyIfEmpty { key: String },
}

impl RegOp {
    pub fn key(&self) -> &str {
        match self {
            Self::SetValue { key, .. }
            | Self::DeleteValue { key, .. }
            | Self::DeleteTree { key }
            | Self::DeleteKeyIfEmpty { key } => key,
        }
    }

    fn set(key: impl Into<String>, name: Option<&str>, data: RegData) -> Self {
        Self::SetValue {
            key: key.into(),
            name: name.map(str::to_owned),
            data,
        }
    }

    fn set_str(key: impl Into<String>, name: Option<&str>, value: impl Into<String>) -> Self {
        Self::set(key, name, RegData::String(value.into()))
    }
}

impl fmt::Display for RegOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = |n: &Option<String>| n.clone().unwrap_or_else(|| "(Default)".into());
        match self {
            Self::SetValue { key, name: n, data } => match data {
                RegData::String(s) => write!(f, r"set {key}\{} = {s:?}", name(n)),
                RegData::None => write!(f, r"set {key}\{} = (REG_NONE)", name(n)),
            },
            Self::DeleteValue { key, name } => write!(f, r"delete value {key}\{name}"),
            Self::DeleteTree { key } => write!(f, "delete key {key} (with subkeys)"),
            Self::DeleteKeyIfEmpty { key } => write!(f, "delete key {key} if empty"),
        }
    }
}

/// Registry operations to run in order, then notify the shell
/// (`SHChangeNotify(SHCNE_ASSOCCHANGED)`) so Explorer picks up the change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub hive: Hive,
    pub ops: Vec<RegOp>,
    pub notify_shell: bool,
}

impl Plan {
    /// The plan as a `.reg` file for review or manual use. regedit expects
    /// such files in UTF-16 LE with a BOM when they contain non-ASCII text.
    /// `DeleteKeyIfEmpty` has no `.reg` equivalent and becomes a comment.
    pub fn to_reg_file(&self) -> String {
        let hive = self.hive.name();
        let mut out = String::from("Windows Registry Editor Version 5.00\r\n");
        let mut section: Option<&str> = None;
        for op in &self.ops {
            match op {
                RegOp::SetValue { key, name, data } => {
                    if section != Some(key.as_str()) {
                        out.push_str(&format!("\r\n[{hive}\\{key}]\r\n"));
                        section = Some(key.as_str());
                    }
                    let lhs = name
                        .as_deref()
                        .map_or_else(|| "@".to_owned(), |n| format!("\"{}\"", reg_escape(n)));
                    let rhs = match data {
                        RegData::String(s) => format!("\"{}\"", reg_escape(s)),
                        RegData::None => "hex(0):".to_owned(),
                    };
                    out.push_str(&format!("{lhs}={rhs}\r\n"));
                }
                RegOp::DeleteValue { key, name } => {
                    if section != Some(key.as_str()) {
                        out.push_str(&format!("\r\n[{hive}\\{key}]\r\n"));
                        section = Some(key.as_str());
                    }
                    out.push_str(&format!("\"{}\"=-\r\n", reg_escape(name)));
                }
                RegOp::DeleteTree { key } => {
                    out.push_str(&format!("\r\n[-{hive}\\{key}]\r\n"));
                    section = None;
                }
                RegOp::DeleteKeyIfEmpty { key } => {
                    out.push_str(&format!("\r\n; delete [{hive}\\{key}] if it is empty\r\n"));
                    section = None;
                }
            }
        }
        out
    }
}

fn reg_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Why an executable path cannot be registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// Registry strings are UTF-16; the path has unpaired surrogates.
    NotUnicode,
    /// Not a full path (`C:\...` or `\\server\share\...`).
    NotAbsolute(String),
    /// `"` cannot be quoted in a command line, `%` would be expanded by the
    /// shell's command templates, control characters are never valid.
    UnsupportedCharacter { path: String, character: char },
    /// The path ends in a separator.
    NoFileName(String),
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotUnicode => f.write_str("the executable path is not valid Unicode"),
            Self::NotAbsolute(p) => write!(f, "not an absolute path: {p}"),
            Self::UnsupportedCharacter { path, character } => {
                write!(f, "unsupported character {character:?} in path: {path}")
            }
            Self::NoFileName(p) => write!(f, "no file name in path: {p}"),
        }
    }
}

impl std::error::Error for PlanError {}

/// A validated FastPDF executable location, from which plans are built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    exe: String,
    file_name: String,
}

impl Registration {
    /// Validates and normalizes `exe` (verbatim `\\?\` prefixes are removed and
    /// `/` becomes `\`). Pass the installed location, e.g.
    /// `std::env::current_exe()`.
    pub fn new(exe: impl AsRef<Path>) -> Result<Self, PlanError> {
        let raw = exe.as_ref().to_str().ok_or(PlanError::NotUnicode)?;
        let exe = normalize(raw);
        if let Some(character) = exe
            .chars()
            .find(|&c| c == '"' || c == '%' || c.is_control())
        {
            return Err(PlanError::UnsupportedCharacter {
                path: exe,
                character,
            });
        }
        if !is_absolute(&exe) {
            return Err(PlanError::NotAbsolute(exe));
        }
        let file_name = match exe.rsplit('\\').next() {
            Some(name) if !name.is_empty() => name.to_owned(),
            _ => return Err(PlanError::NoFileName(exe)),
        };
        Ok(Self { exe, file_name })
    }

    /// Normalized executable path.
    pub fn exe(&self) -> &str {
        &self.exe
    }

    /// `shell\open\command` value: quoted executable and quoted `%1`, so
    /// spaces and non-ASCII characters in either path are safe.
    pub fn open_command(&self) -> String {
        format!("\"{}\" \"%1\"", self.exe)
    }

    /// `DefaultIcon` value: the executable's first icon resource.
    pub fn icon_location(&self) -> String {
        format!("\"{}\",0", self.exe)
    }

    fn prog_id_key(&self) -> String {
        format!(r"{CLASSES}\{PROG_ID}")
    }

    fn application_key(&self) -> String {
        format!(r"{CLASSES}\Applications\{}", self.file_name)
    }

    /// Writes that add FastPDF to "Open with" and Default apps.
    pub fn register_plan(&self) -> Plan {
        let prog = self.prog_id_key();
        let app = self.application_key();
        let command = self.open_command();
        let icon = self.icon_location();
        let mut ops = vec![
            // ProgID
            RegOp::set_str(&prog, None, DOCUMENT_TYPE_NAME),
            RegOp::set_str(format!(r"{prog}\DefaultIcon"), None, &icon),
            RegOp::set_str(format!(r"{prog}\shell"), None, "open"),
            RegOp::set_str(format!(r"{prog}\shell\open\command"), None, &command),
            RegOp::set_str(
                format!(r"{prog}\Application"),
                Some("ApplicationName"),
                APP_NAME,
            ),
            RegOp::set_str(
                format!(r"{prog}\Application"),
                Some("ApplicationIcon"),
                &icon,
            ),
            RegOp::set_str(
                format!(r"{prog}\Application"),
                Some("ApplicationDescription"),
                APP_DESCRIPTION,
            ),
        ];
        // "Open with" for each extension (never the extension's default).
        for ext in EXTENSIONS {
            ops.push(RegOp::set(
                format!(r"{CLASSES}\{ext}\OpenWithProgids"),
                Some(PROG_ID),
                RegData::None,
            ));
        }
        // Application entry (Open with > Choose another app).
        ops.push(RegOp::set_str(&app, Some("FriendlyAppName"), APP_NAME));
        ops.push(RegOp::set_str(format!(r"{app}\DefaultIcon"), None, &icon));
        ops.push(RegOp::set_str(
            format!(r"{app}\shell\open\command"),
            None,
            &command,
        ));
        for ext in EXTENSIONS {
            ops.push(RegOp::set_str(
                format!(r"{app}\SupportedTypes"),
                Some(ext),
                "",
            ));
        }
        // Capabilities, then the RegisteredApplications value that points at
        // them last, so Settings never sees a half-written entry.
        ops.push(RegOp::set_str(
            CAPABILITIES_KEY,
            Some("ApplicationName"),
            APP_NAME,
        ));
        ops.push(RegOp::set_str(
            CAPABILITIES_KEY,
            Some("ApplicationDescription"),
            APP_DESCRIPTION,
        ));
        ops.push(RegOp::set_str(
            CAPABILITIES_KEY,
            Some("ApplicationIcon"),
            &icon,
        ));
        for ext in EXTENSIONS {
            ops.push(RegOp::set_str(
                format!(r"{CAPABILITIES_KEY}\FileAssociations"),
                Some(ext),
                PROG_ID,
            ));
        }
        ops.push(RegOp::set_str(
            REGISTERED_APPLICATIONS_KEY,
            Some(REGISTERED_APP_NAME),
            CAPABILITIES_KEY,
        ));
        Plan {
            hive: Hive::CurrentUser,
            ops,
            notify_shell: true,
        }
    }

    /// Removes everything [`Self::register_plan`] writes, in reverse order.
    /// Shared keys (`.pdf`, `OpenWithProgids`, `RegisteredApplications`) only
    /// lose FastPDF's values. A `UserChoice` that still names FastPDF is left
    /// to Windows, which falls back to asking the user.
    pub fn unregister_plan(&self) -> Plan {
        let mut ops = vec![
            RegOp::DeleteValue {
                key: REGISTERED_APPLICATIONS_KEY.into(),
                name: REGISTERED_APP_NAME.into(),
            },
            RegOp::DeleteTree {
                key: CAPABILITIES_KEY.into(),
            },
            RegOp::DeleteKeyIfEmpty {
                key: VENDOR_KEY.into(),
            },
            RegOp::DeleteTree {
                key: self.application_key(),
            },
        ];
        for ext in EXTENSIONS {
            ops.push(RegOp::DeleteValue {
                key: format!(r"{CLASSES}\{ext}\OpenWithProgids"),
                name: PROG_ID.into(),
            });
        }
        ops.push(RegOp::DeleteTree {
            key: self.prog_id_key(),
        });
        Plan {
            hive: Hive::CurrentUser,
            ops,
            notify_shell: true,
        }
    }
}

/// `\\?\C:\x` -> `C:\x`, `\\?\UNC\srv\share\x` -> `\\srv\share\x`, `/` -> `\`.
fn normalize(path: &str) -> String {
    let path = path.replace('/', "\\");
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        path
    }
}

/// Drive-absolute (`C:\...`) or UNC (`\\server\share\...`); drive-relative
/// (`C:x`) and rooted-without-drive (`\x`) paths are rejected.
fn is_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    let drive =
        bytes.len() > 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\';
    let unc = path
        .strip_prefix(r"\\")
        .is_some_and(|rest| rest.split('\\').filter(|part| !part.is_empty()).count() >= 3);
    drive || unc
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPACED_CJK: &str = r"C:\Program Files\FastPDF 測試\fastpdf.exe";

    fn value<'a>(plan: &'a Plan, key: &str, name: Option<&str>) -> Option<&'a RegData> {
        plan.ops.iter().find_map(|op| match op {
            RegOp::SetValue {
                key: k,
                name: n,
                data,
            } if k == key && n.as_deref() == name => Some(data),
            _ => None,
        })
    }

    fn string<'a>(plan: &'a Plan, key: &str, name: Option<&str>) -> &'a str {
        match value(plan, key, name) {
            Some(RegData::String(s)) => s,
            other => panic!("{key} {name:?}: expected a string, got {other:?}"),
        }
    }

    #[test]
    fn quotes_paths_with_spaces_and_cjk() {
        let reg = Registration::new(SPACED_CJK).unwrap();
        assert_eq!(
            reg.open_command(),
            r#""C:\Program Files\FastPDF 測試\fastpdf.exe" "%1""#
        );
        assert_eq!(
            reg.icon_location(),
            r#""C:\Program Files\FastPDF 測試\fastpdf.exe",0"#
        );
        let plan = reg.register_plan();
        assert_eq!(
            string(
                &plan,
                r"Software\Classes\FastPDF.Document\shell\open\command",
                None
            ),
            reg.open_command()
        );
        assert_eq!(
            string(
                &plan,
                r"Software\Classes\Applications\fastpdf.exe\shell\open\command",
                None
            ),
            reg.open_command()
        );
        assert_eq!(
            string(
                &plan,
                r"Software\Classes\FastPDF.Document\DefaultIcon",
                None
            ),
            reg.icon_location()
        );
    }

    #[test]
    fn registers_open_with_and_default_apps_capabilities() {
        let plan = Registration::new(SPACED_CJK).unwrap().register_plan();
        assert_eq!(plan.hive, Hive::CurrentUser);
        assert!(plan.notify_shell);
        // "Open with": ProgID listed for .pdf, never as the extension's default.
        assert_eq!(
            value(
                &plan,
                r"Software\Classes\.pdf\OpenWithProgids",
                Some(PROG_ID)
            ),
            Some(&RegData::None)
        );
        assert_eq!(value(&plan, r"Software\Classes\.pdf", None), None);
        assert_eq!(
            string(
                &plan,
                r"Software\Classes\Applications\fastpdf.exe",
                Some("FriendlyAppName")
            ),
            "FastPDF"
        );
        assert_eq!(
            string(
                &plan,
                r"Software\Classes\Applications\fastpdf.exe\SupportedTypes",
                Some(".pdf")
            ),
            ""
        );
        // Default apps: capabilities plus the RegisteredApplications pointer, last.
        assert_eq!(
            string(
                &plan,
                r"Software\FastPDF\Capabilities\FileAssociations",
                Some(".pdf")
            ),
            PROG_ID
        );
        assert_eq!(
            string(
                &plan,
                r"Software\FastPDF\Capabilities",
                Some("ApplicationName")
            ),
            "FastPDF"
        );
        assert_eq!(
            plan.ops.last(),
            Some(&RegOp::SetValue {
                key: r"Software\RegisteredApplications".into(),
                name: Some("FastPDF".into()),
                data: RegData::String(CAPABILITIES_KEY.into()),
            })
        );
    }

    #[test]
    fn never_touches_protected_or_machine_wide_state() {
        let reg = Registration::new(SPACED_CJK).unwrap();
        for plan in [reg.register_plan(), reg.unregister_plan()] {
            for op in &plan.ops {
                let key = op.key();
                assert!(key.starts_with(r"Software\"), "{op}");
                assert!(!key.contains("UserChoice"), "{op}");
                assert!(!key.contains(r"Explorer\FileExts"), "{op}");
                assert!(!key.starts_with("HKEY_"), "keys are hive-relative: {op}");
            }
        }
    }

    #[test]
    fn unregister_removes_everything_registered() {
        let reg = Registration::new(SPACED_CJK).unwrap();
        let undo = reg.unregister_plan();
        for op in reg.register_plan().ops {
            let RegOp::SetValue { key, name, .. } = &op else {
                panic!("register plans only set values");
            };
            let removed = undo.ops.iter().any(|u| match u {
                RegOp::DeleteTree { key: tree } => {
                    key.eq_ignore_ascii_case(tree)
                        || key
                            .to_ascii_lowercase()
                            .starts_with(&format!("{}\\", tree.to_ascii_lowercase()))
                }
                RegOp::DeleteValue { key: k, name: n } => {
                    k.eq_ignore_ascii_case(key) && Some(n.as_str()) == name.as_deref()
                }
                _ => false,
            });
            assert!(removed, "not undone: {op}");
        }
    }

    #[test]
    fn unregister_keeps_shared_keys() {
        let undo = Registration::new(SPACED_CJK).unwrap().unregister_plan();
        for op in &undo.ops {
            if let RegOp::DeleteTree { key } = op {
                let k = key.to_ascii_lowercase();
                assert!(
                    k.ends_with(r"\fastpdf.document")
                        || k.ends_with(r"\applications\fastpdf.exe")
                        || k == r"software\fastpdf\capabilities",
                    "deletes a shared tree: {op}"
                );
            }
        }
        assert!(undo.ops.contains(&RegOp::DeleteValue {
            key: r"Software\Classes\.pdf\OpenWithProgids".into(),
            name: PROG_ID.into(),
        }));
    }

    #[test]
    fn application_key_follows_the_executable_name() {
        let plan = Registration::new(r"D:\Apps\FastPDF-portable.exe")
            .unwrap()
            .register_plan();
        assert!(
            value(
                &plan,
                r"Software\Classes\Applications\FastPDF-portable.exe",
                Some("FriendlyAppName")
            )
            .is_some()
        );
    }

    #[test]
    fn normalizes_verbatim_unc_and_forward_slashes() {
        let cases = [
            (r"\\?\C:\Tools\fastpdf.exe", r"C:\Tools\fastpdf.exe"),
            (
                "C:/Tools/FastPDF/fastpdf.exe",
                r"C:\Tools\FastPDF\fastpdf.exe",
            ),
            (
                r"\\?\UNC\server\share\fastpdf.exe",
                r"\\server\share\fastpdf.exe",
            ),
            (
                r"\\server\share\apps\fastpdf.exe",
                r"\\server\share\apps\fastpdf.exe",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(Registration::new(input).unwrap().exe(), expected, "{input}");
        }
    }

    #[test]
    fn rejects_unusable_paths() {
        let not_absolute = [
            "fastpdf.exe",
            r"C:fastpdf.exe",
            r"\Tools\fastpdf.exe",
            r"\\server\fastpdf.exe",
        ];
        for p in not_absolute {
            assert!(
                matches!(Registration::new(p), Err(PlanError::NotAbsolute(_))),
                "{p}"
            );
        }
        for (p, c) in [
            (r"C:\100%\fastpdf.exe", '%'),
            ("C:\\a\"b\\fastpdf.exe", '"'),
            ("C:\\a\tb\\fastpdf.exe", '\t'),
        ] {
            assert_eq!(
                Registration::new(p),
                Err(PlanError::UnsupportedCharacter {
                    path: p.into(),
                    character: c
                }),
                "{p}"
            );
        }
        assert!(matches!(
            Registration::new(r"C:\Tools\"),
            Err(PlanError::NoFileName(_))
        ));
    }

    #[test]
    fn reg_file_escapes_quotes_and_backslashes() {
        let reg = Registration::new(SPACED_CJK).unwrap();
        let text = reg.register_plan().to_reg_file();
        assert!(text.starts_with("Windows Registry Editor Version 5.00\r\n"));
        assert!(text.contains(
            "[HKEY_CURRENT_USER\\Software\\Classes\\FastPDF.Document\\shell\\open\\command]\r\n"
        ));
        assert!(text.contains(r#"@="\"C:\\Program Files\\FastPDF 測試\\fastpdf.exe\" \"%1\"""#));
        assert!(text.contains("\"FastPDF.Document\"=hex(0):\r\n"));
        let undo = reg.unregister_plan().to_reg_file();
        assert!(undo.contains("[-HKEY_CURRENT_USER\\Software\\Classes\\FastPDF.Document]"));
        assert!(undo.contains("\"FastPDF\"=-"));
        assert!(undo.contains("; delete [HKEY_CURRENT_USER\\Software\\FastPDF] if it is empty"));
    }

    #[test]
    fn default_apps_links() {
        assert_eq!(DEFAULT_APPS_SETTINGS_URI, "ms-settings:defaultapps");
        assert_eq!(
            default_apps_settings_uri(),
            "ms-settings:defaultapps?registeredAppUser=FastPDF"
        );
    }

    #[cfg(windows)]
    #[test]
    fn executor_signature_is_stable() {
        // Type check only: the executor is never run by tests.
        let _apply: fn(&Plan) -> Result<usize, ApplyError> = apply;
    }
}
