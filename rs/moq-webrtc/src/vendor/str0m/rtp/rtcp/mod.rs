#![allow(clippy::unusual_byte_groupings)]

mod header;
use std::collections::VecDeque;

pub use header::{RtcpHeader, RtcpType};

mod list;
pub use list::ReportList;
use list::private::WordSized;

mod fmt;
pub use fmt::{FeedbackMessageType, PayloadType, TransportType};

mod sr;
pub use sr::{SenderInfo, SenderReport};

mod rr;
pub use rr::{ReceiverReport, ReceptionReport};

mod xr;
pub use xr::{Dlrr, DlrrItem, ExtendedReport, ReportBlock, Rrtr};

mod sdes;
pub use sdes::{Descriptions, Sdes, SdesType};

mod bb;
pub use bb::Goodbye;

mod nack;
pub use nack::{Nack, NackEntry};

mod pli;
pub use pli::Pli;

mod fir;
pub use fir::{Fir, FirEntry};

mod twcc;
pub use twcc::{Twcc, TwccPacketId, TwccRecvRegister, TwccSendRecord, TwccSendRegister};

mod rtcpfb;
pub use rtcpfb::RtcpFb;

mod psfbapp;
mod remb;
pub use psfbapp::AppSpecificFeedback;
pub use remb::Remb;

use super::SeqNo;
use super::Ssrc;
use super::extend_u16;

pub trait RtcpPacket {
    /// The...
    fn header(&self) -> RtcpHeader;

    /// Length of entire RTCP packet (including header) in words (4 bytes).
    fn length_words(&self) -> usize;

    /// Write this packet to the buffer.
    ///
    /// Panics if the buffer doesn't have capacity to hold length_words * 4 bytes.
    fn write_to(&self, buf: &mut [u8]) -> usize;
}

/// RTCP reports handled by str0m.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rtcp {
    /// Sender report. Also known as SR.
    SenderReport(SenderReport),
    /// Receiver report. Also known as RR.
    ReceiverReport(ReceiverReport),
    /// Extended  receiver report. Sometimes called XR.
    ///
    /// Always sent together with a receiver report.
    ExtendedReport(ExtendedReport),
    /// Description of Synchronization Sources (senders).
    SourceDescription(Descriptions),
    /// BYE. When a stream is over.
    Goodbye(Goodbye),
    /// Reports missing packets.
    Nack(Nack),
    /// Picture Loss Indiciation. When decoding a picture is not possible.
    Pli(Pli),
    /// Full Intra Request. Complete restart of a video decoder.
    Fir(Fir),
    /// Transport Wide Congestion Control. Feedback for every received RTP packet.
    Twcc(Twcc),
    /// Receiver Estimated Maximum Bitrate. Feedback to the sender about the maximum bitrate.
    Remb(Remb),
    /// Application-specific Payload-Specific Feedback (FMT=15, PT=206) that is not REMB.
    /// Used for application-specific PSFB feedback (RFC 4585 Section 6.4).
    AppSpecificFeedback(AppSpecificFeedback),
}

impl Rtcp {
    pub(crate) fn read_packet(buf: &[u8], feedback: &mut VecDeque<Rtcp>) {
        let mut buf = buf;
        loop {
            if buf.is_empty() {
                break;
            }

            let header: RtcpHeader = match buf.try_into() {
                Ok(v) => v,
                Err(e) => {
                    debug!("{}", e);
                    break;
                }
            };
            let has_padding = buf[0] & 0b00_1_00000 > 0;
            let full_length = header.length_words() * 4;

            if full_length > buf.len() {
                // this length is incorrect.
                break;
            }

            let unpadded_length = if has_padding {
                let pad = buf[full_length - 1] as usize;
                if full_length < pad {
                    debug!("buf.len() is less than padding: {} < {}", full_length, pad);
                    break;
                }
                full_length - pad
            } else {
                full_length
            };

            match (&buf[..unpadded_length]).try_into() {
                Ok(v) => feedback.push_back(v),
                Err(e) => debug!("{}", e),
            }

            buf = &buf[full_length..];
        }
    }

