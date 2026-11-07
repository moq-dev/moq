use super::{CertificateTypeVec, ClientCertificateType, DistinguishedName};
use super::{SignatureAndHashAlgorithm, SignatureAndHashAlgorithmVec};
use crate::buffer::Buf;
use crate::util::{many0, many1};
use arrayvec::ArrayVec;
use nom::Err;
use nom::error::{Error, ErrorKind};
use nom::number::complete::{be_u8, be_u16};
use nom::{IResult, bytes::complete::take};

#[derive(Debug, PartialEq, Eq)]
pub struct CertificateRequest {
    pub certificate_types: CertificateTypeVec,
    pub supported_signature_algorithms: SignatureAndHashAlgorithmVec,
    pub certificate_authorities: ArrayVec<DistinguishedName, 32>,
}

impl CertificateRequest {
    pub fn new(
        certificate_types: CertificateTypeVec,
        supported_signature_algorithms: SignatureAndHashAlgorithmVec,
        certificate_authorities: ArrayVec<DistinguishedName, 32>,
    ) -> Self {
        CertificateRequest {
            certificate_types,
            supported_signature_algorithms,
            certificate_authorities,
        }
    }

    pub fn parse(input: &[u8], base_offset: usize) -> IResult<&[u8], CertificateRequest> {
        let original_input = input;
        let (input, cert_types_len) = be_u8(input)?;
        let (input, input_type) = take(cert_types_len)(input)?;
        let (rest, certificate_types) = many1(
            ClientCertificateType::parse,
            ClientCertificateType::is_supported,
        )(input_type)?;
        if !rest.is_empty() {
            return Err(Err::Failure(Error::new(rest, ErrorKind::LengthValue)));
        }

        let (input, sig_algs_len) = be_u16(input)?;
        let (input, input_sigs) = take(sig_algs_len)(input)?;
        let (rest, supported_signature_algorithms) = many0(
            SignatureAndHashAlgorithm::parse,
            SignatureAndHashAlgorithm::is_supported,
        )(input_sigs)?;
        if !rest.is_empty() {
            return Err(Err::Failure(Error::new(rest, ErrorKind::LengthValue)));
        }

        let (input, cert_auths_len) = be_u16(input)?;
        let (input, input_auths) = take(cert_auths_len)(input)?;

        // Calculate base offset for input_auths within the root buffer
        let auths_base_offset =
            base_offset + (input_auths.as_ptr() as usize - original_input.as_ptr() as usize);

        // Parse certificate authorities manually with dynamic base_offset
        let mut certificate_authorities = ArrayVec::new();
        let mut rest = input_auths;
        while !rest.is_empty() {
            let offset =
                auths_base_offset + (rest.as_ptr() as usize - input_auths.as_ptr() as usize);
            let (new_rest, auth) = DistinguishedName::parse(rest, offset)?;
            certificate_authorities
                .try_push(auth)
                .map_err(|_| Err::Failure(Error::new(rest, ErrorKind::LengthValue)))?;
            rest = new_rest;
        }

        Ok((
            input,
            CertificateRequest {
                certificate_types,
                supported_signature_algorithms,
                certificate_authorities,
            },
        ))
    }

    pub fn serialize(&self, buf: &[u8], output: &mut Buf) {
        output.push(self.certificate_types.len() as u8);
        for cert_type in &self.certificate_types {
            output.push(cert_type.as_u8());
        }

        let sig_algs_len = (self.supported_signature_algorithms.len() * 2) as u16;
        output.extend_from_slice(&sig_algs_len.to_be_bytes());
        for sig_alg in &self.supported_signature_algorithms {
            output.extend_from_slice(&sig_alg.as_u16().to_be_bytes());
        }

        let cert_auths_len: usize = self
            .certificate_authorities
            .iter()
            .map(|name| 2 + name.as_slice(buf).len())
            .sum();
        output.extend_from_slice(&(cert_auths_len as u16).to_be_bytes());
        for name in &self.certificate_authorities {
            let name_data = name.as_slice(buf);
            output.extend_from_slice(&(name_data.len() as u16).to_be_bytes());
            output.extend_from_slice(name_data);
        }
    }

