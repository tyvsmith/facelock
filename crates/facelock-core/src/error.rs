use crate::config::ConfigError;

#[derive(Debug, thiserror::Error)]
pub enum FacelockError {
    #[error("config error: {0}")]
    Config(#[from] ConfigError),
    #[error("camera error: {0}")]
    Camera(String),
    #[error("detection error: {0}")]
    Detection(String),
    #[error("alignment error: {0}")]
    Alignment(String),
    #[error("embedding error: {0}")]
    Embedding(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("daemon error: {0}")]
    Daemon(String),
    #[error("IPC error: {0}")]
    Ipc(String),
    #[error("TPM error: {0}")]
    Tpm(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, FacelockError>;