    pub(crate) fn write_packet(
        feedback: &mut VecDeque<Rtcp>,
        buf: &mut [u8],
        mut output: impl FnMut(Rtcp),
    ) -> usize {
        if feedback.is_empty() {
            return 0;
        }

        // Total length, in bytes, shrunk to be on the pad_to boundary.
        let total_len = buf.len();

        // Capacity in words
        let word_capacity = total_len / 4;

        // Pack RTCP feedback packets. Merge together ones of the same type.
        Rtcp::pack(feedback, word_capacity);

        let mut offset = 0;
        while let Some(fb) = feedback.front() {
            // Length of next item.
            let item_len = fb.length_words() * 4;

            // Capacity left in the buffer.
            let capacity = total_len - offset;
            if capacity < item_len {
                break;
            }

            // We definitely can fit the next RTCP item.
            let fb = feedback.pop_front().unwrap();
            let written = fb.write_to(&mut buf[offset..]);

            assert_eq!(
                written, item_len,
                "length_words equals write_to length: {fb:?}"
            );

            // When debugging we can pass an output to get the serialized packets.
            output(fb);

            // Move offsets for the amount written.
            offset += item_len;
        }

        offset
    }

    fn merge(&mut self, other: &mut Rtcp, words_left: usize) -> bool {
        match (self, other) {
            // Stack receiver reports into sender reports.
            (Rtcp::SenderReport(sr), Rtcp::ReceiverReport(rr)) => {
                let n = sr.reports.append_all_possible(&mut rr.reports, words_left);
                n > 0
            }

            // Stack receiver reports.
            (Rtcp::ReceiverReport(r1), Rtcp::ReceiverReport(r2)) => {
                let n = r1.reports.append_all_possible(&mut r2.reports, words_left);
                n > 0
            }

            // Stack source descriptions.
            (Rtcp::SourceDescription(s1), Rtcp::SourceDescription(s2)) => {
                let n = s1.reports.append_all_possible(&mut s2.reports, words_left);
                n > 0
            }

            // Stack source descriptions.
            (Rtcp::Goodbye(g1), Rtcp::Goodbye(g2)) => {
                let n = g1.reports.append_all_possible(&mut g2.reports, words_left);
                n > 0
            }

            // Stack Nack
            (Rtcp::Nack(n1), Rtcp::Nack(n2)) if n1.ssrc == n2.ssrc => {
                let n = n1.reports.append_all_possible(&mut n2.reports, words_left);
                n > 0
            }

            // Stack source descriptions.
            (Rtcp::Fir(f1), Rtcp::Fir(f2)) => {
                let n = f1.reports.append_all_possible(&mut f2.reports, words_left);
                n > 0
            }

            // No merge possible
            _ => false,
        }
    }

    fn is_full(&self) -> bool {
        match self {
            Rtcp::SenderReport(v) => v.reports.is_full(),
            Rtcp::ReceiverReport(v) => v.reports.is_full(),
            Rtcp::ExtendedReport(_) => true,
            Rtcp::SourceDescription(v) => v.reports.is_full(),
            Rtcp::Goodbye(v) => v.reports.is_full(),
            Rtcp::Nack(v) => v.reports.is_full(),
            Rtcp::Pli(_) => true,
            Rtcp::Fir(v) => v.reports.is_full(),
            Rtcp::Twcc(_) => true,
            Rtcp::Remb(_) => true,
            Rtcp::AppSpecificFeedback(_) => true,
        }
    }

    /// If this RtcpFb contains no reports (anymore). This can happen after
    /// merging reports together.
    fn is_empty(&self) -> bool {
        match self {
            // A SenderReport always has, at least, the SenderInfo part.
            Rtcp::SenderReport(_) => false,
            // ReceiverReport can become empty.
            Rtcp::ReceiverReport(v) => v.reports.is_empty(),
            // ExtendedReport can become empty.
            Rtcp::ExtendedReport(v) => v.blocks.is_empty(),
            // SourceDescription can become empty.
            Rtcp::SourceDescription(v) => v.reports.is_empty(),
            // Goodbye can become empty,
            Rtcp::Goodbye(v) => v.reports.is_empty(),
            // Nack can become empty
            Rtcp::Nack(v) => v.reports.is_empty(),
            // Nack is never empty
            Rtcp::Pli(_) => false,
            // Fir can be merged to empty.
            Rtcp::Fir(v) => v.reports.is_empty(),
            // A twcc report is never empty.
            Rtcp::Twcc(_) => false,
            // A REMB report is never empty.
            Rtcp::Remb(_) => false,
            // An AppSpecificFeedback report is never empty.
            Rtcp::AppSpecificFeedback(_) => false,
        }
    }

