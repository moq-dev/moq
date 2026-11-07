use crate::rtp::Ssrc;

use super::RtcpType;
use super::{FeedbackMessageType, PayloadType, RtcpHeader, RtcpPacket};

const BITRATE_MAX: f32 = 2.417_842_4e24; //0x3FFFFp+63;
const MANTISSA_MAX: u32 = 0x7FFFFF;
const REMB_OFFSET: usize = 16;

const UNIQUE_IDENTIFIER: [u8; 4] = *b"REMB";

/*
    0                   1                   2                   3
    0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
    +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
    |V=2|P| FMT=15  |   PT=206      |             length            |
    +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
    |                  SSRC of packet sender                        |
    +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
    |                  SSRC of media source                         |
    +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
    |  Unique identifier 'R' 'E' 'M' 'B'                            |
    +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
    |  Num SSRC     | BR Exp    |  BR Mantissa                      |
    +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
    |   SSRC feedback                                               |
    +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
    |  ...                                                          |
*/

#[derive(Debug, Clone)]
pub struct Remb {
    /// SSRC of sender
    pub sender_ssrc: Ssrc,

    /// SSRC of source, in Remb is default 0
    pub ssrc: Ssrc,

    /// Estimated maximum bitrate
    pub bitrate: f32,

    /// SSRC entries which this packet applies to
    pub ssrcs: Vec<u32>,
}

impl Eq for Remb {}
impl PartialEq for Remb {
    fn eq(&self, other: &Self) -> bool {
        self.sender_ssrc == other.sender_ssrc
            && (self.bitrate as u64) == (other.bitrate as u64)
            && self.ssrcs == other.ssrcs
    }
}

impl RtcpPacket for Remb {
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
        // header
        // remb
        // ssrcs
        1 + REMB_OFFSET / 4 + self.ssrcs.len()
    }

    fn write_to(&self, buf: &mut [u8]) -> usize {
        let mut exp = 0;
        let mut bitrate = self.bitrate.clamp(0.0, BITRATE_MAX);

        while bitrate >= (1 << 18) as f32 {
            bitrate /= 2.0;
            exp += 1;
        }

        let mantissa = bitrate.floor() as u32;

        self.header().write_to(&mut buf[..4]);
        buf[4..8].copy_from_slice(&self.sender_ssrc.to_be_bytes());
        buf[8..12].copy_from_slice(&[0; 4]);
        buf[12..16].copy_from_slice(&UNIQUE_IDENTIFIER);
        buf[16] = self.ssrcs.len() as u8;
        // We can't quite use the binary package because
        // a) it's a uint24 and b) the exponent is only 6-bits
        // Just trust me; this is big-endian encoding.
        buf[17] = (exp << 2) as u8 | (mantissa >> 16) as u8;
        buf[18] = (mantissa >> 8) as u8;
        buf[19] = mantissa as u8;

        // Write the SSRCs at the very end.
        for (index, ssrc) in self.ssrcs.iter().enumerate() {
            let begin = 4 + REMB_OFFSET + index * 4;
            let end = begin + 4;
            buf[begin..end].copy_from_slice(&ssrc.to_be_bytes());
        }

        4 + REMB_OFFSET + self.ssrcs.len() * 4
    }
}

impl<'a> TryFrom<&'a [u8]> for Remb {
    type Error = &'static str;

