//! AMR-WB RTP payload format, [RFC 4867](https://datatracker.ietf.org/doc/html/rfc4867).
//!
//! str0m doesn't encode or decode AMR-WB. The application exchanges frames in the
//! 3GPP IF / storage format (RFC 4867 §5.3): a header octet `(FT << 3) | (Q << 2)`
//! followed by the frame's speech bits, zero-padded to a whole octet.
//!
//! Sending: write exactly one 20 ms frame per call. It goes out as one RTP payload
//! with codec mode request 15 (no request). NO_DATA frames aren't sent.
//!
//! Receiving: `MediaData` holds every frame of an RTP payload back to back, 20 ms
//! apart starting at `MediaData::time`. Peers may resend earlier frames for
//! redundancy (RFC 4867 §4.1), so the same frame time can arrive more than once.
//! The codec mode request is ignored and padding bits are ignored. Payloads with
//! reserved frame types, missing speech bits, or more bytes than their table of
//! contents describes are dropped.
//!
//! Supported in SDP negotiation:
//! - Mono payloads in the octet-aligned (`octet-align=1`, used by `enable_amr_wb`)
//!   or bandwidth-efficient layout. Both sides must use the same layout.
//! - `mode-set`, from either side. It applies to both directions, so the
//!   application must only write frames in those modes.
//!
//! Declined: CRCs (`crc=1`), robust sorting (`robust-sorting=1`), interleaving,
//! more than one channel, and `mode-change-period=2`, which str0m can't enforce on
//! application frames. Other AMR-WB parameters, such as `max-red` and
//! `mode-change-capability`, are ignored and left out of answers.

use super::{BitRead, CodecExtra, Depacketizer, PacketError, Packetizer};

/// Speech bits per frame type (RFC 4867 Table 1a). `None` marks the reserved types.
const FRAME_BITS: [Option<usize>; 16] = [
    Some(132), // 6.60 kbit/s
    Some(177), // 8.85 kbit/s
    Some(253), // 12.65 kbit/s
    Some(285), // 14.25 kbit/s
    Some(317), // 15.85 kbit/s
    Some(365), // 18.25 kbit/s
    Some(397), // 19.85 kbit/s
    Some(461), // 23.05 kbit/s
    Some(477), // 23.85 kbit/s
    Some(40),  // SID
    None,
    None,
    None,
    None,
    Some(0), // SPEECH_LOST
    Some(0), // NO_DATA
];

const NO_DATA: u8 = 15;

/// First payload octet with Codec Mode Request 15 ("no mode requested").
const CMR_NO_REQUEST: u8 = 0xf0;

/// Packetizes one AMR-WB IF frame per RTP payload.
#[derive(Debug, Default)]
pub struct AmrWbPacketizer {
    pub(crate) octet_align: bool,
}

impl Packetizer for AmrWbPacketizer {
    fn packetize(&mut self, mtu: usize, frame: &[u8]) -> Result<Vec<Vec<u8>>, PacketError> {
        let Some((&header, speech)) = frame.split_first() else {
            return Ok(vec![]);
        };

        let frame_type = (header >> 3) & 0x0f;
        let bits = FRAME_BITS[frame_type as usize].ok_or(PacketError::ErrAmrWbCorruptedPacket)?;
        if speech.len() != bits.div_ceil(8) {
            return Err(PacketError::ErrAmrWbCorruptedPacket);
        }

        // RFC 4867 §4.3.2: payloads with only NO_DATA frames should not be sent.
        if frame_type == NO_DATA {
            return Ok(vec![]);
        }

        // A ToC entry F|FT|Q with F=0 (last frame) is the IF header without its P bits.
        let toc = header & 0x7c;
        let (mut payload, header_bits) = if self.octet_align {
            // CMR and 4 reserved bits, the ToC entry and 2 padding bits, then the
            // speech bits, which are already octet-aligned in the IF frame.
            ([&[CMR_NO_REQUEST, toc][..], speech].concat(), 16)
        } else {
            // The 4-bit CMR and 6-bit ToC entry put the speech bits 2 bits to the
            // right of where they are in the IF frame.
            let mut payload = vec![CMR_NO_REQUEST | (toc >> 4)];
            let mut carry = toc << 4;
            for &byte in speech {
                payload.push(carry | (byte >> 2));
                carry = byte << 6;
            }
            payload.push(carry);
            (payload, 10)
        };
        payload.truncate((header_bits + bits).div_ceil(8));

        // Clear the trailing padding bits, which the IF frame may leave set.
        let padding = (8 - (header_bits + bits) % 8) % 8;
        let last = payload.len() - 1;
        payload[last] &= 0xff << padding;

        if payload.len() > mtu {
            return Err(PacketError::ErrPayloadTooLarge);
        }

        Ok(vec![payload])
    }

