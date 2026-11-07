use super::Ssrc;
use super::list::private::WordSized;
use super::{FeedbackMessageType, ReportList, RtcpHeader, RtcpPacket, RtcpType};

/// A receiver report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiverReport {
    /// Sender of this feedback. Mostly irrelevant, but part of RTCP packets.
    pub sender_ssrc: Ssrc,
    /// The individual reports for received SSRC.
    pub reports: ReportList<ReceptionReport>,
}

/// An individual report of reception.
#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceptionReport {
    pub ssrc: Ssrc,
    pub fraction_lost: u8,
    pub packets_lost: u32, // 24 bit
    pub max_seq: u32,
    pub jitter: u32,
    pub last_sr_time: u32,
    pub last_sr_delay: u32,
}

impl RtcpPacket for ReceiverReport {
    fn header(&self) -> RtcpHeader {
        RtcpHeader {
            rtcp_type: RtcpType::ReceiverReport,
            feedback_message_type: FeedbackMessageType::ReceptionReport(self.reports.len() as u8),
            words_less_one: (self.length_words() - 1) as u16,
        }
    }

    fn length_words(&self) -> usize {
        // * header: 1
        // * sender SSRC
        // * reports: x 6
        1 + 1 + 6 * self.reports.len()
    }

    fn write_to(&self, buf: &mut [u8]) -> usize {
        self.header().write_to(buf);

        buf[4..8].copy_from_slice(&self.sender_ssrc.to_be_bytes());

        for (i, r) in self.reports.iter().enumerate() {
            r.write_to(&mut buf[8 + i * 24..]);
        }

        self.length_words() * 4
    }
}

impl WordSized for ReceptionReport {
    fn word_size(&self) -> usize {
        6
    }
}

impl ReceptionReport {
    pub(crate) fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.ssrc.to_be_bytes());
        buf[4..8].copy_from_slice(&self.packets_lost.to_be_bytes());
        buf[4] = self.fraction_lost;
        buf[8..12].copy_from_slice(&self.max_seq.to_be_bytes());
        buf[12..16].copy_from_slice(&self.jitter.to_be_bytes());
        buf[16..20].copy_from_slice(&self.last_sr_time.to_be_bytes());
        buf[20..24].copy_from_slice(&self.last_sr_delay.to_be_bytes());
    }
}

impl<'a> TryFrom<&'a [u8]> for ReceiverReport {
    type Error = &'static str;

    fn try_from(buf: &'a [u8]) -> Result<Self, Self::Error> {
        if buf.len() < 4 {
            return Err("Less than 4 bytes for ReceiverReport");
        }

        let sender_ssrc = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]).into();

        let mut reports = ReportList::new();
        let mut buf = &buf[4..];

        let count = buf.len() / 24;

        let max = count.min(31);

        for _ in 0..max {
            let report = buf.try_into()?;
            reports.push(report);
            buf = &buf[24..];
        }

        Ok(ReceiverReport {
            sender_ssrc,
            reports,
        })
    }
}

impl<'a> TryFrom<&'a [u8]> for ReceptionReport {
    type Error = &'static str;

    fn try_from(buf: &'a [u8]) -> Result<Self, Self::Error> {
        if buf.len() < 24 {
            return Err("Less than 24 bytes for ReceptionReport");
        }

        // Receiver report shape is here
        // https://www.rfc-editor.org/rfc/rfc3550#section-6.4.2

        let ssrc = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]).into();
        let fraction_lost = buf[4];
        let packets_lost = u32::from_be_bytes([0, buf[5], buf[6], buf[7]]);
        let max_seq = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);
        let jitter = u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]]);
        let last_sr_time = u32::from_be_bytes([buf[16], buf[17], buf[18], buf[19]]);
        let last_sr_delay = u32::from_be_bytes([buf[20], buf[21], buf[22], buf[23]]);

        Ok(ReceptionReport {
            ssrc,
            fraction_lost,
            packets_lost,
            max_seq,
            jitter,
            last_sr_time,
            last_sr_delay,
        })
    }
}
