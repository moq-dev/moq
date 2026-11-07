use super::{CipherSuiteVec, CompressionMethod, CompressionMethodVec, Dtls12CipherSuite};
use super::{Cookie, Extension, ExtensionType, ProtocolVersion, Random, SessionId};
use super::{ECPointFormatsExtension, SignatureAlgorithmsExtension};
use super::{NamedGroupVec, SignatureAndHashAlgorithmVec, SrtpProfileVec};
use super::{SupportedGroupsExtension, UseSrtpExtension};
use arrayvec::ArrayVec;
use nom::bytes::complete::take;
use nom::error::{Error, ErrorKind};
use nom::number::complete::{be_u8, be_u16};
use nom::{Err, IResult};

use super::extension::ExtensionVec;
use crate::Config;
use crate::buffer::Buf;
use crate::util::many1;

#[derive(Debug, PartialEq, Eq)]
pub struct ClientHello {
    pub client_version: ProtocolVersion,
    pub random: Random,
    pub session_id: SessionId,
    pub cookie: Cookie,
    pub cipher_suites: CipherSuiteVec,
    pub compression_methods: CompressionMethodVec,
    pub extensions: ExtensionVec,
    // Receive-side values; outbound serialization uses the raw extensions above.
    parsed_extensions: ParsedExtensions,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ParsedExtensions {
    supported_groups: Option<NamedGroupVec>,
    signature_algorithms: Option<SignatureAndHashAlgorithmVec>,
    srtp_profiles: Option<SrtpProfileVec>,
}

impl ClientHello {
    pub fn new(
        client_version: ProtocolVersion,
        random: Random,
        session_id: SessionId,
        cookie: Cookie,
        cipher_suites: CipherSuiteVec,
        compression_methods: CompressionMethodVec,
    ) -> Self {
        ClientHello {
            client_version,
            random,
            session_id,
            cookie,
            cipher_suites,
            compression_methods,
            extensions: ArrayVec::new(),
            parsed_extensions: ParsedExtensions::default(),
        }
    }

    /// Add all required extensions for DTLS handshake
    pub fn with_extensions(mut self, buf: &mut Buf, config: &Config) -> Self {
        // Clear the extension data buffer
        buf.clear();

        // First write all extension data
        let mut ranges = ArrayVec::<(ExtensionType, usize, usize), 8>::new();

        // Check if provider has ECDH support
        let has_ecdh = config.crypto_provider().has_ecdh();

        // Add supported groups and EC point formats if using ECDH
        if has_ecdh {
            // Add supported groups extension from config
            let mut groups = super::NamedGroupVec::new();
            for kx_group in config.kx_groups() {
                groups.push(kx_group.name());
            }
            let supported_groups = SupportedGroupsExtension { groups };
            let start_pos = buf.len();
            supported_groups.serialize(buf);
            ranges.push((ExtensionType::SupportedGroups, start_pos, buf.len()));

            // Add EC point formats extension
            let ec_point_formats = ECPointFormatsExtension::default();
            let start_pos = buf.len();
            ec_point_formats.serialize(buf);
            ranges.push((ExtensionType::EcPointFormats, start_pos, buf.len()));
        }

        // Add signature algorithms extension (required for TLS 1.2+)
        let signature_algorithms = SignatureAlgorithmsExtension::default();
        let start_pos = buf.len();
        signature_algorithms.serialize(buf);
        ranges.push((ExtensionType::SignatureAlgorithms, start_pos, buf.len()));

        // Add use_srtp extension for DTLS-SRTP support
        let use_srtp = UseSrtpExtension::default();
        let start_pos = buf.len();
        use_srtp.serialize(buf);
        ranges.push((ExtensionType::UseSrtp, start_pos, buf.len()));

        // // Add session_ticket extension (empty)
        // let start_pos = buf.len();
        // buf.extend_from_slice(&[0x00]); // Empty extension data
        // ranges.push((ExtensionType::SessionTicket, start_pos, buf.len()));

        let need_etm = self
            .cipher_suites
            .iter()
            .any(|suite| suite.need_encrypt_then_mac());
        if need_etm {
            // Add encrypt_then_mac extension (empty)
            let start_pos = buf.len();
            buf.extend_from_slice(&[0x00]); // Empty extension data
            ranges.push((ExtensionType::EncryptThenMac, start_pos, buf.len()));
        }

        let start_pos = buf.len();
        ranges.push((
            ExtensionType::ExtendedMasterSecret,
            start_pos,
            start_pos, // No data at all
        ));

        // Now create all extensions using ranges
        for (extension_type, start, end) in ranges {
            self.extensions.push(Extension {
                extension_type,
                extension_data_range: start..end,
            });
        }

        self
    }

    pub fn parse(input: &[u8], base_offset: usize) -> IResult<&[u8], ClientHello> {
        let original_input = input;
        let (input, client_version) = ProtocolVersion::parse(input)?;
        let (input, random) = Random::parse(input)?;
        let (input, session_id) = SessionId::parse(input)?;
        let (input, cookie) = Cookie::parse(input)?;
        let (input, cipher_suites_len) = be_u16(input)?;
        let (input, input_cipher) = take(cipher_suites_len)(input)?;
        let (rest, cipher_suites) =
            many1(Dtls12CipherSuite::parse, Dtls12CipherSuite::is_supported)(input_cipher)?;
        if !rest.is_empty() {
            return Err(Err::Failure(Error::new(rest, ErrorKind::LengthValue)));
        }
        let (input, compression_methods_len) = be_u8(input)?;
        let (input, input_compression) = take(compression_methods_len)(input)?;
        let (rest, compression_methods) =
            many1(CompressionMethod::parse, CompressionMethod::is_supported)(input_compression)?;
        if !rest.is_empty() {
            return Err(Err::Failure(Error::new(rest, ErrorKind::LengthValue)));
        }

        // Calculate base_offset for extensions parsing
        let consumed = input.as_ptr() as usize - original_input.as_ptr() as usize;
        let extensions_base_offset = base_offset + consumed;

        // Parse extensions if there are any left
        let mut parsed_extensions = ParsedExtensions::default();
        let (remaining_input, extensions) =
            Self::parse_extensions(input, extensions_base_offset, &mut parsed_extensions)?;

        Ok((
            remaining_input,
            ClientHello {
                client_version,
                random,
                session_id,
                cookie,
                cipher_suites,
                compression_methods,
                extensions,
                parsed_extensions,
            },
        ))
    }

