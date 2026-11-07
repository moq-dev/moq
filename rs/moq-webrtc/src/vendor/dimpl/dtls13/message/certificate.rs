use super::Asn1Cert;
use crate::buffer::Buf;
use arrayvec::ArrayVec;
use nom::Err;
use nom::IResult;
use nom::bytes::complete::take;
use nom::error::{Error, ErrorKind};
use nom::number::complete::{be_u8, be_u16, be_u24};
use std::ops::Range;

/// TLS 1.3 CertificateEntry (RFC 8446 Section 4.4.2).
#[derive(Debug, PartialEq, Eq)]
pub struct CertificateEntry {
    pub cert: Asn1Cert,
    pub extensions_range: Range<usize>,
}

/// TLS 1.3 Certificate message (RFC 8446 Section 4.4.2).
#[derive(Debug, PartialEq, Eq)]
pub struct Certificate {
    pub context_range: Range<usize>,
    pub certificate_list: ArrayVec<CertificateEntry, 32>,
}

impl Certificate {
    pub fn parse(input: &[u8], base_offset: usize) -> IResult<&[u8], Certificate> {
        let original_input = input;

        // certificate_request_context<0..255>
        let (input, context_len) = be_u8(input)?;
        let (input, context_slice) = take(context_len)(input)?;
        let context_relative = context_slice.as_ptr() as usize - original_input.as_ptr() as usize;
        let context_range = (base_offset + context_relative)
            ..(base_offset + context_relative + context_slice.len());

        // certificate_list<0..2^24-1>
        let (input, total_len) = be_u24(input)?;
        let (input, certs_data) = take(total_len)(input)?;

        let certs_base_offset =
            base_offset + (certs_data.as_ptr() as usize - original_input.as_ptr() as usize);

        let mut certificate_list = ArrayVec::new();
        let mut rest = certs_data;
        while !rest.is_empty() {
            let entry_base =
                certs_base_offset + (rest.as_ptr() as usize - certs_data.as_ptr() as usize);

            // cert_data<1..2^24-1>
            let (r, cert) = Asn1Cert::parse(rest, entry_base)?;

            // extensions<0..2^16-1>
            let (r, ext_len) = be_u16(r)?;
            let (r, ext_slice) = take(ext_len)(r)?;
            let ext_relative = ext_slice.as_ptr() as usize - certs_data.as_ptr() as usize;
            let extensions_range = (certs_base_offset + ext_relative)
                ..(certs_base_offset + ext_relative + ext_slice.len());

            certificate_list
                .try_push(CertificateEntry {
                    cert,
                    extensions_range,
                })
                .map_err(|_| Err::Failure(Error::new(rest, ErrorKind::LengthValue)))?;
            rest = r;
        }

        Ok((
            input,
            Certificate {
                context_range,
                certificate_list,
            },
        ))
    }

    pub fn serialize(&self, buf: &[u8], output: &mut Buf) {
        // certificate_request_context
        let context = &buf[self.context_range.clone()];
        output.push(context.len() as u8);
        output.extend_from_slice(context);

        // Calculate total certificate_list length
        let total_len: usize = self
            .certificate_list
            .iter()
            .map(|entry| {
                let cert_data = entry.cert.as_slice(buf);
                let ext_data = &buf[entry.extensions_range.clone()];
                3 + cert_data.len() + 2 + ext_data.len()
            })
            .sum();
        output.extend_from_slice(&(total_len as u32).to_be_bytes()[1..]);

        for entry in &self.certificate_list {
            let cert_data = entry.cert.as_slice(buf);
            output.extend_from_slice(&(cert_data.len() as u32).to_be_bytes()[1..]);
            output.extend_from_slice(cert_data);

            let ext_data = &buf[entry.extensions_range.clone()];
            output.extend_from_slice(&(ext_data.len() as u16).to_be_bytes());
            output.extend_from_slice(ext_data);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Buf;

    const MESSAGE: &[u8] = &[
        0x02, // context length
        0xAA, 0xBB, // context bytes
        0x00, 0x00, 0x12, // total certificate_list length (18)
        // CertificateEntry 1
        0x00, 0x00, 0x04, // cert_data length
        0x01, 0x02, 0x03, 0x04, // cert_data
        0x00, 0x00, // extensions length (0)
        // CertificateEntry 2
        0x00, 0x00, 0x02, // cert_data length
        0x05, 0x06, // cert_data
        0x00, 0x02, // extensions length (2)
        0xDE, 0xAD, // extension bytes (opaque)
    ];

    #[test]
    fn roundtrip() {
        let (rest, parsed) = Certificate::parse(MESSAGE, 0).unwrap();
        assert!(rest.is_empty());

        let mut serialized = Buf::new();
        parsed.serialize(MESSAGE, &mut serialized);
        assert_eq!(&*serialized, MESSAGE);
    }

    #[test]
    fn rejects_too_many_certificates() {
        let mut message = Vec::new();
        message.push(0x00); // empty certificate_request_context
        let total_len = 33 * 6;
        message.extend_from_slice(&(total_len as u32).to_be_bytes()[1..]);
        for _ in 0..33 {
            message.extend_from_slice(&[0x00, 0x00, 0x01, 0xAA]); // cert_data
            message.extend_from_slice(&[0x00, 0x00]); // empty extensions
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
