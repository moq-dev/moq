use super::extend_u16;
use super::{FeedbackMessageType, ReportList, RtcpHeader, RtcpPacket, SeqNo};
use super::{RtcpType, Ssrc, TransportType};

use super::list::private::WordSized;

/// A NACK entry indiciating packets missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nack {
    /// Sender of this feedback. Mostly irrelevant, but part of RTCP packets.
    pub sender_ssrc: Ssrc,
    /// The SSRC this nack reports missing packets for.
    pub ssrc: Ssrc,
    /// The missing nack. This can be multiple segments.
    pub reports: ReportList<NackEntry>,
}

/// A range of sequence numbers missing.
#[allow(missing_docs)]
#[derive(Debug, PartialEq, Eq, Default, Clone, Copy)]
pub struct NackEntry {
    pub pid: u16,
    pub blp: u16,
}

impl RtcpPacket for Nack {
    fn header(&self) -> RtcpHeader {
        RtcpHeader {
            rtcp_type: RtcpType::TransportLayerFeedback,
            feedback_message_type: FeedbackMessageType::TransportFeedback(TransportType::Nack),
            words_less_one: (self.length_words() - 1) as u16,
        }
    }

    fn length_words(&self) -> usize {
        // header
        // sender SSRC
        // media SSRC
        // 1 word per NackPair
        1 + 2 + self.reports.len()
    }

    fn write_to(&self, buf: &mut [u8]) -> usize {
        self.header().write_to(&mut buf[..4]);
        buf[4..8].copy_from_slice(&self.sender_ssrc.to_be_bytes());
        buf[8..12].copy_from_slice(&self.ssrc.to_be_bytes());
        let mut buf = &mut buf[12..];
        for r in &self.reports {
            buf[0..2].copy_from_slice(&r.pid.to_be_bytes());
            buf[2..4].copy_from_slice(&r.blp.to_be_bytes());
            buf = &mut buf[4..];
        }
        self.length_words() * 4
    }
}

impl WordSized for NackEntry {
    fn word_size(&self) -> usize {
        1
    }
}

impl<'a> TryFrom<&'a [u8]> for Nack {
    type Error = &'static str;

    fn try_from(buf: &'a [u8]) -> Result<Self, Self::Error> {
        if buf.len() < 12 {
            return Err("Nack less than 12 bytes");
        }

        let sender_ssrc = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]).into();
        let ssrc = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]).into();

        let mut reports = ReportList::new();

        let mut buf = &buf[8..];
        let count = buf.len() / 4;
        let max = count.min(31);

        for _ in 0..max {
            let pid = u16::from_be_bytes([buf[0], buf[1]]);
            let blp = u16::from_be_bytes([buf[2], buf[3]]);
            reports.push(NackEntry { pid, blp });
            buf = &buf[4..];
        }

        Ok(Nack {
            sender_ssrc,
            ssrc,
            reports,
        })
    }
}

impl NackEntry {
    /// Iterator over sequence numbers missing.
    ///
    /// The given sequence number is used to interpret ROC.
    pub fn into_iter(self, seq_no: SeqNo) -> impl Iterator<Item = SeqNo> {
        NackEntryIterator(self, 0, seq_no)
    }
}

pub struct NackEntryIterator(NackEntry, u16, SeqNo);

impl Iterator for NackEntryIterator {
    type Item = SeqNo;

    fn next(&mut self) -> Option<Self::Item> {
        let seq_16 = if self.1 == 0 {
            self.1 += 1;
            self.0.pid
        } else {
            loop {
                if self.1 >= 17 {
                    return None;
                }
                let i = self.1 - 1;
                self.1 += 1;
                if 1 << i & self.0.blp > 0 {
                    break self.0.pid.wrapping_add(self.1 - 1);
                }
            }
        };
        let l = extend_u16(Some(*self.2), seq_16);
        Some(l.into())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn nack_entry_iter() {
        // 196_618
        let seq_no: SeqNo = (65_536_u64 * 3 + 10).into();

        // 196_508
        let pid = (65_536_u32 - 100) as u16;

        println!("{seq_no:?} {pid:?}");

        // 196_509, 196_512, 196_524
        let blp = 0b1000_0000_0000_1001;

        let entry = NackEntry { pid, blp };

        let nacks: Vec<_> = entry.into_iter(seq_no).collect();

        assert_eq!(
            nacks,
            vec![196508.into(), 196509.into(), 196512.into(), 196524.into()]
        );
    }
}
