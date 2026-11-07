use std::error::Error;
use std::fmt;

/// Errors from parsing and serializing SDP.
#[derive(Debug)]
#[allow(missing_docs)]
pub enum SdpError {
    ParseError(String),
    Inconsistent(String),
}

impl fmt::Display for SdpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SdpError::ParseError(msg) => write!(f, "SDP parse: {}", msg),
            SdpError::Inconsistent(msg) => write!(f, "SDP inconsistent: {}", msg),
        }
    }
}

impl Error for SdpError {}
