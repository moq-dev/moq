use std::str::from_utf8;

use sctp_proto::ReliabilityType;

use super::SctpError as Error;

// 0                   1                   2                   3
// 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |  Message Type |  Channel Type |            Priority           |
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |                    Reliability Parameter                      |
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// |         Label Length          |       Protocol Length         |
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// \                                                               /
// |                             Label                             |
// /                                                               \
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
// \                                                               /
// |                            Protocol                           |
// /                                                               \
// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+

/// Data channel establishment protocol.
/// RFC 8832
#[derive(Clone, Debug)]
pub struct DcepOpen {
    pub unordered: bool,
    pub channel_type: ReliabilityType,
    pub priority: u16,
    pub reliability_parameter: u32,
    pub label: String,
    pub protocol: String,
}

impl TryFrom<&[u8]> for DcepOpen {
    type Error = Error;

    fn try_from(buf: &[u8]) -> Result<Self, Self::Error> {
        if buf.len() < 12 {
            return Err(Error::DcepOpenTooSmall);
        }

        if buf[0] != 0x03 {
            return Err(Error::DcepIncorrectMessageType);
        }

        let unordered = buf[1] & 0x80 > 0;
        let channel_type: ReliabilityType = (buf[1] & 0x7f).into();
        let priority = u16::from_be_bytes([buf[2], buf[3]]);
        let reliability_parameter = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let len_label = u16::from_be_bytes([buf[8], buf[9]]) as usize;
        let len_proto = u16::from_be_bytes([buf[10], buf[11]]) as usize;

        if buf.len() < 12 + len_label + len_proto {
            return Err(Error::DcepOpenTooSmall);
        }

        let buf = &buf[12..];
        let label = from_utf8(&buf[0..len_label])
            .map_err(|_| Error::DcepBadUtf8)?
            .to_string();

        let buf = &buf[len_label..];
        let protocol = from_utf8(&buf[0..len_proto])
            .map_err(|_| Error::DcepBadUtf8)?
            .to_string();

        Ok(DcepOpen {
            unordered,
            channel_type,
            priority,
            reliability_parameter,
            label,
            protocol,
        })
    }
}

impl DcepOpen {
    pub fn marshal_to(&self, buf: &mut [u8]) -> usize {
        buf[0] = 0x03;
        buf[1] = (self.channel_type as u8) | if self.unordered { 0x80 } else { 0x00 };
        buf[2..4].copy_from_slice(&self.priority.to_be_bytes());
        buf[4..8].copy_from_slice(&self.reliability_parameter.to_be_bytes());
        let bytes_label = self.label.as_bytes();
        let bytes_proto = self.protocol.as_bytes();
        buf[8..10].copy_from_slice(&(bytes_label.len() as u16).to_be_bytes());
        buf[10..12].copy_from_slice(&(bytes_proto.len() as u16).to_be_bytes());
        let buf = &mut buf[12..];
        buf[..bytes_label.len()].copy_from_slice(bytes_label);
        let buf = &mut buf[bytes_label.len()..];
        buf[..bytes_proto.len()].copy_from_slice(bytes_proto);
        12 + bytes_label.len() + bytes_proto.len()
    }
}

pub struct DcepAck;

impl TryFrom<&[u8]> for DcepAck {
    type Error = Error;

    fn try_from(buf: &[u8]) -> Result<Self, Self::Error> {
        if buf.is_empty() {
            return Err(Error::DcepOpenTooSmall);
        }

        if buf[0] != 0x02 {
            return Err(Error::DcepIncorrectMessageType);
        }

        Ok(DcepAck)
    }
}

impl DcepAck {
    pub fn marshal_to(&self, buf: &mut [u8]) -> usize {
        buf[0] = 0x02;
        1
    }
}
