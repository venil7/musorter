use thiserror::Error;

/// Fatal, run-level errors → process exit code 2.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("{0}")]
    Usage(String),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

impl AppError {
    pub fn usage(msg: impl Into<String>) -> Self {
        AppError::Usage(msg.into())
    }
}
