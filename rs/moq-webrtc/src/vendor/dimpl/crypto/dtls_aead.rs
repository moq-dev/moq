//! DTLS AEAD record formatting types and constants.
//!
//! This module contains types and constants specific to DTLS AEAD record formatting,
//! separate from the pluggable crypto provider abstraction.

use arrayvec::ArrayVec;

use crate::types::{ContentType, Sequence};

/// Explicit nonce length for DTLS AEAD records.
///
/// The explicit nonce is transmitted with each record.
#[cfg(test)]
pub(crate) const DTLS_EXPLICIT_NONCE_LEN: usize = 8;

/// GCM authentication tag length.
///
/// The tag is appended to the ciphertext.
#[cfg(test)]
pub(crate) const GCM_TAG_LEN: usize = 16;

/// Overhead per DTLS 1.2 AES-GCM record (explicit nonce + tag).
///
/// This equals 24 bytes for DTLS AES-GCM.
#[cfg(test)]
pub(crate) const DTLS_AEAD_OVERHEAD: usize = DTLS_EXPLICIT_NONCE_LEN + GCM_TAG_LEN; // 24

/// Compute AAD length from plaintext length for DTLS 1.2 AES-GCM records.
#[inline]
#[cfg(test)]
pub fn aad_len_from_plaintext_len(plaintext_len: u16) -> u16 {
    plaintext_len
}

/// Compute fragment length from plaintext length for DTLS 1.2 AES-GCM records.
/// fragment_len = explicit_nonce(8) + ciphertext(plaintext_len + 16 tag)
#[inline]
#[cfg(test)]
pub fn fragment_len_from_plaintext_len(plaintext_len: usize) -> usize {
    DTLS_EXPLICIT_NONCE_LEN + plaintext_len + GCM_TAG_LEN
}

/// Compute plaintext length from fragment length for DTLS 1.2 AES-GCM records.
/// Returns None if the fragment is smaller than the mandatory AEAD overhead.
#[inline]
#[cfg(test)]
pub fn plaintext_len_from_fragment_len(fragment_len: usize) -> Option<usize> {
    fragment_len.checked_sub(DTLS_AEAD_OVERHEAD)
}

/// Fixed IV portion for DTLS AEAD.
///
/// DTLS 1.2 uses:
/// - AES-GCM: 4-byte fixed IV + 8-byte explicit nonce (per record)
/// - ChaCha20-Poly1305: 12-byte fixed IV + 0-byte explicit nonce
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Iv {
    bytes: [u8; 12],
    len: u8,
}

impl Iv {
    pub(crate) fn new(iv: &[u8]) -> Self {
        assert!(
            iv.len() <= 12,
            "invalid IV length: expected <= 12, got {}",
            iv.len()
        );
        let mut bytes = [0u8; 12];
        bytes[..iv.len()].copy_from_slice(iv);
        Self {
            bytes,
            len: iv.len() as u8,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.len as usize
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len()]
    }

    /// Returns the full 12-byte backing array.
    ///
    /// Only valid for 12-byte IVs (ChaCha20-Poly1305). For 4-byte IVs
    /// (AES-GCM), use [`as_slice`] instead.
    pub(crate) fn as_12_bytes(&self) -> &[u8; 12] {
        assert_eq!(
            self.len(),
            12,
            "as_12_bytes called on {}-byte IV",
            self.len()
        );
        &self.bytes
    }
}

/// Full AEAD nonce (fixed IV + explicit nonce).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nonce(pub [u8; 12]);

impl Nonce {
    /// Create a new AEAD nonce by combining fixed IV and explicit nonce (DTLS 1.2).
    pub(crate) fn new(iv: Iv, explicit_nonce: &[u8]) -> Self {
        assert_eq!(
            iv.len() + explicit_nonce.len(),
            12,
            "invalid DTLS 1.2 nonce parts: iv_len={}, explicit_nonce_len={}",
            iv.len(),
            explicit_nonce.len()
        );
        let mut nonce = [0u8; 12];
        let iv_len = iv.len();
        nonce[..iv_len].copy_from_slice(iv.as_slice());
        nonce[iv_len..].copy_from_slice(explicit_nonce);
        Self(nonce)
    }

    /// Create a nonce by XORing the IV with the padded sequence number.
    ///
    /// Used by both DTLS 1.2 (ChaCha20-Poly1305) and DTLS 1.3:
    /// nonce = iv XOR pad_left(sequence_number, 12)
    /// See RFC 8446 Section 5.3 / RFC 7905.
    pub(crate) fn xor(iv: &[u8; 12], seq: u64) -> Self {
        let mut nonce = *iv;
        let seq_bytes = seq.to_be_bytes(); // 8 bytes
        // XOR the last 8 bytes of the 12-byte IV with the sequence number
        for i in 0..8 {
            nonce[4 + i] ^= seq_bytes[i];
        }
        Self(nonce)
    }
}

/// Additional Authenticated Data for DTLS records.
///
/// Variable-length to support both DTLS 1.2 (13 bytes) and DTLS 1.3 (3-5 bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aad(pub ArrayVec<u8, 13>);

impl Aad {
    /// Create Additional Authenticated Data for a DTLS 1.2 record.
    pub(crate) fn new_dtls12(content_type: ContentType, sequence: Sequence, length: u16) -> Self {
        let mut aad = ArrayVec::new();

        // First set the full 8-byte sequence number
        let seq_bytes = sequence.sequence_number.to_be_bytes();
        aad.try_extend_from_slice(&seq_bytes).unwrap();

        // Overwrite the first 2 bytes with epoch
        let epoch_bytes = sequence.epoch.to_be_bytes();
        aad[0] = epoch_bytes[0];
        aad[1] = epoch_bytes[1];

        // Content type at index 8
        aad.push(content_type.as_u8());

        // Protocol version bytes (major:minor) at indexes 9-10
        aad.push(0xfe); // DTLS 1.2 major version
        aad.push(0xfd); // DTLS 1.2 minor version

        // Payload length (2 bytes) at indexes 11-12
        aad.try_extend_from_slice(&length.to_be_bytes()).unwrap();

        Aad(aad)
    }

    /// Create Additional Authenticated Data for a DTLS 1.3 record.
    ///
    /// The AAD is the raw unified header bytes (3-5 bytes).
    pub(crate) fn new_dtls13(header_bytes: &[u8]) -> Self {
        let mut aad = ArrayVec::new();
        // unwrap: header_bytes is at most 5 bytes, well within capacity 13
        aad.try_extend_from_slice(header_bytes).unwrap();
        Aad(aad)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aead_constants_and_length_helpers() {
        assert_eq!(DTLS_EXPLICIT_NONCE_LEN, 8);
        assert_eq!(GCM_TAG_LEN, 16);
        assert_eq!(DTLS_AEAD_OVERHEAD, 24);

        for &pt_len in &[0usize, 1, 37, 512, 1350, 16384] {
            let aad_len = aad_len_from_plaintext_len(pt_len as u16);
            assert_eq!(aad_len as usize, pt_len);

            let frag_len = fragment_len_from_plaintext_len(pt_len);
            assert_eq!(frag_len, DTLS_EXPLICIT_NONCE_LEN + pt_len + GCM_TAG_LEN);

            let roundtrip =
                plaintext_len_from_fragment_len(frag_len).expect("frag_len >= overhead");
            assert_eq!(roundtrip, pt_len);
        }

        assert!(plaintext_len_from_fragment_len(0).is_none());
        assert!(plaintext_len_from_fragment_len(3).is_none());
        assert!(plaintext_len_from_fragment_len(DTLS_AEAD_OVERHEAD - 1).is_none());
    }
}
