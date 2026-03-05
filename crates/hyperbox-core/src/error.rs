use thiserror::Error;

pub type Result<T> = std::result::Result<T, HyperboxError>;

#[derive(Debug, Error)]
pub enum HyperboxError {
    #[error("invalid config: {0}")]
    InvalidConfig(String),

    #[error("template not found: {0}")]
    TemplateNotFound(String),

    #[error("sandbox not found: {0}")]
    SandboxNotFound(String),

    #[error("execution failed: {0}")]
    ExecutionFailed(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}
