//! One error type for the whole SDK, so a plugin's `main` has one thing to
//! match on.

use std::fmt;

#[derive(Debug)]
pub enum SdkError {
    /// The local gateway could not be reached, or refused the request. A
    /// plugin can usually keep running: it just has no contacts and no
    /// persistence until the gateway is back.
    Gateway(String),
    /// The key or resource does not exist. Often a first-run default rather
    /// than a fault.
    NotFound,
    /// The radio daemon could not be reached or the channel could not open.
    Radio(String),
}

impl SdkError {
    pub fn gateway(detail: impl Into<String>) -> Self {
        Self::Gateway(detail.into())
    }

    pub fn radio(detail: impl Into<String>) -> Self {
        Self::Radio(detail.into())
    }

    /// True when the value was simply absent, so a caller can fall back to a
    /// default without treating it as a failure.
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound)
    }
}

impl fmt::Display for SdkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gateway(detail) => write!(f, "gateway: {detail}"),
            Self::NotFound => write!(f, "not found"),
            Self::Radio(detail) => write!(f, "radio: {detail}"),
        }
    }
}

impl std::error::Error for SdkError {}
