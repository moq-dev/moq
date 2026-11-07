use super::{AppSpecificFeedback, DlrrItem, FirEntry, NackEntry, ReceptionReport, Remb};
use super::{ReportBlock, ReportList, Rrtr, Rtcp, Sdes, SenderInfo, Ssrc, Twcc};

/// Normalization of [`Rtcp`] so we can deal with one SSRC at a time.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum RtcpFb {
    SenderInfo(SenderInfo),                   // tx -> rx
    ReceptionReport(ReceptionReport),         // rx -> tx
    DlrrItem(DlrrItem),                       // rx <- tx
    Rrtr((Rrtr, Ssrc)),                       // rx -> tx
    SourceDescription(Sdes),                  // tx -> rx
    Goodbye(Ssrc),                            // tx -> rx
    Nack(Ssrc, ReportList<NackEntry>),        // rx -> tx
    Pli(Ssrc),                                // rx -> tx
    Fir(FirEntry),                            // rx -> tx
    Twcc(Twcc),                               // rx -> tx
    Remb(Remb),                               // rx -> tx
    AppSpecificFeedback(AppSpecificFeedback), // not stream-routed
}

impl RtcpFb {
    pub fn is_for_rx(&self) -> bool {
        matches!(
            self,
            RtcpFb::SenderInfo(_)
                | RtcpFb::SourceDescription(_)
                | RtcpFb::Goodbye(_)
                | RtcpFb::DlrrItem(_)
        )
    }

    pub fn from_rtcp<T: IntoIterator<Item = Rtcp>>(t: T) -> impl Iterator<Item = RtcpFb> {
        let mut q = Vec::new();
        let iter = t.into_iter();
        for pkt in iter {
            match pkt {
                Rtcp::SenderReport(v) => {
                    q.push(RtcpFb::SenderInfo(v.sender_info));
                    q.extend(v.reports.into_iter().map(RtcpFb::ReceptionReport));
                }
                Rtcp::ReceiverReport(v) => {
                    q.extend(v.reports.into_iter().map(RtcpFb::ReceptionReport));
                }
                Rtcp::ExtendedReport(v) => {
                    for block in v.blocks {
                        match block {
                            ReportBlock::Rrtr(b) => q.push(RtcpFb::Rrtr((b, v.ssrc))),
                            ReportBlock::Dlrr(v) => {
                                q.extend(v.items.iter().map(|i| RtcpFb::DlrrItem(*i)))
                            }
                        }
                    }
                }
                Rtcp::SourceDescription(v) => {
                    q.extend(v.reports.into_iter().map(RtcpFb::SourceDescription));
                }
                Rtcp::Goodbye(v) => {
                    q.extend(v.reports.into_iter().map(RtcpFb::Goodbye));
                }
                Rtcp::Nack(v) => {
                    q.push(RtcpFb::Nack(v.ssrc, v.reports));
                }
                Rtcp::Pli(v) => {
                    q.push(RtcpFb::Pli(v.ssrc));
                }
                Rtcp::Fir(v) => {
                    q.extend(v.reports.into_iter().map(RtcpFb::Fir));
                }
                Rtcp::Twcc(v) => {
                    q.push(RtcpFb::Twcc(v));
                }
                Rtcp::Remb(v) => {
                    q.push(RtcpFb::Remb(v));
                }
                Rtcp::AppSpecificFeedback(v) => {
                    q.push(RtcpFb::AppSpecificFeedback(v));
                }
            }
        }
        q.into_iter()
    }

    pub fn ssrc(&self) -> Ssrc {
        match self {
            RtcpFb::SenderInfo(v) => v.ssrc,
            RtcpFb::ReceptionReport(v) => v.ssrc,
            RtcpFb::DlrrItem(v) => v.ssrc,
            RtcpFb::Rrtr((_, ssrc)) => *ssrc,
            RtcpFb::SourceDescription(v) => v.ssrc,
            RtcpFb::Goodbye(v) => *v,
            RtcpFb::Nack(v, _) => *v,
            RtcpFb::Pli(v) => *v,
            RtcpFb::Fir(v) => v.ssrc,
            RtcpFb::Twcc(v) => v.ssrc,
            RtcpFb::Remb(v) => v.ssrcs.first().map(|ssrc| (*ssrc).into()).unwrap_or(v.ssrc),
            RtcpFb::AppSpecificFeedback(v) => v.media_ssrc,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::AppSpecificFeedback;
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn app_specific_feedback_passes_through_rtcpfb() {
        let fb = AppSpecificFeedback {
            sender_ssrc: 100.into(),
            media_ssrc: 200.into(),
            payload: vec![0xDE, 0xAD, 0xBE, 0xEF].into(),
        };

        let rtcp_items = vec![Rtcp::AppSpecificFeedback(fb)];
        let result: Vec<_> = RtcpFb::from_rtcp(rtcp_items).collect();

        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], RtcpFb::AppSpecificFeedback(v) if v.media_ssrc == 200.into()));
    }

    #[test]
    fn app_specific_feedback_write_and_parse_round_trip() {
        use crate::rtp_::RtcpPacket;

        let fb = AppSpecificFeedback {
            sender_ssrc: 1001.into(),
            media_ssrc: 2002.into(),
            payload: vec![0x01, 0x00, 0x00, 0x44, 0xAA, 0xBB, 0xCC, 0xDD].into(),
        };

        // Write the RTCP packet
        let rtcp = Rtcp::AppSpecificFeedback(fb.clone());
        let mut buf = [0u8; 256];
        let written = rtcp.write_to(&mut buf);

        // Parse it back via the Rtcp parser
        let mut parsed = VecDeque::new();
        Rtcp::read_packet(&buf[..written], &mut parsed);

        assert_eq!(parsed.len(), 1);
        match parsed.pop_front().unwrap() {
            Rtcp::AppSpecificFeedback(parsed_fb) => {
                assert_eq!(parsed_fb.sender_ssrc, fb.sender_ssrc);
                assert_eq!(parsed_fb.media_ssrc, fb.media_ssrc);
                assert_eq!(parsed_fb.payload, fb.payload);
            }
            _ => panic!("Expected AppSpecificFeedback"),
        }
    }

    #[test]
    fn app_specific_feedback_not_confused_with_remb() {
        use crate::rtp_::RtcpPacket;

        // A non-REMB FMT=15 payload should parse as AppSpecificFeedback, not Remb
        let fb = AppSpecificFeedback {
            sender_ssrc: 42.into(),
            media_ssrc: 0.into(),
            // Payload that does NOT start with "REMB" magic bytes
            payload: vec![0x01, 0x02, 0x03, 0x04].into(),
        };

        let rtcp = Rtcp::AppSpecificFeedback(fb);
        let mut buf = [0u8; 256];
        let written = rtcp.write_to(&mut buf);

        let mut parsed = VecDeque::new();
        Rtcp::read_packet(&buf[..written], &mut parsed);

        assert_eq!(parsed.len(), 1);
        assert!(matches!(parsed[0], Rtcp::AppSpecificFeedback(_)));
    }
}
