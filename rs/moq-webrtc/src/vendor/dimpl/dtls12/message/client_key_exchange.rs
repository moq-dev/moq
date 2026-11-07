use super::KeyExchangeAlgorithm;
use super::{CurveType, NamedGroup};
use crate::buffer::Buf;
use nom::bytes::complete::take;
use nom::error::Error;
use nom::number::complete::be_u8;
use nom::{Err, IResult};
use std::ops::Range;

#[derive(Debug, PartialEq, Eq)]
pub struct ClientKeyExchange {
    pub exchange_keys: ExchangeKeys,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExchangeKeys {
    Ecdh(ClientEcdhKeys),
    Psk(ClientPskKeys),
}

/// ECDHE key exchange parameters
#[derive(Debug, PartialEq, Eq)]
pub struct ClientEcdhKeys {
    pub curve_type: CurveType,
    pub named_group: NamedGroup,
    pub public_key_range: Range<usize>,
}

impl ClientEcdhKeys {
    pub fn parse(input: &[u8], base_offset: usize) -> IResult<&[u8], ClientEcdhKeys> {
        let original_input = input;
        let (input, public_key_length) = be_u8(input)?;
        let (input, public_key_slice) = take(public_key_length)(input)?;

        // Calculate absolute range in root buffer
        let relative_offset = public_key_slice.as_ptr() as usize - original_input.as_ptr() as usize;
        let start = base_offset + relative_offset;
        let end = start + public_key_slice.len();

        Ok((
            input,
            ClientEcdhKeys {
                // In ClientKeyExchange, we don't include curve_type and named_group
                // since they're already established during ServerKeyExchange
                curve_type: CurveType::NamedCurve,  // Default
                named_group: NamedGroup::Secp256r1, // Default
                public_key_range: start..end,
            },
        ))
    }

    pub fn public_key<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        &buf[self.public_key_range.clone()]
    }

    pub fn serialize(&self, buf: &[u8], output: &mut Buf) {
        // For client key exchange, we only need to include the public key length and value
        // The curve_type and named_group are already established during ServerKeyExchange
        let public_key = self.public_key(buf);
        output.push(public_key.len() as u8);
        output.extend_from_slice(public_key);
    }
}

impl ClientKeyExchange {
    pub fn parse(
        input: &[u8],
        base_offset: usize,
        key_exchange_algorithm: KeyExchangeAlgorithm,
    ) -> IResult<&[u8], ClientKeyExchange> {
        let (input, exchange_keys) = match key_exchange_algorithm {
            KeyExchangeAlgorithm::EECDH => {
                let (input, ecdh_keys) = ClientEcdhKeys::parse(input, base_offset)?;
                (input, ExchangeKeys::Ecdh(ecdh_keys))
            }
            KeyExchangeAlgorithm::PSK => {
                let (input, psk_keys) = ClientPskKeys::parse(input, base_offset)?;
                (input, ExchangeKeys::Psk(psk_keys))
            }
            _ => return Err(Err::Failure(Error::new(input, nom::error::ErrorKind::Tag))),
        };

        Ok((input, ClientKeyExchange { exchange_keys }))
    }

    pub fn serialize(&self, buf: &[u8], output: &mut Buf) {
        match &self.exchange_keys {
            ExchangeKeys::Ecdh(ecdh_keys) => ecdh_keys.serialize(buf, output),
            ExchangeKeys::Psk(psk_keys) => psk_keys.serialize(buf, output),
        }
    }

    /// Helper to serialize directly from public key bytes (for sending)
    pub fn serialize_from_bytes(public_key: &[u8], output: &mut Buf) {
        output.push(public_key.len() as u8);
        output.extend_from_slice(public_key);
    }
}

/// PSK identity sent by the client (RFC 4279 §2).
///
/// Wire format: `uint16 identity_length + identity`
#[derive(Debug, PartialEq, Eq)]
pub struct ClientPskKeys {
    pub identity_range: Range<usize>,
}

