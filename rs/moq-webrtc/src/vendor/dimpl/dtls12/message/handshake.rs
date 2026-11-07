use std::fmt;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

use super::Certificate;
use super::CertificateRequest;
use super::CertificateVerify;
use super::ClientHello;
use super::ClientKeyExchange;
use super::Dtls12CipherSuite;
use super::Finished;
use super::HelloVerifyRequest;
use super::ServerHello;
use super::ServerKeyExchange;
use crate::buffer::Buf;
use arrayvec::ArrayVec;
use nom::Err;
use nom::IResult;
use nom::bytes::complete::take;
use nom::error::{Error, ErrorKind};
use nom::number::complete::be_u8;
use nom::number::complete::{be_u16, be_u24, be_u32};

// Defensive stack cap over flattened handshake fragments selected for one
// defragmentation attempt. This intentionally does not mirror the receive
// queue's record-count cap; exceeding 50 fragments for one handshake implies
// pathologically tiny records and is treated as invalid input.
const MAX_DEFRAGMENT_HANDSHAKES: usize = 50;

#[derive(Debug, PartialEq, Eq, Default, Clone, Copy)]
pub struct Header {
    pub msg_type: MessageType,
    pub length: u32,
    pub message_seq: u16,
    pub fragment_offset: u32,
    pub fragment_length: u32,
}

#[derive(Debug, Default)]
pub struct Handshake {
    pub header: Header,
    pub body: Body,
    pub handled: AtomicBool,
}

impl PartialEq for Handshake {
    fn eq(&self, other: &Self) -> bool {
        self.header == other.header
            && self.body == other.body
            && self.handled.load(Ordering::Relaxed) == other.handled.load(Ordering::Relaxed)
    }
}

impl Eq for Handshake {}

impl Handshake {
    #[cfg(test)]
    pub fn new(
        msg_type: MessageType,
        length: u32,
        message_seq: u16,
        fragment_offset: u32,
        fragment_length: u32,
        body: Body,
    ) -> Self {
        Handshake {
            header: Header {
                msg_type,
                length,
                message_seq,
                fragment_offset,
                fragment_length,
            },
            body,
            handled: AtomicBool::new(false),
        }
    }

    pub fn parse_header(input: &[u8]) -> IResult<&[u8], Header> {
        let (input, msg_type) = MessageType::parse(input)?;
        let (input, length) = be_u24(input)?;
        let (input, message_seq) = be_u16(input)?;
        let (input, fragment_offset) = be_u24(input)?;
        let (input, fragment_length) = be_u24(input)?;

        Ok((
            input,
            Header {
                msg_type,
                length,
                message_seq,
                fragment_offset,
                fragment_length,
            },
        ))
    }

    pub fn parse(
        input: &[u8],
        base_offset: usize,
        c: Option<Dtls12CipherSuite>,
        as_fragment: bool,
    ) -> IResult<&[u8], Handshake> {
        let original_input = input;
        let (input, header) = Self::parse_header(input)?;

        let fragment_end = header.fragment_offset + header.fragment_length;
        let fragment_out_of_bounds = fragment_end > header.length;
        let empty_client_hello_fragment =
            header.msg_type == MessageType::ClientHello && header.fragment_length == 0;

        if fragment_out_of_bounds || empty_client_hello_fragment {
            return Err(Err::Failure(Error::new(input, ErrorKind::LengthValue)));
        }

        let is_fragment = header.fragment_offset > 0 || header.fragment_length < header.length;

        if !as_fragment && is_fragment {
            return Err(Err::Failure(Error::new(input, ErrorKind::LengthValue)));
        }

        let (input, body) = if as_fragment {
            let (input, fragment_slice) = take(header.fragment_length as usize)(input)?;
            // Calculate range relative to original input
            let relative_offset =
                fragment_slice.as_ptr() as usize - original_input.as_ptr() as usize;
            let start = base_offset + relative_offset;
            let end = start + fragment_slice.len();
            (input, Body::Fragment(start..end))
        } else {
            let (input, body_bytes) = take(header.length as usize)(input)?;
            // Calculate base_offset for body parsing
            let consumed = body_bytes.as_ptr() as usize - original_input.as_ptr() as usize;
            let body_base_offset = base_offset + consumed;
            let (_, body) = Body::parse(body_bytes, body_base_offset, header.msg_type, c)?;
            (input, body)
        };

        Ok((
            input,
            Handshake {
                header,
                body,
                handled: AtomicBool::new(false),
            },
        ))
    }