    fn pack(feedback: &mut VecDeque<Self>, mut word_capacity: usize) {
        // Index into feedback of item we are to pack into.
        let mut i = 0;
        let len = feedback.len();

        // Need at least on feedback to pack into, and one to take from.
        if len < 2 {
            return;
        }

        // SenderReport/ReceiveReport first for SRTCP.
        feedback.make_contiguous().sort_by_key(Self::order_no);

        'outer: loop {
            // If we reach last element, there is no more packing to do.
            if i == len - 1 {
                break;
            }

            let (pack_into, pack_from) = feedback.make_contiguous().split_at_mut(i + 1);
            let fb_a = pack_into.last_mut().unwrap();

            // abort if fb_a won't fit in the spare capacity.
            if word_capacity < fb_a.length_words() {
                break 'outer;
            }

            // if we manage to merge anything into fb_a.
            let mut any_change = false;

            // fb_b goes from the item _after_ i
            for fb_b in pack_from {
                // if fb_a is full (or empty), we don't want to move any more elements into fb_a.
                if fb_a.is_full() || fb_a.is_empty() {
                    break;
                }

                // abort if fb_a won't fit in the spare capacity.
                if word_capacity < fb_a.length_words() {
                    break 'outer;
                }

                // amount of capacity (in words) left to fill.
                let capacity = word_capacity - fb_a.length_words();

                // attempt to merge some elements into fb_a from fb_b.
                let did_merge = fb_a.merge(fb_b, capacity);
                any_change |= did_merge;
            }

            if !any_change {
                word_capacity -= fb_a.length_words();
                i += 1;
            }
        }

        // Prune empty.
        feedback.retain(|f| !f.is_empty());
    }

    fn order_no(&self) -> u8 {
        use Rtcp::*;
        match self {
            // SenderReport/ReceiverReport first since they possibly contain
            // the SSRC for the SRTCP encryption.
            SenderReport(_) => 0,
            ReceiverReport(_) => 1,

            SourceDescription(_) => 2,
            Nack(_) => 3,
            Pli(_) => 4,
            Fir(_) => 5,
            Twcc(_) => 6,
            Remb(_) => 7,
            AppSpecificFeedback(_) => 8,
            ExtendedReport(_) => 10,

            // Goodbye last since they remove stuff.
            Goodbye(_) => 11,
        }
    }
}

impl RtcpPacket for Rtcp {
    fn header(&self) -> RtcpHeader {
        match self {
            Rtcp::SenderReport(v) => v.header(),
            Rtcp::ReceiverReport(v) => v.header(),
            Rtcp::ExtendedReport(v) => v.header(),
            Rtcp::SourceDescription(v) => v.header(),
            Rtcp::Goodbye(v) => v.header(),
            Rtcp::Nack(v) => v.header(),
            Rtcp::Pli(v) => v.header(),
            Rtcp::Fir(v) => v.header(),
            Rtcp::Twcc(v) => v.header(),
            Rtcp::Remb(v) => v.header(),
            Rtcp::AppSpecificFeedback(v) => v.header(),
        }
    }

    fn length_words(&self) -> usize {
        match self {
            Rtcp::SenderReport(v) => v.length_words(),
            Rtcp::ReceiverReport(v) => v.length_words(),
            Rtcp::ExtendedReport(v) => v.length_words(),
            Rtcp::SourceDescription(v) => v.length_words(),
            Rtcp::Goodbye(v) => v.length_words(),
            Rtcp::Nack(v) => v.length_words(),
            Rtcp::Pli(v) => v.length_words(),
            Rtcp::Fir(v) => v.length_words(),
            Rtcp::Twcc(v) => v.length_words(),
            Rtcp::Remb(v) => v.length_words(),
            Rtcp::AppSpecificFeedback(v) => v.length_words(),
        }
    }

    fn write_to(&self, buf: &mut [u8]) -> usize {
        match self {
            Rtcp::SenderReport(v) => v.write_to(buf),
            Rtcp::ReceiverReport(v) => v.write_to(buf),
            Rtcp::ExtendedReport(v) => v.write_to(buf),
            Rtcp::SourceDescription(v) => v.write_to(buf),
            Rtcp::Goodbye(v) => v.write_to(buf),
            Rtcp::Nack(v) => v.write_to(buf),
            Rtcp::Pli(v) => v.write_to(buf),
            Rtcp::Fir(v) => v.write_to(buf),
            Rtcp::Twcc(v) => v.write_to(buf),
            Rtcp::Remb(v) => v.write_to(buf),
            Rtcp::AppSpecificFeedback(v) => v.write_to(buf),
        }
    }
}

