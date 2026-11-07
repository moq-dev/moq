use std::fmt;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, Ordering};

use arrayvec::ArrayVec;

use crate::buffer::{Buf, TmpBuf};
use crate::dtls13::message::{ContentType, Dtls13CipherSuite, Dtls13Record, Handshake, Sequence};
use crate::{Error, InternalError};

/// Holds both the UDP packet and the parsed result of that packet.
pub struct Incoming {
    // Box is here to reduce the size of the Incoming struct
    // to be passed in register instead of using memmove.
    records: Box<Records>,
}

impl Incoming {
    pub fn records(&self) -> &Records {
        &self.records
    }

    pub fn first(&self) -> &Record {
        // Invariant: Every Incoming must have at least one Record
        // or the parser of Incoming returns None.
        &self.records()[0]
    }

    pub fn into_records(self) -> impl Iterator<Item = Record> {
        self.records.records.into_iter()
    }
}

impl Incoming {
    /// Parse an incoming UDP packet
    ///
    /// * `packet` is the data from the UDP socket.
    /// * `decrypt` provides the decryption operations for encrypted records.
    /// * `cs` is the negotiated cipher suite, if any.
    ///
    /// Will surface parser errors.
    pub fn parse_packet(
        packet: &[u8],
        decrypt: &mut dyn RecordHandler,
        cs: Option<Dtls13CipherSuite>,
    ) -> Result<Option<Self>, InternalError> {
        // Parse records directly from packet, copying each record ONCE into its own buffer
        let records = Records::parse(packet, decrypt, cs)?;

        // We need at least one Record to be valid. For replayed frames, we discard
        // the records, hence this might be None
        if records.records.is_empty() {
            return Ok(None);
        }

        let records = Box::new(records);

        Ok(Some(Incoming { records }))
    }
}

/// A number of records parsed from a single UDP packet.
#[derive(Debug)]
pub struct Records {
    pub records: ArrayVec<Record, 16>,
}

impl Records {
    pub fn parse(
        packet: &[u8],
        decrypt: &mut dyn RecordHandler,
        cs: Option<Dtls13CipherSuite>,
    ) -> Result<Records, InternalError> {
        let mut parsed_records: ArrayVec<Record, 16> = ArrayVec::new();
        if let Err(error) = Self::parse_records(packet, decrypt, cs, &mut parsed_records) {
            for record in parsed_records {
                decrypt.push_buffer(record.into_buffer());
            }
            return Err(error);
        }

        let mut records = ArrayVec::new();
        for record in parsed_records {
            if let Some(record) = decrypt.classify_record(record)? {
                records
                    .try_push(record)
                    .expect("filtered records cannot exceed parsed records");
            }
        }

        Ok(Records { records })
    }

    fn parse_records(
        mut packet: &[u8],
        decrypt: &mut dyn RecordHandler,
        cs: Option<Dtls13CipherSuite>,
        parsed_records: &mut ArrayVec<Record, 16>,
    ) -> Result<(), InternalError> {
        // Find record boundaries and copy each record ONCE from the packet
        while !packet.is_empty() {
            let record_end = if Dtls13Record::is_ciphertext_header(packet[0]) {
                // CID bit set means we can't determine record boundaries (unsupported).
                // Discard the rest of the datagram.
                if packet[0] & 0x10 != 0 {
                    break;
                }

                // Unified header: variable length
                if packet.len() < 2 {
                    return Err(InternalError::parse_incomplete());
                }

                let flags = packet[0];
                let s_flag = flags & 0b0000_1000 != 0;
                let l_flag = flags & 0b0000_0100 != 0;
                let seq_len = if s_flag { 2 } else { 1 };
                let len_len = if l_flag { 2 } else { 0 };
                let header_len = 1 + seq_len + len_len;

                if packet.len() < header_len {
                    return Err(InternalError::parse_incomplete());
                }

                if l_flag {
                    let len_offset = 1 + seq_len;
                    // unwrap: header_len check above ensures 2 bytes at len_offset
                    let length_bytes: [u8; 2] =
                        packet[len_offset..len_offset + 2].try_into().unwrap();
                    let length = u16::from_be_bytes(length_bytes) as usize;
                    header_len + length
                } else {
                    // No length field: record consumes the rest of the datagram
                    packet.len()
                }
            } else {
                // Plaintext: fixed 13-byte header
                if packet.len() < Dtls13Record::PLAINTEXT_HEADER_LEN {
                    return Err(InternalError::parse_incomplete());
                }

                // unwrap: PLAINTEXT_HEADER_LEN check above ensures 2 bytes at offset
                let length_bytes: [u8; 2] = packet[Dtls13Record::PLAINTEXT_LENGTH_OFFSET]
                    .try_into()
                    .unwrap();
                let length = u16::from_be_bytes(length_bytes) as usize;
                Dtls13Record::PLAINTEXT_HEADER_LEN + length
            };

            if packet.len() < record_end {
                return Err(InternalError::parse_incomplete());
            }

            // This is the ONLY copy: packet -> record buffer
            let record_slice = &packet[..record_end];
            let record = Record::parse(record_slice, decrypt, cs)?;
            if let Some(record) = record {
                if let Err(error) = parsed_records.try_push(record) {
                    decrypt.push_buffer(error.element().into_buffer());
                    return Err(InternalError::too_many_records());
                }
            } else {
                trace!("Discarding replayed rec");
            }

            packet = &packet[record_end..];
        }

        Ok(())
    }
}