    pub fn serialize(&self, source_buf: &[u8], output: &mut Buf) {
        output.push(self.header.msg_type.as_u8());
        output.extend_from_slice(&self.header.length.to_be_bytes()[1..]);
        output.extend_from_slice(&self.header.message_seq.to_be_bytes());
        output.extend_from_slice(&self.header.fragment_offset.to_be_bytes()[1..]);
        output.extend_from_slice(&self.header.fragment_length.to_be_bytes()[1..]);
        self.body.serialize(source_buf, output);
    }

    #[allow(private_interfaces)]
    pub fn defragment<'b>(
        mut iter: impl Iterator<Item = (&'b Handshake, &'b [u8])>,
        buffer: &mut Buf,
        cipher_suite: Option<Dtls12CipherSuite>,
        transcript: Option<&mut Buf>,
    ) -> Result<Handshake, crate::InternalError> {
        buffer.clear();

        // Invariant is upheld by the caller.
        let (first_handshake, first_buffer) = iter.next().unwrap();

        let Body::Fragment(range) = &first_handshake.body else {
            unreachable!("Non-Fragment body in defragment()")
        };
        let mut handled = ArrayVec::<&Handshake, MAX_DEFRAGMENT_HANDSHAKES>::new();
        handled
            .try_push(first_handshake)
            .map_err(|_| crate::InternalError::too_many_records())?;
        buffer.extend_from_slice(&first_buffer[range.clone()]);

        for (handshake, source_buf) in iter {
            if handshake.header.msg_type != first_handshake.header.msg_type
                || handshake.header.message_seq != first_handshake.header.message_seq
            {
                break;
            }

            let Body::Fragment(range) = &handshake.body else {
                unreachable!("Non-Fragment body in defragment()")
            };

            handled
                .try_push(handshake)
                .map_err(|_| crate::InternalError::too_many_records())?;

            buffer.extend_from_slice(&source_buf[range.clone()]);
        }

        if buffer.len() != first_handshake.header.length as usize {
            debug!("Defragmentation failed. Fragment length mismatch");
            return Err(crate::InternalError::parse_incomplete());
        }

        let (rest, body) =
            match Body::parse(buffer, 0, first_handshake.header.msg_type, cipher_suite) {
                Ok(parsed) => parsed,
                Err(err) => {
                    mark_handled(handled);
                    return Err(err.into());
                }
            };

        if !rest.is_empty()
            && first_handshake
                .header
                .msg_type
                .rejects_trailing_body_bytes()
        {
            debug!("Defragmentation failed. Body::parse() did not consume the entire buffer");
            mark_handled(handled);
            return Err(crate::InternalError::parse_incomplete());
        }

        // Parsing boundary: Body::parse and its callees must perform all
        // structural validation, including extension payloads, before returning
        // success. Below we consume the message and append it to the transcript;
        // deferring parsing-level checks to state handlers would commit malformed
        // input and poison the transcript for a corrected retransmission.
        // Negotiation and cryptographic verification belong in the state handlers.
        mark_handled(handled);

        // If transcript is provided, write the handshake header + body after parsing succeeds.
        if let Some(transcript) = transcript {
            transcript.push(first_handshake.header.msg_type.as_u8());
            transcript.extend_from_slice(&first_handshake.header.length.to_be_bytes()[1..]);
            transcript.extend_from_slice(&first_handshake.header.message_seq.to_be_bytes());
            // Defragmented handshake has fragment_offset=0 and fragment_length=length
            transcript.extend_from_slice(&0u32.to_be_bytes()[1..]);
            transcript.extend_from_slice(&first_handshake.header.length.to_be_bytes()[1..]);
            transcript.extend_from_slice(&buffer[..first_handshake.header.length as usize]);
        }

        let handshake = Handshake {
            header: Header {
                msg_type: first_handshake.header.msg_type,
                length: first_handshake.header.length,
                message_seq: first_handshake.header.message_seq,
                fragment_offset: 0,
                fragment_length: first_handshake.header.length,
            },
            body,
            handled: AtomicBool::new(false),
        };

        // Create a new Handshake with the merged body
        Ok(handshake)
    }

    #[cfg(test)]
    fn do_clone(&self) -> Handshake {
        Handshake {
            header: Header {
                msg_type: self.header.msg_type,
                length: self.header.length,
                message_seq: self.header.message_seq,
                fragment_offset: self.header.fragment_offset,
                fragment_length: self.header.fragment_length,
            },
            body: Body::HelloRequest, // Placeholder
            handled: AtomicBool::new(false),
        }
    }

    #[cfg(test)]
    pub fn fragment<'b>(
        &self,
        max: usize,
        buffer: &'b mut Buf,
    ) -> impl Iterator<Item = Handshake> + 'b {
        // Must be called with an empty buffer.
        assert!(buffer.is_empty());

        // Note: For fragmentize, self is already serialized data in Body::Fragment
        // which doesn't need source_buf, so we pass an empty slice
        self.body.serialize(&[], buffer);

        // If this is wrong, the serialize has not produced the same output as we parsed.
        assert_eq!(buffer.len(), self.header.length as usize);

        let to_clone = self.do_clone();

        buffer.chunks(max).enumerate().map(move |(i, chunk)| {
            let fragment_length = chunk.len() as u32;
            let offset = i * max;
            let fragment_range = offset..(offset + chunk.len());

            let mut fragment = to_clone.do_clone();
            fragment.header.fragment_offset = offset as u32;
            fragment.header.fragment_length = fragment_length;
            fragment.header.message_seq = to_clone.header.message_seq;
            fragment.body = Body::Fragment(fragment_range);

            fragment
        })
    }

    // These are (unencrypted) handshakes that, when detected as
    // duplicates, trigger a resend of the entire flight.
    pub fn dupe_triggers_resend(&self) -> Option<u16> {
        // Only trigger on the first fragment of a handshake message to avoid
        // multiple resends caused by fragmented duplicates of the same message.
        if self.header.fragment_offset != 0 {
            return None;
        }

        let qualifies = matches!(
            self.header.msg_type,
            MessageType::ClientHello |        // flight 1 and 3
            MessageType::HelloVerifyRequest | // flight 2
            MessageType::ServerHelloDone |    // flight 4
            MessageType::ClientKeyExchange // flight 5
        );

        qualifies.then_some(self.header.message_seq)
    }

    pub fn is_handled(&self) -> bool {
        self.handled.load(Ordering::Relaxed)
    }

    pub fn set_handled(&self) {
        self.handled.store(true, Ordering::Relaxed);
    }
}

