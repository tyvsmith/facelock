//! Integration tests for IPC round-trip serialization and wire protocol.

use std::io::Cursor;
use std::os::unix::net::UnixStream;

use visage_core::ipc::{
    decode_request, decode_response, encode_request, encode_response, recv_message, send_message,
    DaemonRequest, DaemonResponse, MAX_MESSAGE_SIZE,
};
use visage_core::types::{FaceModelInfo, MatchResult};

// ---------------------------------------------------------------------------
// DaemonRequest round-trip tests
// ---------------------------------------------------------------------------

#[test]
fn request_round_trip_authenticate() {
    let req = DaemonRequest::Authenticate {
        user: "alice".into(),
    };
    let bytes = encode_request(&req).unwrap();
    let decoded = decode_request(&bytes).unwrap();
    match decoded {
        DaemonRequest::Authenticate { user } => assert_eq!(user, "alice"),
        other => panic!("expected Authenticate, got {other:?}"),
    }
}

#[test]
fn request_round_trip_enroll() {
    let req = DaemonRequest::Enroll {
        user: "bob".into(),
        label: "office-desk".into(),
    };
    let bytes = encode_request(&req).unwrap();
    let decoded = decode_request(&bytes).unwrap();
    match decoded {
        DaemonRequest::Enroll { user, label } => {
            assert_eq!(user, "bob");
            assert_eq!(label, "office-desk");
        }
        other => panic!("expected Enroll, got {other:?}"),
    }
}

#[test]
fn request_round_trip_list_models() {
    let req = DaemonRequest::ListModels {
        user: "charlie".into(),
    };
    let bytes = encode_request(&req).unwrap();
    let decoded = decode_request(&bytes).unwrap();
    match decoded {
        DaemonRequest::ListModels { user } => assert_eq!(user, "charlie"),
        other => panic!("expected ListModels, got {other:?}"),
    }
}

#[test]
fn request_round_trip_remove_model() {
    let req = DaemonRequest::RemoveModel {
        user: "dave".into(),
        model_id: 42,
    };
    let bytes = encode_request(&req).unwrap();
    let decoded = decode_request(&bytes).unwrap();
    match decoded {
        DaemonRequest::RemoveModel { user, model_id } => {
            assert_eq!(user, "dave");
            assert_eq!(model_id, 42);
        }
        other => panic!("expected RemoveModel, got {other:?}"),
    }
}

#[test]
fn request_round_trip_clear_models() {
    let req = DaemonRequest::ClearModels {
        user: "eve".into(),
    };
    let bytes = encode_request(&req).unwrap();
    let decoded = decode_request(&bytes).unwrap();
    match decoded {
        DaemonRequest::ClearModels { user } => assert_eq!(user, "eve"),
        other => panic!("expected ClearModels, got {other:?}"),
    }
}

#[test]
fn request_round_trip_preview_frame() {
    let req = DaemonRequest::PreviewFrame;
    let bytes = encode_request(&req).unwrap();
    let decoded = decode_request(&bytes).unwrap();
    assert!(
        matches!(decoded, DaemonRequest::PreviewFrame),
        "expected PreviewFrame, got {decoded:?}"
    );
}

#[test]
fn request_round_trip_ping() {
    let req = DaemonRequest::Ping;
    let bytes = encode_request(&req).unwrap();
    let decoded = decode_request(&bytes).unwrap();
    assert!(
        matches!(decoded, DaemonRequest::Ping),
        "expected Ping, got {decoded:?}"
    );
}

#[test]
fn request_round_trip_shutdown() {
    let req = DaemonRequest::Shutdown;
    let bytes = encode_request(&req).unwrap();
    let decoded = decode_request(&bytes).unwrap();
    assert!(
        matches!(decoded, DaemonRequest::Shutdown),
        "expected Shutdown, got {decoded:?}"
    );
}

