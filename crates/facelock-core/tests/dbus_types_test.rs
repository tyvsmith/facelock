//! Conformance test for gap E3: pin the exact D-Bus wire signature of every
//! type crossing the bus, plus the bus name/path/interface constants.
//!
//! D-Bus is the *only* auth path for user-run lockers (hyprlock, swaylock —
//! they cannot read the `0600 root:root` key, see gap-register.md DEC-2), so
//! an accidental wire-format change during the upgrade window (old client,
//! new daemon or vice versa) is a face-auth outage, not just a test failure.
//! The previous version of this file asserted `!sig.is_empty()`, which
//! cannot fail for any struct — it caught zero regressions. Every assertion
//! below is an exact string: if a field is added, removed, reordered, or
//! retyped on any of these structs, this file must be touched deliberately.

use facelock_core::dbus_interface::*;
use zvariant::{DynamicType, Type};

#[test]
fn auth_result_signature_is_pinned() {
    let val = AuthResult {
        matched: false,
        model_id: -1,
        label: String::new(),
        similarity: 0.0,
    };
    // bool, i32, String, f64 -> b, i, s, d
    assert_eq!(val.signature().to_string(), "(bisd)");
}

/// `AuthResult` is the reply of **two** methods now: `Authenticate` (real
/// authentication, always charges the rate limit) and the root-only
/// `TestAuthenticate` (the diagnostic entry point behind `facelock test`,
/// which charges nothing). They deliberately share one wire shape — a single
/// `s` username in, one `AuthResult` out, with the same `-1`/`-2`/`-3`
/// sentinels — so the in-band encoding cannot drift between them, and so the
/// upgrade-window concern above applies once rather than twice.
///
/// The method *names* cannot be pinned from this crate: zbus derives them
/// from the `#[interface]` function names in facelock-daemon, which pins the
/// wire method set against its authorization matrix in
/// `server::tests::interface_methods_and_the_authz_matrix_are_the_same_set`.
/// What is pinned here is the shape a client must be able to encode and
/// decode for either method.
#[test]
fn both_authentication_methods_share_the_pinned_wire_shape() {
    // The argument list: one username, for both methods.
    assert_eq!(<(String,) as Type>::SIGNATURE.to_string(), "(s)");

    let reply = AuthResult {
        matched: false,
        model_id: -2,
        label: "rate limited".into(),
        similarity: 0.0,
    };
    assert_eq!(reply.signature().to_string(), "(bisd)");
}

#[test]
fn model_info_signature_is_pinned() {
    let val = ModelInfo {
        id: 0,
        user: String::new(),
        label: String::new(),
        created_at: 0,
        embedder_model: String::new(),
        device_id: String::new(),
        key_id: String::new(),
    };
    // u32, String, String, u64, String, String, String -> u, s, s, t, s, s, s
    assert_eq!(val.signature().to_string(), "(usstsss)");
}

#[test]
fn preview_face_info_signature_is_pinned() {
    let val = PreviewFaceInfo {
        x: 0.0,
        y: 0.0,
        width: 0.0,
        height: 0.0,
        confidence: 0.0,
        similarity: 0.0,
        recognized: false,
    };
    // f64 x5, then bool -> d, d, d, d, d, d, b
    assert_eq!(val.signature().to_string(), "(ddddddb)");
}

#[test]
fn device_info_signature_is_pinned() {
    let val = DeviceInfo {
        path: String::new(),
        name: String::new(),
        driver: String::new(),
        is_ir: false,
    };
    // String x3, bool -> s, s, s, b
    assert_eq!(val.signature().to_string(), "(sssb)");
}

#[test]
fn constants_correct() {
    assert_eq!(BUS_NAME, "org.facelock.Daemon");
    assert_eq!(OBJECT_PATH, "/org/facelock/Daemon");
    assert_eq!(INTERFACE_NAME, "org.facelock.Daemon");
}

/// The three D-Bus identity constants are compared for byte-for-byte
/// equality *to each other* in addition to their literal values above: the
/// bus name and interface name happen to coincide today (a single-object,
/// single-interface daemon), but nothing enforces that they must — pin both
/// facts independently so a future divergence is a deliberate edit here, not
/// a surprise at connection time.
#[test]
fn bus_name_and_interface_name_currently_coincide() {
    assert_eq!(BUS_NAME, INTERFACE_NAME);
}
