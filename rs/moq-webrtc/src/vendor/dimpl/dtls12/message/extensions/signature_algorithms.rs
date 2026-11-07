use super::super::{SignatureAndHashAlgorithm, SignatureAndHashAlgorithmVec};
use crate::buffer::Buf;
use nom::Err;
use nom::IResult;
use nom::bytes::complete::take;
use nom::error::{Error, ErrorKind};

/// SignatureAlgorithms extension as defined in RFC 5246
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureAlgorithmsExtension {
    pub supported_signature_algorithms: SignatureAndHashAlgorithmVec,
}

impl SignatureAlgorithmsExtension {
    /// Create a default SignatureAlgorithmsExtension with standard algorithms
    pub fn default() -> Self {
        SignatureAlgorithmsExtension {
            supported_signature_algorithms: SignatureAndHashAlgorithmVec::from(
                *SignatureAndHashAlgorithm::supported(),
            ),
        }
    }

    pub fn parse(input: &[u8]) -> IResult<&[u8], SignatureAlgorithmsExtension> {
        let (input, list_len) = nom::number::complete::be_u16(input)?;
        if list_len == 0 || list_len % 2 != 0 {
            return Err(Err::Failure(Error::new(input, ErrorKind::LengthValue)));
        }

        let mut algorithms = SignatureAndHashAlgorithmVec::new();
        let (input, mut current_input) = take(list_len)(input)?;
        if !input.is_empty() {
            return Err(Err::Failure(Error::new(input, ErrorKind::LengthValue)));
        }

        // Parse algorithms, filtering to only keep supported ones
        while !current_input.is_empty() {
            let (rest, alg) = SignatureAndHashAlgorithm::parse(current_input)?;
            // Only keep supported signature+hash combinations
            if alg.is_supported() {
                algorithms
                    .try_push(alg)
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
        // Write the total length of all algorithms (2 bytes per algorithm)
        output.extend_from_slice(
            &((self.supported_signature_algorithms.len() * 2) as u16).to_be_bytes(),
        );

        // Write each algorithm
        for alg in &self.supported_signature_algorithms {
            output.extend_from_slice(&alg.as_u16().to_be_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::{HashAlgorithm, SignatureAlgorithm};

    use super::*;

    #[test]
    fn test_signature_algorithms_extension() {
        let mut algorithms = SignatureAndHashAlgorithmVec::new();
        algorithms.push(SignatureAndHashAlgorithm::new(
            HashAlgorithm::SHA256,
            SignatureAlgorithm::ECDSA,
        ));
        algorithms.push(SignatureAndHashAlgorithm::new(
            HashAlgorithm::SHA256,
            SignatureAlgorithm::RSA,
        ));

        let ext = SignatureAlgorithmsExtension {
            supported_signature_algorithms: algorithms.clone(),
        };

        let mut serialized = Buf::new();
        ext.serialize(&mut serialized);

        let expected = [
            0x00, 0x04, // Length (4 bytes)
            0x04, 0x03, // SHA256/ECDSA
            0x04, 0x01, // SHA256/RSA
        ];

        assert_eq!(&*serialized, expected);

        let (_, parsed) = SignatureAlgorithmsExtension::parse(&serialized).unwrap();

        assert_eq!(parsed.supported_signature_algorithms.len(), 1);
        assert_eq!(
            parsed.supported_signature_algorithms[0],
            SignatureAndHashAlgorithm::new(HashAlgorithm::SHA256, SignatureAlgorithm::ECDSA,)
        );
    }

    #[test]
    fn default_does_not_advertise_rsa() {
        let extension = SignatureAlgorithmsExtension::default();

        assert!(
            extension
                .supported_signature_algorithms
                .iter()
                .all(|algorithm| algorithm.signature != SignatureAlgorithm::RSA)
        );
    }

    #[test]
    fn capacity_matches_supported() {
        let extension = SignatureAlgorithmsExtension::default();

        assert_eq!(
            extension.supported_signature_algorithms.capacity(),
            SignatureAndHashAlgorithm::supported().len()
        );
    }

    #[test]
    fn too_many_supported_signature_algorithms_are_rejected() {
        let mut bytes = Vec::new();
        let count = SignatureAndHashAlgorithm::supported().len() + 1;
        bytes.extend_from_slice(&(count as u16 * 2).to_be_bytes());
        for _ in 0..count {
            bytes.extend_from_slice(
                &SignatureAndHashAlgorithm::new(HashAlgorithm::SHA256, SignatureAlgorithm::ECDSA)
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
            0x04, 0x03, // SHA256/ECDSA
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
