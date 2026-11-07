//! DTLS 1.3 record layer types.
//!
//! Supports both DTLSPlaintext (epoch 0) and DTLSCiphertext (unified header)
//! record formats per RFC 9147 Section 4.

use std::fmt;
use std::ops::Range;

use crate::buffer::Buf;
use crate::types::{ContentType, ProtocolVersion, Sequence};
use crate::util::be_u48;
use nom::bytes::complete::take;
use nom::number::complete::{be_u8, be_u16};
use nom::{Err, IResult};

/// DTLS 1.3 record structure.
///
/// Represents both DTLSPlaintext (epoch 0) and DTLSCiphertext (unified header)
/// records. The format is determined during parsing based on the first byte.
#[derive(PartialEq, Eq, Default)]
pub struct Dtls13Record {
    /// The content type of this record.
    /// For plaintext records, this is the actual content type.
    /// For ciphertext records before decryption, this is ApplicationData.
    pub content_type: ContentType,
    /// The epoch and sequence number.
    /// For ciphertext records, epoch holds only the low 2 bits and
    /// sequence_number holds the 8- or 16-bit partial value until resolved.
    pub sequence: Sequence,
    /// The length of the fragment.
    pub length: u16,
    /// The range of the fragment in the source buffer.
    pub fragment_range: Range<usize>,
}

impl Dtls13Record {
    /// DTLSPlaintext header length: content_type(1) + version(2) + epoch(2) + seq(6) + length(2)
    pub const PLAINTEXT_HEADER_LEN: usize = 13;

    /// Byte offset in the plaintext record header where the 2-byte length field is
    pub const PLAINTEXT_LENGTH_OFFSET: Range<usize> = 11..13;

    /// Returns true if the first byte indicates a unified (ciphertext) header.
    ///
    /// The fixed bit pattern `001` in the top 3 bits distinguishes ciphertext
    /// records from plaintext ContentType values (all < 0x20).
    pub fn is_ciphertext_header(byte: u8) -> bool {
        byte & 0b1110_0000 == 0b0010_0000
    }

    /// Parse a DTLS 1.3 record from the input buffer.
    ///
    /// Detects plaintext vs ciphertext format from the first byte.
    pub fn parse(input: &[u8], base_offset: usize) -> IResult<&[u8], Dtls13Record> {
        if input.is_empty() {
            return Err(Err::Error(nom::error::Error::new(
                input,
                nom::error::ErrorKind::Eof,
            )));
        }

        if Self::is_ciphertext_header(input[0]) {
            Self::parse_ciphertext(input, base_offset)
        } else {
            Self::parse_plaintext(input, base_offset)
        }
    }

    /// Parse a DTLSPlaintext record (epoch 0).
    fn parse_plaintext(input: &[u8], base_offset: usize) -> IResult<&[u8], Dtls13Record> {
        let original_input = input;
        let (input, content_type) = ContentType::parse(input)?; // u8
        let (input, version) = ProtocolVersion::parse(input)?; // u16

        // RFC 9147 §4.1: Only alert(21), handshake(22), and ack(26) are valid
        // plaintext content types in DTLS 1.3. Reject all others.
        match content_type {
            ContentType::Alert | ContentType::Handshake | ContentType::Ack => {}
            _ => {
                return Err(Err::Failure(nom::error::Error::new(
                    input,
                    nom::error::ErrorKind::Tag,
                )));
            }
        }

        // Accept DTLS 1.0 or 1.2 in record layer per RFC 9147 §5.1
        // (same legacy version handling as DTLS 1.2)
        match version {
            ProtocolVersion::DTLS1_0 | ProtocolVersion::DTLS1_2 => {}
            _ => {
                return Err(Err::Failure(nom::error::Error::new(
                    input,
                    nom::error::ErrorKind::Tag,
                )));
            }
        }

        let (input, epoch) = be_u16(input)?; // u16

        // RFC 9147 §4.1: DTLSPlaintext records must use epoch 0.
        // Epoch values other than 0 in plaintext format are invalid.
        if epoch != 0 {
            return Err(Err::Failure(nom::error::Error::new(
                input,
                nom::error::ErrorKind::Tag,
            )));
        }

        let (input, sequence_number) = be_u48(input)?; // u48
        let (input, length) = be_u16(input)?; // u16
        let (rest, fragment_slice) = take(length as usize)(input)?;

        // Calculate absolute range in root buffer
        let relative_offset = fragment_slice.as_ptr() as usize - original_input.as_ptr() as usize;
        let start = base_offset + relative_offset;
        let end = start + fragment_slice.len();

        let sequence = Sequence {
            epoch,
            sequence_number,
        };

        Ok((
            rest,
            Dtls13Record {
                content_type,
                sequence,
                length,
                fragment_range: start..end,
            },
        ))
    }