impl Deref for Records {
    type Target = [Record];

    fn deref(&self) -> &Self::Target {
        &self.records
    }
}

pub struct Record {
    buffer: Buf,
    // Box is here to reduce the size of the Record struct
    // to be passed in register instead of using memmove.
    parsed: Box<ParsedRecord>,
}

impl Record {
    /// The first parse pass only parses the record header which is unencrypted.
    /// Copies record data from UDP packet ONCE into a pooled buffer.
    pub fn parse(
        record_slice: &[u8],
        decrypt: &mut dyn RecordHandler,
        cs: Option<Dtls13CipherSuite>,
    ) -> Result<Option<Record>, InternalError> {
        // ONLY COPY: UDP packet slice -> pooled buffer
        let mut buffer = decrypt.pop_buffer();
        buffer.extend_from_slice(record_slice);

        match Self::parse_buffer(&mut buffer, decrypt, cs) {
            Ok(Some(parsed)) => Ok(Some(Record { buffer, parsed })),
            result => {
                decrypt.push_buffer(buffer);
                result.map(|_| None)
            }
        }
    }

    fn parse_buffer(
        buffer: &mut Buf,
        decrypt: &mut dyn RecordHandler,
        cs: Option<Dtls13CipherSuite>,
    ) -> Result<Option<Box<ParsedRecord>>, InternalError> {
        let is_ciphertext = Dtls13Record::is_ciphertext_header(buffer[0]);

        // Before keys are available, only handshake epoch 2 and application
        // epoch 3 can be buffered. Epochs with low bits 0 or 1 require KeyUpdate.
        if is_ciphertext && !decrypt.is_peer_encryption_enabled() && buffer[0] & 0x03 < 2 {
            return Ok(None);
        }

        // Decrypt record number in-place before parsing (RFC 9147 Section 4.2.3)
        if is_ciphertext && decrypt.is_peer_encryption_enabled() {
            let flags = buffer[0];
            let s_flag = flags & 0b0000_1000 != 0;
            let l_flag = flags & 0b0000_0100 != 0;
            let seq_len: usize = if s_flag { 2 } else { 1 };
            let len_len: usize = if l_flag { 2 } else { 0 };
            let header_len = 1 + seq_len + len_len;

            if buffer.len() >= header_len + 16 {
                // unwrap: bounds checked above
                let ciphertext_sample: [u8; 16] =
                    buffer[header_len..header_len + 16].try_into().unwrap();

                // Resolve epoch from 2-bit field (doesn't depend on seq bytes)
                let epoch_bits = flags & 0x03;
                let full_epoch = decrypt.resolve_epoch(epoch_bits);

                // Decrypt sequence bytes in place
                decrypt.decrypt_sequence_number(
                    full_epoch,
                    &mut buffer[1..1 + seq_len],
                    &ciphertext_sample,
                );
            }
        }

        let parsed = match ParsedRecord::parse(buffer, cs) {
            Ok(p) => p,
            Err(e) => {
                trace!("Discarding record: parse failed: {}", e);
                return Ok(None);
            }
        };
        let parsed = Box::new(parsed);

        // Plaintext records (epoch 0) are not encrypted
        if !is_ciphertext || !decrypt.is_peer_encryption_enabled() {
            return Ok(Some(parsed));
        }

        // Resolve the full epoch from the 2-bit value in the unified header
        let epoch_bits = parsed.record.sequence.epoch as u8;
        let full_epoch = decrypt.resolve_epoch(epoch_bits);

        // Resolve the full sequence number from the (now decrypted) partial value
        let seq_bits = parsed.record.sequence.sequence_number;
        let s_flag = buffer[0] & 0b0000_1000 != 0;
        let full_seq = decrypt.resolve_sequence(full_epoch, seq_bits, s_flag);

        let full_sequence = Sequence {
            epoch: full_epoch,
            sequence_number: full_seq,
        };

        // Anti-replay check (read-only, does not update window)
        if !decrypt.replay_check(full_sequence) {
            return Ok(None);
        }

        // Save the raw header bytes for AAD before mutating the buffer.
        // Max unified header without CID: flags(1) + seq(2) + length(2) = 5 bytes.
        let header_end = parsed.record.fragment_range.start;

        // Reject protected records whose encrypted fragment is shorter than
        // the per-suite minimum — they cannot hold a valid ciphertext + tag,
        // so decryption would necessarily fail. Catching it here keeps the
        // cipher impls' bounds-checking from being the only line of defence.
        if buffer.len() - header_end < decrypt.min_protected_fragment_len() {
            return Ok(None);
        }
        let mut header_buf = [0u8; 5];
        header_buf[..header_end].copy_from_slice(&buffer[..header_end]);

        // The encrypted part starts right after the unified header.
        let ciphertext = &mut buffer[header_end..];

        let new_len = {
            let mut buffer = TmpBuf::new(ciphertext);

            // This decrypts in place.
            // RFC 9147 §4.5.2: failed-to-decrypt ciphertext records MUST be silently discarded.
            match decrypt.decrypt_record(&header_buf[..header_end], full_sequence, &mut buffer) {
                Ok(()) => {}
                Err(e) => {
                    trace!("Discarding ciphertext record: decryption failed: {}", e);
                    return Ok(None);
                }
            }

            buffer.len()
        };

        // Decryption succeeded — now commit the replay window update.
        // RFC 9147 §4.5.1: "The window MUST NOT be updated due to a received
        // record until that record has been deprotected successfully."
        decrypt.replay_update(full_sequence);

        // Recover inner content type from DTLSInnerPlaintext
        let decrypted = &buffer[header_end..header_end + new_len];
        let (inner_content_type, content_len) = match recover_inner_content_type(decrypted) {
            Ok(v) => v,
            Err(e) => {
                trace!("Discarding record: invalid inner content type: {}", e);
                return Ok(None);
            }
        };

        let parsed = ParsedRecord::parse_decrypted(
            Dtls13Record {
                content_type: inner_content_type,
                sequence: full_sequence,
                length: content_len as u16,
                fragment_range: header_end..(header_end + content_len),
            },
            buffer,
            cs,
        );
        let parsed = Box::new(parsed);

        Ok(Some(parsed))
    }