    fn try_from(buf: &'a [u8]) -> Result<Self, Self::Error> {
        if buf.len() < 16 {
            return Err("Remb less than 16 bytes");
        }

        let sender_ssrc = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]).into();
        let media_ssrc = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        if media_ssrc != 0 {
            return Err("Ssrc must be zero");
        }

        if buf[8] != UNIQUE_IDENTIFIER[0]
            || buf[9] != UNIQUE_IDENTIFIER[1]
            || buf[10] != UNIQUE_IDENTIFIER[2]
            || buf[11] != UNIQUE_IDENTIFIER[3]
        {
            return Err("Missing remb identifier");
        }

        // The next byte is the number of SSRC entries at the end.
        let ssrcs_len = buf[12] as usize;

        // Get the 6-bit exponent value.
        let b17 = buf[13];
        let mut exp = (b17 as u64) >> 2;
        exp += 127; // bias for IEEE754
        exp += 23; // IEEE754 biases the decimal to the left, abs-send-time biases it to the right

        // The remaining 2-bits plus the next 16-bits are the mantissa.
        let b18 = buf[14];
        let b19 = buf[15];
        let mut mantissa = ((b17 & 3) as u32) << 16 | (b18 as u32) << 8 | b19 as u32;

        if mantissa != 0 {
            // ieee754 requires an implicit leading bit
            while (mantissa & (MANTISSA_MAX + 1)) == 0 {
                exp -= 1;
                mantissa *= 2;
            }
        }

        // bitrate = mantissa * 2^exp
        let bitrate = f32::from_bits(((exp as u32) << 23) | (mantissa & MANTISSA_MAX));

        // `ssrcs_len` is attacker-controlled byte  (buf[12], 0..=255)
        // reject packets too short to hold that many trailing SSRC bytes.
        if buf.len() < REMB_OFFSET + ssrcs_len * 4 {
            return Err("Remb ssrcs length exceeds buffer");
        }

        let mut ssrcs = vec![];
        for i in 0..ssrcs_len {
            let b_index = 16 + i * 4;
            ssrcs.push(u32::from_be_bytes([
                buf[b_index],
                buf[b_index + 1],
                buf[b_index + 2],
                buf[b_index + 3],
            ]));
        }

        Ok(Remb {
            sender_ssrc,
            ssrc: 0.into(),
            ssrcs,
            bitrate,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_receiver_estimated_maximum_bitrate_marshal() {
        let input = Remb {
            sender_ssrc: 1.into(),
            ssrc: 0.into(),
            bitrate: 8927168.0,
            ssrcs: vec![1215622422],
        };

        let expected = [
            143, 206, 0, 5, 0, 0, 0, 1, 0, 0, 0, 0, 82, 69, 77, 66, 1, 26, 32, 223, 72, 116, 237,
            22,
        ];

        let mut output = [0; 1500];
        let len = input.write_to(&mut output);
        assert_eq!(expected, output[0..len]);
    }

    #[test]
    fn test_receiver_estimated_maximum_bitrate_unmarshal() {
        // Real data sent by Chrome while watching a 6Mb/s stream
        let input = [
            143, 206, 0, 5, 0, 0, 0, 1, 0, 0, 0, 0, 82, 69, 77, 66, 1, 26, 32, 223, 72, 116, 237,
            22,
        ];

        // mantissa = []byte{26 & 3, 32, 223} = []byte{2, 32, 223} = 139487
        // exp = 26 >> 2 = 6
        // bitrate = 139487 * 2^6 = 139487 * 64 = 8927168 = 8.9 Mb/s
        let expected = Remb {
            sender_ssrc: 1.into(),
            ssrc: 0.into(),
            bitrate: 8927168.0,
            ssrcs: vec![1215622422],
        };

        let packet = Remb::try_from(&input[4..]).unwrap();
        assert_eq!(expected, packet);
    }

    #[test]
    fn test_receiver_estimated_maximum_bitrate_truncate() {
        let input = [
            143, 206, 0, 5, 0, 0, 0, 1, 0, 0, 0, 0, 82, 69, 77, 66, 1, 26, 32, 223, 72, 116, 237,
            22,
        ];

        // Make sure that we're interpreting the bitrate correctly.
        // For the above example, we have:

        // mantissa = 139487
        // exp = 6
        // bitrate = 8927168

        let mut packet = Remb::try_from(&input[4..]).unwrap();
        assert_eq!(8927168.0, packet.bitrate);

        // Just verify marshal produces the same input.
        let mut output = [0; 1500];
        let output_len = packet.write_to(&mut output);
        assert_eq!(input, output[0..output_len]);

        // If we subtract the bitrate by 1, we'll round down a lower mantissa
        packet.bitrate -= 1.0;

        // bitrate = 8927167
        // mantissa = 139486
        // exp = 6

        let output_len = packet.write_to(&mut output);
        assert_ne!(input, output[0..output_len]);
        let expected = [
            143, 206, 0, 5, 0, 0, 0, 1, 0, 0, 0, 0, 82, 69, 77, 66, 1, 26, 32, 222, 72, 116, 237,
            22,
        ];
        assert_eq!(expected, output[0..output_len]);

        // Which if we actually unmarshal again, we'll find that it's actually decreased by 63 (which is exp)
        // mantissa = 139486
        // exp = 6
        // bitrate = 8927104

        let packet = Remb::try_from(&output[4..]).unwrap();
        assert_eq!(8927104.0, packet.bitrate);
    }

    #[test]
    fn test_receiver_estimated_maximum_bitrate_overflow() {
        // Marshal a packet with the maximum possible bitrate.
        let packet = Remb {
            sender_ssrc: 0.into(),
            ssrc: 0.into(),
            bitrate: f32::MAX,
            ssrcs: vec![],
        };

        // mantissa = 262143 = 0x3FFFF
        // exp = 63

        let expected = [
            143, 206, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 82, 69, 77, 66, 0, 255, 255, 255,
        ];

        let mut output = [0; 1500];
        let output_len = packet.write_to(&mut output);
        assert_eq!(expected, output[0..output_len]);

        // mantissa = 262143
        // exp = 63
        // bitrate = 0xFFFFC00000000000

        let packet = Remb::try_from(&output[4..output_len]).unwrap();
        assert_eq!(f32::from_bits(0x67FFFFC0), packet.bitrate);

        // Make sure we marshal to the same result again.
        let output_len = packet.write_to(&mut output);
        assert_eq!(expected, output[0..output_len]);

        // Finally, try unmarshalling one number higher than we used to be able to handle.
        let input = [
            143, 206, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 82, 69, 77, 66, 0, 188, 0, 0,
        ];
        let packet = Remb::try_from(&input[4..]).unwrap();
        assert_eq!(f32::from_bits(0x62800000), packet.bitrate);
    }
}
