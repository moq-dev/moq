use super::{CompressionMethod, Dtls13CipherSuite, Extension};
use super::{ExtensionType, KeyShareHelloRetryRequest, KeyShareServerHello, NamedGroup};
use super::{ProtocolVersion, Random, SessionId};
use super::{SupportedVersionsServerHello, parse_cookie_extension};
use crate::buffer::Buf;
use arrayvec::ArrayVec;
use nom::Err;
use nom::error::{Error, ErrorKind};
use nom::{IResult, bytes::complete::take, number::complete::be_u16};

/// Magic random value indicating HelloRetryRequest (RFC 8446 Section 4.1.3).
const HRR_RANDOM: [u8; 32] = [
    0xCF, 0x21, 0xAD, 0x74, 0xE5, 0x9A, 0x61, 0x11, 0xBE, 0x1D, 0x8C, 0x02, 0x1E, 0x65, 0xB8, 0x91,
    0xC2, 0xA2, 0x11, 0x16, 0x7A, 0xBB, 0x8C, 0x5E, 0x07, 0x9E, 0x09, 0xE2, 0xC8, 0xA8, 0x33, 0x9C,
];

#[derive(Debug, PartialEq, Eq)]
pub struct ServerHello {
    pub legacy_version: ProtocolVersion,
    pub random: Random,
    pub legacy_session_id: SessionId,
    pub cipher_suite: Dtls13CipherSuite,
    pub legacy_compression_method: CompressionMethod,
    pub extensions: Option<ArrayVec<Extension, 5>>,
    // Receive-side values; outbound serialization uses the raw extensions above.
    parsed_extensions: ParsedExtensions,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ParsedExtensions {
    selected_version: Option<ProtocolVersion>,
    key_share: Option<KeyShareServerHello>,
    retry_group: Option<NamedGroup>,
}

impl ServerHello {
    pub fn new(
        legacy_version: ProtocolVersion,
        random: Random,
        legacy_session_id: SessionId,
        cipher_suite: Dtls13CipherSuite,
        legacy_compression_method: CompressionMethod,
        extensions: Option<ArrayVec<Extension, 5>>,
    ) -> Self {
        ServerHello {
            legacy_version,
            random,
            legacy_session_id,
            cipher_suite,
            legacy_compression_method,
            extensions,
            parsed_extensions: ParsedExtensions::default(),
        }
    }

    /// Returns true if this ServerHello is actually a HelloRetryRequest.
    pub fn is_hello_retry_request(&self) -> bool {
        self.random.bytes == HRR_RANDOM
    }

    pub fn parse(input: &[u8], base_offset: usize) -> IResult<&[u8], ServerHello> {
        let original_input = input;
        let mut parsed_extensions = ParsedExtensions::default();
        let (input, legacy_version) = ProtocolVersion::parse(input)?;
        let (input, random) = Random::parse(input)?;
        let (input, legacy_session_id) = SessionId::parse(input)?;
        let (input, cipher_suite) = Dtls13CipherSuite::parse(input)?;
        let (input, legacy_compression_method) = CompressionMethod::parse(input)?;

        let (input, extensions) = if !input.is_empty() {
            if input.len() < 2 {
                return Err(Err::Failure(Error::new(input, ErrorKind::Eof)));
            }
            let (input, extensions_len) = be_u16(input)?;

            if input.len() < extensions_len as usize {
                return Err(Err::Failure(Error::new(input, ErrorKind::Eof)));
            }

            if extensions_len > 0 {
                let (rest, input_ext) = take(extensions_len)(input)?;
                if !rest.is_empty() {
                    return Err(Err::Failure(Error::new(rest, ErrorKind::LengthValue)));
                }
                let consumed_to_ext_data =
                    input_ext.as_ptr() as usize - original_input.as_ptr() as usize;
                let ext_base_offset = base_offset + consumed_to_ext_data;

                let mut extensions_vec: ArrayVec<Extension, 5> = ArrayVec::new();
                let mut current_input = input_ext;
                let mut current_offset = ext_base_offset;
                while !current_input.is_empty() {
                    let before_len = current_input.len();
                    let (new_rest, ext) = Extension::parse(current_input, current_offset)?;
                    let parsed_len = before_len - new_rest.len();
                    current_offset += parsed_len;
                    if ext.extension_type.is_supported() {
                        if extensions_vec
                            .iter()
                            .any(|existing| existing.extension_type == ext.extension_type)
                        {
                            return Err(Err::Failure(Error::new(
                                current_input,
                                ErrorKind::LengthValue,
                            )));
                        }
                        let range = &ext.extension_data_range;
                        let data =
                            &original_input[range.start - base_offset..range.end - base_offset];
                        match ext.extension_type {
                            ExtensionType::SupportedVersions => {
                                parsed_extensions.selected_version = Some(
                                    SupportedVersionsServerHello::parse(data)?
                                        .1
                                        .selected_version,
                                );
                            }
                            ExtensionType::KeyShare if random.bytes == HRR_RANDOM => {
                                parsed_extensions.retry_group =
                                    Some(KeyShareHelloRetryRequest::parse(data)?.1.selected_group);
                            }
                            ExtensionType::KeyShare => {
                                parsed_extensions.key_share =
                                    Some(KeyShareServerHello::parse(data, range.start)?.1);
                            }
                            ExtensionType::Cookie if random.bytes == HRR_RANDOM => {
                                parse_cookie_extension(data)?;
                            }
                            _ => {}
                        }
                        extensions_vec.try_push(ext).map_err(|_| {
                            Err::Failure(Error::new(current_input, ErrorKind::LengthValue))
                        })?;
                    }
                    current_input = new_rest;
                }
                (rest, Some(extensions_vec))
            } else {
                (input, None)
            }
        } else {
            (input, None)
        };

        Ok((
            input,
            ServerHello {
                legacy_version,
                random,
                legacy_session_id,
                cipher_suite,
                legacy_compression_method,
                extensions,
                parsed_extensions,
            },
        ))
    }

