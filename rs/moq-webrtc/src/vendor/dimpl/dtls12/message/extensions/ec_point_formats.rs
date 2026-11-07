use crate::buffer::Buf;
use arrayvec::ArrayVec;
use nom::bytes::complete::take;
use nom::error::{Error, ErrorKind};
use nom::{Err, IResult, number::complete::be_u8};

/// EC Point Format as defined in RFC 4492 Section 5.1.2
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(unused)]
pub enum ECPointFormat {
    #[default]
    Uncompressed = 0x00,
    AnsiX962CompressedPrime = 0x01,
    AnsiX962CompressedChar2 = 0x02,
}

impl ECPointFormat {
    #[allow(unused)]
    pub fn parse(input: &[u8]) -> IResult<&[u8], ECPointFormat> {
        let (input, value) = be_u8(input)?;
        let format = match value {
            0x00 => ECPointFormat::Uncompressed,
            0x01 => ECPointFormat::AnsiX962CompressedPrime,
            0x02 => ECPointFormat::AnsiX962CompressedChar2,
            _ => {
                return Err(nom::Err::Error(nom::error::Error::new(
                    input,
                    nom::error::ErrorKind::Switch,
                )));
            }
        };
        Ok((input, format))
    }

    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
}

/// ECPointFormats extension as defined in RFC 4492
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ECPointFormatsExtension {
    pub formats: ArrayVec<ECPointFormat, 3>,
}

impl ECPointFormatsExtension {
    /// Create a default ECPointFormatsExtension with standard formats
    pub fn default() -> Self {
        let mut formats = ArrayVec::new();
        // Most implementations only support uncompressed format
        formats.push(ECPointFormat::Uncompressed);

        ECPointFormatsExtension { formats }
    }

    #[allow(unused)]
    pub fn parse(input: &[u8]) -> IResult<&[u8], ECPointFormatsExtension> {
        let (input, list_len) = be_u8(input)?;
        if list_len == 0 {
            return Err(Err::Failure(Error::new(input, ErrorKind::LengthValue)));
        }
        let (input, formats_data) = take(list_len)(input)?;
        if !input.is_empty() {
            return Err(Err::Failure(Error::new(input, ErrorKind::LengthValue)));
        }

        let mut formats = ArrayVec::new();
        let mut current_input = formats_data;

        while !current_input.is_empty() {
            let format_input = current_input;
            let (rest, format) = be_u8(current_input)?;
            let Some(format) = (match format {
                0x00 => Some(ECPointFormat::Uncompressed),
                0x01 => Some(ECPointFormat::AnsiX962CompressedPrime),
                0x02 => Some(ECPointFormat::AnsiX962CompressedChar2),
                _ => None,
            }) else {
                current_input = rest;
                continue;
            };

            formats
                .try_push(format)
                .map_err(|_| Err::Failure(Error::new(format_input, ErrorKind::LengthValue)))?;
            current_input = rest;
        }

        Ok((current_input, ECPointFormatsExtension { formats }))
    }

    pub fn serialize(&self, output: &mut Buf) {
        // Write the number of formats
        output.push(self.formats.len() as u8);

        // Write each format
        for format in &self.formats {
            output.push(format.as_u8());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Buf;

    #[test]
    fn test_ec_point_formats_extension() {
        let mut formats = ArrayVec::new();
        formats.push(ECPointFormat::Uncompressed);
        formats.push(ECPointFormat::AnsiX962CompressedPrime);

        let ext = ECPointFormatsExtension { formats };

        let mut serialized = Buf::new();
        ext.serialize(&mut serialized);

        let expected = [
            0x02, // Number of formats (2)
            0x00, // Uncompressed (0x00)
            0x01, // ANSI X9.62 compressed prime (0x01)
        ];

        assert_eq!(&*serialized, expected);

        let (_, parsed) = ECPointFormatsExtension::parse(&serialized).unwrap();

        assert_eq!(parsed.formats.as_slice(), ext.formats.as_slice());
    }

    #[test]
    fn too_many_ec_point_formats_are_rejected() {
        let bytes = [
            0x04, // Four point formats.
            0x00, // Uncompressed.
            0x00, // Uncompressed.
            0x00, // Uncompressed.
            0x00, // Uncompressed.
        ];

        let err = ECPointFormatsExtension::parse(&bytes).unwrap_err();

        assert!(matches!(
            err,
            Err::Failure(Error {
                code: ErrorKind::LengthValue,
                ..
            })
        ));
    }

    #[test]
    fn unknown_ec_point_formats_are_ignored() {
        let (rest, parsed) = ECPointFormatsExtension::parse(&[
            0x03, // Three point formats.
            0x02, // ANSI X9.62 compressed char2.
            0x00, // Uncompressed.
            0xFF, // Unknown/private point format.
        ])
        .unwrap();

        assert!(rest.is_empty());
        assert_eq!(
            parsed.formats.as_slice(),
            &[
                ECPointFormat::AnsiX962CompressedChar2,
                ECPointFormat::Uncompressed,
            ]
        );
    }

    #[test]
    fn trailing_ec_point_format_bytes_are_rejected() {
        let result = ECPointFormatsExtension::parse(&[
            0x01, // One point format.
            0x00, // Uncompressed.
            0xFF, // Extension body has a trailing byte beyond the vector.
        ]);

        assert!(matches!(
            result,
            Err(Err::Failure(Error {
                code: ErrorKind::LengthValue,
                ..
            }))
        ));
    }
}
