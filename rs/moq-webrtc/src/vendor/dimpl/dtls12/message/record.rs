//! DTLS 1.2 record layer types.
//!
//! ContentType and Sequence are now in crate::types as they're shared between DTLS versions.

use std::fmt;
use std::ops::Range;

use super::ProtocolVersion;
use crate::buffer::Buf;
use crate::types::{ContentType, Sequence};
use crate::util::be_u48;
use nom::bytes::complete::take;
use nom::number::complete::be_u16;
use nom::{Err, IResult};

/// DTLS 1.2 record structure.
#[derive(PartialEq, Eq, Default)]
pub struct DTLSRecord {
    /// The content type of this record.
    pub content_type: ContentType,
    /// The protocol version.
    pub version: ProtocolVersion,
    /// The epoch and sequence number.
    pub sequence: Sequence,
    /// The length of the fragment.
    pub length: u16,
    /// The range of the fragment in the source buffer.
    pub fragment_range: Range<usize>,
}

impl DTLSRecord {
    /// DTLS record header length: content_type(1) + version(2) + epoch(2) + seq(6) + length(2)
    pub const HEADER_LEN: usize = 13;

    /// Length of the explicit nonce prefix in DTLS 1.2 AES-GCM records.
    pub const EXPLICIT_NONCE_LEN: usize = 8;

    /// Byte offset in the record header where the 2-byte length field is
    pub const LENGTH_OFFSET: Range<usize> = 11..13;

    /// Parse a DTLS record from the input buffer.
    pub fn parse(
        input: &[u8],
        base_offset: usize,
        skip_offset: usize,
    ) -> IResult<&[u8], DTLSRecord> {
        let original_input = input;
        let (input, content_type) = ContentType::parse(input)?; // u8
        let (input, version) = ProtocolVersion::parse(input)?; // u16

        // Accept DTLS 1.0 or 1.2 in record layer per RFC 6347
        // DTLS 1.0 (0xFEFF) is often used in record layer during handshake for compatibility
        // The actual protocol version is negotiated in the handshake messages
        match version {
            ProtocolVersion::DTLS1_0 | ProtocolVersion::DTLS1_2 => {
                // Valid DTLS versions for record layer
            }
            _ => {
                return Err(Err::Failure(nom::error::Error::new(
                    input,
                    nom::error::ErrorKind::Tag,
                )));
            }
        }

        let (input, epoch) = be_u16(input)?; // u16

        // Epoch 0 records are plaintext in DTLS 1.2. Reject plaintext
        // ApplicationData before record protection is active, and only accept
        // the epoch-0 content types this implementation supports.
        if epoch == 0 {
            match content_type {
                ContentType::ChangeCipherSpec | ContentType::Alert | ContentType::Handshake => {}
                _ => {
                    return Err(Err::Failure(nom::error::Error::new(
                        input,
                        nom::error::ErrorKind::Tag,
                    )));
                }
            }
        }
        let (input, sequence_number) = be_u48(input)?; // u48
        let (input, length) = be_u16(input)?; // u16

        // When encrypted, skip_offset is 0 and this has the explicit nonce.
        // When decrypted, skip_offset is > 0 to skip the explicit nonce.
        let input = &input[skip_offset..];

        let (rest, fragment_slice) = take(length as usize)(input)?;

        // Calculate absolute range in root buffer
        // fragment_slice is already offset from original_input by all the header bytes and skip_offset
        let relative_offset = fragment_slice.as_ptr() as usize - original_input.as_ptr() as usize;
        let start = base_offset + relative_offset;
        let end = start + fragment_slice.len();

        let sequence = Sequence {
            epoch,
            sequence_number,
        };

        Ok((
            rest,
            DTLSRecord {
                content_type,
                version,
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
        output.push(self.content_type.as_u8());
        self.version.serialize(output);
        output.extend_from_slice(&self.sequence.epoch.to_be_bytes());
        output.extend_from_slice(&self.sequence.sequence_number.to_be_bytes()[2..]);
        output.extend_from_slice(&self.length.to_be_bytes());
        output.extend_from_slice(self.fragment(buf));
    }

    /// Get the explicit nonce from the fragment using the requested length.
    pub fn nonce_with_len<'a>(&self, buf: &'a [u8], len: usize) -> &'a [u8] {
        let fragment = self.fragment(buf);
        &fragment[..len]
    }

    /// Get the explicit nonce from the fragment (AES-GCM default).
    pub fn nonce<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        self.nonce_with_len(buf, Self::EXPLICIT_NONCE_LEN)
    }
}

