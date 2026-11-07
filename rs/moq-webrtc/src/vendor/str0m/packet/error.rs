use std::error::Error;
use std::fmt;

/// Errors arising in packet- and depacketization.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum PacketError {
    ErrShortPacket,
    ErrPayloadTooLarge,
    ErrTooManySpatialLayers,
    ErrTooManyPDiff,
    ErrH265CorruptedPacket,
    ErrH266CorruptedPacket,
    ErrInvalidH265PacketType,
    ErrH265PACIPHESTooLong,
    StapASizeLargerThanBuffer(usize, usize),
    NaluTypeIsNotHandled(u8),
    ErrVP8CorruptedPacket,
    ErrVP9CorruptedPacket,
    ErrAv1CorruptedPacket,
    ErrRedCorruptedPacket,
    InvalidTelephoneEvent(&'static str),
    ErrAmrWbCorruptedPacket,
}

impl fmt::Display for PacketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PacketError::ErrShortPacket => write!(f, "Packet is too short"),
            PacketError::ErrPayloadTooLarge => write!(f, "Payload exceeds the MTU"),
            PacketError::ErrTooManySpatialLayers => write!(f, "Too many spatial layers"),
            PacketError::ErrTooManyPDiff => write!(f, "Too many P-Diff"),
            PacketError::ErrH265CorruptedPacket => write!(f, "H265 corrupted packet"),
            PacketError::ErrH266CorruptedPacket => write!(f, "H266 corrupted packet"),
            PacketError::ErrInvalidH265PacketType => write!(f, "H265 invalid packet type"),
            PacketError::ErrH265PACIPHESTooLong => {
                write!(f, "H265 PACI PHES field exceeds maximum size of 31 bytes")
            }
            PacketError::StapASizeLargerThanBuffer(size, buffer) => write!(
                f,
                "H264 StapA size larger than buffer: {} > {}",
                size, buffer
            ),
            PacketError::NaluTypeIsNotHandled(nalu_type) => {
                write!(f, "H264 NALU type is not handled: {}", nalu_type)
            }
            PacketError::ErrVP8CorruptedPacket => write!(f, "VP8 corrupted packet"),
            PacketError::ErrVP9CorruptedPacket => write!(f, "VP9 corrupted packet"),
            PacketError::ErrAv1CorruptedPacket => write!(f, "AV1 corrupted packet"),
            PacketError::ErrRedCorruptedPacket => write!(f, "RED corrupted packet"),
            PacketError::InvalidTelephoneEvent(reason) => {
                write!(f, "Invalid telephone event: {reason}")
            }
            PacketError::ErrAmrWbCorruptedPacket => write!(f, "AMR-WB corrupted packet"),
        }
    }
}

impl Error for PacketError {}