impl<'a> TryFrom<&'a [u8]> for Rtcp {
    type Error = &'static str;

    fn try_from(buf: &'a [u8]) -> Result<Self, Self::Error> {
        let header: RtcpHeader = buf.try_into()?;

        // By constraining the length, all subparsing can go
        // until they exhaust the buffer length. This presupposes
        // padding is removed from the input.
        let buf = &buf[4..];

        Ok(match header.rtcp_type() {
            RtcpType::SenderReport => Rtcp::SenderReport(buf.try_into()?),
            RtcpType::ReceiverReport => Rtcp::ReceiverReport(buf.try_into()?),
            RtcpType::SourceDescription => Rtcp::SourceDescription(buf.try_into()?),
            RtcpType::Goodbye => Rtcp::Goodbye((header.count(), buf).try_into()?),
            RtcpType::ApplicationDefined => return Err("Ignore RTCP type: ApplicationDefined"),
            RtcpType::TransportLayerFeedback => {
                let tlfb = match header.feedback_message_type() {
                    FeedbackMessageType::TransportFeedback(v) => v,
                    _ => return Err("Expected TransportFeedback in FeedbackMessageType"),
                };

                match tlfb {
                    TransportType::Nack => Rtcp::Nack(buf.try_into()?),
                    TransportType::TransportWide => Rtcp::Twcc(buf.try_into()?),
                }
            }
            RtcpType::PayloadSpecificFeedback => {
                let plfb = match header.feedback_message_type() {
                    FeedbackMessageType::PayloadFeedback(v) => v,
                    _ => return Err("Expected PayloadFeedback in FeedbackMessageType"),
                };

                match plfb {
                    PayloadType::PictureLossIndication => Rtcp::Pli(buf.try_into()?),
                    PayloadType::SliceLossIndication => return Err("Ignore PayloadType type: SLI"),
                    PayloadType::ReferencePictureSelectionIndication => {
                        return Err("Ignore PayloadType type: RPSI");
                    }
                    PayloadType::FullIntraRequest => Rtcp::Fir(buf.try_into()?),
                    PayloadType::ApplicationLayer => {
                        if header.rtcp_type() == RtcpType::PayloadSpecificFeedback {
                            if let Ok(remb) = Remb::try_from(buf) {
                                return Ok(Rtcp::Remb(remb));
                            }
                            // Not REMB — parse as generic application-specific feedback
                            if let Ok(fb) = AppSpecificFeedback::try_from(buf) {
                                return Ok(Rtcp::AppSpecificFeedback(fb));
                            }
                        }
                        return Err("Ignore PayloadType: ApplicationLayer");
                    }
                }
            }
            RtcpType::ExtendedReport => Rtcp::ExtendedReport(buf.try_into()?),
        })
    }
}

impl WordSized for Ssrc {
    fn word_size(&self) -> usize {
        1
    }
}

/// Pad up to the next word (4 byte) boundary.
fn pad_bytes_to_word(n: usize) -> usize {
    let pad = 4 - n % 4;
    if pad == 4 { n } else { n + pad }
}

#[cfg(test)]
mod test {
    use std::time::{Duration, SystemTime};

    use crate::rtp_::MediaTime;

    use super::twcc::{Delta, PacketChunk, PacketStatus};
    use super::*;

    #[test]
    fn padding_of_rtcp() {
        let mut queue = VecDeque::new();
        let mut twcc = Twcc {
            sender_ssrc: 1.into(),
            ssrc: 0.into(),
            base_seq: 82,
            status_count: 3,
            reference_time: 25,
            feedback_count: 17,
            chunks: VecDeque::new(),
            delta: VecDeque::new(),
        };
        twcc.chunks
            .push_back(PacketChunk::Run(PacketStatus::ReceivedSmallDelta, 3));
        twcc.delta.push_back(Delta::Small(0x7c));
        twcc.delta.push_back(Delta::Small(0x93));
        twcc.delta.push_back(Delta::Small(0x84));
        queue.push_back(Rtcp::Twcc(twcc));
        let mut buf = vec![0; 1500];
        let n = Rtcp::write_packet(&mut queue, &mut buf, |_| {});
        buf.truncate(n);
        println!("{buf:02x?}");
        assert_eq!(
            &buf,
            &[
                // TWCC 0xaf got padding bit set
                0xaf, 0xcd, 0x00, 0x06, //
                0x00, 0x00, 0x00, 0x01, // sender SSRC
                0x00, 0x00, 0x00, 0x00, // media SSRC
                0x00, 0x52, // base seq
                0x00, 0x03, // status count
                0x00, 0x00, 0x19, // reference time
                0x11, // feedback count
                0x20, 0x03, // run of 3
                0x7c, 0x93, 0x84, // three small delta
                0x00, 0x00, 0x03 // padding
            ]
        );
    }