#[test]
fn request_round_trip_release_camera() {
    let req = DaemonRequest::ReleaseCamera;
    let bytes = encode_request(&req).unwrap();
    let decoded = decode_request(&bytes).unwrap();
    assert!(
        matches!(decoded, DaemonRequest::ReleaseCamera),
        "expected ReleaseCamera, got {decoded:?}"
    );
}

// ---------------------------------------------------------------------------
// DaemonResponse round-trip tests
// ---------------------------------------------------------------------------

#[test]
fn response_round_trip_auth_result_matched() {
    let resp = DaemonResponse::AuthResult(MatchResult {
        matched: true,
        model_id: Some(7),
        label: Some("home-camera".into()),
        similarity: 0.92,
    });
    let bytes = encode_response(&resp).unwrap();
    let decoded = decode_response(&bytes).unwrap();
    match decoded {
        DaemonResponse::AuthResult(m) => {
            assert!(m.matched);
            assert_eq!(m.model_id, Some(7));
            assert_eq!(m.label.as_deref(), Some("home-camera"));
            assert!((m.similarity - 0.92).abs() < 1e-6);
        }
        other => panic!("expected AuthResult, got {other:?}"),
    }
}

#[test]
fn response_round_trip_auth_result_not_matched() {
    let resp = DaemonResponse::AuthResult(MatchResult {
        matched: false,
        model_id: None,
        label: None,
        similarity: 0.15,
    });
    let bytes = encode_response(&resp).unwrap();
    let decoded = decode_response(&bytes).unwrap();
    match decoded {
        DaemonResponse::AuthResult(m) => {
            assert!(!m.matched);
            assert_eq!(m.model_id, None);
            assert_eq!(m.label, None);
            assert!((m.similarity - 0.15).abs() < 1e-6);
        }
        other => panic!("expected AuthResult, got {other:?}"),
    }
}

#[test]
fn response_round_trip_enrolled() {
    let resp = DaemonResponse::Enrolled {
        model_id: 99,
        embedding_count: 3,
    };
    let bytes = encode_response(&resp).unwrap();
    let decoded = decode_response(&bytes).unwrap();
    match decoded {
        DaemonResponse::Enrolled {
            model_id,
            embedding_count,
        } => {
            assert_eq!(model_id, 99);
            assert_eq!(embedding_count, 3);
        }
        other => panic!("expected Enrolled, got {other:?}"),
    }
}

#[test]
fn response_round_trip_models_empty() {
    let resp = DaemonResponse::Models(vec![]);
    let bytes = encode_response(&resp).unwrap();
    let decoded = decode_response(&bytes).unwrap();
    match decoded {
        DaemonResponse::Models(models) => assert!(models.is_empty()),
        other => panic!("expected Models, got {other:?}"),
    }
}

#[test]
fn response_round_trip_models_multiple() {
    let resp = DaemonResponse::Models(vec![
        FaceModelInfo {
            id: 1,
            user: "alice".into(),
            label: "default".into(),
            created_at: 1700000000,
        },
        FaceModelInfo {
            id: 2,
            user: "alice".into(),
            label: "glasses".into(),
            created_at: 1700001000,
        },
    ]);
    let bytes = encode_response(&resp).unwrap();
    let decoded = decode_response(&bytes).unwrap();
    match decoded {
        DaemonResponse::Models(models) => {
            assert_eq!(models.len(), 2);
            assert_eq!(models[0].id, 1);
            assert_eq!(models[0].label, "default");
            assert_eq!(models[1].id, 2);
            assert_eq!(models[1].label, "glasses");
            assert_eq!(models[1].created_at, 1700001000);
        }
        other => panic!("expected Models, got {other:?}"),
    }
}

#[test]
fn response_round_trip_removed() {
    let resp = DaemonResponse::Removed;
    let bytes = encode_response(&resp).unwrap();
    let decoded = decode_response(&bytes).unwrap();
    assert!(
        matches!(decoded, DaemonResponse::Removed),
        "expected Removed, got {decoded:?}"
    );
}