    pub fn record(&self) -> &Dtls13Record {
        &self.parsed.record
    }

    pub fn handshakes(&self) -> &[Handshake] {
        &self.parsed.handshakes
    }

    pub fn retain_handshakes(&mut self, mut keep: impl FnMut(&Handshake) -> bool) {
        self.parsed.handshakes.retain(|handshake| keep(handshake));
    }

    pub fn first_handshake(&self) -> Option<&Handshake> {
        self.parsed.handshakes.first()
    }

    pub fn is_handled(&self) -> bool {
        if self.parsed.handshakes.is_empty() {
            self.parsed.handled.load(Ordering::Relaxed)
        } else {
            self.parsed.handshakes.iter().all(|h| h.is_handled())
        }
    }

    pub fn set_handled(&self) {
        // Handshakes should be empty because we set_handled() on them individually
        // during defragmentation. set_handled() on the record is only for non-handshakes.
        assert!(self.parsed.handshakes.is_empty());
        self.parsed.handled.store(true, Ordering::Relaxed);
    }

    pub fn buffer(&self) -> &[u8] {
        &self.buffer
    }

    pub(crate) fn into_buffer(self) -> Buf {
        self.buffer
    }
}

pub struct ParsedRecord {
    record: Dtls13Record,
    handshakes: ArrayVec<Handshake, 8>,
    handled: AtomicBool,
}