    #[test]
    fn pack_sr_4_rr() {
        let now = SystemTime::now();
        let mut queue = VecDeque::new();
        queue.push_back(rr(3));
        queue.push_back(rr(4));
        queue.push_back(rr(5));
        queue.push_back(sr(1, now)); // should be sorted to front

        Rtcp::pack(&mut queue, 350);

        assert_eq!(queue.len(), 1);

        let sr = match queue.pop_front().unwrap() {
            Rtcp::SenderReport(v) => v,
            _ => unreachable!(),
        };

        assert_eq!(sr.reports.len(), 4);
        let mut iter = sr.reports.iter();
        assert_eq!(iter.next().unwrap(), &report(2));
        assert_eq!(iter.next().unwrap(), &report(3));
        assert_eq!(iter.next().unwrap(), &report(4));
        assert_eq!(iter.next().unwrap(), &report(5));
    }

    #[test]
    fn pack_4_rr() {
        let mut queue = VecDeque::new();
        queue.push_back(rr(1));
        queue.push_back(rr(2));
        queue.push_back(rr(3));
        queue.push_back(rr(4));

        Rtcp::pack(&mut queue, 350);

        assert_eq!(queue.len(), 1);

        let sr = match queue.pop_front().unwrap() {
            Rtcp::ReceiverReport(v) => v,
            _ => unreachable!(),
        };

        assert_eq!(sr.reports.len(), 4);
        let mut iter = sr.reports.iter();
        assert_eq!(iter.next().unwrap(), &report(1));
        assert_eq!(iter.next().unwrap(), &report(2));
        assert_eq!(iter.next().unwrap(), &report(3));
        assert_eq!(iter.next().unwrap(), &report(4));
    }

    #[test]
    fn roundtrip_sr_rr() {
        let now = SystemTime::now();
        let mut feedback = VecDeque::new();
        feedback.push_back(sr(1, now));
        feedback.push_back(rr(3));
        feedback.push_back(rr(4));
        feedback.push_back(rr(5));

        let mut buf = vec![0_u8; 1360];
        let n = Rtcp::write_packet(&mut feedback, &mut buf, |_| {});
        buf.truncate(n);

        let mut parsed = VecDeque::new();
        Rtcp::read_packet(&buf, &mut parsed);

        let Rtcp::SenderReport(s) = parsed.get(0).unwrap() else {
            panic!("Not a SenderReport in Rtcp");
        };
        let now2 = s.sender_info.ntp_time;

        let mut compare = VecDeque::new();
        compare.push_back(sr(1, now2));
        compare.push_back(rr(3));
        compare.push_back(rr(4));
        compare.push_back(rr(5));
        Rtcp::pack(&mut compare, 1400);

        assert_eq!(parsed, compare);

        // Ensure ntp_time is not too far off.
        let abs = abs_time_delta(now, now2);
        assert!(abs < Duration::from_millis(1));
    }

    fn abs_time_delta(st1: SystemTime, st2: SystemTime) -> Duration {
        let delta = if st1 > st2 {
            st1.duration_since(st2)
        } else {
            st2.duration_since(st1)
        };
        delta.expect("delta should be absolute")
    }

    fn sr(ssrc: u32, ntp_time: SystemTime) -> Rtcp {
        Rtcp::SenderReport(SenderReport {
            sender_info: SenderInfo {
                ssrc: ssrc.into(),
                ntp_time,
                rtp_time: MediaTime::from_secs(4),
                sender_packet_count: 5,
                sender_octet_count: 6,
            },
            reports: report(2).into(),
        })
    }

    fn rr(ssrc: u32) -> Rtcp {
        Rtcp::ReceiverReport(ReceiverReport {
            sender_ssrc: 42.into(),
            reports: report(ssrc).into(),
        })
    }

    fn report(ssrc: u32) -> ReceptionReport {
        ReceptionReport {
            ssrc: ssrc.into(),
            fraction_lost: 3,
            packets_lost: 1234,
            max_seq: 4000,
            jitter: 5,
            last_sr_time: 12,
            last_sr_delay: 1,
        }
    }