    fn is_marker(&mut self, _data: &[u8], _previous: Option<&[u8]>, _last: bool) -> bool {
        false
    }

    fn marks_talkspurt(&self) -> bool {
        true
    }

    fn nackable(&self) -> bool {
        false
    }
}

/// Depacketizes AMR-WB RTP payloads into concatenated IF frames.
#[derive(Debug, Default)]
pub struct AmrWbDepacketizer {
    pub(crate) octet_align: bool,
}

impl Depacketizer for AmrWbDepacketizer {
    fn out_size_hint(&self, packets_size: usize) -> Option<usize> {
        Some(packets_size)
    }

    fn depacketize(
        &mut self,
        packet: &[u8],
        out: &mut Vec<u8>,
        _: &mut CodecExtra,
    ) -> Result<(), PacketError> {
        if packet.is_empty() {
            return Ok(());
        }

        // Skip the codec mode request, and its 4 reserved bits when octet-aligned.
        let mut reader: (&[u8], usize) = (packet, if self.octet_align { 8 } else { 4 });

        // Table of contents: an F|FT|Q entry per frame, where F=1 means another follows.
        let mut headers = Vec::new();
        loop {
            let entry = reader.get_bits(6).ok_or(PacketError::ErrShortPacket)?;
            if self.octet_align {
                reader.get_bits(2).ok_or(PacketError::ErrShortPacket)?;
            }
            // The IF frame header holds the entry's FT and Q.
            headers.push((entry << 2) & 0x7c);
            if entry & 0x20 == 0 {
                break;
            }
        }

        for header in headers {
            let bits =
                FRAME_BITS[(header >> 3) as usize].ok_or(PacketError::ErrAmrWbCorruptedPacket)?;
            out.push(header);
            for _ in 0..bits / 8 {
                out.push(reader.get_u8().ok_or(PacketError::ErrShortPacket)?);
            }
            let rest = bits % 8;
            if rest > 0 {
                let tail = reader.get_bits(rest).ok_or(PacketError::ErrShortPacket)?;
                out.push(tail << (8 - rest));
            }
            if self.octet_align {
                reader.1 = reader.1.next_multiple_of(8);
            }
        }

        // RFC 4867 §4.5.1: discard payloads longer than the ToC describes. At most
        // seven padding bits may remain, and their values are ignored.
        if reader.remaining_bits() >= 8 {
            return Err(PacketError::ErrAmrWbCorruptedPacket);
        }

        Ok(())
    }

    fn is_partition_head(&self, _packet: &[u8]) -> bool {
        true
    }

