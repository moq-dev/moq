use super::parse_cookie_extension;
use super::{CompressionMethod, Dtls13CipherSuite, ProtocolVersion};
use super::{Cookie, Extension, Random, SessionId};
use super::{ExtensionType, KeyShareClientHello, SignatureAlgorithmsExtension};
use super::{NamedGroup, SrtpProfileId};
use super::{SupportedGroupsExtension, SupportedVersionsClientHello, UseSrtpExtension};
use arrayvec::ArrayVec;
use nom::bytes::complete::take;
use nom::error::{Error, ErrorKind};
use nom::number::complete::{be_u8, be_u16};
use nom::{Err, IResult};

use crate::buffer::Buf;
use crate::util::{many0, many1};

#[derive(Debug, PartialEq, Eq)]
pub struct ClientHello {
    pub legacy_version: ProtocolVersion,
    pub random: Random,
    pub legacy_session_id: SessionId,
    pub legacy_cookie: Cookie,
    pub cipher_suites: ArrayVec<Dtls13CipherSuite, 3>,
    pub legacy_compression_methods: ArrayVec<CompressionMethod, 1>,
    pub extensions: ArrayVec<Extension, 8>,
    // Receive-side values; outbound serialization uses the raw extensions above.
    parsed_extensions: ParsedExtensions,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ParsedExtensions {
    supported_versions: Option<SupportedVersionsClientHello>,
    key_share: Option<KeyShareClientHello>,
    supported_groups: Option<ArrayVec<NamedGroup, 4>>,
    srtp_profiles: Option<ArrayVec<SrtpProfileId, 3>>,
}

impl ClientHello {
    pub fn new(
        legacy_version: ProtocolVersion,
        random: Random,
        legacy_session_id: SessionId,
        legacy_cookie: Cookie,
        cipher_suites: ArrayVec<Dtls13CipherSuite, 3>,
        legacy_compression_methods: ArrayVec<CompressionMethod, 1>,
    ) -> Self {
        ClientHello {
            legacy_version,
            random,
            legacy_session_id,
            legacy_cookie,
            cipher_suites,
            legacy_compression_methods,
            extensions: ArrayVec::new(),
            parsed_extensions: ParsedExtensions::default(),
        }
    }

    pub fn parse(input: &[u8], base_offset: usize) -> IResult<&[u8], ClientHello> {
        Self::parse_with_options(input, base_offset, false)
    }

    pub(crate) fn parse_allow_unknown_suites(
        input: &[u8],
        base_offset: usize,
    ) -> IResult<&[u8], ClientHello> {
        Self::parse_with_options(input, base_offset, true)
    }

    fn parse_with_options(
        input: &[u8],
        base_offset: usize,
        allow_unknown_suites: bool,
    ) -> IResult<&[u8], ClientHello> {
        let original_input = input;
        let (input, legacy_version) = ProtocolVersion::parse(input)?;
        let (input, random) = Random::parse(input)?;
        let (input, legacy_session_id) = SessionId::parse(input)?;
        let (input, legacy_cookie) = Cookie::parse(input)?;
        let (input, cipher_suites_len) = be_u16(input)?;
        if cipher_suites_len == 0 || cipher_suites_len % 2 != 0 {
            return Err(Err::Failure(Error::new(input, ErrorKind::LengthValue)));
        }
        let (input, input_cipher) = take(cipher_suites_len)(input)?;
        let (rest, cipher_suites) = if allow_unknown_suites {
            many0(Dtls13CipherSuite::parse, Dtls13CipherSuite::is_supported)(input_cipher)?
        } else {
            many1(Dtls13CipherSuite::parse, Dtls13CipherSuite::is_supported)(input_cipher)?
        };
        if !rest.is_empty() {
            return Err(Err::Failure(Error::new(rest, ErrorKind::LengthValue)));
        }
        let (input, compression_methods_len) = be_u8(input)?;
        let (input, input_compression) = take(compression_methods_len)(input)?;
        let (rest, legacy_compression_methods) =
            many1(CompressionMethod::parse, CompressionMethod::is_supported)(input_compression)?;
        if !rest.is_empty() {
            return Err(Err::Failure(Error::new(rest, ErrorKind::LengthValue)));
        }

        let consumed = input.as_ptr() as usize - original_input.as_ptr() as usize;
        let extensions_base_offset = base_offset + consumed;

        let mut parsed_extensions = ParsedExtensions::default();
        let (remaining_input, extensions) =
            Self::parse_extensions(input, extensions_base_offset, &mut parsed_extensions)?;

        Ok((
            remaining_input,
            ClientHello {
                legacy_version,
                random,
                legacy_session_id,
                legacy_cookie,
                cipher_suites,
                legacy_compression_methods,
                extensions,
                parsed_extensions,
            },
        ))
    }

