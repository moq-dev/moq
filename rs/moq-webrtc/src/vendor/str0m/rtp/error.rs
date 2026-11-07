use crate::crypto::CryptoError;
use std::error::Error;
use std::fmt;
use std::io;

/// Errors from parsing and decrypting RTP.
#[derive(Debug)]
pub enum RtpError {
    /// Error arising in the crypto
    CryptoError(CryptoError),

    /// Other io error
    Io(io::Error),

    /// Failed to parse RTP header.
    ParseHeader,
}

impl fmt::Display for RtpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RtpError::CryptoError(err) => write!(f, "{}", err),
            RtpError::Io(err) => write!(f, "{}", err),
            RtpError::ParseHeader => write!(f, "Failed to parse RTP header"),
        }
    }
}

impl Error for RtpError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            RtpError::CryptoError(err) => Some(err),
            RtpError::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for RtpError {
    fn from(err: io::Error) -> Self {
        RtpError::Io(err)
    }
}

impl From<CryptoError> for RtpError {
    fn from(err: CryptoError) -> Self {
        RtpError::CryptoError(err)
    }
}
