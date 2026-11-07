use super::{chunk_forward_tsn::NEW_CUMULATIVE_TSN_LENGTH, chunk_header::*, chunk_type::*, *};
use alloc::string::ToString;

/// I-FORWARD-TSN chunk (RFC 8260).
///
/// Identical purpose to FORWARD-TSN (RFC 3758) but carries per-stream
/// entries with a 32-bit Message Identifier (MID) instead of a 16-bit SSN,
/// and an explicit unordered flag per entry.
///
/// 0                   1                   2                   3
/// 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
///+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///|   Type = 194  |  Flags = 0x00 |        Length = Variable      |
///+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///|                   New Cumulative TSN                          |
///+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///|       Stream Identifier       |     Flags     |   Reserved   |
///+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///|                    Message Identifier (MID)                   |
///+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///                             ...
#[derive(Default, Debug, Clone)]
pub(crate) struct ChunkIForwardTsn {
    pub(crate) new_cumulative_tsn: u32,
    pub(crate) streams: Vec<ChunkIForwardTsnStream>,
}

const I_FORWARD_TSN_STREAM_ENTRY_LENGTH: usize = 8;

impl fmt::Display for ChunkIForwardTsn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut res = vec![self.header().to_string()];
        res.push(format!("New Cumulative TSN: {}", self.new_cumulative_tsn));
        for s in &self.streams {
            res.push(format!(
                " - si={}, unordered={}, mid={}",
                s.identifier, s.unordered, s.mid
            ));
        }
        write!(f, "{}", res.join("\n"))
    }
}

impl Chunk for ChunkIForwardTsn {
    fn header(&self) -> ChunkHeader {
        ChunkHeader {
            typ: CT_I_FORWARD_TSN,
            flags: 0,
            value_length: self.value_length() as u16,
        }
    }

    fn unmarshal(buf: &Bytes) -> Result<Self> {
        let header = ChunkHeader::unmarshal(buf)?;

        if header.typ != CT_I_FORWARD_TSN {
            return Err(Error::ErrChunkTypeNotForwardTsn);
        }

        let value_end = CHUNK_HEADER_SIZE + header.value_length();
        if header.value_length() < NEW_CUMULATIVE_TSN_LENGTH {
            return Err(Error::ErrChunkTooShort);
        }

        let reader = &mut buf.slice(CHUNK_HEADER_SIZE..value_end);
        let new_cumulative_tsn = reader.get_u32();

        let mut streams = vec![];
        let mut offset = CHUNK_HEADER_SIZE + NEW_CUMULATIVE_TSN_LENGTH;
        while offset + I_FORWARD_TSN_STREAM_ENTRY_LENGTH <= value_end {
            let entry_buf = &mut buf.slice(offset..value_end);
            let identifier = entry_buf.get_u16();
            let flags = entry_buf.get_u8();
            let _reserved = entry_buf.get_u8();
            let mid = entry_buf.get_u32();

            streams.push(ChunkIForwardTsnStream {
                identifier,
                unordered: (flags & 0x01) != 0,
                mid,
            });
            offset += I_FORWARD_TSN_STREAM_ENTRY_LENGTH;
        }

        Ok(ChunkIForwardTsn {
            new_cumulative_tsn,
            streams,
        })
    }

    fn marshal_to(&self, writer: &mut BytesMut) -> Result<usize> {
        self.header().marshal_to(writer)?;
        writer.put_u32(self.new_cumulative_tsn);
        for s in &self.streams {
            writer.put_u16(s.identifier);
            writer.put_u8(if s.unordered { 0x01 } else { 0x00 });
            writer.put_u8(0); // reserved
            writer.put_u32(s.mid);
        }
        Ok(writer.len())
    }

    fn check(&self) -> Result<()> {
        Ok(())
    }

    fn value_length(&self) -> usize {
        4 + I_FORWARD_TSN_STREAM_ENTRY_LENGTH * self.streams.len()
    }

    fn as_any(&self) -> &(dyn Any + Send + Sync) {
        self
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ChunkIForwardTsnStream {
    pub(crate) identifier: u16,
    pub(crate) unordered: bool,
    pub(crate) mid: u32,
}