fn mark_handled(handled: ArrayVec<&Handshake, MAX_DEFRAGMENT_HANDSHAKES>) {
    for handshake in handled {
        handshake.set_handled();
    }
}

#[repr(transparent)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct MessageType(u8);

#[allow(non_upper_case_globals)]
impl MessageType {
    pub const HelloRequest: Self = Self(0);
    pub const ClientHello: Self = Self(1);
    pub const ServerHello: Self = Self(2);
    pub const HelloVerifyRequest: Self = Self(3);
    pub const NewSessionTicket: Self = Self(4);
    pub const Certificate: Self = Self(11);
    pub const ServerKeyExchange: Self = Self(12);
    pub const CertificateRequest: Self = Self(13);
    pub const ServerHelloDone: Self = Self(14);
    pub const CertificateVerify: Self = Self(15);
    pub const ClientKeyExchange: Self = Self(16);
    pub const Finished: Self = Self(20);

    pub const fn from_u8(value: u8) -> Self {
        Self(value)
    }

    pub const fn as_u8(&self) -> u8 {
        self.0
    }

    const fn is_unknown(&self) -> bool {
        !matches!(*self, Self(0..=4 | 11..=16 | 20))
    }

    const fn rejects_trailing_body_bytes(&self) -> bool {
        !self.is_unknown()
    }

    pub fn parse(input: &[u8]) -> IResult<&[u8], MessageType> {
        let (input, byte) = be_u8(input)?;
        Ok((input, Self::from_u8(byte)))
    }

    pub fn epoch(&self) -> u16 {
        if matches!(*self, MessageType::NewSessionTicket | MessageType::Finished) {
            1
        } else {
            0
        }
    }
}

impl fmt::Debug for MessageType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_unknown() {
            return f.debug_tuple("Unknown").field(&self.0).finish();
        }

        let name = match *self {
            MessageType::HelloRequest => "HelloRequest",
            MessageType::ClientHello => "ClientHello",
            MessageType::HelloVerifyRequest => "HelloVerifyRequest",
            MessageType::ServerHello => "ServerHello",
            MessageType::Certificate => "Certificate",
            MessageType::ServerKeyExchange => "ServerKeyExchange",
            MessageType::CertificateRequest => "CertificateRequest",
            MessageType::ServerHelloDone => "ServerHelloDone",
            MessageType::CertificateVerify => "CertificateVerify",
            MessageType::ClientKeyExchange => "ClientKeyExchange",
            MessageType::NewSessionTicket => "NewSessionTicket",
            MessageType::Finished => "Finished",
            _ => unreachable!("known DTLS 1.2 handshake message type missing Debug label"),
        };

        f.write_str(name)
    }
}

