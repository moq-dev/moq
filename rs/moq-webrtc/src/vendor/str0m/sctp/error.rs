use sctp_proto::Error as ProtoError;
use std::error::Error;
use std::fmt;

/// Errors from the SCTP subsystem.
#[derive(Debug, Eq, Clone, PartialEq)]
pub enum SctpError {
    /// Some protocol error as wrapped from the sctp_proto crate.
    Proto(ProtoError),

    /// Stream was not ready and we tried to write.
    WriteBeforeEstablished,

    /// The initial DCEP is not valid.
    DcepOpenTooSmall,

    /// The initial DCEP is not the correct message type.
    DcepIncorrectMessageType,

    /// The initial DCEP cant be read as utf-8.
    DcepBadUtf8,

    /// Invalid SNAP token in SCTP INIT string.
    InvalidSnap,
}

impl fmt::Display for SctpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SctpError::Proto(err) => write!(f, "{}", err),
            SctpError::WriteBeforeEstablished => {
                write!(f, "Write on a stream before it was established")
            }
            SctpError::DcepOpenTooSmall => write!(f, "DCEP open message too small"),
            SctpError::DcepIncorrectMessageType => write!(f, "DCEP incorrect message type"),
            SctpError::DcepBadUtf8 => write!(f, "DCEP bad UTF-8 string"),
            SctpError::InvalidSnap => write!(f, "Invalid SNAP token"),
        }
    }
}

impl Error for SctpError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            SctpError::Proto(err) => Some(err),
            _ => None,
        }
    }
}

impl From<ProtoError> for SctpError {
    fn from(err: ProtoError) -> Self {
        SctpError::Proto(err)
    }
}
