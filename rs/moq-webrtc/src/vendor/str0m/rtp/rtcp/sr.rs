use std::time::SystemTime;

use crate::rtp_::MediaTime;
use crate::util::SystemTimeExt;

use super::{FeedbackMessageType, RtcpType, Ssrc};
use super::{ReceptionReport, ReportList, RtcpHeader, RtcpPacket};

/// A report of packets sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenderReport {
    /// Information about the sender of this report.
    pub sender_info: SenderInfo,
    /// A sender report is implicitly also a receiver report. This
    /// might hold data that would otherwise come in a separate RR.
    pub reports: ReportList<ReceptionReport>,
}

/// Information about a stream being sent.
///
/// A subset of the information contained in Sender Reports(SR).
///
/// See [RFC 3550 6.4.1](https://www.rfc-editor.org/rfc/rfc3550#section-6.4.1)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SenderInfo {
    /// The SSRC of the SR originator.
    pub ssrc: Ssrc,
    /// The 64 bit NTP timestamp converted to a [`SystemTime`].
    pub ntp_time: SystemTime,
    /// The RTP timestamp that corresponds to the same point in time as the NTP timestamp above.
    pub rtp_time: MediaTime,
    /// The total number of packets the sender had sent when this information was generated.
    pub sender_packet_count: u32,
    /// The total number of octets the sender had sent when this information was generated.
    pub sender_octet_count: u32,
}

impl RtcpPacket for SenderReport {
    fn header(&self) -> RtcpHeader {
        RtcpHeader {
            rtcp_type: RtcpType::SenderReport,
            feedback_message_type: FeedbackMessageType::ReceptionReport(self.reports.len() as u8),
            words_less_one: (self.length_words() - 1) as u16,
        }
    }

    fn length_words(&self) -> usize {
        // * header: 1
        // * sender info: 6
        // * reports: x 6
        1 + 6 + 6 * self.reports.len()
    }

    fn write_to(&self, buf: &mut [u8]) -> usize {
        self.header().write_to(buf);

        self.sender_info.write_to(&mut buf[4..]);

        for (i, r) in self.reports.iter().enumerate() {
            r.write_to(&mut buf[28 + i * 24..]);
        }

        self.length_words() * 4
    }
}
impl SenderInfo {
    fn write_to(&self, buf: &mut [u8]) {
        // pub ssrc: Ssrc,
        // pub ntp_time: MediaTime,
        // pub rtp_time: u32,
        // pub sender_packet_count: u32,
        // pub sender_octet_count: u32,
        buf[..4].copy_from_slice(&self.ssrc.to_be_bytes());

        let mt = self.ntp_time.as_ntp_64();
        buf[4..12].copy_from_slice(&mt.to_be_bytes());

        buf[12..16].copy_from_slice(&(self.rtp_time.numer() as u32).to_be_bytes());
        buf[16..20].copy_from_slice(&self.sender_packet_count.to_be_bytes());
        buf[20..24].copy_from_slice(&self.sender_octet_count.to_be_bytes());
    }
}

impl<'a> TryFrom<&'a [u8]> for SenderReport {
    type Error = &'static str;

    fn try_from(buf: &'a [u8]) -> Result<Self, Self::Error> {
        let sender_info = buf.try_into()?;

        let mut reports = ReportList::new();
        let mut buf = &buf[24..];

        let count = buf.len() / 24;

        let max = count.min(31);

        for _ in 0..max {
            let report = buf.try_into()?;
            reports.push(report);
            buf = &buf[24..];
        }

        Ok(SenderReport {
            sender_info,
            reports,
        })
    }
}

impl<'a> TryFrom<&'a [u8]> for SenderInfo {
    type Error = &'static str;

    fn try_from(buf: &'a [u8]) -> Result<Self, Self::Error> {
        if buf.len() < 24 {
            return Err("Less than 24 bytes for SenderInfo");
        }

        // Sender report shape is here
        // https://www.rfc-editor.org/rfc/rfc3550#section-6.4.1

        let ssrc = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]).into();

        let ntp_time = u64::from_be_bytes([
            buf[4], buf[5], buf[6], buf[7], buf[8], buf[9], buf[10], buf[11],
        ]);
        let ntp_time = SystemTime::from_ntp_64(ntp_time);

        // https://www.cs.columbia.edu/~hgs/rtp/faq.html#timestamp-computed
        // For video, time clock rate is fixed at 90 kHz. The timestamps generated
        // depend on whether the application can determine the frame number or not.
        // If it can or it can be sure that it is transmitting every frame with a
        // fixed frame rate, the timestamp is governed by the nominal frame rate.
        // Thus, for a 30 f/s video, timestamps would increase by 3,000 for each
        // frame, for a 25 f/s video by 3,600 for each frame.
        let rtp_time = u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]]);
        // The base (90kHz etc) is set higher up the stack (in StreamRx to be precise).
        let rtp_time = MediaTime::from_secs(rtp_time as u64);

        let sender_packet_count = u32::from_be_bytes([buf[16], buf[17], buf[18], buf[19]]);
        let sender_octet_count = u32::from_be_bytes([buf[20], buf[21], buf[22], buf[23]]);

        Ok(SenderInfo {
            ssrc,
            ntp_time,
            rtp_time,
            sender_packet_count,
            sender_octet_count,
        })
    }
}