impl ParsedRecord {
    pub fn parse(
        input: &[u8],
        cipher_suite: Option<Dtls13CipherSuite>,
    ) -> Result<ParsedRecord, InternalError> {
        let (_, record) = Dtls13Record::parse(input, 0)?;

        let handshakes = if record.content_type == ContentType::Handshake {
            let fragment_offset = record.fragment_range.start;
            parse_handshakes(record.fragment(input), fragment_offset, cipher_suite)
        } else {
            ArrayVec::new()
        };

        Ok(ParsedRecord {
            record,
            handshakes,
            handled: AtomicBool::new(false),
        })
    }

    /// Build a ParsedRecord from an already-constructed record (after decryption).
    pub fn parse_decrypted(
        record: Dtls13Record,
        input: &[u8],
        cipher_suite: Option<Dtls13CipherSuite>,
    ) -> ParsedRecord {
        let handshakes = if record.content_type == ContentType::Handshake {
            let fragment_offset = record.fragment_range.start;
            parse_handshakes(record.fragment(input), fragment_offset, cipher_suite)
        } else {
            ArrayVec::new()
        };

        ParsedRecord {
            record,
            handshakes,
            handled: AtomicBool::new(false),
        }
    }
}

/// Trait abstracting record parsing-time handling for incoming records.
///
/// This decouples the record parser from the full `Engine`, allowing the parse loop
/// to acquire buffers, decrypt records, classify control records, and queue only the records that
/// should survive into `Incoming`.
pub trait RecordHandler {
    /// Acquire an empty buffer whose allocation can be reused for a record.
    fn pop_buffer(&mut self) -> Buf;
    /// Recycle a buffer when parsing fails or silently discards a record.
    fn push_buffer(&mut self, buffer: Buf);
    fn classify_record(&mut self, record: Record) -> Result<Option<Record>, Error>;
    fn is_peer_encryption_enabled(&self) -> bool;
    fn resolve_epoch(&self, epoch_bits: u8) -> u16;
    fn resolve_sequence(&self, epoch: u16, seq_bits: u64, s_flag: bool) -> u64;
    fn replay_check(&self, seq: Sequence) -> bool;
    fn replay_update(&mut self, seq: Sequence);

    /// Minimum length of a protected record's encrypted fragment for the
    /// negotiated suite (tag length in DTLS 1.3). Used to reject records that
    /// cannot possibly contain a valid ciphertext + tag.
    fn min_protected_fragment_len(&self) -> usize;

    fn decrypt_record(
        &mut self,
        header: &[u8],
        seq: Sequence,
        ciphertext: &mut TmpBuf,
    ) -> Result<(), Error>;

    /// Decrypt the sequence number bytes in a unified header (RFC 9147 Section 4.2.3).
    ///
    /// `epoch` is the resolved full epoch, `seq_bytes` are the encrypted sequence
    /// bytes from the header (1 or 2 bytes), and `ciphertext_sample` is the first
    /// 16 bytes of the ciphertext following the header.
    ///
    /// Returns the decrypted sequence bytes in-place.
    fn decrypt_sequence_number(
        &self,
        epoch: u16,
        seq_bytes: &mut [u8],
        ciphertext_sample: &[u8; 16],
    );
}

fn parse_handshakes(
    mut input: &[u8],
    mut base_offset: usize,
    cipher_suite: Option<Dtls13CipherSuite>,
) -> ArrayVec<Handshake, 8> {
    let mut handshakes = ArrayVec::new();
    while !input.is_empty() {
        if let Ok((remaining, handshake)) = Handshake::parse(input, base_offset, cipher_suite, true)
        {
            let len = input.len() - remaining.len();
            base_offset += len;
            input = remaining;
            if handshakes.try_push(handshake).is_err() {
                break;
            }
        } else {
            break;
        }
    }
    handshakes
}

/// Recover the inner content type from a decrypted DTLSInnerPlaintext.
///
/// The format is: `content || ContentType || zeros*`
/// Scan backward past zero padding to find the content type byte.
fn recover_inner_content_type(decrypted: &[u8]) -> Result<(ContentType, usize), InternalError> {
    let mut i = decrypted.len();
    // Skip zero padding
    while i > 0 && decrypted[i - 1] == 0 {
        i -= 1;
    }
    if i == 0 {
        return Err(InternalError::parse(nom::error::ErrorKind::Fail));
    }
    // The byte before padding is the content type
    i -= 1;
    let content_type = ContentType::from_u8(decrypted[i]);
    // Content length is everything before the content type byte
    Ok((content_type, i))
}

