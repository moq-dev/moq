use super::SrtpProfileVec;
use super::extension::ExtensionVec;
use super::extensions::use_srtp::{SrtpProfileId, UseSrtpExtension};
use super::{CompressionMethod, Dtls12CipherSuite, Extension, ExtensionType};
use super::{ProtocolVersion, Random, SessionId};
use crate::buffer::Buf;
use arrayvec::ArrayVec;
use nom::Err;
use nom::error::{Error, ErrorKind};
use nom::{IResult, bytes::complete::take, number::complete::be_u16};

#[derive(Debug, PartialEq, Eq)]
pub struct ServerHello {
    pub server_version: ProtocolVersion,
    pub random: Random,
    pub session_id: SessionId,
    pub cipher_suite: Dtls12CipherSuite,
    pub compression_method: CompressionMethod,
    pub extensions: Option<ExtensionVec>,
    // Receive-side values; outbound serialization uses the raw extensions above.
    srtp_profiles: Option<SrtpProfileVec>,
}

impl ServerHello {
    pub fn new(
        server_version: ProtocolVersion,
        random: Random,
        session_id: SessionId,
        cipher_suite: Dtls12CipherSuite,
        compression_method: CompressionMethod,
        extensions: Option<ExtensionVec>,
    ) -> Self {
        ServerHello {
            server_version,
            random,
            session_id,
            cipher_suite,
            compression_method,
            extensions,
            srtp_profiles: None,
        }
    }

    /// Add extensions to ServerHello using a builder-style API, mirroring ClientHello::with_extensions
    ///
    /// - Uses the provided buffer to stage extension bytes and then stores Range references
    /// - Includes UseSRTP if a profile is provided
    /// - Includes Extended Master Secret if the flag is set
    pub fn with_extensions(mut self, buf: &mut Buf, srtp_profile: Option<SrtpProfileId>) -> Self {
        // Clear the buffer and collect extension byte ranges
        buf.clear();

        let mut ranges: ArrayVec<
            (ExtensionType, usize, usize),
            { ExtensionType::supported().len() },
        > = ArrayVec::new();

        // UseSRTP (if negotiated)
        if let Some(pid) = srtp_profile {
            let start = buf.len();
            let mut profiles = ArrayVec::new();
            profiles.push(pid);
            let ext = UseSrtpExtension::new(profiles, ArrayVec::new());
            ext.serialize(buf);
            ranges.push((ExtensionType::UseSrtp, start, buf.len()));
        }

        // Extended Master Secret (mandatory)
        let start = buf.len();
        ranges.push((ExtensionType::ExtendedMasterSecret, start, start));

        // Renegotiation Info (RFC 5746) - empty for initial handshake
        let start = buf.len();
        buf.push(0); // renegotiated_connection length = 0
        ranges.push((ExtensionType::RenegotiationInfo, start, buf.len()));

        let mut extensions = ExtensionVec::new();
        for (t, s, e) in ranges {
            extensions.push(Extension {
                extension_type: t,
                extension_data_range: s..e,
            });
        }
        self.extensions = Some(extensions);

        self
    }

