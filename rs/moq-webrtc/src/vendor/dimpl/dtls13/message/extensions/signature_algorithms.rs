use crate::buffer::Buf;
use crate::types::SignatureScheme;
use arrayvec::ArrayVec;
use nom::Err;
use nom::IResult;
use nom::bytes::complete::take;
use nom::error::{Error, ErrorKind};

/// SignatureAlgorithms extension for TLS 1.3 (RFC 8446 Section 4.2.3).
///
/// Uses `SignatureScheme` (u16) instead of the TLS 1.2 `SignatureAndHashAlgorithm`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureAlgorithmsExtension {
    pub supported_signature_algorithms: ArrayVec<SignatureScheme, 2>,
}

impl SignatureAlgorithmsExtension {
    /// Create a default SignatureAlgorithmsExtension with supported schemes.
    pub fn default() -> Self {
        SignatureAlgorithmsExtension {
            supported_signature_algorithms: SignatureScheme::supported(),
        }
    }

    pub fn parse(input: &[u8]) -> IResult<&[u8], SignatureAlgorithmsExtension> {
        let (input, list_len) = nom::number::complete::be_u16(input)?;
        if list_len == 0 || list_len % 2 != 0 {
            return Err(Err::Failure(Error::new(input, ErrorKind::LengthValue)));
        }

        let mut algorithms: ArrayVec<SignatureScheme, 2> = ArrayVec::new();
        let (input, mut current_input) = take(list_len)(input)?;
        if !input.is_empty() {
            return Err(Err::Failure(Error::new(input, ErrorKind::LengthValue)));
        }

        while !current_input.is_empty() {
            let (rest, scheme) = SignatureScheme::parse(current_input)?;
            if scheme.is_supported() {
                algorithms
                    .try_push(scheme)
                    .map_err(|_| Err::Failure(Error::new(current_input, ErrorKind::LengthValue)))?;
            }
            current_input = rest;
        }

        Ok((
            input,
            SignatureAlgorithmsExtension {
                supported_signature_algorithms: algorithms,
            },
        ))
    }

    pub fn serialize(&self, output: &mut Buf) {
        output.extend_from_slice(
            &((self.supported_signature_algorithms.len() * 2) as u16).to_be_bytes(),
        );

        for scheme in &self.supported_signature_algorithms {
            output.extend_from_slice(&scheme.as_u16().to_be_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signature_algorithms_extension() {
        let mut algorithms: ArrayVec<SignatureScheme, 2> = ArrayVec::new();
        algorithms.push(SignatureScheme::ECDSA_SECP256R1_SHA256);
        algorithms.push(SignatureScheme::ECDSA_SECP384R1_SHA384);

        let ext = SignatureAlgorithmsExtension {
            supported_signature_algorithms: algorithms.clone(),
        };

        let mut serialized = Buf::new();
        ext.serialize(&mut serialized);

        let expected = [
            0x00, 0x04, // Length (4 bytes)
            0x04, 0x03, // ECDSA_SECP256R1_SHA256
            0x05, 0x03, // ECDSA_SECP384R1_SHA384
        ];

        assert_eq!(&*serialized, expected);

        let (_, parsed) = SignatureAlgorithmsExtension::parse(&serialized).unwrap();

        assert_eq!(parsed.supported_signature_algorithms, algorithms);
    }

    #[test]
    fn capacity_matches_supported() {
        let ext = SignatureAlgorithmsExtension::default();
        assert_eq!(
            ext.supported_signature_algorithms.capacity(),
            SignatureScheme::supported().len(),
            "SignatureAlgorithmsExtension capacity must match supported schemes count"
        );
    }

    #[test]
    fn too_many_supported_signature_algorithms_are_rejected() {
        let mut bytes = Vec::new();
        let supported = SignatureScheme::supported();
        let count = supported.len() + 1;
        bytes.extend_from_slice(&(count as u16 * 2).to_be_bytes());
        for _ in 0..count {
            bytes.extend_from_slice(
                &SignatureScheme::ECDSA_SECP256R1_SHA256
                    .as_u16()
                    .to_be_bytes(),
            );
        }

        let result = SignatureAlgorithmsExtension::parse(&bytes);
        assert!(
            matches!(
                result,
                Err(nom::Err::Failure(error))
                    if error.code == nom::error::ErrorKind::LengthValue
            ),
            "too many supported signature algorithms should fail with LengthValue"
        );
    }

    #[test]
    fn odd_length_signature_algorithms_are_rejected_without_panic() {
        let result = std::panic::catch_unwind(|| {
            SignatureAlgorithmsExtension::parse(&[
                0x00, 0x01, // Declared list length is not divisible by 2.
                0x04, 0x03, // Extra byte proves the parser must reject before looping.
            ])
        });

        assert!(
            result.is_ok(),
            "odd signature_algorithms length must not panic"
        );
        assert!(
            matches!(
                result.unwrap(),
                Err(nom::Err::Failure(error))
                    if error.code == nom::error::ErrorKind::LengthValue
            ),
            "odd signature_algorithms length should fail with LengthValue"
        );
    }

    #[test]
    fn empty_signature_algorithms_are_rejected() {
        let result = SignatureAlgorithmsExtension::parse(&[
            0x00, 0x00, // Empty signature_algorithms vector is invalid.
        ]);

        assert!(
            matches!(
                result,
                Err(nom::Err::Failure(error))
                    if error.code == nom::error::ErrorKind::LengthValue
            ),
            "empty signature_algorithms vector should fail with LengthValue"
        );
    }

    #[test]
    fn trailing_signature_algorithms_bytes_are_rejected() {
        let result = SignatureAlgorithmsExtension::parse(&[
            0x00, 0x02, // One algorithm follows.
            0x04, 0x03, // ECDSA_SECP256R1_SHA256
            0xff, // Extension body has a trailing byte beyond the vector.
        ]);

        assert!(
            matches!(
                result,
                Err(nom::Err::Failure(error))
                    if error.code == nom::error::ErrorKind::LengthValue
            ),
            "trailing signature_algorithms bytes should fail with LengthValue"
        );
    }
}