impl ClientPskKeys {
    pub fn identity<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        &buf[self.identity_range.clone()]
    }

    pub fn parse(input: &[u8], base_offset: usize) -> IResult<&[u8], ClientPskKeys> {
        let original_input = input;
        let (input, identity_len) = nom::number::complete::be_u16(input)?;
        let (input, identity_slice) = take(identity_len as usize)(input)?;

        let relative_offset = identity_slice.as_ptr() as usize - original_input.as_ptr() as usize;
        let start = base_offset + relative_offset;
        let end = start + identity_slice.len();

        Ok((
            input,
            ClientPskKeys {
                identity_range: start..end,
            },
        ))
    }

    pub fn serialize(&self, buf: &[u8], output: &mut Buf) {
        let identity = self.identity(buf);
        output.extend_from_slice(&(identity.len() as u16).to_be_bytes());
        output.extend_from_slice(identity);
    }

    /// Serialize directly from identity bytes (for sending).
    pub fn serialize_from_bytes(identity: &[u8], output: &mut Buf) {
        output.extend_from_slice(&(identity.len() as u16).to_be_bytes());
        output.extend_from_slice(identity);
    }
}

#[cfg(test)]
mod test {
    use super::super::KeyExchangeAlgorithm;
    use super::*;
    use crate::buffer::Buf;

    const ECDH_MESSAGE: &[u8] = &[
        0x04, // Public key length
        0x01, 0x02, 0x03, 0x04, // Public key data
    ];

    #[test]
    fn roundtrip_ecdh() {
        // Parse the message with base_offset 0
        let (rest, parsed) =
            ClientKeyExchange::parse(ECDH_MESSAGE, 0, KeyExchangeAlgorithm::EECDH).unwrap();
        assert!(rest.is_empty());

        // Serialize and compare to ECDH_MESSAGE
        let mut serialized = Buf::new();
        parsed.serialize(ECDH_MESSAGE, &mut serialized);
        assert_eq!(&*serialized, ECDH_MESSAGE);
    }

    #[test]
    fn psk_roundtrip() {
        const PSK_MESSAGE: &[u8] = &[
            0x00, 0x05, // identity length = 5
            b'h', b'e', b'l', b'l', b'o',
        ];
        let (rest, parsed) =
            ClientKeyExchange::parse(PSK_MESSAGE, 0, KeyExchangeAlgorithm::PSK).unwrap();
        assert!(rest.is_empty());

        let ExchangeKeys::Psk(psk) = &parsed.exchange_keys else {
            panic!("expected Psk variant");
        };
        assert_eq!(&PSK_MESSAGE[psk.identity_range.clone()], b"hello");

        let mut serialized = Buf::new();
        parsed.serialize(PSK_MESSAGE, &mut serialized);
        assert_eq!(&*serialized, PSK_MESSAGE);
    }

    #[test]
    fn psk_rejects_oversized_length() {
        // identity_length=0x0064 (100) but only 3 bytes follow — parser must fail
        let bad: &[u8] = &[0x00, 0x64, b'a', b'b', b'c'];
        let result = ClientKeyExchange::parse(bad, 0, KeyExchangeAlgorithm::PSK);
        assert!(
            result.is_err(),
            "parser must reject PSK identity shorter than advertised length"
        );
    }

    #[test]
    fn psk_empty_identity() {
        // identity_length=0 is wire-legal; parser should accept an empty range.
        // (RFC 4279 §5.1 says server MAY reject this — that's an application
        // policy decision, not a parse error.)
        let empty: &[u8] = &[0x00, 0x00];
        let (rest, parsed) = ClientKeyExchange::parse(empty, 0, KeyExchangeAlgorithm::PSK).unwrap();
        assert!(rest.is_empty());
        let ExchangeKeys::Psk(psk) = &parsed.exchange_keys else {
            panic!("expected Psk variant");
        };
        assert!(psk.identity_range.is_empty());
    }
}
