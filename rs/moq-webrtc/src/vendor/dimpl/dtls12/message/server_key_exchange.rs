use super::{CurveType, DigitallySigned, KeyExchangeAlgorithm, NamedGroup};
use crate::buffer::Buf;
use nom::Err;
use nom::error::{Error, ErrorKind};
use nom::number::complete::be_u8;
use nom::{IResult, bytes::complete::take};
use std::ops::Range;

#[derive(Debug, PartialEq, Eq)]
pub struct ServerKeyExchange {
    pub params: ServerKeyExchangeParams,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ServerKeyExchangeParams {
    Ecdh(EcdhParams),
    Psk(PskParams),
}

impl ServerKeyExchange {
    pub fn parse(
        input: &[u8],
        base_offset: usize,
        key_exchange_algorithm: KeyExchangeAlgorithm,
    ) -> IResult<&[u8], ServerKeyExchange> {
        let (input, params) = match key_exchange_algorithm {
            KeyExchangeAlgorithm::EECDH => {
                let (input, ecdh_params) = EcdhParams::parse(input, base_offset)?;
                (input, ServerKeyExchangeParams::Ecdh(ecdh_params))
            }
            KeyExchangeAlgorithm::PSK => {
                let (input, psk_params) = PskParams::parse(input, base_offset)?;
                (input, ServerKeyExchangeParams::Psk(psk_params))
            }
            _ => return Err(Err::Failure(Error::new(input, ErrorKind::Tag))),
        };

        Ok((input, ServerKeyExchange { params }))
    }

    pub fn serialize(&self, buf: &[u8], output: &mut Buf, with_signature: bool) {
        match &self.params {
            ServerKeyExchangeParams::Ecdh(ecdh_params) => {
                ecdh_params.serialize(buf, output, with_signature)
            }
            ServerKeyExchangeParams::Psk(psk_params) => psk_params.serialize(buf, output),
        }
    }

    pub fn signature(&self) -> Option<&DigitallySigned> {
        match &self.params {
            ServerKeyExchangeParams::Ecdh(ecdh_params) => ecdh_params.signature.as_ref(),
            ServerKeyExchangeParams::Psk(_) => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct EcdhParams {
    pub curve_type: CurveType,
    pub named_group: NamedGroup,
    pub public_key_range: Range<usize>,
    pub signature: Option<DigitallySigned>,
}

impl EcdhParams {
    pub fn public_key<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        &buf[self.public_key_range.clone()]
    }

    pub fn parse(input: &[u8], base_offset: usize) -> IResult<&[u8], EcdhParams> {
        let original_input = input;
        let (input, curve_type) = CurveType::parse(input)?;
        let (input, named_group) = NamedGroup::parse(input)?;

        // First byte is the length of the public key
        let (input, public_key_len) = be_u8(input)?;
        let (input, public_key_slice) = take(public_key_len as usize)(input)?;

        // Calculate absolute range for public key
        let relative_offset = public_key_slice.as_ptr() as usize - original_input.as_ptr() as usize;
        let start = base_offset + relative_offset;
        let end = start + public_key_slice.len();
        let public_key_range = start..end;

        // Optionally parse a trailing DigitallySigned structure
        let (input, signature) = if !input.is_empty() {
            // Calculate absolute offset for the signature part
            let sig_offset =
                base_offset + (input.as_ptr() as usize - original_input.as_ptr() as usize);
            let (rest, signed) = DigitallySigned::parse(input, sig_offset)?;
            (rest, Some(signed))
        } else {
            (input, None)
        };

        Ok((
            input,
            EcdhParams {
                curve_type,
                named_group,
                public_key_range,
                signature,
            },
        ))
    }

    pub fn serialize(&self, buf: &[u8], output: &mut Buf, with_signature: bool) {
        let public_key = self.public_key(buf);
        output.push(self.curve_type.as_u8());
        output.extend_from_slice(&self.named_group.as_u16().to_be_bytes());
        output.push(public_key.len() as u8);
        output.extend_from_slice(public_key);

        if with_signature && let Some(signed) = &self.signature {
            signed.serialize(buf, output);
        }
    }
}

/// PSK identity hint (RFC 4279 §2).
///
/// Wire format: `uint16 hint_length + hint`
#[derive(Debug, PartialEq, Eq)]
pub struct PskParams {
    pub hint_range: Range<usize>,
}

impl PskParams {
    pub fn hint<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        &buf[self.hint_range.clone()]
    }

    pub fn parse(input: &[u8], base_offset: usize) -> IResult<&[u8], PskParams> {
        let original_input = input;
        let (input, hint_len) = nom::number::complete::be_u16(input)?;
        let (input, hint_slice) = take(hint_len as usize)(input)?;

        let relative_offset = hint_slice.as_ptr() as usize - original_input.as_ptr() as usize;
        let start = base_offset + relative_offset;
        let end = start + hint_slice.len();

        Ok((
            input,
            PskParams {
                hint_range: start..end,
            },
        ))
    }

    pub fn serialize(&self, buf: &[u8], output: &mut Buf) {
        let hint = self.hint(buf);
        output.extend_from_slice(&(hint.len() as u16).to_be_bytes());
        output.extend_from_slice(hint);
    }

    /// Serialize directly from hint bytes (for sending).
    pub fn serialize_from_bytes(hint: &[u8], output: &mut Buf) {
        output.extend_from_slice(&(hint.len() as u16).to_be_bytes());
        output.extend_from_slice(hint);
    }
}

#[cfg(test)]
mod test {
    use super::super::{HashAlgorithm, SignatureAlgorithm, SignatureAndHashAlgorithm};
    use super::*;
    use crate::buffer::Buf;