    /// Checks if the CertificateRequest supports a specific hash algorithm
    pub fn supports_hash_algorithm(&self, hash_algorithm: super::HashAlgorithm) -> bool {
        self.supported_signature_algorithms
            .iter()
            .any(|algo| algo.hash == hash_algorithm)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::buffer::Buf;

    // Test message with recognized values:
    // - Certificate type: 0x40 (ECDSA_SIGN)
    // - Signature algorithms: SHA256/ECDSA (0x04, 0x03), SHA256/RSA (0x04, 0x01)
    const MESSAGE: &[u8] = &[
        0x01, // Certificate types length (1 byte)
        0x40, // Certificate type: ECDSA_SIGN
        0x00, 0x04, // Signature algorithms length (4 bytes = 2 algorithms)
        0x04, 0x03, // SHA256/ECDSA
        0x04, 0x01, // SHA256/RSA
        0x00, 0x0C, // Certificate authorities length
        0x00, 0x04, // Distinguished name 1 length
        0x01, 0x02, 0x03, 0x04, // Distinguished name 1 data
        0x00, 0x04, // Distinguished name 2 length
        0x05, 0x06, 0x07, 0x08, // Distinguished name 2 data
    ];

    #[test]
    fn filters_unsupported_rsa_signature_algorithm() {
        // Parse the message with base_offset 0
        let (rest, parsed) = CertificateRequest::parse(MESSAGE, 0).unwrap();
        assert!(rest.is_empty());
        assert_eq!(parsed.supported_signature_algorithms.len(), 1);
        assert_eq!(
            parsed.supported_signature_algorithms[0],
            SignatureAndHashAlgorithm::new(
                super::super::HashAlgorithm::SHA256,
                super::super::SignatureAlgorithm::ECDSA,
            )
        );

        // Serialization must not re-advertise the filtered RSA algorithm.
        let mut serialized = Buf::new();
        parsed.serialize(MESSAGE, &mut serialized);
        let expected = [
            0x01, // Certificate types length (1 byte)
            0x40, // Certificate type: ECDSA_SIGN
            0x00, 0x02, // Signature algorithms length (2 bytes = 1 algorithm)
            0x04, 0x03, // SHA256/ECDSA
            0x00, 0x0C, // Certificate authorities length
            0x00, 0x04, // Distinguished name 1 length
            0x01, 0x02, 0x03, 0x04, // Distinguished name 1 data
            0x00, 0x04, // Distinguished name 2 length
            0x05, 0x06, 0x07, 0x08, // Distinguished name 2 data
        ];
        assert_eq!(&*serialized, expected);
    }

    #[test]
    fn filters_unsupported_types() {
        // Message with unsupported certificate types and signature algorithms
        let message_with_unsupported: &[u8] = &[
            0x03, // Certificate types length (3 bytes)
            0x01, // RSA_SIGN (unsupported, filtered)
            0x40, // ECDSA_SIGN (supported)
            0x02, // DSS_SIGN (unsupported, filtered)
            0x00, 0x06, // Signature algorithms length (6 bytes = 3 algorithms)
            0x05, 0x02, // SHA384/DSA (unsupported)
            0x04, 0x03, // SHA256/ECDSA (supported)
            0x01, 0x01, // MD5/RSA (unsupported)
            0x00, 0x00, // Certificate authorities length (0)
        ];

        let (rest, parsed) = CertificateRequest::parse(message_with_unsupported, 0).unwrap();
        assert!(rest.is_empty());

        // Only supported types should remain
        assert_eq!(parsed.certificate_types.len(), 1);
        assert_eq!(
            parsed.certificate_types[0],
            ClientCertificateType::ECDSA_SIGN
        );

        assert_eq!(parsed.supported_signature_algorithms.len(), 1);
        assert_eq!(
            parsed.supported_signature_algorithms[0],
            SignatureAndHashAlgorithm::new(
                super::super::HashAlgorithm::SHA256,
                super::super::SignatureAlgorithm::ECDSA
            )
        );
    }

    #[test]
    fn too_many_certificate_authorities_are_rejected() {
        let mut message = Vec::new();
        message.push(1); // Certificate types length
        message.push(ClientCertificateType::ECDSA_SIGN.as_u8());
        message.extend_from_slice(&0u16.to_be_bytes()); // Signature algorithms length

        let count = 33;
        let cert_auths_len = count * 3;
        message.extend_from_slice(&(cert_auths_len as u16).to_be_bytes());
        for _ in 0..count {
            message.extend_from_slice(&1u16.to_be_bytes());
            message.push(0xAA);
        }

        let result = CertificateRequest::parse(&message, 0);
        assert!(
            matches!(
                result,
                Err(nom::Err::Failure(error))
                    if error.code == nom::error::ErrorKind::LengthValue
            ),
            "too many certificate authorities should fail with LengthValue"
        );
    }
}