    /// Parse extensions from the input, filtering to only known extension types
    fn parse_extensions<'a>(
        input: &'a [u8],
        base_offset: usize,
        parsed: &mut ParsedExtensions,
    ) -> IResult<&'a [u8], ExtensionVec> {
        let mut extensions = ArrayVec::new();

        // Early return if input is empty
        if input.is_empty() {
            return Ok((input, extensions));
        }

        let original_input = input;

        // Parse extensions length
        let (remaining, extensions_len) = be_u16(input)?;

        // Early return if extensions length is 0
        if extensions_len == 0 {
            return Ok((remaining, extensions));
        }

        // Take the extensions data
        let (remaining, extensions_data) = take(extensions_len)(remaining)?;

        // Calculate base_offset for extension data
        let consumed = extensions_data.as_ptr() as usize - original_input.as_ptr() as usize;
        let data_base_offset = base_offset + consumed;

        // Parse individual extensions, filtering to only known types
        let mut extensions_rest = extensions_data;
        let mut current_offset = data_base_offset;
        while !extensions_rest.is_empty() {
            let before_len = extensions_rest.len();
            let (rest, extension) = Extension::parse(extensions_rest, current_offset)?;
            let parsed_len = before_len - rest.len();
            current_offset += parsed_len;

            // Only keep supported extension types
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
                        parsed.signature_algorithms = Some(
                            SignatureAlgorithmsExtension::parse(data)?
                                .1
                                .supported_signature_algorithms,
                        );
                    }
                    ExtensionType::UseSrtp => {
                        parsed.srtp_profiles = Some(UseSrtpExtension::parse(data)?.1.profiles);
                    }
                    ExtensionType::EcPointFormats => {
                        ECPointFormatsExtension::parse(data)?;
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

    pub fn supported_groups(&self) -> Option<&NamedGroupVec> {
        self.parsed_extensions.supported_groups.as_ref()
    }

    pub fn signature_algorithms(&self) -> Option<&SignatureAndHashAlgorithmVec> {
        self.parsed_extensions.signature_algorithms.as_ref()
    }

    pub fn srtp_profiles(&self) -> Option<&SrtpProfileVec> {
        self.parsed_extensions.srtp_profiles.as_ref()
    }

    pub fn serialize(&self, source_buf: &[u8], output: &mut Buf) {
        output.extend_from_slice(&self.client_version.as_u16().to_be_bytes());
        self.random.serialize(output);
        output.push(self.session_id.len() as u8);
        output.extend_from_slice(&self.session_id);
        output.push(self.cookie.len() as u8);
        output.extend_from_slice(&self.cookie);
        output.extend_from_slice(&(self.cipher_suites.len() as u16 * 2).to_be_bytes());
        for suite in &self.cipher_suites {
            output.extend_from_slice(&suite.as_u16().to_be_bytes());
        }
        output.push(self.compression_methods.len() as u8);
        for method in &self.compression_methods {
            output.push(method.as_u8());
        }

        // Add extensions if any
        if !self.extensions.is_empty() {
            // First calculate total extensions length
            let mut extensions_len = 0;
            for ext in &self.extensions {
                // Extension type (2) + Extension length (2) + Extension data
                let ext_data = ext.extension_data(source_buf);
                extensions_len += 4 + ext_data.len();
            }

            // Write extensions length
            output.extend_from_slice(&(extensions_len as u16).to_be_bytes());

            // Write each extension
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

    const MESSAGE: &[u8] = &[
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

    #[test]
    fn roundtrip() {
        let random = Random::parse(&MESSAGE[2..34]).unwrap().1;
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

        // Serialize and compare to MESSAGE
        let mut serialized = Buf::new();
        client_hello.serialize(&[], &mut serialized);
        assert_eq!(&*serialized, MESSAGE);

        // Parse and compare with original
        let (rest, parsed) = ClientHello::parse(&serialized, 0).unwrap();
        assert_eq!(parsed, client_hello);

        assert!(rest.is_empty());
    }

    #[test]
    fn session_id_too_long() {
        let mut message = MESSAGE.to_vec();
        message[34] = 0x21; // SessionId length (33, which is too long)

        let result = ClientHello::parse(&message, 0);
        assert!(result.is_err());
    }

    #[test]
    fn cookie_too_long() {
        let mut message = MESSAGE.to_vec();
        message[36] = 0xFF; // Cookie length (255, which is too long)

        let result = ClientHello::parse(&message, 0);
        assert!(result.is_err());
    }

    #[test]
    fn duplicate_supported_extensions_are_rejected() {
        for count in [2, 9] {
            let mut message = MESSAGE.to_vec();
            message.extend_from_slice(&(count as u16 * 4).to_be_bytes());
            for _ in 0..count {
                message
                    .extend_from_slice(&ExtensionType::ExtendedMasterSecret.as_u16().to_be_bytes());
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
}
