use super::Asn1Cert;
use crate::buffer::Buf;
use arrayvec::ArrayVec;
use nom::Err;
use nom::bytes::complete::take;
use nom::error::{Error, ErrorKind};
use nom::{IResult, number::complete::be_u24};

#[derive(Debug, PartialEq, Eq)]
pub struct Certificate {
    pub certificate_list: ArrayVec<Asn1Cert, 32>,
}

impl Certificate {
    pub fn new(certificate_list: ArrayVec<Asn1Cert, 32>) -> Self {
        Certificate { certificate_list }
    }

    pub fn parse(input: &[u8], base_offset: usize) -> IResult<&[u8], Certificate> {
        let original_input = input;
        let (input, total_len) = be_u24(input)?;
        let (input, certs_data) = take(total_len)(input)?;

        // Calculate base offset for certs_data within the root buffer
        let certs_base_offset =
            base_offset + (certs_data.as_ptr() as usize - original_input.as_ptr() as usize);

        // Parse certificates manually with dynamic base_offset
        let mut certificate_list = ArrayVec::new();
        let mut rest = certs_data;
        while !rest.is_empty() {
            let offset =
                certs_base_offset + (rest.as_ptr() as usize - certs_data.as_ptr() as usize);
            let (new_rest, cert) = Asn1Cert::parse(rest, offset)?;
            certificate_list
                .try_push(cert)
                .map_err(|_| Err::Failure(Error::new(rest, ErrorKind::LengthValue)))?;
            rest = new_rest;
        }

        Ok((input, Certificate { certificate_list }))
    }

    pub fn serialize(&self, buf: &[u8], output: &mut Buf) {
        let total_len: usize = self
            .certificate_list
            .iter()
            .map(|cert| 3 + cert.as_slice(buf).len())
            .sum();
        output.extend_from_slice(&(total_len as u32).to_be_bytes()[1..]);

        for cert in &self.certificate_list {
            let cert_data = cert.as_slice(buf);
            output.extend_from_slice(&(cert_data.len() as u32).to_be_bytes()[1..]);
            output.extend_from_slice(cert_data);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Buf;

    const MESSAGE: &[u8] = &[
        0x00, 0x00, 0x0C, // Total length
        0x00, 0x00, 0x04, // Certificate 1 length
        0x01, 0x02, 0x03, 0x04, // Certificate 1 data
        0x00, 0x00, 0x02, // Certificate 2 length
        0x05, 0x06, // Certificate 2 data
    ];

    #[test]
    fn roundtrip() {
        // Parse the message with base_offset 0
        let (rest, parsed) = Certificate::parse(MESSAGE, 0).unwrap();
        assert!(rest.is_empty());

        // Serialize and compare to MESSAGE
        let mut serialized = Buf::new();
        parsed.serialize(MESSAGE, &mut serialized);
        assert_eq!(&*serialized, MESSAGE);
    }

    #[test]
    fn rejects_too_many_certificates() {
        let mut message = Vec::new();
        let total_len = 33 * 4;
        message.extend_from_slice(&(total_len as u32).to_be_bytes()[1..]);
        for _ in 0..33 {
            message.extend_from_slice(&[0x00, 0x00, 0x01, 0xAA]);
        }

        let result = Certificate::parse(&message, 0);
        assert!(
            matches!(
                result,
                Err(nom::Err::Failure(error))
                    if error.code == nom::error::ErrorKind::LengthValue
            ),
            "oversized certificate list should fail with LengthValue"
        );
    }
}