    fn parse_extensions<'a>(
        input: &'a [u8],
        base_offset: usize,
        parsed: &mut ParsedExtensions,
    ) -> IResult<&'a [u8], ArrayVec<Extension, 8>> {
        let mut extensions = ArrayVec::new();

        if input.is_empty() {
            return Ok((input, extensions));
        }

        let original_input = input;
        let (remaining, extensions_len) = be_u16(input)?;

        let (remaining, extensions_data) = take(extensions_len)(remaining)?;
        if !remaining.is_empty() {
            return Err(Err::Failure(Error::new(remaining, ErrorKind::LengthValue)));
        }

        let consumed = extensions_data.as_ptr() as usize - original_input.as_ptr() as usize;
        let data_base_offset = base_offset + consumed;

        let mut extensions_rest = extensions_data;
        let mut current_offset = data_base_offset;
        while !extensions_rest.is_empty() {
            let before_len = extensions_rest.len();
            let (rest, extension) = Extension::parse(extensions_rest, current_offset)?;
            let parsed_len = before_len - rest.len();
            current_offset += parsed_len;

            if extension.extension_type.is_supported() {
                if extensions
                    .iter()
                    .any(|existing| existing.extension_type == extension.extension_type)
                {
                    return Err(Err::Failure(Error::new(
                        extensions_rest,
                        ErrorKind::LengthValue,
                    )));
                }
                // Parse payloads once, retaining the values used by the server.
                // Ranges are absolute, while this input starts at base_offset.
                let range = &extension.extension_data_range;
                let data = &original_input[range.start - base_offset..range.end - base_offset];
                match extension.extension_type {
                    ExtensionType::SupportedGroups => {
                        parsed.supported_groups =
                            Some(SupportedGroupsExtension::parse(data)?.1.groups);
                    }
                    ExtensionType::SignatureAlgorithms => {
                        SignatureAlgorithmsExtension::parse(data)?;
                    }
                    ExtensionType::UseSrtp => {
                        parsed.srtp_profiles = Some(UseSrtpExtension::parse(data)?.1.profiles);
                    }
                    ExtensionType::SupportedVersions => {
                        parsed.supported_versions =
                            Some(SupportedVersionsClientHello::parse(data)?.1);
                    }
                    ExtensionType::KeyShare => {
                        parsed.key_share = Some(
                            KeyShareClientHello::parse(data, extension.extension_data_range.start)?
                                .1,
                        );
                    }
                    ExtensionType::Cookie => {
                        parse_cookie_extension(data)?;
                    }
                    _ => {}
                }
                extensions.try_push(extension).map_err(|_| {
                    Err::Failure(Error::new(extensions_rest, ErrorKind::LengthValue))
                })?;
            }
            extensions_rest = rest;
        }

