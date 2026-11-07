use super::{FeedbackMessageType, PayloadType, RtcpHeader, RtcpPacket};
use super::{RtcpType, Ssrc};

/// Picture loss indicator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pli {
    /// Sender of this feedback. Mostly irrelevant, but part of RTCP packets.
    pub sender_ssrc: Ssrc,
    /// The SSRC this picture loss indication is for.
    pub ssrc: Ssrc,
}

impl RtcpPacket for Pli {
    fn header(&self) -> RtcpHeader {
        RtcpHeader {
            rtcp_type: RtcpType::PayloadSpecificFeedback,
            feedback_message_type: FeedbackMessageType::PayloadFeedback(
                PayloadType::PictureLossIndication,
            ),
            words_less_one: (self.length_words() - 1) as u16,
        }
    }

    fn length_words(&self) -> usize {
        // header
        // sender SSRC
        // media SSRC
        3
    }

    fn write_to(&self, buf: &mut [u8]) -> usize {
        self.header().write_to(&mut buf[..4]);
        buf[4..8].copy_from_slice(&self.sender_ssrc.to_be_bytes());
        buf[8..12].copy_from_slice(&self.ssrc.to_be_bytes());
        12
    }
}

impl<'a> TryFrom<&'a [u8]> for Pli {
    type Error = &'static str;

    fn try_from(buf: &'a [u8]) -> Result<Self, Self::Error> {
        if buf.len() < 8 {
            return Err("Pli less than 8 bytes");
        }

        let sender_ssrc = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]).into();
        let ssrc = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]).into();

        Ok(Pli { sender_ssrc, ssrc })
    }
}
