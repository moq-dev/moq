use super::DigitallySigned;
use crate::buffer::Buf;
use nom::IResult;

#[derive(Debug, PartialEq, Eq)]
pub struct CertificateVerify {
    pub signed: DigitallySigned,
}

impl CertificateVerify {
    pub fn parse(input: &[u8], base_offset: usize) -> IResult<&[u8], CertificateVerify> {
        let (input, signed) = DigitallySigned::parse(input, base_offset)?;
        Ok((input, CertificateVerify { signed }))
    }

    pub fn serialize(&self, buf: &[u8], output: &mut Buf) {
        self.signed.serialize(buf, output);
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::buffer::Buf;

    const MESSAGE: &[u8] = &[
        0x04, 0x01, // SignatureAndHashAlgorithm (SHA256 + RSA)
        0x00, 0x04, // Signature length
        0x01, 0x02, 0x03, 0x04, // Signature data
    ];

    #[test]
    fn roundtrip() {
        // Parse the message with base_offset 0
        let (rest, parsed) = CertificateVerify::parse(MESSAGE, 0).unwrap();
        assert!(rest.is_empty());

        // Serialize and compare to MESSAGE
        let mut serialized = Buf::new();
        parsed.serialize(MESSAGE, &mut serialized);
        assert_eq!(&*serialized, MESSAGE);
    }
}
