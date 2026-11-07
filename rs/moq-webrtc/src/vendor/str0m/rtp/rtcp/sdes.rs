use std::str::from_utf8;

use super::list::private::WordSized;
use super::{FeedbackMessageType, RtcpType, Ssrc};
use super::{ReportList, RtcpHeader, RtcpPacket, pad_bytes_to_word};

/// Multiple source descriptions (SDES).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptions {
    /// The descriptions.
    pub reports: Box<ReportList<Sdes>>,
}

/// A single source description (SDES).
#[allow(missing_docs)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sdes {
    pub ssrc: Ssrc,
    pub values: ReportList<(SdesType, String)>,
}

/// Types of SDES values.
#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[allow(missing_docs)]
pub enum SdesType {
    /// End of SDES list
    END = 0,
    /// Canonical name.
    CNAME = 1,
    /// User name
    NAME = 2,
    /// User's electronic mail address
    EMAIL = 3,
    /// User's phone number
    PHONE = 4,
    /// Geographic user location
    LOC = 5,
    /// Name of application or tool
    TOOL = 6,
    /// Notice about the source
    NOTE = 7,
    /// Private extensions
    PRIV = 8,
    /// Rtp stream ID.
    RtpStreamId = 12,
    /// Repaired rtp stream ID.
    RepairedRtpStreamId = 13,
    /// Who knows
    Unknown,
}

impl RtcpPacket for Descriptions {
    fn header(&self) -> RtcpHeader {
        RtcpHeader {
            rtcp_type: RtcpType::SourceDescription,
            feedback_message_type: FeedbackMessageType::SourceCount(self.reports.len() as u8),
            words_less_one: (self.length_words() - 1) as u16,
        }
    }

    fn length_words(&self) -> usize {
        // * header: 1
        // * size-per-item * items
        1 + self.reports.iter().map(|r| r.word_size()).sum::<usize>()
    }

    fn write_to(&self, buf: &mut [u8]) -> usize {
        self.header().write_to(buf);

        let mut buf = &mut buf[4..];
        let mut tot = 4;

        for r in &*self.reports {
            let n = r.write_to(buf);
            buf = &mut buf[n..];
            tot += n;
        }

        tot
    }
}

impl Sdes {
    fn write_to(&self, buf: &mut [u8]) -> usize {
        buf[..4].copy_from_slice(&self.ssrc.to_be_bytes());
        let mut tot = 4;

        let mut buf = &mut buf[4..];
        for (t, v) in &self.values {
            let bytes = v.as_bytes();
            let len = bytes.len();

            buf[0] = *t as u8;
            buf[1] = len as u8;

            buf = &mut buf[2..];
            buf[..len].copy_from_slice(bytes);

            buf = &mut buf[len..];
            tot += 2 + len;
        }

        buf[0] = SdesType::END as u8;
        buf = &mut buf[1..];
        tot += 1;

        let pad = 4 - tot % 4;
        if pad < 4 {
            #[allow(clippy::needless_range_loop)]
            for i in 0..pad {
                buf[i] = SdesType::END as u8;
            }
            tot += pad;
        }

        assert!(tot % 4 == 0, "Sdes is padded to word boundary");

        tot
    }
}

impl WordSized for Sdes {
    fn word_size(&self) -> usize {
        let byte_size = 4
            + self
                .values
                .iter()
                // 2 here for 2 byte encoding of type + length
                .map(|(_, s)| 2 + s.len())
                .sum::<usize>()
            + 1; // 1 here for the end byte

        let padded = pad_bytes_to_word(byte_size);

        padded / 4
    }
}

impl From<u8> for SdesType {
    fn from(v: u8) -> Self {
        use SdesType::*;
        match v {
            0 => END,
            1 => CNAME,
            2 => NAME,
            3 => EMAIL,
            4 => PHONE,
            5 => LOC,
            6 => TOOL,
            7 => NOTE,
            8 => PRIV,
            12 => RtpStreamId,
            13 => RepairedRtpStreamId,
            _ => Unknown,
        }
    }
}

impl<'a> TryFrom<&'a [u8]> for Descriptions {
    type Error = &'static str;

