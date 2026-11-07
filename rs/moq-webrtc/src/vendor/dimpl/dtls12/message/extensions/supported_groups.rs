use super::super::{NamedGroup, NamedGroupVec};
use crate::buffer::Buf;
use nom::Err;
use nom::IResult;
use nom::bytes::complete::take;
use nom::error::{Error, ErrorKind};

/// SupportedGroups extension as defined in RFC 8422
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupportedGroupsExtension {
    pub groups: NamedGroupVec,
}

impl SupportedGroupsExtension {
    pub fn parse(input: &[u8]) -> IResult<&[u8], SupportedGroupsExtension> {
        let (input, list_len) = nom::number::complete::be_u16(input)?;
        if list_len == 0 || list_len % 2 != 0 {
            return Err(Err::Failure(Error::new(input, ErrorKind::LengthValue)));
        }

        let (input, mut current_input) = take(list_len)(input)?;
        if !input.is_empty() {
            return Err(Err::Failure(Error::new(input, ErrorKind::LengthValue)));
        }

        let mut groups = NamedGroupVec::new();

        // Parse groups; only include supported groups (skip Unknown and unsupported)
        while !current_input.is_empty() {
            let group_input = current_input;
            let (rest, group) = NamedGroup::parse(group_input)?;
            current_input = rest;
            // Only add supported groups
            if group.is_supported() {
                groups
                    .try_push(group)
                    .map_err(|_| Err::Failure(Error::new(group_input, ErrorKind::LengthValue)))?;
            }
        }

        Ok((input, SupportedGroupsExtension { groups }))
    }

    pub fn serialize(&self, output: &mut Buf) {
        // Write the total length of all groups (2 bytes per group)
        output.extend_from_slice(&((self.groups.len() * 2) as u16).to_be_bytes());

        // Write each group
        for group in &self.groups {
            output.extend_from_slice(&group.as_u16().to_be_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Buf;

    #[test]
    fn test_supported_groups_extension() {
        let mut groups = NamedGroupVec::new();
        groups.push(NamedGroup::X25519);
        groups.push(NamedGroup::Secp256r1);

        let ext = SupportedGroupsExtension { groups };

        let mut serialized = Buf::new();
        ext.serialize(&mut serialized);

        let expected = [
            0x00, 0x04, // Groups length (4 bytes)
            0x00, 0x1D, // X25519 (0x001D)
            0x00, 0x17, // secp256r1 (0x0017)
        ];

        assert_eq!(&*serialized, expected);

        let (_, parsed) = SupportedGroupsExtension::parse(&serialized).unwrap();

        assert_eq!(parsed.groups.as_slice(), ext.groups.as_slice());
    }

    #[test]
    fn test_supported_groups_parse_provided_bytes() {
        // Provided bytes: [0,10,0,29,0,23,0,24,1,0,1,1]
        // Meaning:
        // 0x000A -> list length = 10 bytes (5 groups)
        // groups: 0x001D (X25519), 0x0017 (P-256), 0x0018 (P-384), 0x0100 (unknown), 0x0101 (unknown)
        let bytes = [0, 10, 0, 29, 0, 23, 0, 24, 1, 0, 1, 1];

        let (rest, parsed) =
            SupportedGroupsExtension::parse(&bytes).expect("parse SupportedGroups");
        assert!(rest.is_empty());

        // Expect only the supported groups in order as they appear
        assert_eq!(
            parsed.groups.as_slice(),
            &[
                NamedGroup::X25519,
                NamedGroup::Secp256r1,
                NamedGroup::Secp384r1
            ]
        );
    }

    #[test]
    fn too_many_supported_groups_are_rejected() {
        let mut bytes = Vec::new();
        let count = NamedGroup::supported().len() + 1;
        bytes.extend_from_slice(&(count as u16 * 2).to_be_bytes());
        for _ in 0..count {
            bytes.extend_from_slice(&NamedGroup::X25519.as_u16().to_be_bytes());
        }

        let result = SupportedGroupsExtension::parse(&bytes);
        assert!(
            matches!(
                result,
                Err(nom::Err::Failure(error))
                    if error.code == nom::error::ErrorKind::LengthValue
            ),
            "too many supported groups should fail with LengthValue"
        );
    }

    #[test]
    fn odd_supported_groups_vector_is_rejected() {
        let result = SupportedGroupsExtension::parse(&[
            0x00, 0x03, // Declared vector length is not divisible by 2.
            0x00, 0x1D, 0xFF,
        ]);

        assert!(
            matches!(
                result,
                Err(nom::Err::Failure(error))
                    if error.code == nom::error::ErrorKind::LengthValue
            ),
            "odd supported_groups vector should fail with LengthValue"
        );
    }

    #[test]
    fn trailing_supported_groups_bytes_are_rejected() {
        let result = SupportedGroupsExtension::parse(&[
            0x00, 0x02, // One named group.
            0x00, 0x1D, // X25519.
            0xFF, // Extension body has a trailing byte beyond the vector.
        ]);

        assert!(
            matches!(
                result,
                Err(nom::Err::Failure(error))
                    if error.code == nom::error::ErrorKind::LengthValue
            ),
            "trailing supported_groups bytes should fail with LengthValue"
        );
    }
}