    // fn sdes(ssrc: u32) -> RtcpFb {
    //     RtcpFb::Sdes(Sdes {
    //         ssrc: ssrc.into(),
    //         values: vec![
    //             (SdesType::NAME, "Martin".into()),
    //             (SdesType::TOOL, "str0m".into()),
    //             (SdesType::NOTE, "Writing things right here".into()),
    //         ],
    //     })
    // }

    // fn nack(ssrc: u32, pid: u16) -> RtcpFb {
    //     RtcpFb::Nack(Nack {
    //         ssrc: ssrc.into(),
    //         pid,
    //         blp: 0b1010_0101,
    //     })
    // }

    // fn gb(ssrc: u32) -> RtcpFb {
    //     RtcpFb::Goodbye(ssrc.into())
    // }

    #[test]
    fn fuzz_failures() {
        const TESTS: &[&[u8]] = &[
            //
            &[133, 201, 0, 0],
            &[191, 202, 54, 74],
            &[166, 202, 0, 2, 218, 54, 214, 222, 160, 2, 146, 0, 251],
            &[
                151, 203, 0, 40, 88, 236, 217, 19, 82, 62, 73, 84, 112, 252, 69, 78, 38, 72, 43, 4,
                21, 136, 90, 29, 89, 70, 90, 196, 149, 168, 54, 1, 57, 16, 128, 8, 53, 172, 192,
                248, 175, 7, 92, 54, 82, 153, 179, 204, 181, 64, 94, 211, 67, 77, 110, 252, 181,
                18, 53, 48, 180, 179, 205, 234, 139, 61, 179, 54, 19, 120, 79, 119, 232, 208, 210,
                73, 78, 28, 242, 156, 242, 239, 19, 246, 183, 10, 49, 114, 216, 64, 105, 161, 50,
                99, 156, 113, 153, 90, 207, 53, 145, 96, 158, 198, 224, 114, 9, 20, 30, 156, 220,
                56, 151, 216, 164, 129, 156, 40, 85, 70, 189, 210, 146, 242, 242, 55, 70, 144, 113,
                9, 44, 74, 22, 123, 180, 153, 18, 88, 1, 185, 85, 227, 200, 62, 53, 142, 89, 28,
                37, 128, 223, 36, 248, 117, 26, 182, 173, 112, 42, 1, 2, 117, 203, 114, 179,
            ],
            &[
                150, 202, 0, 54, 0, 149, 201, 0, 0, 138, 201, 0, 0, 152, 201, 0, 0, 151, 201, 0, 0,
                150, 201, 0, 0, 141, 201, 0, 0, 159, 201, 0, 0, 150, 201, 0, 0, 159, 201, 0, 0,
                134, 201, 0, 0, 143, 201, 0, 0, 162, 201, 0, 0, 166, 201, 0, 0, 177, 201, 0, 0,
                182, 201, 0, 0, 131, 201, 0, 0, 164, 201, 0, 0, 133, 201, 0, 0, 143, 201, 0, 0,
                174, 201, 0, 0, 186, 201, 0, 0, 165, 201, 0, 0, 173, 201, 0, 0, 186, 201, 0, 0,
                166, 201, 0, 0, 159, 201, 0, 0, 158, 201, 0, 0, 190, 201, 0, 0, 156, 201, 0, 0,
                147, 201, 0, 0, 169, 201, 0, 0, 135, 201, 0, 0, 148, 201, 0, 0, 132, 201, 0, 0,
                138, 201, 0, 0, 162, 201, 0, 0, 185, 201, 0, 0, 157, 201, 0, 0, 183, 201, 0, 0,
                145, 201, 0, 0, 130, 201, 0, 0, 183, 201, 0, 0, 152, 201, 0, 0, 153, 201, 0, 0,
                154, 201, 0, 0, 138, 201, 0, 0, 148, 201, 0, 0, 158, 201, 0, 0, 156, 201, 0, 0,
                181, 201, 0, 0, 173, 201, 0, 0, 171, 201, 0, 0, 169, 201, 0, 0, 167, 201, 41, 216,
            ],
            &[
                143, 205, 0, 8, 143, 93, 208, 93, 201, 4, 131, 131, 131, 3, 0, 143, 1, 143, 0, 143,
                0, 80, 143, 231, 231, 0, 143, 181, 202, 0, 143, 236, 242, 0, 238, 21,
            ],
        ];

        let mut parsed = VecDeque::new();

        for t in TESTS {
            parsed.clear();
            Rtcp::read_packet(t, &mut parsed);
        }
    }
}