#[derive(Debug, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum Body {
    HelloRequest, // empty
    ClientHello(ClientHello),
    HelloVerifyRequest(HelloVerifyRequest),
    ServerHello(ServerHello),
    Certificate(Certificate),
    ServerKeyExchange(ServerKeyExchange),
    CertificateRequest(CertificateRequest),
    ServerHelloDone, // empty
    CertificateVerify(CertificateVerify),
    ClientKeyExchange(ClientKeyExchange),
    NewSessionTicket(Range<usize>),
    Finished(Finished),
    Unknown(u8),
    Fragment(Range<usize>),
}

impl Default for Body {
    fn default() -> Self {
        Self::Unknown(0)
    }
}

impl Body {
    pub fn parse(
        input: &[u8],
        base_offset: usize,
        m: MessageType,
        c: Option<Dtls12CipherSuite>,
    ) -> IResult<&[u8], Body> {
        match m {
            MessageType::HelloRequest => Ok((input, Body::HelloRequest)),
            MessageType::ClientHello => {
                let (input, client_hello) = ClientHello::parse(input, base_offset)?;
                Ok((input, Body::ClientHello(client_hello)))
            }
            MessageType::HelloVerifyRequest => {
                let (input, hello_verify_request) = HelloVerifyRequest::parse(input)?;
                Ok((input, Body::HelloVerifyRequest(hello_verify_request)))
            }
            MessageType::ServerHello => {
                let (input, server_hello) = ServerHello::parse(input, base_offset)?;
                Ok((input, Body::ServerHello(server_hello)))
            }
            MessageType::Certificate => {
                let (input, certificate) = Certificate::parse(input, base_offset)?;
                Ok((input, Body::Certificate(certificate)))
            }
            MessageType::ServerKeyExchange => {
                let cipher_suite =
                    c.ok_or_else(|| Err::Failure(Error::new(input, ErrorKind::Fail)))?;
                let algo = cipher_suite.as_key_exchange_algorithm();
                let (input, server_key_exchange) =
                    ServerKeyExchange::parse(input, base_offset, algo)?;
                Ok((input, Body::ServerKeyExchange(server_key_exchange)))
            }
            MessageType::CertificateRequest => {
                let (input, certificate_request) = CertificateRequest::parse(input, base_offset)?;
                Ok((input, Body::CertificateRequest(certificate_request)))
            }
            MessageType::ServerHelloDone => Ok((input, Body::ServerHelloDone)),
            MessageType::CertificateVerify => {
                let (input, certificate_verify) = CertificateVerify::parse(input, base_offset)?;
                Ok((input, Body::CertificateVerify(certificate_verify)))
            }
            MessageType::ClientKeyExchange => {
                let cipher_suite =
                    c.ok_or_else(|| Err::Failure(Error::new(input, ErrorKind::Fail)))?;
                let algo = cipher_suite.as_key_exchange_algorithm();
                let (input, client_key_exchange) =
                    ClientKeyExchange::parse(input, base_offset, algo)?;
                Ok((input, Body::ClientKeyExchange(client_key_exchange)))
            }
            MessageType::NewSessionTicket => {
                let range = base_offset..(base_offset + input.len());
                let (input, _lifetime_hint) = be_u32(input)?;
                let (input, ticket_len) = be_u16(input)?;
                let (input, _ticket) = take(ticket_len)(input)?;
                if !input.is_empty() {
                    return Err(Err::Failure(Error::new(input, ErrorKind::LengthValue)));
                }
                Ok((input, Body::NewSessionTicket(range)))
            }
            MessageType::Finished => {
                let cipher_suite =
                    c.ok_or_else(|| Err::Failure(Error::new(input, ErrorKind::Fail)))?;
                let (input, finished) = Finished::parse(input, cipher_suite)?;
                Ok((input, Body::Finished(finished)))
            }
            _ => Ok((input, Body::Unknown(m.as_u8()))),
        }
    }

