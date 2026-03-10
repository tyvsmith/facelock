use std::io::{Read, Write};

use bincode::{Decode, Encode};

use crate::error::HowdyError;
use crate::types::{FaceModelInfo, MatchResult};

/// Maximum IPC message size (10MB — generous for JPEG preview frames)
pub const MAX_MESSAGE_SIZE: usize = 10 * 1024 * 1024;

#[derive(Debug, Clone, Encode, Decode)]
pub enum DaemonRequest {
    Authenticate { user: String },
    Enroll { user: String, label: String },
    ListModels { user: String },
    RemoveModel { user: String, model_id: u32 },
    ClearModels { user: String },
    PreviewFrame,
    /// Preview with face detection + recognition against the given user's models.
    PreviewDetectFrame { user: String },
    ReleaseCamera,
    Ping,
    Shutdown,
}

/// A detected face in a preview frame with its recognition status.
#[derive(Debug, Clone, Encode, Decode)]
pub struct PreviewFace {
    /// Bounding box in original (pre-JPEG) frame coordinates.
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// Detection confidence from SCRFD.
    pub confidence: f32,
    /// Best cosine similarity against stored embeddings (0.0 if no models).
    pub similarity: f32,
    /// Whether similarity exceeded the recognition threshold.
    pub recognized: bool,
}

#[derive(Debug, Clone, Encode, Decode)]
pub enum DaemonResponse {
    AuthResult(MatchResult),
    Enrolled { model_id: u32, embedding_count: u32 },
    Models(Vec<FaceModelInfo>),
    Removed,
    Frame { jpeg_data: Vec<u8> },
    /// Preview frame with face detection results.
    DetectFrame {
        jpeg_data: Vec<u8>,
        faces: Vec<PreviewFace>,
    },
    Ok,
    Error { message: String },
}

/// Send a length-prefixed message.
pub fn send_message<W: Write>(writer: &mut W, data: &[u8]) -> crate::error::Result<()> {
    let len = data.len() as u32;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(data)?;
    writer.flush()?;
    Ok(())
}

/// Read a length-prefixed message. Rejects messages > MAX_MESSAGE_SIZE.
pub fn recv_message<R: Read>(reader: &mut R) -> crate::error::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;

    if len > MAX_MESSAGE_SIZE {
        return Err(HowdyError::Ipc(format!("message too large: {len} bytes")));
    }

    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(buf)
}

/// Encode a request to bytes using bincode.
pub fn encode_request(req: &DaemonRequest) -> crate::error::Result<Vec<u8>> {
    bincode::encode_to_vec(req, bincode::config::standard())
        .map_err(|e| HowdyError::Ipc(format!("encode error: {e}")))
}

/// Decode a request from bytes.
pub fn decode_request(data: &[u8]) -> crate::error::Result<DaemonRequest> {
    bincode::decode_from_slice(data, bincode::config::standard())
        .map(|(req, _)| req)
        .map_err(|e| HowdyError::Ipc(format!("decode error: {e}")))
}

/// Encode a response to bytes using bincode.
pub fn encode_response(resp: &DaemonResponse) -> crate::error::Result<Vec<u8>> {
    bincode::encode_to_vec(resp, bincode::config::standard())
        .map_err(|e| HowdyError::Ipc(format!("encode error: {e}")))
}

/// Decode a response from bytes.
pub fn decode_response(data: &[u8]) -> crate::error::Result<DaemonResponse> {
    bincode::decode_from_slice(data, bincode::config::standard())
        .map(|(resp, _)| resp)
        .map_err(|e| HowdyError::Ipc(format!("decode error: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn round_trip_request_authenticate() {
        let req = DaemonRequest::Authenticate {
            user: "alice".into(),
        };
        let encoded = encode_request(&req).unwrap();
        let decoded = decode_request(&encoded).unwrap();
        match decoded {
            DaemonRequest::Authenticate { user } => assert_eq!(user, "alice"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn round_trip_request_enroll() {
        let req = DaemonRequest::Enroll {
            user: "bob".into(),
            label: "home".into(),
        };
        let encoded = encode_request(&req).unwrap();
        let decoded = decode_request(&encoded).unwrap();
        match decoded {
            DaemonRequest::Enroll { user, label } => {
                assert_eq!(user, "bob");
                assert_eq!(label, "home");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn round_trip_request_ping() {
        let req = DaemonRequest::Ping;
        let encoded = encode_request(&req).unwrap();
        let decoded = decode_request(&encoded).unwrap();
        assert!(matches!(decoded, DaemonRequest::Ping));
    }

    #[test]
    fn round_trip_response_auth_result() {
        let resp = DaemonResponse::AuthResult(MatchResult {
            matched: true,
            model_id: Some(42),
            label: Some("office".into()),
            similarity: 0.87,
        });
        let encoded = encode_response(&resp).unwrap();
        let decoded = decode_response(&encoded).unwrap();
        match decoded {
            DaemonResponse::AuthResult(m) => {
                assert!(m.matched);
                assert_eq!(m.model_id, Some(42));
                assert_eq!(m.label.as_deref(), Some("office"));
                assert!((m.similarity - 0.87).abs() < 1e-5);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn round_trip_response_models() {
        let resp = DaemonResponse::Models(vec![FaceModelInfo {
            id: 1,
            user: "alice".into(),
            label: "default".into(),
            created_at: 1700000000,
        }]);
        let encoded = encode_response(&resp).unwrap();
        let decoded = decode_response(&encoded).unwrap();
        match decoded {
            DaemonResponse::Models(models) => {
                assert_eq!(models.len(), 1);
                assert_eq!(models[0].user, "alice");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn send_recv_round_trip() {
        let data = b"hello world";
        let mut buf = Vec::new();
        send_message(&mut buf, data).unwrap();

        let mut cursor = Cursor::new(buf);
        let received = recv_message(&mut cursor).unwrap();
        assert_eq!(received, data);
    }

    #[test]
    fn recv_rejects_oversized_message() {
        // Craft a message header claiming to be larger than MAX_MESSAGE_SIZE
        let huge_len = (MAX_MESSAGE_SIZE + 1) as u32;
        let mut buf = Vec::new();
        buf.extend_from_slice(&huge_len.to_le_bytes());
        buf.extend_from_slice(&[0u8; 64]); // some dummy data

        let mut cursor = Cursor::new(buf);
        let result = recv_message(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("message too large"));
    }

    #[test]
    fn round_trip_preview_frame() {
        // Simulate a JPEG payload
        let fake_jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        let resp = DaemonResponse::Frame {
            jpeg_data: fake_jpeg.clone(),
        };
        let encoded = encode_response(&resp).unwrap();
        let decoded = decode_response(&encoded).unwrap();
        match decoded {
            DaemonResponse::Frame { jpeg_data } => assert_eq!(jpeg_data, fake_jpeg),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn full_wire_round_trip() {
        // Encode a request, send over wire, receive, decode
        let req = DaemonRequest::ListModels {
            user: "charlie".into(),
        };
        let encoded = encode_request(&req).unwrap();

        let mut wire = Vec::new();
        send_message(&mut wire, &encoded).unwrap();

        let mut cursor = Cursor::new(wire);
        let received = recv_message(&mut cursor).unwrap();
        let decoded = decode_request(&received).unwrap();

        match decoded {
            DaemonRequest::ListModels { user } => assert_eq!(user, "charlie"),
            _ => panic!("wrong variant"),
        }
    }
}
