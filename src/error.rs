use thiserror::Error;

#[derive(Debug, Error)]
pub enum UicError {
    #[error("browser launch/connection error: {0}")]
    Browser(String),

    #[error("CDP protocol error: {0}")]
    Cdp(String),

    #[error("DOM extraction error: {0}")]
    Dom(String),

    #[error("vision/image error: {0}")]
    Vision(String),

    #[error("Gemini Decisions API error: {0}")]
    DecisionsApi(String),

    #[error("low confidence ({confidence:.2} < {threshold:.2}) for target '{target}'")]
    LowConfidence {
        target: String,
        confidence: f64,
        threshold: f64,
    },

    #[error("target not found on page: {0}")]
    TargetNotFound(String),

    #[error("assertion failed (p={probability:.2}): {condition}")]
    AssertionFailed { condition: String, probability: f64 },

    #[error("timeout waiting for: {0}")]
    Timeout(String),

    #[error("invalid recipe or configuration: {0}")]
    Config(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, UicError>;