    fn is_partition_tail(&self, _marker: bool, _packet: &[u8]) -> bool {
        true
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /// An IF frame of `frame_type` (Q=1) whose speech bytes are `fill`, padding cleared.
    fn if_frame(frame_type: u8, fill: u8) -> Vec<u8> {
        let bits = FRAME_BITS[frame_type as usize].unwrap();
        let mut frame = vec![(frame_type << 3) | 0x04];
        frame.extend(std::iter::repeat_n(fill, bits.div_ceil(8)));
        if bits % 8 != 0 {
            *frame.last_mut().unwrap() &= 0xff << (8 - bits % 8);
        }
        frame
    }

    fn packetize(octet_align: bool, frame: &[u8]) -> Vec<u8> {
        let mut packets = AmrWbPacketizer { octet_align }
            .packetize(1200, frame)
            .unwrap();
        assert_eq!(packets.len(), 1);
        packets.remove(0)
    }

    fn depacketize(octet_align: bool, payload: &[u8]) -> Result<Vec<u8>, PacketError> {
        let mut out = Vec::new();
        AmrWbDepacketizer { octet_align }.depacketize(payload, &mut out, &mut CodecExtra::None)?;
        Ok(out)
    }

    #[test]
    fn packetizer_uses_audio_packet_policy() {
        let pack = crate::packet::CodecPacketizer::from(crate::format::Codec::AmrWb);
        assert!(pack.marks_talkspurt());
        assert!(!pack.nackable());
    }

    #[test]
    fn round_trips_every_frame_type_in_both_layouts() {
        for octet_align in [false, true] {
            for frame_type in (0..=9).chain([14]) {
                let frame = if_frame(frame_type, 0xa5);
                let payload = packetize(octet_align, &frame);
                assert_eq!(depacketize(octet_align, &payload).unwrap(), frame);
            }
        }
    }

    #[test]
    fn payload_layout_matches_rfc_4867() {
        let frame = if_frame(8, 0xa5);

        // CMR=15 and reserved bits, ToC F=0 FT=8 Q=1, then the octet-aligned frame.
        let payload = packetize(true, &frame);
        assert_eq!(payload[..2], [0xf0, 0x44]);
        assert_eq!(payload[2..], frame[1..]);

        // 4-bit CMR and 6-bit ToC, then the 477 speech bits shifted 2 bits right, so
        // each 0xa5 becomes 0x69, and one padding bit.
        let payload = packetize(false, &frame);
        assert_eq!(payload.len(), 61);
        assert_eq!(payload[0], 0xf4);
        assert!(payload[1..60].iter().all(|&b| b == 0x69));
        assert_eq!(payload[60], 0x68);
    }

    #[test]
    fn depacketizes_compound_payloads() {
        let lost = if_frame(14, 0);
        let speech = if_frame(0, 0xa5);
        let sid = if_frame(9, 0x5a);

        // One ToC octet per frame, then each frame padded to an octet. The padding
        // after the 132 speech bits is non-zero and must be ignored.
        let mut payload = vec![0xf0, 0x80 | lost[0], 0x80 | speech[0], sid[0]];
        payload.extend(&speech[1..]);
        *payload.last_mut().unwrap() |= 0x0f;
        payload.extend(&sid[1..]);
        assert_eq!(
            depacketize(true, &payload).unwrap(),
            [lost.clone(), speech, sid.clone()].concat()
        );

        // CMR=15, ToC [F=1 FT=14 Q=1] [F=0 FT=9 Q=1], then the 40 SID bits.
        let payload = [0xff, 0x53, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a];
        assert_eq!(depacketize(false, &payload).unwrap(), [lost, sid].concat());
    }

    #[test]
    fn padding_and_reserved_bits_are_ignored() {
        let frame = if_frame(0, 0xa5);

        let mut dirty = frame.clone();
        dirty[0] |= 0x83;
        *dirty.last_mut().unwrap() |= 0x0f;
        assert_eq!(packetize(true, &dirty), packetize(true, &frame));

        let mut payload = packetize(true, &frame);
        payload[0] |= 0x0f;
        payload[1] |= 0x03;
        *payload.last_mut().unwrap() |= 0x0f;
        assert_eq!(depacketize(true, &payload).unwrap(), frame);

        let mut payload = packetize(false, &frame);
        *payload.last_mut().unwrap() |= 0x03;
        assert_eq!(depacketize(false, &payload).unwrap(), frame);
    }

    #[test]
    fn packetizer_skips_empty_and_no_data_frames() {
        let mut pack = AmrWbPacketizer::default();
        assert!(pack.packetize(1200, &[]).unwrap().is_empty());
        assert!(pack.packetize(1200, &[0x7c]).unwrap().is_empty());
    }

    #[test]
    fn packetizer_rejects_invalid_frames() {
        let mut pack = AmrWbPacketizer { octet_align: true };
        let frame = if_frame(8, 0x11);

        let reserved = vec![10 << 3];
        let truncated = frame[..30].to_vec();
        let concatenated = [frame.clone(), if_frame(9, 0x22)].concat();
        for invalid in [reserved, truncated, concatenated] {
            assert_eq!(
                pack.packetize(1200, &invalid),
                Err(PacketError::ErrAmrWbCorruptedPacket)
            );
        }

        assert_eq!(
            pack.packetize(61, &frame),
            Err(PacketError::ErrPayloadTooLarge)
        );
    }

    #[test]
    fn depacketizer_rejects_malformed_payloads() {
        let mut payload = packetize(true, &if_frame(8, 0x11));

        assert_eq!(
            depacketize(true, &payload[..30]),
            Err(PacketError::ErrShortPacket)
        );
        assert_eq!(
            depacketize(false, &[0xf0]),
            Err(PacketError::ErrShortPacket)
        );
        assert_eq!(
            depacketize(true, &[0xf0, 10 << 3]),
            Err(PacketError::ErrAmrWbCorruptedPacket)
        );

        payload.push(0);
        assert_eq!(
            depacketize(true, &payload),
            Err(PacketError::ErrAmrWbCorruptedPacket)
        );
    }

    #[test]
    fn depacketizes_empty_payload_to_nothing() {
        assert!(depacketize(true, &[]).unwrap().is_empty());
    }
}
