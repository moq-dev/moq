use crate::buffer::Buf;
use crate::types::SignatureScheme;
use nom::number::complete::be_u16;
use nom::{IResult, bytes::complete::take};
use std::ops::Range;

#[derive(Debug, PartialEq, Eq)]
pub struct DigitallySigned {
    pub scheme: SignatureScheme,
    pub signature_range: Range<usize>,
}

impl DigitallySigned {
    pub fn signature<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        &buf[self.signature_range.clone()]
    }

    pub fn parse(input: &[u8], base_offset: usize) -> IResult<&[u8], DigitallySigned> {
        let original_input = input;
        let (rest, scheme) = SignatureScheme::parse(input)?;
        let (rest, signature_len) = be_u16(rest)?;
        let (rest, signature_slice) = take(signature_len)(rest)?;

        // Calculate absolute range in root buffer
        let relative_offset = signature_slice.as_ptr() as usize - original_input.as_ptr() as usize;
        let start = base_offset + relative_offset;
        let end = start + signature_slice.len();

        Ok((
            rest,
            DigitallySigned {
                scheme,
                signature_range: start..end,
            },
        ))
    }

    pub fn serialize(&self, buf: &[u8], output: &mut Buf) {
        output.extend_from_slice(&self.scheme.as_u16().to_be_bytes());
        let signature = self.signature(buf);
        output.extend_from_slice(&(signature.len() as u16).to_be_bytes());
        output.extend_from_slice(signature);
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::buffer::Buf;

    const MESSAGE: &[u8] = &[
        0x04, 0x03, // SignatureScheme::ECDSA_SECP256R1_SHA256
        0x00, 0x04, // Signature length
        0x01, 0x02, 0x03, 0x04, // Signature data
    ];

    #[test]
    fn roundtrip() {
        // Parse the message with base_offset 0
        let (rest, parsed) = DigitallySigned::parse(MESSAGE, 0).unwrap();
        assert!(rest.is_empty());

        // Serialize and compare to MESSAGE
        let mut serialized = Buf::new();
        parsed.serialize(MESSAGE, &mut serialized);
        assert_eq!(&*serialized, MESSAGE);
    }
}