    pub fn parse(input: &[u8], base_offset: usize) -> IResult<&[u8], ServerHello> {
        let original_input = input;
        let mut srtp_profiles = None;
        let (input, server_version) = ProtocolVersion::parse(input)?;
        let (input, random) = Random::parse(input)?;
        let (input, session_id) = SessionId::parse(input)?;
        let (input, cipher_suite) = Dtls12CipherSuite::parse(input)?;
        let (input, compression_method) = CompressionMethod::parse(input)?;

        // Parse extensions if there are any bytes left
        let (input, extensions) = if !input.is_empty() {
            // Check if we have enough bytes to read the extensions length (2 bytes)
            if input.len() < 2 {
                return Err(Err::Failure(Error::new(input, ErrorKind::Eof)));
            }
            let (input, extensions_len) = be_u16(input)?;

            // Check if we have enough bytes to read the extensions data
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

                // Parse extensions manually to pass base_offset, filtering unknown types
                let mut extensions_vec = ExtensionVec::new();
                let mut current_input = input_ext;
                let mut current_offset = ext_base_offset;
                while !current_input.is_empty() {
                    let before_len = current_input.len();
                    let (new_rest, ext) = Extension::parse(current_input, current_offset)?;
                    let parsed_len = before_len - new_rest.len();
                    current_offset += parsed_len;
                    // Only keep supported extension types
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
                        if ext.extension_type == ExtensionType::UseSrtp {
                            let range = &ext.extension_data_range;
                            let data =
                                &original_input[range.start - base_offset..range.end - base_offset];
                            srtp_profiles = Some(UseSrtpExtension::parse(data)?.1.profiles);
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
                server_version,
                random,
                session_id,
                cipher_suite,
                compression_method,
                extensions,
                srtp_profiles,
            },
        ))
    }

    pub fn srtp_profiles(&self) -> Option<&SrtpProfileVec> {
        self.srtp_profiles.as_ref()
    }

    pub fn serialize(&self, source_buf: &[u8], output: &mut Buf) {
        output.extend_from_slice(&self.server_version.as_u16().to_be_bytes());
        self.random.serialize(output);
        output.push(self.session_id.len() as u8);
        output.extend_from_slice(&self.session_id);
        output.extend_from_slice(&self.cipher_suite.as_u16().to_be_bytes());
        output.push(self.compression_method.as_u8());
        if let Some(extensions) = &self.extensions {
            // Calculate total extensions length according to spec:
            // For each extension: type (2) + length (2) + data
            let mut extensions_len = 0;
            for ext in extensions.iter() {
                let ext_data = ext.extension_data(source_buf);
                extensions_len += 2 + 2 + ext_data.len();
            }

            // Write extensions length
            output.extend_from_slice(&(extensions_len as u16).to_be_bytes());

            // Write each extension
            for ext in extensions {
                ext.serialize(source_buf, output);
            }
        }
    }
}

#[cfg(test)]
mod test {
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
        0xC0, 0x2B, // Dtls12CipherSuite::ECDHE_ECDSA_AES128_GCM_SHA256
        0x00, // CompressionMethod::Null
        0x00, 0x0C, // Extensions length (12 bytes total: 2 type + 2 length + 8 data)
        0x00, 0x0A, // ExtensionType::SupportedGroups
        0x00, 0x08, // Extension data length (8 bytes)
        0x00, 0x06, // Extension data
        0x00, 0x17, // NamedGroup::Secp256r1
        0x00, 0x18, // NamedGroup::Secp384r1
        0x00, 0x19, // NamedGroup::Secp521r1
    ];

    #[test]
    fn roundtrip() {
        // Parse the message first to get the Extension with proper ranges
        let (rest, parsed) = ServerHello::parse(MESSAGE, 0).unwrap();
        assert!(rest.is_empty());

        // Serialize and compare to MESSAGE
        let mut serialized = Buf::new();
        parsed.serialize(MESSAGE, &mut serialized);
        assert_eq!(&*serialized, MESSAGE);
    }

    #[test]
    fn session_id_too_long() {
        let mut message = MESSAGE.to_vec();
        message[34] = 0x21; // SessionId length (33, which is too long)

        let result = ServerHello::parse(&message, 0);
        assert!(result.is_err());
    }

    #[test]
    fn duplicate_supported_extensions_are_rejected() {
        for count in [2, 9] {
            let mut message = MESSAGE[..39].to_vec();
            message.extend_from_slice(&(count as u16 * 4).to_be_bytes());
            for _ in 0..count {
                message
                    .extend_from_slice(&ExtensionType::ExtendedMasterSecret.as_u16().to_be_bytes());
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
    }
}
