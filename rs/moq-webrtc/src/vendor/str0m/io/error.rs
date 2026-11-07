use std::error::Error;
use std::fmt;
use std::io;

pub use is::StunError;

/// Errors from parsing network data.
#[derive(Debug)]
pub enum NetError {
    /// Some STUN protocol error.
    Stun(StunError),

    /// A wrapped IO error.
    Io(io::Error),
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NetError::Stun(err) => write!(f, "{}", err),
            NetError::Io(err) => write!(f, "{}", err),
        }
    }
}

impl Error for NetError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            NetError::Stun(err) => Some(err),
            NetError::Io(err) => Some(err),
        }
    }
}

impl From<StunError> for NetError {
    fn from(err: StunError) -> Self {
        NetError::Stun(err)
    }
}

impl From<io::Error> for NetError {
    fn from(err: io::Error) -> Self {
        NetError::Io(err)
    }
}