impl fmt::Debug for Incoming {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Incoming")
            .field("records", &self.records())
            .finish()
    }
}

impl fmt::Debug for Record {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Record")
            .field("record", &self.parsed.record)
            .field("handshakes", &self.parsed.handshakes)
            .finish()
    }
}

/*
Why it is sound to assert UnwindSafe for Incoming

- No internal unwind boundaries: this crate does not use catch_unwind. We do not
  cross panic boundaries internally while mutating state. This marker exists to
  document that external callers can wrap our APIs in catch_unwind without
  observing broken invariants from this type.

- Read-only builders: our dependent builders (e.g., ParsedRecord::parse) take
  only a &[u8] to the buffer and do not mutate the buffer during construction.
  An unwind during builder execution therefore cannot leave the buffer partially
  mutated across a boundary.

- Decrypt-and-reparse is publish-after-complete: when decrypting we first extract
  the buffer, mutate it (in-place decrypt), and only then construct a fresh Record
  from the fully transformed bytes. If a panic occurs mid-transformation, the new
  Record is not built and the previously-built Record is dropped; no consumer can
  observe a half-transformed record across an unwind boundary.

- Interior mutability is benign across unwind: the only interior mutability is
  AtomicBool "handled" flags. They are monotonic (false -> true). If an external
  caller catches a panic and continues, the worst effect is conservatively
  skipping work already done. This does not introduce memory unsafety or aliasing
  violations, and no invariants rely on "handled implies delivery".

Given the above, an unwind cannot leave Incoming in a state where broken
invariants are later observed across a catch_unwind boundary. Marking Incoming
as UnwindSafe is a sound assertion and clarifies behavior for callers.
*/
impl std::panic::UnwindSafe for Incoming {}

#[cfg(test)]
mod tests {
    use crate::buffer::BufferPool;

    use super::*;

    #[derive(Default)]
    struct TestHandler {
        buffers: BufferPool,
        buffers_acquired: usize,
        buffers_returned: usize,
        classify_calls: usize,
        dropped_acks: usize,
    }

    impl RecordHandler for TestHandler {
        fn pop_buffer(&mut self) -> Buf {
            self.buffers_acquired += 1;
            self.buffers.pop()
        }

        fn push_buffer(&mut self, buffer: Buf) {
            self.buffers_returned += 1;
            self.buffers.push(buffer);
        }

        fn classify_record(&mut self, record: Record) -> Result<Option<Record>, Error> {
            self.classify_calls += 1;
            if record.record().content_type == ContentType::Ack {
                self.dropped_acks += 1;
                self.push_buffer(record.into_buffer());
                return Ok(None);
            }
            Ok(Some(record))
        }

        fn is_peer_encryption_enabled(&self) -> bool {
            false
        }

        fn resolve_epoch(&self, _epoch_bits: u8) -> u16 {
            panic!("resolve_epoch should not be called when peer encryption is disabled");
        }

        fn resolve_sequence(&self, _epoch: u16, _seq_bits: u64, _s_flag: bool) -> u64 {
            panic!("resolve_sequence should not be called when peer encryption is disabled");
        }

        fn replay_check(&self, _seq: Sequence) -> bool {
            panic!("replay_check should not be called when peer encryption is disabled");
        }

        fn replay_update(&mut self, _seq: Sequence) {
            panic!("replay_update should not be called when peer encryption is disabled");
        }

        fn min_protected_fragment_len(&self) -> usize {
            panic!(
                "min_protected_fragment_len should not be called when peer encryption is disabled"
            );
        }

        fn decrypt_record(
            &mut self,
            _header: &[u8],
            _seq: Sequence,
            _ciphertext: &mut TmpBuf,
        ) -> Result<(), Error> {
            panic!("decrypt_record should not be called when peer encryption is disabled");
        }

        fn decrypt_sequence_number(
            &self,
            _epoch: u16,
            _seq_bytes: &mut [u8],
            _ciphertext_sample: &[u8; 16],
        ) {
            panic!("decrypt_sequence_number should not be called when peer encryption is disabled");
        }
    }

