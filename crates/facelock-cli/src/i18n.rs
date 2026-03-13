//! Lightweight gettext wrapper for CLI internationalization.
//!
//! Uses libc::dgettext directly to avoid adding the gettext-rs build dependency.
//! Falls back to the original English string if no translation is available.
//!
//! Translation files (.mo) should be installed to:
//!   /usr/share/locale/<LANG>/LC_MESSAGES/facelock.mo

use std::ffi::{CStr, CString};

/// Text domain for CLI translations.
const GETTEXT_DOMAIN: &[u8] = b"facelock\0";

unsafe extern "C" {
    safe fn dgettext(
        domainname: *const libc::c_char,
        msgid: *const libc::c_char,
    ) -> *const libc::c_char;
    safe fn setlocale(category: libc::c_int, locale: *const libc::c_char) -> *const libc::c_char;
}

/// Translate a message string using gettext.
/// Returns the original string if no translation is found.
pub fn gettext(msgid: &str) -> String {
    let Ok(c_msgid) = CString::new(msgid) else {
        return msgid.to_string();
    };

    let result = dgettext(GETTEXT_DOMAIN.as_ptr().cast(), c_msgid.as_ptr());
    if result.is_null() {
        return msgid.to_string();
    }
    unsafe { CStr::from_ptr(result) }
        .to_str()
        .unwrap_or(msgid)
        .to_string()
}

/// Initialize gettext for the CLI. Call once at startup.
/// Sets the text domain and binds to the standard locale directory.
pub fn init() {
    let empty = b"\0";
    setlocale(libc::LC_ALL, empty.as_ptr().cast());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gettext_fallback_returns_original() {
        // Without .mo files installed, gettext returns the original string
        let result = gettext("Hello, world!");
        assert_eq!(result, "Hello, world!");
    }

    #[test]
    fn gettext_handles_empty_string() {
        let result = gettext("");
        // gettext("") returns the metadata string or empty
        // Either way, it should not panic
        let _ = result;
    }
}
