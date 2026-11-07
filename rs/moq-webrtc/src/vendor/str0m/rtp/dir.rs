use std::fmt;

/// Media direction.
///
/// And also extmap direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Send only direction.
    SendOnly,
    /// Receive only direction.
    RecvOnly,
    /// Bi-directional.
    SendRecv,
    /// Disabled direction.
    Inactive,
}

#[cfg(feature = "drv")]
crate::drv_identity_copy!(Direction);

impl Direction {
    /// Change the direction to the opposite.
    ///
    /// `SendRecv` and `Inactive` are left unchanged.
    pub fn invert(&self) -> Self {
        match self {
            Direction::SendOnly => Direction::RecvOnly,
            Direction::RecvOnly => Direction::SendOnly,
            _ => *self,
        }
    }

    /// Whether this direction is a sending direction.
    pub fn is_sending(&self) -> bool {
        matches!(self, Direction::SendOnly | Direction::SendRecv)
    }

    /// Whether this direction is a receiving direction.
    pub fn is_receiving(&self) -> bool {
        matches!(self, Direction::RecvOnly | Direction::SendRecv)
    }

    /// When negotiating SDP, in certain cases, we need to treat `Inactive`
    /// as if it is receiving.
    pub(crate) fn sdp_is_receiving(&self) -> bool {
        // The spec says:
        //
        // > For streams marked as inactive in the answer, the list of media
        // > formats is constructed based on the offer. If the offer was sendonly,
        // > the list is constructed as if the answer were recvonly.
        //
        // https://datatracker.ietf.org/doc/html/rfc3264#section-6.1
        matches!(
            self,
            Direction::RecvOnly | Direction::SendRecv | Direction::Inactive
        )
    }
}

impl From<&str> for Direction {
    fn from(v: &str) -> Self {
        use Direction::*;
        match v {
            "sendonly" => SendOnly,
            "recvonly" => RecvOnly,
            "sendrecv" => SendRecv,
            _ => Inactive,
        }
    }
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Direction::SendOnly => "sendonly",
                Direction::RecvOnly => "recvonly",
                Direction::SendRecv => "sendrecv",
                Direction::Inactive => "inactive",
            }
        )
    }
}
