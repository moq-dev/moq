use std::ops::Range;

use nom::bytes::complete::take;
use nom::error::{Error, ErrorKind};
use nom::number::complete::{be_u8, be_u16};
use nom::{Err, IResult};

use super::{DistinguishedName, Extension, ExtensionType, SignatureAlgorithmsExtension};
use crate::buffer::Buf;

/// Parsed CertificateRequest framing. Context and serialized bytes stay in the input buffer.
#[derive(Debug, PartialEq, Eq)]
pub struct CertificateRequest {
    body: Range<usize>,
    context: Range<usize>,
}

impl CertificateRequest {
    pub fn parse(input: &[u8], base_offset: usize) -> IResult<&[u8], Self> {
        let original = input;
        let (input, context_len) = be_u8(input)?;
        let (input, _) = take(context_len)(input)?;
        let context = base_offset + 1..base_offset + 1 + context_len as usize;
        let (input, extensions_len) = be_u16(input)?;
        if extensions_len < 2 {
            return Err(Err::Failure(Error::new(input, ErrorKind::LengthValue)));
        }
        let (rest, mut extensions) = take(extensions_len)(input)?;
        if !rest.is_empty() {
            return Err(Err::Failure(Error::new(rest, ErrorKind::LengthValue)));
        }
        let mut seen_signatures = false;
        let mut seen_authorities = false;
        while !extensions.is_empty() {
            let offset = base_offset + original.len() - extensions.len();
            let (remaining, extension) = Extension::parse(extensions, offset)?;
            let range = &extension.extension_data_range;
            let data = &original[range.start - base_offset..range.end - base_offset];
            match extension.extension_type {
                ExtensionType::SignatureAlgorithms => {
                    if seen_signatures {
                        return Err(Err::Failure(Error::new(data, ErrorKind::LengthValue)));
                    }
                    seen_signatures = true;
                    SignatureAlgorithmsExtension::parse(data)?;
                }
                ExtensionType::CertificateAuthorities => {
                    if seen_authorities {
                        return Err(Err::Failure(Error::new(data, ErrorKind::LengthValue)));
                    }
                    seen_authorities = true;
                    let (names, length) = be_u16(data)?;
                    if length == 0 || names.len() != length as usize {
                        return Err(Err::Failure(Error::new(names, ErrorKind::LengthValue)));
                    }
                    let mut names = names;
                    while !names.is_empty() {
                        let offset = range.end - names.len();
                        names = DistinguishedName::parse(names, offset)?.0;
                    }
                }
                _ => {}
            }
            extensions = remaining;
        }
        Ok((
            rest,
            Self {
                body: base_offset..base_offset + original.len(),
                context,
            },
        ))
    }

    pub fn context<'a>(&self, buffer: &'a [u8]) -> &'a [u8] {
        &buffer[self.context.clone()]
    }

    pub fn serialize(&self, buffer: &[u8], output: &mut Buf) {
        output.extend_from_slice(&buffer[self.body.clone()]);
    }
}
