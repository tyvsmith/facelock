pub mod config;
pub mod error;
pub mod ipc;
pub mod paths;
pub mod traits;
pub mod types;

pub use config::{Config, DaemonMode};
pub use error::{VisageError, Result};
pub use traits::{CameraSource, FaceProcessor};
