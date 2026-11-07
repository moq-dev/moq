//! Certificate generation and formatting utilities exposed via the public `certificate` module.
//!
//! This module provides helpers to generate self-signed certificates suitable for DTLS,
//! compute fingerprints, and format them for display.

use aws_lc_rs::digest;
use rand::random;
use rcgen::{CertificateParams, DistinguishedName, DnType, IsCa, KeyPair, PKCS_ECDSA_P256_SHA256};
use std::fmt;

/// Certificate utility error types
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertificateError {
    /// Invalid certificate format
    InvalidFormat,
    /// Fingerprint verification failed
    FingerprintMismatch,
    /// Certificate generation failed
    GenerationFailed,
}

impl fmt::Display for CertificateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CertificateError::InvalidFormat => write!(f, "Invalid certificate format"),
            CertificateError::FingerprintMismatch => write!(f, "Fingerprint mismatch"),
            CertificateError::GenerationFailed => write!(f, "Certificate generation failed"),
        }
    }
}

impl std::error::Error for CertificateError {}

pub use crate::DtlsCertificate;

/// Generate a self-signed certificate for DTLS
///
/// This function requires the `rcgen` feature to be enabled.
pub fn generate_self_signed_certificate() -> Result<DtlsCertificate, CertificateError> {
    // Create a key pair for the certificate
    let key_pair = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
        .map_err(|_| CertificateError::GenerationFailed)?;

    // Set up certificate parameters
    let mut params = CertificateParams::new(Vec::<String>::new())
        .map_err(|_| CertificateError::GenerationFailed)?;

    // Set up distinguished name
    let mut distinguished_name = DistinguishedName::new();
    distinguished_name.push(DnType::OrganizationName, "DTLS".to_string());
    distinguished_name.push(DnType::CommonName, "DTLS Peer".to_string());
    params.distinguished_name = distinguished_name;

    // Configure as end entity certificate (not a CA)
    params.is_ca = IsCa::NoCa;

    // Set validity period (1 year)
    let not_before = time::OffsetDateTime::now_utc();
    let not_after = not_before + time::Duration::days(365);
    params.not_before = not_before;
    params.not_after = not_after;

    // Serial number: must be unique for Firefox compatibility, not only across all certificates
    // of this process, but also across all certificates of other processes/machines!
    // See: https://github.com/versatica/mediasoup/issues/127#issuecomment-474460153
    // and https://github.com/algesten/str0m/issues/517
    let serial_buf: [u8; 16] = random();
    params.serial_number = Some(serial_buf.to_vec().into());

    // Build the certificate
    let cert = params
        .self_signed(&key_pair)
        .map_err(|_| CertificateError::GenerationFailed)?;

    // Get the certificate in DER format
    let cert_der = cert.der().to_vec();

    // Get the private key in DER format
    let key_der = key_pair.serialize_der();

    Ok(DtlsCertificate {
        certificate: cert_der,
        private_key: key_der,
    })
}

/// Calculate a certificate fingerprint using SHA-256
///
/// This function requires the `rcgen` feature to be enabled.
pub fn calculate_fingerprint(cert_der: &[u8]) -> Vec<u8> {
    // Use SHA-256 to calculate the fingerprint
    digest::digest(&digest::SHA256, cert_der).as_ref().to_vec()
}

/// Format a fingerprint as a colon-separated hex string
/// Example: "AF:12:F6:..."
pub fn format_fingerprint(fingerprint: &[u8]) -> String {
    fingerprint
        .iter()
        .map(|byte| format!("{:02X}", byte))
        .collect::<Vec<String>>()
        .join(":")
}

impl DtlsCertificate {
    /// Returns the certificate fingerprint as raw bytes.
    ///
    /// The fingerprint is computed by hashing the DER-encoded certificate
    /// with SHA-256 and is therefore 32 bytes long.
    ///
    /// This method requires the `rcgen` feature to be enabled.
    pub fn fingerprint(&self) -> Vec<u8> {
        calculate_fingerprint(&self.certificate)
    }

    /// Returns the certificate fingerprint as a human-readable string.
    ///
    /// The string is the SHA-256 fingerprint formatted as uppercase
    /// hex byte pairs separated by colons, for example "AF:12:F6:...".
    ///
    /// This method requires the `rcgen` feature to be enabled.
    pub fn fingerprint_str(&self) -> String {
        format_fingerprint(&self.fingerprint())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_self_signed_certificate() {
        // Generate a certificate
        let cert = generate_self_signed_certificate().unwrap();

        // Check certificate format
        assert!(!cert.certificate.is_empty());
        assert!(!cert.private_key.is_empty());

        // Fingerprint should be 32 bytes (SHA-256)
        assert_eq!(cert.fingerprint().len(), 32);
    }

    #[test]
    fn test_unique_serial_numbers() {
        // Generate two certificates
        let cert1 = generate_self_signed_certificate().unwrap();
        let cert2 = generate_self_signed_certificate().unwrap();

        // Fingerprints should be different (unique serial numbers + keys)
        assert_ne!(cert1.fingerprint(), cert2.fingerprint());

        // Parse the certificates to verify serial numbers are different
        use x509_parser::prelude::*;
        let (_, parsed1) = X509Certificate::from_der(&cert1.certificate).unwrap();
        let (_, parsed2) = X509Certificate::from_der(&cert2.certificate).unwrap();
        assert_ne!(
            parsed1.serial, parsed2.serial,
            "Serial numbers must be unique for Firefox compatibility"
        );
    }

    #[test]
    fn test_fingerprint_formatting() {
        let test_fingerprint = vec![0xAF, 0x12, 0xF6, 0x38, 0x2A];
        let formatted = format_fingerprint(&test_fingerprint);
        assert_eq!(formatted, "AF:12:F6:38:2A");

        // Test with an actual generated certificate
        let cert = generate_self_signed_certificate().unwrap();
        let formatted = format_fingerprint(&cert.fingerprint());

        // Verify the format
        assert_eq!(formatted.len(), 95); // 32 bytes * 3 - 1 = 95 (32 hex pairs with : between them)
        assert!(formatted.contains(':'));

        // Each segment should be 2 hex chars
        for segment in formatted.split(':') {
            assert_eq!(segment.len(), 2);
            // Verify it's valid hex
            assert!(u8::from_str_radix(segment, 16).is_ok());
        }
    }
}
