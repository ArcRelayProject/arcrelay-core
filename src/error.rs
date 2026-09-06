use thiserror::Error;

/// Transport-neutral classification used by every outer adapter. Keeping this
/// separate from display text prevents IPC and network protocols from drifting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    OperationFailed,
    NotFound,
    Unsupported,
    Unavailable,
    Internal,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("system information error: {0}")]
    SystemInfo(String),

    #[error("media control error: {0}")]
    MediaControl(String),

    #[error("clipboard error: {0}")]
    Clipboard(String),

    #[error("input control error: {0}")]
    InputControl(String),

    #[error("device error: {0}")]
    Device(String),

    #[error("resource not found: {0}")]
    NotFound(String),

    #[error("not supported on this platform: {0}")]
    NotSupported(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub const fn kind(&self) -> ErrorKind {
        match self {
            Self::SystemInfo(_)
            | Self::MediaControl(_)
            | Self::Clipboard(_)
            | Self::InputControl(_)
            | Self::Device(_) => ErrorKind::OperationFailed,
            Self::NotFound(_) => ErrorKind::NotFound,
            Self::NotSupported(_) => ErrorKind::Unsupported,
            Self::Io(_) => ErrorKind::Unavailable,
            Self::Other(_) => ErrorKind::Internal,
        }
    }

    /// Stable machine-readable category for application adapters.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::SystemInfo(_) => "core.system_information",
            Self::MediaControl(_) => "core.media_control",
            Self::Clipboard(_) => "core.clipboard",
            Self::InputControl(_) => "core.input_control",
            Self::Device(_) => "core.device",
            Self::NotFound(_) => "core.not_found",
            Self::NotSupported(_) => "core.not_supported",
            Self::Io(_) => "core.unavailable",
            Self::Other(_) => "core.operation_failed",
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
