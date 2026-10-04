//! Registry executor for [`Plan`]s (Windows only). Tests never call it: it
//! changes the current user's registry. The CLI wiring (for example
//! `fastpdf --register-file-types`) lives in `fastpdf-app`.
#![allow(unsafe_code)]

use std::fmt;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_NONE, REG_OPTION_NON_VOLATILE,
    REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegDeleteKeyW, RegDeleteTreeW,
    RegOpenKeyExW, RegQueryInfoKeyW, RegSetValueExW,
};
use windows_sys::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};

use crate::{Hive, Plan, RegData, RegOp};

/// A registry call failed. Operations before `index` were applied; running
/// the matching unregister plan cleans up a partial registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyError {
    pub index: usize,
    pub op: String,
    pub code: u32,
}

impl fmt::Display for ApplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "registry operation {} failed with Win32 error {}: {}",
            self.index, self.code, self.op
        )
    }
}

impl std::error::Error for ApplyError {}

/// Runs the plan against the registry, then notifies the shell. Returns the
/// number of operations applied.
pub fn apply(plan: &Plan) -> Result<usize, ApplyError> {
    let root = match plan.hive {
        Hive::CurrentUser => HKEY_CURRENT_USER,
    };
    for (index, op) in plan.ops.iter().enumerate() {
        let status = match op {
            RegOp::SetValue { key, name, data } => set_value(root, key, name.as_deref(), data),
            RegOp::DeleteValue { key, name } => {
                let (key, name) = (wide(key), wide(name));
                // SAFETY: NUL-terminated UTF-16 strings that outlive the call.
                missing_ok(unsafe { RegDeleteKeyValueW(root, key.as_ptr(), name.as_ptr()) })
            }
            RegOp::DeleteTree { key } => delete_tree(root, key),
            RegOp::DeleteKeyIfEmpty { key } => delete_if_empty(root, key),
        };
        if status != ERROR_SUCCESS {
            return Err(ApplyError {
                index,
                op: op.to_string(),
                code: status,
            });
        }
    }
    if plan.notify_shell {
        // SAFETY: SHCNE_ASSOCCHANGED takes no items.
        unsafe {
            SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST, null(), null());
        }
    }
    Ok(plan.ops.len())
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn missing_ok(status: WIN32_ERROR) -> WIN32_ERROR {
    if status == ERROR_FILE_NOT_FOUND {
        ERROR_SUCCESS
    } else {
        status
    }
}

fn set_value(root: HKEY, key: &str, name: Option<&str>, data: &RegData) -> WIN32_ERROR {
    let key = wide(key);
    let mut hkey: HKEY = null_mut();
    // SAFETY: valid NUL-terminated key name and out-pointer; no class or
    // security attributes.
    let status = unsafe {
        RegCreateKeyExW(
            root,
            key.as_ptr(),
            0,
            null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            null(),
            &mut hkey,
            null_mut(),
        )
    };
    if status != ERROR_SUCCESS {
        return status;
    }
    let name = name.map(wide);
    let name_ptr = name.as_ref().map_or(null(), |n| n.as_ptr());
    let status = match data {
        RegData::String(s) => {
            let value = wide(s);
            let bytes = u32::try_from(value.len() * 2).unwrap_or(u32::MAX);
            // SAFETY: `value` holds `bytes` bytes of UTF-16 including the NUL.
            unsafe { RegSetValueExW(hkey, name_ptr, 0, REG_SZ, value.as_ptr().cast(), bytes) }
        }
        // SAFETY: REG_NONE with no data.
        RegData::None => unsafe { RegSetValueExW(hkey, name_ptr, 0, REG_NONE, null(), 0) },
    };
    // SAFETY: `hkey` was opened above.
    unsafe { RegCloseKey(hkey) };
    status
}

fn delete_tree(root: HKEY, key: &str) -> WIN32_ERROR {
    let key = wide(key);
    // SAFETY: NUL-terminated key name. RegDeleteTreeW removes the subkeys and
    // values; RegDeleteKeyW then removes the (now empty) key itself.
    let status = missing_ok(unsafe { RegDeleteTreeW(root, key.as_ptr()) });
    if status != ERROR_SUCCESS {
        return status;
    }
    // SAFETY: as above.
    missing_ok(unsafe { RegDeleteKeyW(root, key.as_ptr()) })
}

fn delete_if_empty(root: HKEY, key: &str) -> WIN32_ERROR {
    let key = wide(key);
    let mut hkey: HKEY = null_mut();
    // SAFETY: NUL-terminated key name and out-pointer.
    let status = unsafe { RegOpenKeyExW(root, key.as_ptr(), 0, KEY_QUERY_VALUE, &mut hkey) };
    if status != ERROR_SUCCESS {
        return missing_ok(status);
    }
    let (mut subkeys, mut values) = (0u32, 0u32);
    // SAFETY: `hkey` is open; every unused out-parameter is null.
    let status = unsafe {
        RegQueryInfoKeyW(
            hkey,
            null_mut(),
            null_mut(),
            null(),
            &mut subkeys,
            null_mut(),
            null_mut(),
            &mut values,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
        )
    };
    // SAFETY: `hkey` was opened above.
    unsafe { RegCloseKey(hkey) };
    if status != ERROR_SUCCESS {
        return status;
    }
    if subkeys != 0 || values != 0 {
        return ERROR_SUCCESS;
    }
    // SAFETY: NUL-terminated key name.
    missing_ok(unsafe { RegDeleteKeyW(root, key.as_ptr()) })
}
