//! Various error types.

use std::error::Error;
use std::fmt;

// Re-export all error types for convenience
pub use crate::crypto::DtlsError;
pub use crate::io::NetError;
pub use crate::packet::{PacketError, Vp8DescriptorError, Vp8PatchError};
pub use crate::rtp_::RtpError;
pub use crate::sctp::ProtoError;
pub use crate::sctp::SctpError;
pub use crate::sdp::SdpError;
pub use is::IceError;

use crate::{Direction, KeyframeRequestKind, Mid, Pt, Rid};

/// Errors for the whole Rtc engine.
#[derive(Debug)]
#[non_exhaustive]
pub enum RtcError {
    /// Some problem with the remote SDP.
    RemoteSdp(String),

    /// SDP errors.
    Sdp(SdpError),

    /// RTP errors.
    Rtp(RtpError),

    /// Other IO errors.
    Io(std::io::Error),

    /// DTLS errors
    Dtls(DtlsError),

    /// RTP packetization, depacketization, or payload validation error.
    Packet(Mid, Pt, PacketError),

    /// The PT attempted to write to is not known.
    UnknownPt(Pt),

    /// The Rid attempted to write is not known.
    UnknownRid(Rid),

    /// If MediaWriter.write fails because we can't find an SSRC to use.
    NoSenderSource,

    /// Using `write_rtp` for a stream with RTX without providing a rtx_pt.
    ResendRequiresRtxPt,

    /// Direction does not allow sending of Media data.
    NotSendingDirection(Direction),

    /// Direction does not allow receiving media data.
    NotReceivingDirection,

    /// If MediaWriter.request_keyframe fails because we can't find an SSRC to use.
    NoReceiverSource(Option<Rid>),

    /// The keyframe request failed because the kind of request is not enabled
    /// in the media.
    FeedbackNotEnabled(KeyframeRequestKind),

    /// Parser errors from network packet parsing.
    Net(NetError),

    /// ICE agent errors.
    Ice(IceError),

    /// SCTP (data channel engine) errors.
    Sctp(SctpError),

    /// [`crate::change::SdpApi`] was not done in a correct order.
    ///
    /// For [`crate::change::SdpApi`]:
    ///
    /// 1. We created an [`crate::change::SdpOffer`].
    /// 2. The remote side created an [`crate::change::SdpOffer`] at the same time.
    /// 3. We applied the remote side [`crate::change::SdpApi::accept_offer`].
    /// 4. The we used the [`crate::change::SdpPendingOffer`] created in step 1.
    ChangesOutOfOrder,

    /// The [`crate::media::Writer`] was used twice without doing [`crate::Rtc::poll_output`] in between. This
    /// is an incorrect usage pattern of the str0m API.
    WriteWithoutPoll,
}

impl fmt::Display for RtcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RtcError::RemoteSdp(msg) => write!(f, "remote sdp: {}", msg),
            RtcError::Sdp(err) => write!(f, "{}", err),
            RtcError::Rtp(err) => write!(f, "{}", err),
            RtcError::Io(err) => write!(f, "{}", err),
            RtcError::Dtls(err) => write!(f, "{}", err),
            RtcError::Packet(mid, pt, err) => write!(f, "{} {} {}", mid, pt, err),
            RtcError::UnknownPt(pt) => write!(f, "PT is unknown {}", pt),
            RtcError::UnknownRid(rid) => write!(f, "RID is unknown {}", rid),
            RtcError::NoSenderSource => write!(f, "No sender source"),
            RtcError::ResendRequiresRtxPt => write!(
                f,
                "When outgoing stream has RTX, write_rtp must be called with rtp_pt set"
            ),
            RtcError::NotSendingDirection(dir) => {
                write!(f, "Direction does not allow sending: {}", dir)
            }
            RtcError::NotReceivingDirection => write!(f, "Direction does not allow receiving"),
            RtcError::NoReceiverSource(rid) => write!(f, "No receiver source (rid: {:?})", rid),
            RtcError::FeedbackNotEnabled(kind) => {
                write!(f, "Requested feedback is not enabled: {:?}", kind)
            }
            RtcError::Net(err) => write!(f, "{}", err),
            RtcError::Ice(err) => write!(f, "{}", err),
            RtcError::Sctp(err) => write!(f, "{}", err),
            RtcError::ChangesOutOfOrder => write!(f, "Changes made out of order"),
            RtcError::WriteWithoutPoll => write!(
                f,
                "Consecutive calls to write() without poll_output() in between"
            ),
        }
    }
}

impl Error for RtcError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            RtcError::Sdp(err) => Some(err),
            RtcError::Rtp(err) => Some(err),
            RtcError::Io(err) => Some(err),
            RtcError::Dtls(err) => Some(err),
            RtcError::Packet(_, _, err) => Some(err),
            RtcError::Net(err) => Some(err),
            RtcError::Ice(err) => Some(err),
            RtcError::Sctp(err) => Some(err),
            _ => None,
        }
    }
}

// From implementations for error conversions
impl From<SdpError> for RtcError {
    fn from(err: SdpError) -> Self {
        RtcError::Sdp(err)
    }
}

impl From<RtpError> for RtcError {
    fn from(err: RtpError) -> Self {
        RtcError::Rtp(err)
    }
}

impl From<std::io::Error> for RtcError {
    fn from(err: std::io::Error) -> Self {
        RtcError::Io(err)
    }
}

impl From<DtlsError> for RtcError {
    fn from(err: DtlsError) -> Self {
        RtcError::Dtls(err)
    }
}

impl From<NetError> for RtcError {
    fn from(err: NetError) -> Self {
        RtcError::Net(err)
    }
}

impl From<IceError> for RtcError {
    fn from(err: IceError) -> Self {
        RtcError::Ice(err)
    }
}

impl From<SctpError> for RtcError {
    fn from(err: SctpError) -> Self {
        RtcError::Sctp(err)
    }
}
