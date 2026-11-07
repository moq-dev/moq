use super::list::private::WordSized;
use super::{FeedbackMessageType, PayloadType, ReportList, RtcpHeader, RtcpPacket, RtcpType, Ssrc};

/// Full Intra Request (FIR).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fir {
    /// Sender of this feedback. Mostly irrelevant, but part of RTCP packets.
    pub sender_ssrc: Ssrc,
    /// The SSRC needing a full codec restart.
    pub reports: ReportList<FirEntry>,
}

/// Entry reported needing a codec restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirEntry {
    /// The SSRC needing a full codec restart.
    pub ssrc: Ssrc,
    /// Counter keeping track of which restart request this is.
    pub seq_no: u8,
}

impl RtcpPacket for Fir {
    fn header(&self) -> RtcpHeader {
        RtcpHeader {
            rtcp_type: RtcpType::PayloadSpecificFeedback,
            feedback_message_type: FeedbackMessageType::PayloadFeedback(
                PayloadType::FullIntraRequest,
            ),
            words_less_one: (self.length_words() - 1) as u16,
        }
    }

    fn length_words(&self) -> usize {
        // header
        // sender SSRC
        // media SSRC (set to 0)
        // reports * FirEntry: SSRC + seqNo
        1 + 1 + 1 + self.reports.len() * 2
    }

    fn write_to(&self, buf: &mut [u8]) -> usize {
        self.header().write_to(&mut buf[..4]);

        buf[4..8].copy_from_slice(&self.sender_ssrc.to_be_bytes());

        let media_ssrc = self.reports.iter().next().map(|s| *s.ssrc).unwrap_or(0);
        buf[8..12].copy_from_slice(&media_ssrc.to_be_bytes());

        let mut buf = &mut buf[12..];
        for r in &self.reports {
            buf[0..4].copy_from_slice(&r.ssrc.to_be_bytes());
            buf[4..8].copy_from_slice(&[r.seq_no, 0, 0, 0]);
            buf = &mut buf[8..];
        }

        4 + 4 + 4 + self.reports.len() * 8
    }
}

impl WordSized for FirEntry {
    fn word_size(&self) -> usize {
        2
    }
}

impl<'a> TryFrom<&'a [u8]> for Fir {
    type Error = &'static str;

    fn try_from(buf: &'a [u8]) -> Result<Self, Self::Error> {
        if buf.len() < 16 {
            return Err("Fir less than 16 bytes");
        }

        let sender_ssrc = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]).into();

        let mut reports = ReportList::new();

        let mut buf = &buf[8..];
        let count = buf.len() / 8;
        let max = count.min(31);

        for _ in 0..max {
            let ssrc = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]).into();
            let seq_no = buf[4];
            reports.push(FirEntry { ssrc, seq_no });
            buf = &buf[8..];
        }

        Ok(Fir {
            sender_ssrc,
            reports,
        })
    }
}