    /// Parse a DTLSCiphertext record (unified header, epoch >= 2).
    ///
    /// The unified header flags byte layout:
    /// ```text
    /// 0 1 2 3 4 5 6 7
    /// +-+-+-+-+-+-+-+-+
    /// |0|0|1|C|S|L|E E|
    /// +-+-+-+-+-+-+-+-+
    /// ```
    /// C=0 always (no CID support), S=seq length, L=length present, EE=epoch low bits.
    fn parse_ciphertext(input: &[u8], base_offset: usize) -> IResult<&[u8], Dtls13Record> {
        let original_input = input;
        let (input, flags) = be_u8(input)?;

        let c_flag = flags & 0b0001_0000 != 0;
        if c_flag {
            // CID not supported
            return Err(Err::Failure(nom::error::Error::new(
                input,
                nom::error::ErrorKind::Tag,
            )));
        }

        let s_flag = flags & 0b0000_1000 != 0;
        let l_flag = flags & 0b0000_0100 != 0;
        let epoch_bits = (flags & 0b0000_0011) as u16;

        let (input, sequence_number) = if s_flag {
            let (input, seq) = be_u16(input)?;
            (input, seq as u64)
        } else {
            let (input, seq) = be_u8(input)?;
            (input, seq as u64)
        };

        let (rest, length, fragment_slice) = if l_flag {
            let (input, length) = be_u16(input)?;
            let (rest, fragment) = take(length as usize)(input)?;
            (rest, length, fragment)
        } else {
            // No length field: record consumes the rest of the datagram
            let length = input.len() as u16;
            let (rest, fragment) = take(length as usize)(input)?;
            (rest, length, fragment)
        };

        let relative_offset = fragment_slice.as_ptr() as usize - original_input.as_ptr() as usize;
        let start = base_offset + relative_offset;
        let end = start + fragment_slice.len();

        let sequence = Sequence {
            epoch: epoch_bits,
            sequence_number,
        };

        Ok((
            rest,
            Dtls13Record {
                content_type: ContentType::ApplicationData,
                sequence,
                length,
                fragment_range: start..end,
            },
        ))
    }

    /// Get the fragment data from the source buffer.
    pub fn fragment<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        &buf[self.fragment_range.clone()]
    }

    /// Serialize this record to the output buffer.
    pub fn serialize(&self, buf: &[u8], output: &mut Buf) {
        if self.sequence.epoch == 0 {
            self.serialize_plaintext(buf, output);
        } else {
            self.serialize_ciphertext(buf, output);
        }
    }

    fn serialize_plaintext(&self, buf: &[u8], output: &mut Buf) {
        output.push(self.content_type.as_u8());
        ProtocolVersion::DTLS1_2.serialize(output);
        output.extend_from_slice(&self.sequence.epoch.to_be_bytes());
        output.extend_from_slice(&self.sequence.sequence_number.to_be_bytes()[2..]);
        output.extend_from_slice(&self.length.to_be_bytes());
        output.extend_from_slice(self.fragment(buf));
    }

    fn serialize_ciphertext(&self, buf: &[u8], output: &mut Buf) {
        // Always use S=1 (2-byte sequence) and L=1 (length present)
        let flags: u8 = 0b0010_0000
            | 0b0000_1000 // S=1
            | 0b0000_0100 // L=1
            | (self.sequence.epoch as u8 & 0x03);
        output.push(flags);
        output.extend_from_slice(&(self.sequence.sequence_number as u16).to_be_bytes());
        output.extend_from_slice(&self.length.to_be_bytes());
        output.extend_from_slice(self.fragment(buf));
    }
}