    fn build_plaintext_record(content_type: ContentType, seq: u64, fragment: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(content_type.as_u8());
        out.extend_from_slice(&[0xFE, 0xFD]);
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&seq.to_be_bytes()[2..]);
        out.extend_from_slice(&(fragment.len() as u16).to_be_bytes());
        out.extend_from_slice(fragment);
        out
    }

    fn build_ciphertext_record(epoch: u16, seq: u16, fragment: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let flags = 0b0010_0000 | 0b0000_1000 | 0b0000_0100 | (epoch as u8 & 0x03);
        out.push(flags);
        out.extend_from_slice(&seq.to_be_bytes());
        out.extend_from_slice(&(fragment.len() as u16).to_be_bytes());
        out.extend_from_slice(fragment);
        out
    }

    #[test]
    fn parse_packet_filters_control_records_after_packet_validation() {
        let mut packet = Vec::new();
        packet.extend_from_slice(&build_plaintext_record(ContentType::Ack, 1, &[0xAA, 0xBB]));
        packet.extend_from_slice(&build_ciphertext_record(2, 2, &[0x11, 0x22, 0x33]));

        let mut handler = TestHandler::default();
        let incoming = Incoming::parse_packet(&packet, &mut handler, None)
            .unwrap()
            .expect("ciphertext application data record should remain");

        assert_eq!(handler.classify_calls, 2);
        assert_eq!(handler.dropped_acks, 1);
        assert_eq!(incoming.records().len(), 1);
        assert_eq!(
            incoming.first().record().content_type,
            ContentType::ApplicationData
        );
        assert_eq!(incoming.first().record().sequence.epoch, 2);
    }

    #[test]
    fn receive_records_reuse_pooled_buffers() {
        let mut handler = TestHandler::default();
        let mut buffer = handler.pop_buffer();
        buffer.resize(2048, 0xAA);
        let allocation = buffer.as_ptr();
        handler.push_buffer(buffer);

        let packet = build_ciphertext_record(2, 1, &[0x11; 1500]);
        for _ in 0..10_000 {
            let incoming = Incoming::parse_packet(&packet, &mut handler, None)
                .expect("parse application data")
                .expect("packet contains a record");
            assert_eq!(incoming.first().buffer().as_ptr(), allocation);
            assert_eq!(incoming.first().buffer(), packet);
            for record in incoming.into_records() {
                handler.push_buffer(record.into_buffer());
            }
        }
    }

    #[test]
    fn discarded_records_return_pooled_buffers() {
        let mut handler = TestHandler::default();
        let mut buffer = handler.pop_buffer();
        buffer.resize(2048, 0xAA);
        let allocation = buffer.as_ptr();
        handler.push_buffer(buffer);

        let packet = build_plaintext_record(ContentType::ApplicationData, 1, &[0x11; 1500]);
        let incoming = Incoming::parse_packet(&packet, &mut handler, None)
            .expect("invalid plaintext application data is discarded");
        assert!(incoming.is_none());
        let reused = handler.pop_buffer();
        assert!(reused.is_empty());
        assert_eq!(reused.as_ptr(), allocation);
    }

    #[test]
    fn truncated_datagram_recycles_all_pooled_records() {
        let mut handler = TestHandler::default();
        let mut packet = build_ciphertext_record(2, 1, &[0x11; 1500]);
        packet.push(0xFF);

        assert!(Incoming::parse_packet(&packet, &mut handler, None).is_err());
        assert_eq!(handler.classify_calls, 0);
        assert_eq!(
            handler.buffers_returned, handler.buffers_acquired,
            "discarding a malformed datagram must recycle its accepted records"
        );
    }

    #[test]
    fn oversized_datagram_recycles_all_pooled_records() {
        let mut handler = TestHandler::default();
        let mut packet = Vec::new();
        for sequence in 0..17 {
            packet.extend_from_slice(&build_ciphertext_record(2, sequence, &[0x11; 32]));
        }

        assert!(Incoming::parse_packet(&packet, &mut handler, None).is_err());
        assert_eq!(handler.classify_calls, 0);
        assert_eq!(
            handler.buffers_returned, handler.buffers_acquired,
            "discarding a datagram with too many records must recycle every record"
        );
    }
}
