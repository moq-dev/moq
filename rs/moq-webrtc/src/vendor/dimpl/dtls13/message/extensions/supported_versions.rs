use crate::buffer::Buf;
use crate::types::ProtocolVersion;
use arrayvec::ArrayVec;
use nom::Err;
use nom::IResult;
use nom::bytes::complete::take;
use nom::error::{Error, ErrorKind};
use nom::number::complete::be_u8;

/// SupportedVersions extension in ClientHello (RFC 8446 Section 4.2.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupportedVersionsClientHello {
    pub versions: ArrayVec<ProtocolVersion, 3>,
}

impl SupportedVersionsClientHello {
    pub fn parse(input: &[u8]) -> IResult<&[u8], SupportedVersionsClientHello> {
        let (input, list_len) = be_u8(input)?;
        if list_len == 0 || list_len % 2 != 0 {
            return Err(Err::Failure(Error::new(input, ErrorKind::LengthValue)));
        }

        let (input, versions_data) = take(list_len)(input)?;
        if !input.is_empty() {
            return Err(Err::Failure(Error::new(input, ErrorKind::LengthValue)));
        }

        let mut versions = ArrayVec::new();
        let mut rest = versions_data;
        while !rest.is_empty() {
            let (r, version) = ProtocolVersion::parse(rest)?;
            if !version.is_unknown() {
                versions
                    .try_push(version)
                    .map_err(|_| Err::Failure(Error::new(rest, ErrorKind::LengthValue)))?;
            }
            rest = r;
        }

        Ok((input, SupportedVersionsClientHello { versions }))
    }

    pub fn serialize(&self, output: &mut Buf) {
        output.push((self.versions.len() * 2) as u8);
        for version in &self.versions {
            output.extend_from_slice(&version.as_u16().to_be_bytes());
        }
    }
}

/// SupportedVersions extension in ServerHello (RFC 8446 Section 4.2.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupportedVersionsServerHello {
    pub selected_version: ProtocolVersion,
}

impl SupportedVersionsServerHello {
    pub fn parse(input: &[u8]) -> IResult<&[u8], SupportedVersionsServerHello> {
        let (input, selected_version) = ProtocolVersion::parse(input)?;
        if !input.is_empty() {
            return Err(Err::Failure(Error::new(input, ErrorKind::LengthValue)));
        }

        Ok((input, SupportedVersionsServerHello { selected_version }))
    }

    pub fn serialize(&self, output: &mut Buf) {
        output.extend_from_slice(&self.selected_version.as_u16().to_be_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Buf;

    #[test]
    fn client_hello_roundtrip() {
        let message: &[u8] = &[
            0x04, // list length (4 bytes = 2 versions)
            0xFE, 0xFC, // DTLS 1.3
            0xFE, 0xFD, // DTLS 1.2
        ];

        let (rest, parsed) = SupportedVersionsClientHello::parse(message).unwrap();
        assert!(rest.is_empty());

        let mut serialized = Buf::new();
        parsed.serialize(&mut serialized);
        assert_eq!(&*serialized, message);
    }

    #[test]
    fn server_hello_roundtrip() {
        let message: &[u8] = &[
            0xFE, 0xFC, // DTLS 1.3
        ];

        let (rest, parsed) = SupportedVersionsServerHello::parse(message).unwrap();
        assert!(rest.is_empty());

        let mut serialized = Buf::new();
        parsed.serialize(&mut serialized);
        assert_eq!(&*serialized, message);
    }

    #[test]
    fn unknown_client_hello_versions_are_ignored() {
        let message: &[u8] = &[
            0x08, // list length (8 bytes = 4 versions)
            0xFE, 0xFC, // DTLS 1.3
            0xFE, 0xFD, // DTLS 1.2
            0xFE, 0xFF, // DTLS 1.0
            0xFE, 0xFE, // Unknown
        ];

        let (rest, parsed) = SupportedVersionsClientHello::parse(message).unwrap();
        assert!(rest.is_empty());
        assert_eq!(
            parsed.versions.as_slice(),
            &[
                ProtocolVersion::DTLS1_3,
                ProtocolVersion::DTLS1_2,
                ProtocolVersion::DTLS1_0,
            ]
        );
    }

    #[test]
    fn too_many_duplicate_client_hello_versions_are_rejected() {
        let message: &[u8] = &[
            0x08, // list length (8 bytes = 4 versions)
            0xFE, 0xFC, // DTLS 1.3
            0xFE, 0xFC, // DTLS 1.3
            0xFE, 0xFC, // DTLS 1.3
            0xFE, 0xFC, // DTLS 1.3
        ];

        let result = SupportedVersionsClientHello::parse(message);
        assert!(
            matches!(
                result,
                Err(nom::Err::Failure(error))
                    if error.code == nom::error::ErrorKind::LengthValue
            ),
            "too many supported versions should fail with LengthValue"
        );
    }

    #[test]
    fn odd_client_hello_versions_are_rejected() {
        let result = SupportedVersionsClientHello::parse(&[
            0x03, // Declared vector length is not divisible by 2.
            0xFE, 0xFC, 0xFF,
        ]);

        assert!(
            matches!(
                result,
                Err(nom::Err::Failure(error))
                    if error.code == nom::error::ErrorKind::LengthValue
            ),
            "odd supported_versions vector should fail with LengthValue"
        );
    }

    #[test]
    fn trailing_client_hello_versions_bytes_are_rejected() {
        let result = SupportedVersionsClientHello::parse(&[
            0x02, // One protocol version.
            0xFE, 0xFC, // DTLS 1.3.
            0xFF, // Extension body has a trailing byte beyond the vector.
        ]);

        assert!(
            matches!(
                result,
                Err(nom::Err::Failure(error))
                    if error.code == nom::error::ErrorKind::LengthValue
            ),
            "trailing supported_versions bytes should fail with LengthValue"
        );
    }

    #[test]
    fn trailing_server_hello_version_bytes_are_rejected() {
        let result = SupportedVersionsServerHello::parse(&[
            0xFE, 0xFC, // DTLS 1.3.
            0xFF, // Extension body has a trailing byte after selected_version.
        ]);

        assert!(
            matches!(
                result,
                Err(nom::Err::Failure(error))
                    if error.code == nom::error::ErrorKind::LengthValue
            ),
            "trailing selected_version bytes should fail with LengthValue"
        );
    }
}