impl fmt::Debug for Dtls13Record {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Dtls13Record")
            .field("content_type", &self.content_type)
            .field("sequence", &self.sequence)
            .field("length", &self.length)
            .field("fragment_range", &self.fragment_range)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plaintext_content_type_whitelist() {
        // RFC 9147 §4.1: Only alert(21), handshake(22), and ack(26) are valid
        // plaintext content types in DTLS 1.3. Plaintext records must use epoch 0.
        // ApplicationData and other invalid types are rejected at parse time.

        fn build_plaintext_record(content_type: u8) -> Vec<u8> {
            vec![
                content_type, // ContentType
                0xFE,
                0xFD, // version: DTLS 1.2
                0x00,
                0x00, // epoch: 0 (plaintext)
                0x00,
                0x00,
                0x00,
                0x00,
                0x00,
                0x01, // sequence_number
                0x00,
                0x02, // length: 2
                0xAA,
                0xBB, // fragment payload
            ]
        }

        fn build_plaintext_record_with_epoch(content_type: u8, epoch: u16) -> Vec<u8> {
            vec![
                content_type, // ContentType
                0xFE,
                0xFD, // version: DTLS 1.2
                (epoch >> 8) as u8,
                (epoch & 0xFF) as u8, // epoch
                0x00,
                0x00,
                0x00,
                0x00,
                0x00,
                0x01, // sequence_number
                0x00,
                0x02, // length: 2
                0xAA,
                0xBB, // fragment payload
            ]
        }

        // ALLOWED: Alert (21) with epoch 0
        let alert = build_plaintext_record(0x15);
        assert!(
            Dtls13Record::parse(&alert, 0).is_ok(),
            "Alert should be allowed in plaintext DTLS 1.3"
        );

        // ALLOWED: Handshake (22) with epoch 0
        let handshake = build_plaintext_record(0x16);
        assert!(
            Dtls13Record::parse(&handshake, 0).is_ok(),
            "Handshake should be allowed in plaintext DTLS 1.3"
        );

        // ALLOWED: Ack (26) with epoch 0
        let ack = build_plaintext_record(0x1A);
        assert!(
            Dtls13Record::parse(&ack, 0).is_ok(),
            "Ack should be allowed in plaintext DTLS 1.3"
        );

        // REJECTED: ApplicationData (23)
        let app_data = build_plaintext_record(0x17);
        assert!(
            Dtls13Record::parse(&app_data, 0).is_err(),
            "ApplicationData must be rejected in plaintext DTLS 1.3"
        );

        // REJECTED: ChangeCipherSpec (20) - valid in DTLS 1.2 but not DTLS 1.3
        let ccs = build_plaintext_record(0x14);
        assert!(
            Dtls13Record::parse(&ccs, 0).is_err(),
            "ChangeCipherSpec must be rejected in DTLS 1.3"
        );

        // REJECTED: Unknown ContentType (0xFF)
        let unknown = build_plaintext_record(0xFF);
        assert!(
            Dtls13Record::parse(&unknown, 0).is_err(),
            "Unknown ContentType must be rejected in plaintext DTLS 1.3"
        );

        // REJECTED: Plaintext format with epoch 1 (invalid per RFC 9147 §4.1)
        let epoch_1_handshake = build_plaintext_record_with_epoch(0x16, 1);
        assert!(
            Dtls13Record::parse(&epoch_1_handshake, 0).is_err(),
            "Plaintext format with epoch 1 must be rejected"
        );

        // REJECTED: Plaintext format with epoch 2 (should use unified header)
        let epoch_2_handshake = build_plaintext_record_with_epoch(0x16, 2);
        assert!(
            Dtls13Record::parse(&epoch_2_handshake, 0).is_err(),
            "Plaintext format with epoch 2 must be rejected"
        );
    }
}