    pub fn selected_version(&self) -> Option<ProtocolVersion> {
        self.parsed_extensions.selected_version
    }
    pub fn key_share(&self) -> Option<&KeyShareServerHello> {
        self.parsed_extensions.key_share.as_ref()
    }
    pub fn retry_group(&self) -> Option<NamedGroup> {
        self.parsed_extensions.retry_group
    }

    pub fn serialize(&self, source_buf: &[u8], output: &mut Buf) {
        output.extend_from_slice(&self.legacy_version.as_u16().to_be_bytes());
        self.random.serialize(output);
        output.push(self.legacy_session_id.len() as u8);
        output.extend_from_slice(&self.legacy_session_id);
        output.extend_from_slice(&self.cipher_suite.as_u16().to_be_bytes());
        output.push(self.legacy_compression_method.as_u8());
        if let Some(extensions) = &self.extensions {
            let mut extensions_len = 0;
            for ext in extensions.iter() {
                let ext_data = ext.extension_data(source_buf);
                extensions_len += 2 + 2 + ext_data.len();
            }

            output.extend_from_slice(&(extensions_len as u16).to_be_bytes());

            for ext in extensions {
                ext.serialize(source_buf, output);
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::super::ExtensionType;
    use super::*;
    use crate::buffer::Buf;

    const MESSAGE: &[u8] = &[
        0xFE, 0xFD, // ProtocolVersion::DTLS1_2 (legacy)
        // Random
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E,
        0x1F, 0x20, //
        0x01, // SessionId length
        0xAA, // SessionId
        0x13, 0x01, // Dtls13CipherSuite::AES_128_GCM_SHA256
        0x00, // CompressionMethod::Null
        0x00, 0x0C, // Extensions length (12 bytes)
        0x00, 0x0A, // ExtensionType::SupportedGroups
        0x00, 0x08, // Extension data length (8 bytes)
        0x00, 0x06, // Extension data
        0x00, 0x17, // NamedGroup::Secp256r1
        0x00, 0x18, // NamedGroup::Secp384r1
        0x00, 0x19, // NamedGroup::Secp521r1
    ];

    #[test]
    fn roundtrip() {
        let (rest, parsed) = ServerHello::parse(MESSAGE, 0).unwrap();
        assert!(rest.is_empty());

        let mut serialized = Buf::new();
        parsed.serialize(MESSAGE, &mut serialized);
        assert_eq!(&*serialized, MESSAGE);
    }

    #[test]
    fn session_id_too_long() {
        let mut message = MESSAGE.to_vec();
        message[34] = 0x21; // SessionId length (33, too long)

        let result = ServerHello::parse(&message, 0);
        assert!(result.is_err());
    }

    #[test]
    fn hello_retry_request_detection() {
        let hrr_random = Random { bytes: HRR_RANDOM };

        let sh = ServerHello::new(
            ProtocolVersion::DTLS1_2,
            hrr_random,
            SessionId::empty(),
            Dtls13CipherSuite::AES_128_GCM_SHA256,
            CompressionMethod::Null,
            None,
        );

        assert!(sh.is_hello_retry_request());
    }

    #[test]
    fn normal_server_hello_not_hrr() {
        let (_, parsed) = ServerHello::parse(MESSAGE, 0).unwrap();
        assert!(!parsed.is_hello_retry_request());
    }

    #[test]
    fn duplicate_supported_extensions_are_rejected() {
        let mut message = MESSAGE[..39].to_vec();
        let count = 2;
        message.extend_from_slice(&(count as u16 * 4).to_be_bytes());
        for _ in 0..count {
            message.extend_from_slice(&ExtensionType::Cookie.as_u16().to_be_bytes());
            message.extend_from_slice(&0u16.to_be_bytes());
        }

        let result = ServerHello::parse(&message, 0);
        assert!(
            matches!(
                result,
                Err(nom::Err::Failure(error))
                    if error.code == nom::error::ErrorKind::LengthValue
            ),
            "duplicate supported extensions should fail with LengthValue"
        );
    }

    #[test]
    fn too_many_distinct_supported_extensions_are_rejected() {
        let mut extensions = Vec::new();
        for extension_type in ExtensionType::supported() {
            let data: &[u8] = match *extension_type {
                ExtensionType::SupportedVersions => &[0xfe, 0xfc],
                ExtensionType::KeyShare => &[0, 23, 0, 1, 7],
                ExtensionType::SupportedGroups => &[0, 2, 0, 23],
                ExtensionType::SignatureAlgorithms => &[0, 2, 4, 3],
                ExtensionType::UseSrtp => &[0, 2, 0, 1, 0],
                ExtensionType::Cookie => &[0, 1, 7],
                _ => unreachable!(),
            };
            extensions.extend_from_slice(&extension_type.as_u16().to_be_bytes());
            extensions.extend_from_slice(&(data.len() as u16).to_be_bytes());
            extensions.extend_from_slice(data);
        }
        let mut message = MESSAGE[..39].to_vec();
        message.extend_from_slice(&(extensions.len() as u16).to_be_bytes());
        message.extend_from_slice(&extensions);

        let result = ServerHello::parse(&message, 0);
        assert!(
            matches!(
                result,
                Err(nom::Err::Failure(error))
                    if error.code == nom::error::ErrorKind::LengthValue
            ),
            "too many distinct supported extensions should fail with LengthValue"
        );
    }
}