impl fmt::Debug for DTLSRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DTLSRecord")
            .field("content_type", &self.content_type)
            .field("version", &self.version)
            .field("sequence", &self.sequence)
            .field("length", &self.length)
            .field("fragment_range", &self.fragment_range)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Buf;

    const RECORD: &[u8] = &[
        0x16, // ContentType::Handshake
        0xFE, 0xFD, // ProtocolVersion::DTLS1_2
        0x00, 0x01, // epoch
        0x00, 0x00, 0x00, 0x00, 0x00, 0x01, // sequence_number
        0x00, 0x10, // length
        // fragment
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
        0x10,
    ];

    #[test]
    fn roundtrip() {
        // Parse the record with base_offset 0, skip_offset 0
        let (rest, parsed) = DTLSRecord::parse(RECORD, 0, 0).unwrap();
        assert!(rest.is_empty());

        // Serialize and compare to RECORD
        let mut serialized = Buf::new();
        parsed.serialize(RECORD, &mut serialized);
        assert_eq!(&*serialized, RECORD);
    }

    #[test]
    fn epoch_0_content_type_whitelist() {
        // Epoch 0 is plaintext (RFC 6347 §4.1: epoch starts at 0, incremented by each CCS).
        // Only ChangeCipherSpec(20), Alert(21), and Handshake(22) can legitimately
        // appear unencrypted. ApplicationData in epoch 0 is rejected at parse time.

        fn build_epoch_0_record(content_type: u8) -> Vec<u8> {
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

        // ALLOWED: ChangeCipherSpec (20)
        let ccs = build_epoch_0_record(0x14);
        assert!(
            DTLSRecord::parse(&ccs, 0, 0).is_ok(),
            "ChangeCipherSpec should be allowed in epoch 0"
        );

        // ALLOWED: Alert (21)
        let alert = build_epoch_0_record(0x15);
        assert!(
            DTLSRecord::parse(&alert, 0, 0).is_ok(),
            "Alert should be allowed in epoch 0"
        );

        // ALLOWED: Handshake (22)
        let handshake = build_epoch_0_record(0x16);
        assert!(
            DTLSRecord::parse(&handshake, 0, 0).is_ok(),
            "Handshake should be allowed in epoch 0"
        );

        // REJECTED: ApplicationData (23)
        let app_data = build_epoch_0_record(0x17);
        assert!(
            DTLSRecord::parse(&app_data, 0, 0).is_err(),
            "ApplicationData must be rejected in epoch 0"
        );

        // REJECTED: Ack (26) - valid in DTLS 1.3 but not DTLS 1.2
        let ack = build_epoch_0_record(0x1A);
        assert!(
            DTLSRecord::parse(&ack, 0, 0).is_err(),
            "Ack must be rejected in DTLS 1.2 epoch 0"
        );

        // REJECTED: Unknown ContentType (0x99)
        let unknown = build_epoch_0_record(0x99);
        assert!(
            DTLSRecord::parse(&unknown, 0, 0).is_err(),
            "Unknown ContentType must be rejected in epoch 0"
        );

        // Verify that epoch 1+ allows ApplicationData (no whitelist restriction)
        let mut epoch_1_app_data = build_epoch_0_record(0x17);
        epoch_1_app_data[3] = 0x00; // epoch high byte
        epoch_1_app_data[4] = 0x01; // epoch low byte = 1
        assert!(
            DTLSRecord::parse(&epoch_1_app_data, 0, 0).is_ok(),
            "ApplicationData should be allowed in epoch 1+"
        );
    }
}