    pub fn serialize(&self, source_buf: &[u8], output: &mut Buf) {
        match self {
            Body::HelloRequest => {
                // Serialize HelloRequest (empty)
            }
            Body::ClientHello(client_hello) => {
                client_hello.serialize(source_buf, output);
            }
            Body::HelloVerifyRequest(hello_verify_request) => {
                hello_verify_request.serialize(output);
            }
            Body::ServerHello(server_hello) => {
                server_hello.serialize(source_buf, output);
            }
            Body::Certificate(certificate) => {
                certificate.serialize(source_buf, output);
            }
            Body::ServerKeyExchange(server_key_exchange) => {
                server_key_exchange.serialize(source_buf, output, true);
            }
            Body::CertificateRequest(certificate_request) => {
                certificate_request.serialize(source_buf, output);
            }
            Body::ServerHelloDone => {
                // Serialize ServerHelloDone (empty)
            }
            Body::CertificateVerify(certificate_verify) => {
                certificate_verify.serialize(source_buf, output);
            }
            Body::ClientKeyExchange(client_key_exchange) => {
                client_key_exchange.serialize(source_buf, output);
            }
            Body::NewSessionTicket(range) => {
                output.extend_from_slice(&source_buf[range.clone()]);
            }
            Body::Finished(finished) => {
                finished.serialize(source_buf, output);
            }
            Body::Unknown(value) => {
                output.push(*value);
            }
            Body::Fragment(range) => {
                output.extend_from_slice(&source_buf[range.clone()]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use arrayvec::ArrayVec;
    use std::collections::VecDeque;

    use super::*;

    use crate::buffer::Buf;
    use crate::dtls12::message::CompressionMethod;
    use crate::dtls12::message::Cookie;
    use crate::dtls12::message::Dtls12CipherSuite;
    use crate::dtls12::message::ProtocolVersion;
    use crate::dtls12::message::Random;
    use crate::dtls12::message::SessionId;

    const MESSAGE: &[u8] = &[
        0x01, // MessageType::ClientHello
        0x00, 0x00, 0x2E, // length
        0x00, 0x00, // message_seq
        0x00, 0x00, 0x00, // fragment_offset
        0x00, 0x00, 0x2E, // fragment_length
        // ClientHello
        0xFE, 0xFD, // ProtocolVersion::DTLS1_2
        // Random
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E,
        0x1F, 0x20, //
        0x01, // SessionId length
        0xAA, // SessionId
        0x01, // Cookie length
        0xBB, // Cookie
        0x00, 0x04, // Dtls12CipherSuites length
        0xC0, 0x2B, // Dtls12CipherSuite::ECDHE_ECDSA_AES128_GCM_SHA256
        0xC0, 0x2C, // Dtls12CipherSuite::ECDHE_ECDSA_AES256_GCM_SHA384
        0x01, // CompressionMethods length
        0x00, // CompressionMethod::Null
    ];

    fn check_body_before_commit(kind: MessageType, body: &[u8], accepted: bool) {
        let mut source = vec![0; 7];
        source.extend_from_slice(body);
        let parsed = Body::parse(&source[7..], 7, kind, None);
        assert_eq!(parsed.is_ok(), accepted, "{kind:?}: {body:02x?}");
        if let Ok((rest, parsed)) = parsed {
            assert!(rest.is_empty());
            let mut serialized = Buf::new();
            parsed.serialize(&source, &mut serialized);
            assert_eq!(&*serialized, body);
        }
        for fragmented in [false, true] {
            let split = if fragmented {
                body.len() / 2
            } else {
                body.len()
            };
            let mut fragments = vec![Handshake::new(
                kind,
                body.len() as u32,
                0,
                0,
                split as u32,
                Body::Fragment(0..split),
            )];
            if fragmented {
                fragments.push(Handshake::new(
                    kind,
                    body.len() as u32,
                    0,
                    split as u32,
                    (body.len() - split) as u32,
                    Body::Fragment(split..body.len()),
                ));
            }
            let mut buffer = Buf::new();
            let mut transcript = Buf::new();
            transcript.extend_from_slice(b"prior transcript");
            let result = Handshake::defragment(
                fragments.iter().map(|h| (h, body)),
                &mut buffer,
                None,
                Some(&mut transcript),
            );
            assert_eq!(result.is_ok(), accepted, "{kind:?}: {body:02x?}");
            if !accepted {
                assert_eq!(&*transcript, b"prior transcript");
            }
        }
    }

    #[test]
    fn empty_groups_rejected_before_commit() {
        let mut body = MESSAGE[12..].to_vec();
        append_extension(&mut body, 10, &[0, 0]);
        check_body_before_commit(MessageType::ClientHello, &body, false);
    }

    #[test]
    fn empty_srtp_rejected_before_commit() {
        let mut body = MESSAGE[12..].to_vec();
        append_extension(&mut body, 14, &[0, 0, 0]);
        check_body_before_commit(MessageType::ClientHello, &body, false);
    }

    #[test]
    fn empty_point_formats_rejected_before_commit() {
        let mut body = MESSAGE[12..].to_vec();
        append_extension(&mut body, 11, &[0]);
        check_body_before_commit(MessageType::ClientHello, &body, false);
    }

    #[test]
    fn nonempty_unknown_extension_vectors_remain_structurally_valid() {
        for (kind, payload) in [(10, &[0, 2, 0xff, 0xff][..]), (14, &[0, 2, 0xff, 0xff, 0])] {
            let mut body = MESSAGE[12..].to_vec();
            append_extension(&mut body, kind, payload);
            check_body_before_commit(MessageType::ClientHello, &body, true);
        }
    }

    fn append_extension(body: &mut Vec<u8>, kind: u16, payload: &[u8]) {
        body.extend_from_slice(&((4 + payload.len()) as u16).to_be_bytes());
        body.extend_from_slice(&kind.to_be_bytes());
        body.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        body.extend_from_slice(payload);
    }

    #[test]
    fn server_hello_and_ticket_parse_before_commit() {
        let valid_srtp = [0, 2, 0, 1, 0];
        for len in 0..=valid_srtp.len() {
            let mut hello = vec![0xfe, 0xfd];
            hello.extend_from_slice(&[0; 32]);
            hello.extend_from_slice(&[0, 0xc0, 0x2b, 0]);
            append_extension(&mut hello, 14, &valid_srtp[..len]);
            check_body_before_commit(MessageType::ServerHello, &hello, len == valid_srtp.len());
        }
        let ticket = [0, 0, 0, 1, 0, 1, 7];
        for len in 0..=ticket.len() {
            check_body_before_commit(
                MessageType::NewSessionTicket,
                &ticket[..len],
                len == ticket.len(),
            );
        }
        let mut trailing = ticket.to_vec();
        trailing.push(0);
        check_body_before_commit(MessageType::NewSessionTicket, &trailing, false);
        check_body_before_commit(MessageType::NewSessionTicket, &[0; 6], true);
    }

    #[test]
    fn message_type_newtype_shape() {
        assert_eq!(std::mem::size_of::<MessageType>(), 1);
        assert_eq!(MessageType::default().as_u8(), 0);
        assert_eq!(MessageType::default(), MessageType::HelloRequest);
    }

    #[test]
    fn message_type_wire_roundtrip() {
        for message_type in [
            MessageType::HelloRequest,
            MessageType::ClientHello,
            MessageType::ServerHello,
            MessageType::HelloVerifyRequest,
            MessageType::NewSessionTicket,
            MessageType::Certificate,
            MessageType::ServerKeyExchange,
            MessageType::CertificateRequest,
            MessageType::ServerHelloDone,
            MessageType::CertificateVerify,
            MessageType::ClientKeyExchange,
            MessageType::Finished,
        ] {
            assert_eq!(MessageType::from_u8(message_type.as_u8()), message_type);
            assert!(!message_type.is_unknown());
        }

        let unknown = MessageType::from_u8(0xFF);
        assert_eq!(unknown.as_u8(), 0xFF);
        assert!(unknown.is_unknown());
    }

    #[test]
    fn message_type_debug_stays_enum_like() {
        assert_eq!(format!("{:?}", MessageType::ClientHello), "ClientHello");
        assert_eq!(format!("{:?}", MessageType::from_u8(0xFF)), "Unknown(255)");
    }

    #[test]
    fn handshake_size() {
        let h = Handshake::new(
            // ServerHelloDone has a 0 sized body.
            MessageType::ServerHelloDone,
            0,
            0,
            0,
            0,
            Body::ServerHelloDone,
        );

        let mut v = Buf::new();
        h.serialize(&[], &mut v);

        assert_eq!(v.len(), 12);
    }

    #[test]
    fn roundtrip() {
        let mut serialized = Buf::new();

        let random = Random::parse(&MESSAGE[14..46]).unwrap().1;
        let session_id = SessionId::try_new(&[0xAA]).unwrap();
        let cookie = Cookie::try_new(&[0xBB]).unwrap();
        let mut cipher_suites = ArrayVec::new();
        cipher_suites.push(Dtls12CipherSuite::ECDHE_ECDSA_AES128_GCM_SHA256);
        cipher_suites.push(Dtls12CipherSuite::ECDHE_ECDSA_AES256_GCM_SHA384);
        let mut compression_methods = ArrayVec::new();
        compression_methods.push(CompressionMethod::Null);

        let client_hello = ClientHello::new(
            ProtocolVersion::DTLS1_2,
            random,
            session_id,
            cookie,
            cipher_suites,
            compression_methods,
        );

        let handshake = Handshake::new(
            MessageType::ClientHello,
            0x2E,
            0,
            0,
            0x2E,
            Body::ClientHello(client_hello),
        );

        // Serialize and compare to MESSAGE
        handshake.serialize(&[], &mut serialized);
        assert_eq!(&*serialized, MESSAGE);

        // Parse and compare with original
        let (rest, parsed) = Handshake::parse(&serialized, 0, None, false).unwrap();
        assert_eq!(parsed, handshake);

        assert!(rest.is_empty());
    }

    #[test]
    fn roundtrip_fragment() {
        let mut serialized = Buf::new();
        let mut buffer = Buf::new();

        let random = Random::parse(&MESSAGE[14..46]).unwrap().1;
        let session_id = SessionId::try_new(&[0xAA]).unwrap();
        let cookie = Cookie::try_new(&[0xBB]).unwrap();
        let mut cipher_suites = ArrayVec::new();
        cipher_suites.push(Dtls12CipherSuite::ECDHE_ECDSA_AES128_GCM_SHA256);
        cipher_suites.push(Dtls12CipherSuite::ECDHE_ECDSA_AES256_GCM_SHA384);
        let mut compression_methods = ArrayVec::new();
        compression_methods.push(CompressionMethod::Null);

        let client_hello = ClientHello::new(
            ProtocolVersion::DTLS1_2,
            random,
            session_id,
            cookie,
            cipher_suites,
            compression_methods,
        );

        let handshake = Handshake::new(
            MessageType::ClientHello,
            46,
            0,
            0,
            46,
            Body::ClientHello(client_hello),
        );

        // Fragment the handshake with size 10
        let fragments: VecDeque<_> = handshake.fragment(10, &mut buffer).collect();

        // Defragment the fragments
        let mut defragmented_buffer = Buf::new();
        let defragmented_handshake = Handshake::defragment(
            fragments.iter().map(|h| (h, &buffer[..])),
            &mut defragmented_buffer,
            None,
            None,
        )
        .unwrap();

        // Serialize and compare to MESSAGE
        // Save header info and drop handshake to release buffer borrow
        let header = defragmented_handshake.header;
        drop(defragmented_handshake);

        serialized.push(header.msg_type.as_u8());
        serialized.extend_from_slice(&header.length.to_be_bytes()[1..]);
        serialized.extend_from_slice(&header.message_seq.to_be_bytes());
        serialized.extend_from_slice(&header.fragment_offset.to_be_bytes()[1..]);
        serialized.extend_from_slice(&header.fragment_length.to_be_bytes()[1..]);
        serialized.extend_from_slice(&defragmented_buffer[..header.length as usize]);
        assert_eq!(&*serialized, MESSAGE);

        // Parse and compare with original
        let (rest, parsed) = Handshake::parse(&serialized, 0, None, false).unwrap();
        assert_eq!(parsed, handshake);

        assert!(rest.is_empty());
    }

    #[test]
    fn failed_defragment_parse_discards_candidate_without_writing_transcript() {
        let mut body = MESSAGE[12..].to_vec();
        body[38] = 0;
        body[39] = 3;

        let handshake = Handshake::new(
            MessageType::ClientHello,
            body.len() as u32,
            0,
            0,
            body.len() as u32,
            Body::Fragment(0..body.len()),
        );

        let mut defragmented_buffer = Buf::new();
        let mut transcript = Buf::new();
        let result = Handshake::defragment(
            std::iter::once((&handshake, body.as_slice())),
            &mut defragmented_buffer,
            None,
            Some(&mut transcript),
        );

        assert!(result.is_err());
        assert!(handshake.is_handled());
        assert!(transcript.is_empty());
    }

    #[test]
    fn client_hello_payload_validation_precedes_transcript_commit() {
        let cases: &[(u16, &[u8])] = &[
            (10, &[0, 2, 0, 23]),   // supported_groups
            (13, &[0, 2, 4, 3]),    // signature_algorithms
            (14, &[0, 2, 0, 1, 0]), // use_srtp
            (11, &[1, 0]),          // ec_point_formats
            (0xffff, &[0xff]),      // unknown extensions remain opaque
        ];
        for &(kind, valid) in cases {
            for malformed in [false, true] {
                let mut payload = valid.to_vec();
                if malformed {
                    payload[0] = 0xff; // Internal length exceeds the extension envelope.
                }
                let mut body = MESSAGE[12..].to_vec();
                body.extend_from_slice(&((4 + payload.len()) as u16).to_be_bytes());
                body.extend_from_slice(&kind.to_be_bytes());
                body.extend_from_slice(&(payload.len() as u16).to_be_bytes());
                body.extend_from_slice(&payload);
                // The parse path must also work with a nonzero root-buffer offset.
                let mut source = vec![0; 7];
                source.extend_from_slice(&body);
                let accepted = !malformed || kind == 0xffff;
                assert_eq!(
                    Body::parse(&source[7..], 7, MessageType::ClientHello, None).is_ok(),
                    accepted,
                    "extension {kind}, malformed={malformed}"
                );
                for fragmented in [false, true] {
                    let split = if fragmented {
                        body.len() - 1
                    } else {
                        body.len()
                    };
                    let mut fragments = vec![Handshake::new(
                        MessageType::ClientHello,
                        body.len() as u32,
                        0,
                        0,
                        split as u32,
                        Body::Fragment(0..split),
                    )];
                    if fragmented {
                        fragments.push(Handshake::new(
                            MessageType::ClientHello,
                            body.len() as u32,
                            0,
                            split as u32,
                            (body.len() - split) as u32,
                            Body::Fragment(split..body.len()),
                        ));
                    }
                    let mut buffer = Buf::new();
                    let mut transcript = Buf::new();
                    transcript.extend_from_slice(b"prior transcript");
                    let result = Handshake::defragment(
                        fragments.iter().map(|h| (h, body.as_slice())),
                        &mut buffer,
                        None,
                        Some(&mut transcript),
                    );
                    assert_eq!(result.is_ok(), accepted, "extension {kind}");
                    assert!(fragments.iter().all(Handshake::is_handled));
                    if !accepted {
                        assert_eq!(&*transcript, b"prior transcript");
                    }
                }
            }
        }
    }

    #[test]
    fn defragment_stops_at_cross_sequence_fragment() {
        let body = &MESSAGE[12..];
        let mut source = body.to_vec();
        source.push(0);

        let handshake = Handshake::new(
            MessageType::ClientHello,
            body.len() as u32,
            0,
            0,
            body.len() as u32,
            Body::Fragment(0..body.len()),
        );
        let decoy = Handshake::new(
            MessageType::ClientHello,
            body.len() as u32 + 1,
            1,
            body.len() as u32,
            1,
            Body::Fragment(body.len()..body.len() + 1),
        );

        let mut defragmented_buffer = Buf::new();
        let defragmented_handshake = Handshake::defragment(
            [(&handshake, source.as_slice()), (&decoy, source.as_slice())].into_iter(),
            &mut defragmented_buffer,
            None,
            None,
        )
        .unwrap();

        assert_eq!(defragmented_handshake.header.message_seq, 0);
        assert_eq!(&defragmented_buffer[..body.len()], body);
        assert!(handshake.is_handled());
        assert!(!decoy.is_handled());
    }

    #[test]
    fn known_body_rejects_trailing_bytes() {
        let body = [0];
        let handshake = Handshake::new(
            MessageType::HelloRequest,
            body.len() as u32,
            0,
            0,
            body.len() as u32,
            Body::Fragment(0..body.len()),
        );

        let mut defragmented_buffer = Buf::new();
        let mut transcript = Buf::new();
        let result = Handshake::defragment(
            std::iter::once((&handshake, body.as_slice())),
            &mut defragmented_buffer,
            None,
            Some(&mut transcript),
        );

        assert!(result.is_err());
        assert!(handshake.is_handled());
        assert!(transcript.is_empty());
    }

    #[test]
    fn parse_rejects_invalid_client_hello_fragments() {
        for (length, offset, fragment_length, valid) in [
            (4u32, 3u32, 2u32, false), // extends past the message
            (4, 0, 0, false),          // empty fragment
            (0, 0, 0, false),          // empty ClientHello
            (4, 2, 2, true),           // later fragment can arrive first
            (1, 0, 1, true),           // body validity is decided after reassembly
        ] {
            let mut packet = vec![MessageType::ClientHello.as_u8()];
            packet.extend_from_slice(&length.to_be_bytes()[1..]);
            packet.extend_from_slice(&0u16.to_be_bytes());
            packet.extend_from_slice(&offset.to_be_bytes()[1..]);
            packet.extend_from_slice(&fragment_length.to_be_bytes()[1..]);
            packet.resize(packet.len() + fragment_length as usize, 0);
            assert_eq!(
                Handshake::parse(&packet, 0, None, true).is_ok(),
                valid,
                "length={length}, offset={offset}, fragment_length={fragment_length}"
            );
        }
    }

    #[test]
    fn parse_accepts_empty_server_hello_done() {
        let mut packet = vec![0; 12];
        packet[0] = MessageType::ServerHelloDone.as_u8();
        assert!(Handshake::parse(&packet, 0, None, true).is_ok());
        assert!(Handshake::parse(&packet, 0, None, false).is_ok());
    }
}