    const MESSAGE_ECDH_PUBKEY: &[u8] = &[
        0x03, // curve_type
        0x00, 0x17, // named_group
        0x04, // public_key length
        0x01, 0x02, 0x03, 0x04, // public_key
    ];

    #[test]
    fn roundtrip_ecdh() {
        // Build expected message
        let algorithm =
            SignatureAndHashAlgorithm::new(HashAlgorithm::SHA256, SignatureAlgorithm::RSA);
        let signature_bytes: &[u8] = &[0x05, 0x06, 0x07, 0x08];

        let mut expected = Buf::new();
        expected.extend_from_slice(MESSAGE_ECDH_PUBKEY);
        expected.extend_from_slice(&algorithm.as_u16().to_be_bytes());
        expected.extend_from_slice(&(signature_bytes.len() as u16).to_be_bytes());
        expected.extend_from_slice(signature_bytes);

        // Parse the message with base_offset 0
        let (rest, parsed) =
            ServerKeyExchange::parse(&expected, 0, KeyExchangeAlgorithm::EECDH).unwrap();
        assert!(rest.is_empty());

        // Serialize and compare to expected bytes
        let mut serialized = Buf::new();
        parsed.serialize(&expected, &mut serialized, true);
        assert_eq!(&*serialized, &*expected);
    }

    #[test]
    fn psk_roundtrip() {
        // PSK ServerKeyExchange: just hint_length + hint
        const PSK_MESSAGE: &[u8] = &[
            0x00, 0x04, // hint length = 4
            b'h', b'i', b'n', b't',
        ];
        let (rest, parsed) =
            ServerKeyExchange::parse(PSK_MESSAGE, 0, KeyExchangeAlgorithm::PSK).unwrap();
        assert!(rest.is_empty());

        let ServerKeyExchangeParams::Psk(psk) = &parsed.params else {
            panic!("expected Psk variant");
        };
        assert_eq!(&PSK_MESSAGE[psk.hint_range.clone()], b"hint");
        assert!(
            parsed.signature().is_none(),
            "PSK SKE must have no signature"
        );

        let mut serialized = Buf::new();
        parsed.serialize(PSK_MESSAGE, &mut serialized, true);
        assert_eq!(&*serialized, PSK_MESSAGE);
    }

    #[test]
    fn psk_rejects_oversized_hint_length() {
        // hint_length=0x00FF (255) but only 2 bytes follow — parser must fail
        let bad: &[u8] = &[0x00, 0xFF, b'a', b'b'];
        let result = ServerKeyExchange::parse(bad, 0, KeyExchangeAlgorithm::PSK);
        assert!(
            result.is_err(),
            "parser must reject PSK hint shorter than advertised length"
        );
    }

    #[test]
    fn psk_empty_hint() {
        // Zero-length hint is wire-legal (RFC 4279 §2).
        let empty: &[u8] = &[0x00, 0x00];
        let (rest, parsed) = ServerKeyExchange::parse(empty, 0, KeyExchangeAlgorithm::PSK).unwrap();
        assert!(rest.is_empty());
        let ServerKeyExchangeParams::Psk(psk) = &parsed.params else {
            panic!("expected Psk variant");
        };
        assert!(psk.hint_range.is_empty());
    }
}
