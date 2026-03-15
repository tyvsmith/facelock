use facelock_core::dbus_interface::*;
use zvariant::DynamicType;

#[test]
fn auth_result_has_dbus_signature() {
    let val = AuthResult {
        matched: false,
        model_id: -1,
        label: String::new(),
        similarity: 0.0,
    };
    let sig = val.signature();
    assert!(!sig.to_string().is_empty());
}

#[test]
fn model_info_has_dbus_signature() {
    let val = ModelInfo {
        id: 0,
        user: String::new(),
        label: String::new(),
        created_at: 0,
    };
    let sig = val.signature();
    assert!(!sig.to_string().is_empty());
}

#[test]
fn preview_face_info_has_dbus_signature() {
    let val = PreviewFaceInfo {
        x: 0.0,
        y: 0.0,
        width: 0.0,
        height: 0.0,
        confidence: 0.0,
        similarity: 0.0,
        recognized: false,
    };
    let sig = val.signature();
    assert!(!sig.to_string().is_empty());
}

#[test]
fn device_info_has_dbus_signature() {
    let val = DeviceInfo {
        path: String::new(),
        name: String::new(),
        driver: String::new(),
        is_ir: false,
    };
    let sig = val.signature();
    assert!(!sig.to_string().is_empty());
}

#[test]
fn constants_correct() {
    assert_eq!(BUS_NAME, "org.facelock.Daemon");
    assert_eq!(OBJECT_PATH, "/org/facelock/Daemon");
    assert_eq!(INTERFACE_NAME, "org.facelock.Daemon");
}