        Ok((remaining, extensions))
    }

    pub fn supported_versions(&self) -> Option<&SupportedVersionsClientHello> {
        self.parsed_extensions.supported_versions.as_ref()
    }

    pub fn key_share(&self) -> Option<&KeyShareClientHello> {
        self.parsed_extensions.key_share.as_ref()
    }

    pub fn supported_groups(&self) -> Option<&ArrayVec<NamedGroup, 4>> {
        self.parsed_extensions.supported_groups.as_ref()
    }

    pub fn srtp_profiles(&self) -> Option<&ArrayVec<SrtpProfileId, 3>> {
        self.parsed_extensions.srtp_profiles.as_ref()
    }

    pub fn serialize(&self, source_buf: &[u8], output: &mut Buf) {
        output.extend_from_slice(&self.legacy_version.as_u16().to_be_bytes());
        self.random.serialize(output);
        output.push(self.legacy_session_id.len() as u8);
        output.extend_from_slice(&self.legacy_session_id);
        output.push(self.legacy_cookie.len() as u8);
        output.extend_from_slice(&self.legacy_cookie);
        output.extend_from_slice(&(self.cipher_suites.len() as u16 * 2).to_be_bytes());
        for suite in &self.cipher_suites {
            output.extend_from_slice(&suite.as_u16().to_be_bytes());
        }
        output.push(self.legacy_compression_methods.len() as u8);
        for method in &self.legacy_compression_methods {
            output.push(method.as_u8());
        }

        if !self.extensions.is_empty() {
            let mut extensions_len = 0;
            for ext in &self.extensions {
                let ext_data = ext.extension_data(source_buf);
                extensions_len += 4 + ext_data.len();
            }

            output.extend_from_slice(&(extensions_len as u16).to_be_bytes());

            for ext in &self.extensions {
                ext.serialize(source_buf, output);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Buf;
    use crate::dtls13::message::ExtensionType;

    const MESSAGE: &[u8] = &[
        0xFE, 0xFD, // ProtocolVersion::DTLS1_2 (legacy)
        // Random
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E,
        0x1F, 0x20, //
        0x01, // SessionId length
        0xAA, // SessionId
        0x01, // Cookie length
        0xBB, // Cookie
        0x00, 0x04, // CipherSuites length
        0x13, 0x01, // AES_128_GCM_SHA256
        0x13, 0x02, // AES_256_GCM_SHA384
        0x01, // CompressionMethods length
        0x00, // Null
    ];

    #[test]
    fn roundtrip() {
        let random = Random::parse(&MESSAGE[2..34]).unwrap().1;
        let session_id = SessionId::try_new(&[0xAA]).unwrap();
        let cookie = Cookie::try_new(&[0xBB]).unwrap();
        let mut cipher_suites = ArrayVec::new();
        cipher_suites.push(Dtls13CipherSuite::AES_128_GCM_SHA256);
        cipher_suites.push(Dtls13CipherSuite::AES_256_GCM_SHA384);
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

        let mut serialized = Buf::new();
        client_hello.serialize(&[], &mut serialized);
        assert_eq!(&*serialized, MESSAGE);

        let (rest, parsed) = ClientHello::parse(&serialized, 0).unwrap();
        assert_eq!(parsed, client_hello);

        assert!(rest.is_empty());
    }

    #[test]
    fn dtls12_only_cipher_suites_parse_for_auto_detection() {
        let mut message = MESSAGE.to_vec();
        // DTLS 1.2 ECDHE_ECDSA AES-GCM suites are intentionally unknown to
        // the DTLS 1.3 suite filter, but the ClientHello is structurally valid.
        message[40..44].copy_from_slice(&[0xC0, 0x2B, 0xC0, 0x2C]);

        let (rest, parsed) = ClientHello::parse_allow_unknown_suites(&message, 0).unwrap();

        assert!(rest.is_empty());
        assert!(parsed.cipher_suites.is_empty());
    }

    #[test]
    fn dtls12_only_cipher_suites_are_rejected_by_normal_parse() {
        let mut message = MESSAGE.to_vec();
        message[40..44].copy_from_slice(&[0xC0, 0x2B, 0xC0, 0x2C]);

        assert!(ClientHello::parse(&message, 0).is_err());
    }

    #[test]
    fn empty_cipher_suites_is_malformed_in_both_modes() {
        let mut message = Vec::new();
        message.extend_from_slice(&MESSAGE[..38]);
        message.extend_from_slice(&[0x00, 0x00]);
        message.extend_from_slice(&MESSAGE[44..]);

        assert!(ClientHello::parse(&message, 0).is_err());
        assert!(ClientHello::parse_allow_unknown_suites(&message, 0).is_err());
    }

    #[test]
    fn odd_cipher_suites_is_malformed_in_both_modes() {
        let mut message = Vec::new();
        message.extend_from_slice(&MESSAGE[..38]);
        message.extend_from_slice(&[0x00, 0x03]);
        message.extend_from_slice(&[0x13, 0x01, 0x13]);
        message.extend_from_slice(&MESSAGE[44..]);

        assert!(ClientHello::parse(&message, 0).is_err());
        assert!(ClientHello::parse_allow_unknown_suites(&message, 0).is_err());
    }

    #[test]
    fn session_id_too_long() {
        let mut message = MESSAGE.to_vec();
        message[34] = 0x21; // SessionId length (33, too long)

        let result = ClientHello::parse(&message, 0);
        assert!(result.is_err());
    }

    #[test]
    fn cookie_too_long() {
        let mut message = MESSAGE.to_vec();
        message[36] = 0xFF; // Cookie length (255, too long for available data)

        let result = ClientHello::parse(&message, 0);
        assert!(result.is_err());
    }

    #[test]
    fn cipher_suites_capacity_matches_known() {
        let client_hello = ClientHello {
            legacy_version: ProtocolVersion::DTLS1_2,
            random: Random::parse(&MESSAGE[2..34]).unwrap().1,
            legacy_session_id: SessionId::empty(),
            legacy_cookie: Cookie::empty(),
            cipher_suites: ArrayVec::new(),
            legacy_compression_methods: ArrayVec::new(),
            extensions: ArrayVec::new(),
            parsed_extensions: ParsedExtensions::default(),
        };

        assert_eq!(
            client_hello.cipher_suites.capacity(),
            Dtls13CipherSuite::supported().len(),
            "cipher_suites ArrayVec capacity must match supported Dtls13CipherSuites"
        );
    }

    #[test]
    fn extensions_capacity_fits_supported() {
        let client_hello = ClientHello {
            legacy_version: ProtocolVersion::DTLS1_2,
            random: Random::parse(&MESSAGE[2..34]).unwrap().1,
            legacy_session_id: SessionId::empty(),
            legacy_cookie: Cookie::empty(),
            cipher_suites: ArrayVec::new(),
            legacy_compression_methods: ArrayVec::new(),
            extensions: ArrayVec::new(),
            parsed_extensions: ParsedExtensions::default(),
        };

        assert!(
            client_hello.extensions.capacity() >= ExtensionType::supported().len(),
            "extensions ArrayVec capacity must fit all supported ExtensionTypes"
        );
    }

    #[test]
    fn duplicate_supported_extensions_are_rejected() {
        for count in [2, 9] {
            let mut message = MESSAGE.to_vec();
            message.extend_from_slice(&(count as u16 * 4).to_be_bytes());
            for _ in 0..count {
                message.extend_from_slice(&ExtensionType::Cookie.as_u16().to_be_bytes());
                message.extend_from_slice(&0u16.to_be_bytes());
            }

            let result = ClientHello::parse(&message, 0);
            assert!(
                matches!(
                    result,
                    Err(nom::Err::Failure(error))
                        if error.code == nom::error::ErrorKind::LengthValue
                ),
                "duplicate supported extensions should fail with LengthValue"
            );
        }
    }

    #[test]
    fn zero_length_extension_vector_rejects_trailing_bytes() {
        let mut message = MESSAGE.to_vec();
        message.extend_from_slice(&0u16.to_be_bytes());
        message.extend_from_slice(&ExtensionType::Cookie.as_u16().to_be_bytes());
        message.extend_from_slice(&0u16.to_be_bytes());

        assert!(
            ClientHello::parse(&message, 0).is_err(),
            "extension vector length 0 must consume the remaining ClientHello body"
        );
    }

    #[test]
    fn underdeclared_extension_vector_rejects_trailing_bytes() {
        let mut message = MESSAGE.to_vec();
        message.extend_from_slice(&4u16.to_be_bytes());
        message.extend_from_slice(&ExtensionType::Cookie.as_u16().to_be_bytes());
        message.extend_from_slice(&0u16.to_be_bytes());
        message.push(0);

        assert!(
            ClientHello::parse(&message, 0).is_err(),
            "declared extension vector length must consume the remaining ClientHello body"
        );
    }
}
