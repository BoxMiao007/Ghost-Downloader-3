use pyo3::exceptions::PyRuntimeError;
use pyo3::PyErr;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum EngineError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Server does not support range requests")]
    NoRangeSupport,

    #[error("Download cancelled")]
    Cancelled,

    #[error("Max retries exceeded for connection {id}: {reason}")]
    MaxRetries { id: u32, reason: String },

    #[error("Resume file corrupted: {0}")]
    CorruptedResumeFile(String),
}

impl From<EngineError> for PyErr {
    fn from(err: EngineError) -> PyErr {
        PyRuntimeError::new_err(err.to_string())
    }
}