#[test]
fn response_round_trip_frame() {
    let fake_jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46];
    let resp = DaemonResponse::Frame {
        jpeg_data: fake_jpeg.clone(),
    };
    let bytes = encode_response(&resp).unwrap();
    let decoded = decode_response(&bytes).unwrap();
    match decoded {
        DaemonResponse::Frame { jpeg_data } => assert_eq!(jpeg_data, fake_jpeg),
        other => panic!("expected Frame, got {other:?}"),
    }
}

#[test]
fn response_round_trip_ok() {
    let resp = DaemonResponse::Ok;
    let bytes = encode_response(&resp).unwrap();
    let decoded = decode_response(&bytes).unwrap();
    assert!(
        matches!(decoded, DaemonResponse::Ok),
        "expected Ok, got {decoded:?}"
    );
}

#[test]
fn response_round_trip_error() {
    let resp = DaemonResponse::Error {
        message: "something went wrong".into(),
    };
    let bytes = encode_response(&resp).unwrap();
    let decoded = decode_response(&bytes).unwrap();
    match decoded {
        DaemonResponse::Error { message } => assert_eq!(message, "something went wrong"),
        other => panic!("expected Error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Large payload test
// ---------------------------------------------------------------------------

#[test]
fn response_round_trip_large_preview_frame() {
    // Simulate a realistic JPEG-like payload (~500KB)
    let mut jpeg_data = vec![0xFF, 0xD8, 0xFF, 0xE0]; // JPEG SOI marker
    jpeg_data.extend(vec![0xAB; 500_000]);
    jpeg_data.extend_from_slice(&[0xFF, 0xD9]); // JPEG EOI marker

    let resp = DaemonResponse::Frame {
        jpeg_data: jpeg_data.clone(),
    };
    let bytes = encode_response(&resp).unwrap();
    let decoded = decode_response(&bytes).unwrap();
    match decoded {
        DaemonResponse::Frame { jpeg_data: got } => {
            assert_eq!(got.len(), jpeg_data.len());
            assert_eq!(got, jpeg_data);
        }
        other => panic!("expected Frame, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Wire protocol tests using Unix socket pairs
// ---------------------------------------------------------------------------

#[test]
fn wire_protocol_request_over_unix_socket_pair() {
    let (mut tx, mut rx) = UnixStream::pair().unwrap();

    let req = DaemonRequest::Authenticate {
        user: "alice".into(),
    };
    let encoded = encode_request(&req).unwrap();
    send_message(&mut tx, &encoded).unwrap();

    let received = recv_message(&mut rx).unwrap();
    let decoded = decode_request(&received).unwrap();
    match decoded {
        DaemonRequest::Authenticate { user } => assert_eq!(user, "alice"),
        other => panic!("expected Authenticate, got {other:?}"),
    }
}

#[test]
fn wire_protocol_response_over_unix_socket_pair() {
    let (mut tx, mut rx) = UnixStream::pair().unwrap();

    let resp = DaemonResponse::AuthResult(MatchResult {
        matched: true,
        model_id: Some(5),
        label: Some("work".into()),
        similarity: 0.88,
    });
    let encoded = encode_response(&resp).unwrap();
    send_message(&mut tx, &encoded).unwrap();

    let received = recv_message(&mut rx).unwrap();
    let decoded = decode_response(&received).unwrap();
    match decoded {
        DaemonResponse::AuthResult(m) => {
            assert!(m.matched);
            assert_eq!(m.model_id, Some(5));
            assert!((m.similarity - 0.88).abs() < 1e-6);
        }
        other => panic!("expected AuthResult, got {other:?}"),
    }
}

#[test]
fn wire_protocol_multiple_messages_on_same_socket() {
    let (mut tx, mut rx) = UnixStream::pair().unwrap();

    // Send multiple messages in sequence
    let requests = vec![
        DaemonRequest::Ping,
        DaemonRequest::Authenticate {
            user: "alice".into(),
        },
        DaemonRequest::ListModels {
            user: "bob".into(),
        },
        DaemonRequest::Shutdown,
    ];

    for req in &requests {
        let encoded = encode_request(req).unwrap();
        send_message(&mut tx, &encoded).unwrap();
    }

    // Receive and verify all messages
    let received = recv_message(&mut rx).unwrap();
    assert!(matches!(
        decode_request(&received).unwrap(),
        DaemonRequest::Ping
    ));

    let received = recv_message(&mut rx).unwrap();
    match decode_request(&received).unwrap() {
        DaemonRequest::Authenticate { user } => assert_eq!(user, "alice"),
        other => panic!("expected Authenticate, got {other:?}"),
    }

    let received = recv_message(&mut rx).unwrap();
    match decode_request(&received).unwrap() {
        DaemonRequest::ListModels { user } => assert_eq!(user, "bob"),
        other => panic!("expected ListModels, got {other:?}"),
    }

    let received = recv_message(&mut rx).unwrap();
    assert!(matches!(
        decode_request(&received).unwrap(),
        DaemonRequest::Shutdown
    ));
}

// ---------------------------------------------------------------------------
// Malformed / error tests
// ---------------------------------------------------------------------------

#[test]
fn recv_truncated_message_returns_error() {
    // Write a length header claiming 100 bytes, but only provide 10
    let mut buf = Vec::new();
    buf.extend_from_slice(&100u32.to_le_bytes());
    buf.extend_from_slice(&[0u8; 10]); // only 10 bytes of payload

    let mut cursor = Cursor::new(buf);
    let result = recv_message(&mut cursor);
    assert!(result.is_err(), "truncated message should produce an error");
}

#[test]
fn recv_empty_stream_returns_error() {
    let buf: Vec<u8> = Vec::new();
    let mut cursor = Cursor::new(buf);
    let result = recv_message(&mut cursor);
    assert!(result.is_err(), "empty stream should produce an error");
}

#[test]
fn decode_garbage_request_returns_error() {
    let garbage = vec![0xFF, 0xFE, 0xFD, 0xFC, 0xFB];
    let result = decode_request(&garbage);
    assert!(result.is_err(), "garbage bytes should fail to decode");
}

#[test]
fn decode_garbage_response_returns_error() {
    let garbage = vec![0xFF, 0xFE, 0xFD, 0xFC, 0xFB];
    let result = decode_response(&garbage);
    assert!(result.is_err(), "garbage bytes should fail to decode");
}

#[test]
fn recv_rejects_message_exceeding_max_size() {
    // Craft a header claiming to be just over MAX_MESSAGE_SIZE
    let oversized_len = (MAX_MESSAGE_SIZE + 1) as u32;
    let mut buf = Vec::new();
    buf.extend_from_slice(&oversized_len.to_le_bytes());
    buf.extend_from_slice(&[0u8; 64]); // some dummy trailing data

    let mut cursor = Cursor::new(buf);
    let result = recv_message(&mut cursor);
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("message too large"),
        "error should mention size limit, got: {err_msg}"
    );
}

#[test]
fn recv_accepts_message_at_max_boundary() {
    // A message exactly at MAX_MESSAGE_SIZE should be accepted (if we had enough data).
    // We test that the length check itself passes by using a length == MAX_MESSAGE_SIZE
    // but providing insufficient data -- the error should be an IO error, not a size error.
    let exact_len = MAX_MESSAGE_SIZE as u32;
    let mut buf = Vec::new();
    buf.extend_from_slice(&exact_len.to_le_bytes());
    buf.extend_from_slice(&[0u8; 64]); // not enough data

    let mut cursor = Cursor::new(buf);
    let result = recv_message(&mut cursor);
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    // Should be an IO error (unexpected EOF), NOT a "message too large" error
    assert!(
        !err_msg.contains("message too large"),
        "exact MAX_MESSAGE_SIZE should not be rejected as too large, got: {err_msg}"
    );
}
