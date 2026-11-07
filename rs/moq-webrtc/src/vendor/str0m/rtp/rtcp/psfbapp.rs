use std::sync::Arc;

use crate::rtp::Ssrc;

use super::RtcpType;
use super::{FeedbackMessageType, PayloadType, RtcpHeader, RtcpPacket};

/// Application-specific Payload-Specific Feedback message (PSFB FMT=15, PT=206).
///
/// Generic container for FMT=15 RTCP PSFB messages that are not REMB.
/// Carries an opaque application-dependent payload after the standard
/// sender and media SSRC fields (RFC 4585 Section 6.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSpecificFeedback {
    /// SSRC of the sender of this feedback message.
    pub sender_ssrc: Ssrc,
    /// SSRC of the media source this feedback relates to.
    pub media_ssrc: Ssrc,
    /// Application-dependent payload (after sender_ssrc and media_ssrc).
    pub payload: Arc<[u8]>,
}

impl RtcpPacket for AppSpecificFeedback {
    fn header(&self) -> RtcpHeader {
        RtcpHeader {
            rtcp_type: RtcpType::PayloadSpecificFeedback,
            feedback_message_type: FeedbackMessageType::PayloadFeedback(
                PayloadType::ApplicationLayer,
            ),
            words_less_one: (self.length_words() - 1) as u16,
        }
    }

    fn length_words(&self) -> usize {
        // 1 (RTCP header) + 2 (sender_ssrc + media_ssrc) + payload words
        let payload_words = self.payload.len().div_ceil(4);
        1 + 2 + payload_words
    }

    fn write_to(&self, buf: &mut [u8]) -> usize {
        let total_len = self.length_words() * 4;
        assert!(
            buf.len() >= total_len,
            "AppSpecificFeedback::write_to: buffer too small ({} < {})",
            buf.len(),
            total_len,
        );

        self.header().write_to(&mut buf[..4]);
        buf[4..8].copy_from_slice(&self.sender_ssrc.to_be_bytes());
        buf[8..12].copy_from_slice(&self.media_ssrc.to_be_bytes());

        let payload_len = self.payload.len();
        buf[12..][..payload_len].copy_from_slice(&self.payload);

        // Pad to 4-byte boundary
        let pad_len = payload_len.next_multiple_of(4) - payload_len;
        buf[12 + payload_len..][..pad_len].fill(0);

        12 + payload_len + pad_len
    }
}

impl<'a> TryFrom<&'a [u8]> for AppSpecificFeedback {
    type Error = &'static str;

    fn try_from(buf: &'a [u8]) -> Result<Self, Self::Error> {
        if buf.len() < 8 {
            return Err("AppSpecificFeedback less than 8 bytes");
        }

        let (ssrc_buf, payload) = buf.split_at(8);
        let sender_ssrc =
            u32::from_be_bytes([ssrc_buf[0], ssrc_buf[1], ssrc_buf[2], ssrc_buf[3]]).into();
        let media_ssrc =
            u32::from_be_bytes([ssrc_buf[4], ssrc_buf[5], ssrc_buf[6], ssrc_buf[7]]).into();

        Ok(AppSpecificFeedback {
            sender_ssrc,
            media_ssrc,
            payload: payload.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_round_trip() {
        let input = AppSpecificFeedback {
            sender_ssrc: 1.into(),
            media_ssrc: 2.into(),
            payload: vec![0x01, 0x00, 0x00, 0x44, 0xAA, 0xBB, 0xCC, 0xDD].into(),
        };

        let mut buf = [0u8; 256];
        let written = input.write_to(&mut buf);

        // Parse back (skip 4-byte RTCP header)
        let parsed = AppSpecificFeedback::try_from(&buf[4..written]).unwrap();
        assert_eq!(input.sender_ssrc, parsed.sender_ssrc);
        assert_eq!(input.media_ssrc, parsed.media_ssrc);
        assert_eq!(input.payload, parsed.payload);
    }

    #[test]
    fn test_header() {
        let fb = AppSpecificFeedback {
            sender_ssrc: 0.into(),
            media_ssrc: 0.into(),
            payload: vec![1, 2, 3, 4].into(),
        };

        let header = fb.header();
        assert_eq!(header.rtcp_type, RtcpType::PayloadSpecificFeedback);
    }

    #[test]
    fn test_non_aligned_payload() {
        // Payload not aligned to 4 bytes — should still round-trip
        let input = AppSpecificFeedback {
            sender_ssrc: 100.into(),
            media_ssrc: 200.into(),
            payload: vec![0xAA, 0xBB, 0xCC].into(), // 3 bytes, needs 1 byte padding
        };

        let mut buf = [0u8; 256];
        let written = input.write_to(&mut buf);

        // Total should be 4 (header) + 4 (sender) + 4 (media) + 4 (padded payload) = 16
        assert_eq!(written, 16);

        let parsed = AppSpecificFeedback::try_from(&buf[4..written]).unwrap();
        assert_eq!(parsed.sender_ssrc, input.sender_ssrc);
        assert_eq!(parsed.media_ssrc, input.media_ssrc);
        // Parsed payload includes padding byte
        assert_eq!(parsed.payload.len(), 4);
        assert_eq!(&parsed.payload[..3], &[0xAA, 0xBB, 0xCC]);
        assert_eq!(parsed.payload[3], 0); // padding
    }

    #[test]
    fn test_empty_payload() {
        let input = AppSpecificFeedback {
            sender_ssrc: 1.into(),
            media_ssrc: 2.into(),
            payload: vec![].into(),
        };

        let mut buf = [0u8; 256];
        let written = input.write_to(&mut buf);
        assert_eq!(written, 12); // header + sender + media, no payload

        let parsed = AppSpecificFeedback::try_from(&buf[4..written]).unwrap();
        assert_eq!(parsed.sender_ssrc, input.sender_ssrc);
        assert_eq!(parsed.media_ssrc, input.media_ssrc);
        assert!(parsed.payload.is_empty());
    }

    #[test]
    fn test_too_short_rejected() {
        let buf = [0u8; 7]; // Less than 8 bytes
        assert!(AppSpecificFeedback::try_from(&buf[..]).is_err());
    }
}
