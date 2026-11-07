use super::{Extension, ExtensionType, SrtpProfileId, UseSrtpExtension};
use crate::buffer::Buf;
use arrayvec::ArrayVec;
use nom::Err;
use nom::IResult;
use nom::bytes::complete::take;
use nom::error::{Error, ErrorKind};
use nom::number::complete::be_u16;

/// EncryptedExtensions message (RFC 8446 Section 4.3.1).
#[derive(Debug, PartialEq, Eq)]
pub struct EncryptedExtensions {
    pub extensions: ArrayVec<Extension, 5>,
    // Receive-side values; outbound serialization uses the raw extensions above.
    srtp_profiles: Option<ArrayVec<SrtpProfileId, 3>>,
}

impl EncryptedExtensions {
    pub fn parse(input: &[u8], base_offset: usize) -> IResult<&[u8], EncryptedExtensions> {
        let original_input = input;
        let mut srtp_profiles = None;
        let (input, extensions_len) = be_u16(input)?;
        let (input, extensions_data) = take(extensions_len)(input)?;

        let data_base_offset =
            base_offset + (extensions_data.as_ptr() as usize - original_input.as_ptr() as usize);

        let mut extensions = ArrayVec::new();
        let mut rest = extensions_data;
        let mut current_offset = data_base_offset;
        while !rest.is_empty() {
            let before_len = rest.len();
            let (new_rest, ext) = Extension::parse(rest, current_offset)?;
            let parsed_len = before_len - new_rest.len();
            current_offset += parsed_len;

            if ext.extension_type.is_supported() {
                if extensions
                    .iter()
                    .any(|existing: &Extension| existing.extension_type == ext.extension_type)
                {
                    return Err(Err::Failure(Error::new(rest, ErrorKind::LengthValue)));
                }
                if ext.extension_type == ExtensionType::UseSrtp {
                    let range = &ext.extension_data_range;
                    let data = &original_input[range.start - base_offset..range.end - base_offset];
                    srtp_profiles = Some(UseSrtpExtension::parse(data)?.1.profiles);
                }
                extensions
                    .try_push(ext)
                    .map_err(|_| Err::Failure(Error::new(rest, ErrorKind::LengthValue)))?;
            }
            rest = new_rest;
        }

        Ok((
            input,
            EncryptedExtensions {
                extensions,
                srtp_profiles,
            },
        ))
    }

    #[cfg(test)]
    pub fn new(extensions: ArrayVec<Extension, 5>) -> Self {
        Self {
            extensions,
            srtp_profiles: None,
        }
    }

    pub fn srtp_profiles(&self) -> Option<&ArrayVec<SrtpProfileId, 3>> {
        self.srtp_profiles.as_ref()
    }

    pub fn serialize(&self, buf: &[u8], output: &mut Buf) {
        let mut extensions_len = 0usize;
        for ext in &self.extensions {
            let ext_data = ext.extension_data(buf);
            extensions_len += 4 + ext_data.len();
        }

        output.extend_from_slice(&(extensions_len as u16).to_be_bytes());

        for ext in &self.extensions {
            ext.serialize(buf, output);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::ExtensionType;
    use super::*;
    use crate::buffer::Buf;

    const MESSAGE: &[u8] = &[
        0x00, 0x0C, // Extensions length (12)
        0x00, 0x0A, // ExtensionType::SupportedGroups
        0x00, 0x08, // Extension data length
        0x00, 0x06, 0x00, 0x17, 0x00, 0x18, 0x00, 0x19, // Extension data
    ];

    #[test]
    fn roundtrip() {
        let (rest, parsed) = EncryptedExtensions::parse(MESSAGE, 0).unwrap();
        assert!(rest.is_empty());

        let mut serialized = Buf::new();
        parsed.serialize(MESSAGE, &mut serialized);
        assert_eq!(&*serialized, MESSAGE);
    }

    #[test]
    fn duplicate_supported_extensions_are_rejected() {
        let mut message = Vec::new();
        let count = 2;
        message.extend_from_slice(&(count as u16 * 4).to_be_bytes());
        for _ in 0..count {
            message.extend_from_slice(&ExtensionType::Cookie.as_u16().to_be_bytes());
            message.extend_from_slice(&0u16.to_be_bytes());
        }

        let result = EncryptedExtensions::parse(&message, 0);
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
        let mut message = Vec::new();
        message.extend_from_slice(&(extensions.len() as u16).to_be_bytes());
        message.extend_from_slice(&extensions);

        let result = EncryptedExtensions::parse(&message, 0);
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
