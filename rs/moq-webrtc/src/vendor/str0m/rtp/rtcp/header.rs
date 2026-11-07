#![allow(clippy::unusual_byte_groupings)]

use super::{FeedbackMessageType, PayloadType, TransportType};

pub(crate) const LEN_HEADER: usize = 4;

#[derive(Debug, PartialEq, Eq)]
pub struct RtcpHeader {
    pub(crate) rtcp_type: RtcpType,
    pub(crate) feedback_message_type: FeedbackMessageType,
    pub(crate) words_less_one: u16,
}

impl RtcpHeader {
    /// Type of RTCP packet. This is further divided into subtypes by
    /// `feedback_message_type`.
    pub fn rtcp_type(&self) -> RtcpType {
        self.rtcp_type
    }

    /// Subtype of RTCP message.
    pub fn feedback_message_type(&self) -> FeedbackMessageType {
        self.feedback_message_type
    }

    /// Number of reports stacked in this message.
    ///
    /// Depending `rtcp_type` and `feedback_message_type` multiple reports of the same type can be
    /// stacked into one RTCP packet under the same header.
    pub fn count(&self) -> usize {
        match self.rtcp_type {
            RtcpType::SenderReport
            | RtcpType::ReceiverReport
            | RtcpType::SourceDescription
            | RtcpType::Goodbye => self.feedback_message_type.count() as usize,
            RtcpType::ApplicationDefined => 1,
            RtcpType::TransportLayerFeedback => {
                let transport_type = match self.feedback_message_type {
                    FeedbackMessageType::TransportFeedback(v) => v,
                    _ => unreachable!(),
                };

                match transport_type {
                    TransportType::Nack => {
                        // [ssrc_sender, ssrc_media_source, fci, fci, ...]
                        let fci_length = self.length_words() * 4 - LEN_HEADER - 2 * 4;

                        // each fci is one word: [pid, blp]
                        fci_length / 4
                    }
                    TransportType::TransportWide => {
                        // TODO
                        0
                    }
                }
            }
            RtcpType::PayloadSpecificFeedback => {
                let payload_type = match self.feedback_message_type {
                    FeedbackMessageType::PayloadFeedback(v) => v,
                    _ => unreachable!(),
                };

                match payload_type {
                    PayloadType::PictureLossIndication => {
                        // PLI does not require parameters.  Therefore, the length field MUST be
                        // 2, and there MUST NOT be any Feedback Control Information.
                        1
                    }

                    PayloadType::FullIntraRequest => {
                        // [ssrc_sender, ssrc_media_source, fci, fci, ...]
                        let fci_length = self.length_words() * 4 - LEN_HEADER - 2 * 4;

                        // each fci is two words: [ssrc, [seq_no, reserved]]
                        fci_length / 8
                    }

                    _ => {
                        // PayloadType::SliceLossIndication => {},
                        // PayloadType::ReferencePictureSelectionIndication => {},
                        // PayloadType::ApplicationLayer => {},
                        0
                    }
                }
            }
            RtcpType::ExtendedReport => 0,
        }
    }

    /// Length of entire RTCP packet (including header) in words (4 bytes).
    pub fn length_words(&self) -> usize {
        self.words_less_one as usize + 1
    }

    /// Write header to buffer.
    pub(crate) fn write_to(&self, buf: &mut [u8]) -> usize {
        let fmt: u8 = self.feedback_message_type.into();

        buf[0] = 0b10_0_00000 | fmt;
        buf[1] = self.rtcp_type as u8;

        buf[2..4].copy_from_slice(&self.words_less_one.to_be_bytes());

        4
    }
}

/// Kind of RTCP packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtcpType {
    /// RTCP_PT_SR
    SenderReport = 200,

    /// RTCP_PT_RR
    ReceiverReport = 201,

    /// RTCP_PT_SDES
    SourceDescription = 202,

    /// RTCP_PT_BYE
    Goodbye = 203,

    /// RTCP_PT_APP
    ApplicationDefined = 204,

    /// RTCP_PT_RTPFB
    // https://tools.ietf.org/html/rfc4585
    TransportLayerFeedback = 205,

    /// RTCP_PT_PSFB
    // https://tools.ietf.org/html/rfc4585
    PayloadSpecificFeedback = 206,

    /// RTCP_PT_XR
    ExtendedReport = 207,
}

impl TryFrom<u8> for RtcpType {
    type Error = &'static str;

    fn try_from(v: u8) -> Result<Self, Self::Error> {
        use RtcpType::*;
        match v {
            200 => Ok(SenderReport),   // sr
            201 => Ok(ReceiverReport), // rr
            202 => Ok(SourceDescription),
            203 => Ok(Goodbye),
            204 => Ok(ApplicationDefined),
            205 => Ok(TransportLayerFeedback),
            206 => Ok(PayloadSpecificFeedback),
            207 => Ok(ExtendedReport),
            _ => {
                trace!("Unknown RtcpType: {}", v);
                Err("Unknown RtcpType")
            }
        }
    }
}

impl<'a> TryFrom<&'a [u8]> for RtcpHeader {
    type Error = &'static str;

    fn try_from(buf: &'a [u8]) -> Result<Self, Self::Error> {
        if buf.len() < 4 {
            return Err("Need 4 bytes for RTCP header");
        }

        let version = (buf[0] & 0b11_0_00000) >> 6;
        if version != 2 {
            return Err("RTCP header version should be 2");
        }

        let fmt = buf[0] & 0b00_0_11111;
        let rtcp_type: RtcpType = buf[1].try_into()?;

        let feedback_message_type = {
            use FeedbackMessageType::*;
            match rtcp_type {
                RtcpType::SenderReport => ReceptionReport(fmt),
                RtcpType::ReceiverReport => ReceptionReport(fmt),
                RtcpType::SourceDescription => SourceCount(fmt),
                RtcpType::Goodbye => SourceCount(fmt),
                RtcpType::ApplicationDefined => Subtype(fmt),
                RtcpType::TransportLayerFeedback => TransportFeedback(fmt.try_into()?),
                RtcpType::PayloadSpecificFeedback => PayloadFeedback(fmt.try_into()?),
                RtcpType::ExtendedReport => NotUsed,
            }
        };

        let words_less_one = u16::from_be_bytes([buf[2], buf[3]]);

        Ok(RtcpHeader {
            rtcp_type,
            feedback_message_type,
            words_less_one,
        })
    }
}