    fn try_from(buf: &'a [u8]) -> Result<Self, Self::Error> {
        let mut reports = ReportList::new();

        let mut buf = buf;

        loop {
            if reports.len() == 31 {
                break;
            }
            // For some reason FF sends us a full SDES and then [0,0,0,0] at the end.
            // This can't be interpreted as SDES, so we just ignore it.
            if buf.len() < 8 {
                break;
            }
            let report: Sdes = buf.try_into()?;

            let len = report.word_size() * 4;

            reports.push(report);

            // word_size() pads up to a 32-bit boundary and may report more bytes
            // than remain for a trailing or truncated chunk. Guard against slicing
            // past the end (which would panic) and against a zero len that would
            // otherwise spin forever.
            if len == 0 || len >= buf.len() {
                break;
            }

            buf = &buf[len..];
        }

        Ok(Descriptions {
            reports: Box::new(reports),
        })
    }
}

impl<'a> TryFrom<&'a [u8]> for Sdes {
    type Error = &'static str;

    fn try_from(buf: &'a [u8]) -> Result<Self, Self::Error> {
        if buf.len() < 8 {
            return Err("Less than 8 bytes for Sdes");
        }

        let ssrc = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]).into();
        let mut values = ReportList::new();

        let mut buf = &buf[4..];
        let mut abs = 0;

        loop {
            // SDES items are END-terminated rather than count-bounded, so a
            // malformed chunk could contain more than the ReportList capacity.
            // Stop at the cap rather than panicking on the 32nd push.
            if values.is_full() {
                break;
            }

            // Per RFC 3550 Section 6.5:
            // "The list of items in each chunk MUST be terminated by one or more null octets,
            // the first of which is interpreted as an item type of end of list.
            // No length octet follows the null item type octet, but additional null octets
            // MUST be included if needed to pad until the next 32-bit boundary."
            //
            // This means we need a special case when there's only a single END byte (0x00)
            // at the end of the buffer - it's a valid terminator despite not having a length byte.
            if buf.len() == 1 && buf[0] == SdesType::END as u8 {
                break;
            }

            if buf.len() < 2 {
                return Err("Less than 2 bytes for next Sdes value");
            }

            let stype: SdesType = buf[0].into();

            if matches!(stype, SdesType::END) {
                // The end of SDES.

                // Each chunk consists of an SSRC/CSRC identifier followed by a list of
                // zero or more items, which carry information about the SSRC/CSRC.
                // Each chunk starts on a 32-bit boundary.
                //
                // Items are contiguous, i.e., items are not individually padded to a
                // 32-bit boundary.  Text is not null terminated because some multi-
                // octet encodings include null octets.
                //
                // No length octet follows the null item type octet, but additional null
                // octets MUST be included if needed to pad until the next 32-bit
                // boundary.

                let pad = 4 - abs % 4;
                if pad < 4 && buf.len() < pad {
                    return Err("Not enough buf.len() for Sdes padding");
                }

                break;
            }

            let len = buf[1] as usize;

            if buf.len() < 2 + len {
                return Err("Not enough buf.len() for Sdes value");
            }
            buf = &buf[2..];
            abs += 2;

            if let Ok(value) = from_utf8(&buf[..len]) {
                values.push((stype, value.to_string()));
            } else {
                // failed to read as utf-8. skip.
            }

            buf = &buf[len..];
            abs += len;
        }

        Ok(Sdes { ssrc, values })
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn computed_and_write_to_equal() {
        let mut buf = vec![0; 1500];

        for i in 1usize..=255 {
            let mut r = ReportList::new();
            r.push((
                SdesType::CNAME,
                String::from_utf8_lossy(&vec![b'a'; i][..]).to_string(),
            ));
            let sdes = Sdes {
                ssrc: 1.into(),
                values: r,
            };
            assert_eq!(sdes.write_to(&mut buf), sdes.word_size() * 4);
        }
    }

    #[test]
    fn cname_serialize_deserialize() {
        let mut s1 = Sdes {
            ssrc: 1.into(),
            values: ReportList::new(),
        };
        s1.values.push((SdesType::CNAME, "abc123".into()));

        let mut buf = vec![0; 50];
        let n = s1.write_to(&mut buf);
        buf.truncate(n);

        println!("{buf:02x?}");

        let s2: Sdes = buf.as_slice().try_into().unwrap();

        assert_eq!(s1, s2);
    }
}
