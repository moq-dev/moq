use std::fmt;
use std::panic::{RefUnwindSafe, UnwindSafe};

use crate::crypto::Aes128CmSha1_80Cipher;
use crate::crypto::dtls::{KeyingMaterial, SrtpProfile};
use crate::crypto::{AeadAes128Gcm, AeadAes256Gcm, Aes128CmSha1_80};
use crate::crypto::{AeadAes128GcmCipher, AeadAes256GcmCipher};
use crate::crypto::{CryptoProvider, Sha1HmacProvider, SrtpProvider};
use crate::io::DATAGRAM_MAX_PACKET_SIZE;

use super::header::RtpHeader;

// Common among various profiles(defined in RFC3711 Section 4.3)
const LABEL_RTP_AES: u8 = 0;
const LABEL_RTP_AUTHENTICATION_KEY: u8 = 1;
const LABEL_RTP_SALT: u8 = 2;

const LABEL_RTCP_AES: u8 = 3;
const LABEL_RTCP_AUTHENTICATION_KEY: u8 = 4;
const LABEL_RTCP_SALT: u8 = 5;

// header = 4 bytes
// ssrc   = 4 bytes
// ssrtcp_index = 4 bytes
// tag = <T> bytes
// TOTAL overhead for SRTCP = 12 + T bytes.
// However, each RTCP packet must be on a 4 byte boundary since length is
// given in number of 4 bytes - 1 (making 0 valid).

pub const SRTP_BLOCK_SIZE: usize = 16;
const SRTCP_INDEX_LEN: usize = 4;
const SRTCP_HEADER_LEN: usize = 8;
const MAX_TAG_LEN: usize = AeadAes256Gcm::TAG_LEN;
pub const SRTCP_OVERHEAD: usize = MAX_TAG_LEN + SRTCP_INDEX_LEN;
pub const SRTP_OVERHEAD: usize = MAX_TAG_LEN;

// aws-lc-rs CTR mode requires output buffer to be input.len() + block_len - 1
// This adds 15 bytes of padding that we truncate after encryption
const CTR_BUFFER_PADDING: usize = SRTP_BLOCK_SIZE - 1;

/// Initial capacity for the RTP decryption scratch buffer. Sized to fit the
/// largest possible received datagram plus the CTR padding the AES-CM cipher
/// needs, so `unprotect_rtp` never has to grow the buffer in practice.
const RX_SCRATCH_CAPACITY: usize = DATAGRAM_MAX_PACKET_SIZE + CTR_BUFFER_PADDING;

/// Start repeated warnings here, then double the failure count between reports.
const DECRYPT_WARN_INITIAL_COUNT: u64 = 4_096;

impl SrtpContext {
    /// Create an SRTP context for the relevant profile using the provided keying material.
    pub fn new(
        crypto: &CryptoProvider,
        profile: SrtpProfile,
        mat: &KeyingMaterial,
        left: bool,
    ) -> Self {
        let sha1_hmac_provider = crypto.sha1_hmac_provider;

        match profile {
            SrtpProfile::AES128_CM_SHA1_80 => {
                let key =
                    SrtpKey::<{ Aes128CmSha1_80::KEY_LEN }, { Aes128CmSha1_80::SALT_LEN }>::new(
                        mat, left,
                    );

                let (rtp, rtcp) = Derived::aes_128_cm_sha1_80(crypto.srtp_provider, &key);

                SrtpContext {
                    rtp,
                    rtcp,
                    srtcp_index: 0,
                    sha1_hmac_provider,
                    rx_scratch: Vec::with_capacity(RX_SCRATCH_CAPACITY),
                    decrypt_warnings: DecryptWarnings::default(),
                }
            }
            SrtpProfile::AEAD_AES_128_GCM => {
                let key = SrtpKey::<{ AeadAes128Gcm::KEY_LEN }, { AeadAes128Gcm::SALT_LEN }>::new(
                    mat, left,
                );

                let (rtp, rtcp) = Derived::aead_aes_128_gcm(crypto.srtp_provider, &key);

                SrtpContext {
                    rtp,
                    rtcp,
                    srtcp_index: 0,
                    sha1_hmac_provider,
                    rx_scratch: Vec::with_capacity(RX_SCRATCH_CAPACITY),
                    decrypt_warnings: DecryptWarnings::default(),
                }
            }
            SrtpProfile::AEAD_AES_256_GCM => {
                let key = SrtpKey::<{ AeadAes256Gcm::KEY_LEN }, { AeadAes256Gcm::SALT_LEN }>::new(
                    mat, left,
                );

                let (rtp, rtcp) = Derived::aead_aes_256_gcm(crypto.srtp_provider, &key);

                SrtpContext {
                    rtp,
                    rtcp,
                    srtcp_index: 0,
                    sha1_hmac_provider,
                    rx_scratch: Vec::with_capacity(RX_SCRATCH_CAPACITY),
                    decrypt_warnings: DecryptWarnings::default(),
                }
            }
            _ => panic!("Unexpected SRTP profile: {profile:?}"),
        }
    }

    #[cfg(test)]
    fn new_aead_aes_128_gcm(
        rtp_key: [u8; AeadAes128Gcm::KEY_LEN],
        rtp_salt: [u8; AeadAes128Gcm::SALT_LEN],
        rtcp_key: [u8; AeadAes128Gcm::KEY_LEN],
        rtcp_salt: [u8; AeadAes128Gcm::SALT_LEN],
        srtcp_index: u32,
    ) -> Self {
        let provider = crate::crypto::test_default_provider();

        Self {
            rtp: Derived::AeadAes128Gcm {
                salt: rtp_salt,
                enc: provider
                    .srtp_provider
                    .aead_aes_128_gcm()
                    .create_cipher(rtp_key, true),
                dec: provider
                    .srtp_provider
                    .aead_aes_128_gcm()
                    .create_cipher(rtp_key, false),
            },
            rtcp: Derived::AeadAes128Gcm {
                salt: rtcp_salt,
                enc: provider
                    .srtp_provider
                    .aead_aes_128_gcm()
                    .create_cipher(rtcp_key, true),
                dec: provider
                    .srtp_provider
                    .aead_aes_128_gcm()
                    .create_cipher(rtcp_key, false),
            },
            srtcp_index,
            sha1_hmac_provider: provider.sha1_hmac_provider,
            rx_scratch: Vec::with_capacity(RX_SCRATCH_CAPACITY),
            decrypt_warnings: DecryptWarnings::default(),
        }
    }

    #[cfg(test)]
    fn new_aead_aes_256_gcm(
        rtp_key: [u8; AeadAes256Gcm::KEY_LEN],
        rtp_salt: [u8; AeadAes256Gcm::SALT_LEN],
        rtcp_key: [u8; AeadAes256Gcm::KEY_LEN],
        rtcp_salt: [u8; AeadAes256Gcm::SALT_LEN],
        srtcp_index: u32,
    ) -> Self {
        let provider = crate::crypto::test_default_provider();

        Self {
            rtp: Derived::AeadAes256Gcm {
                salt: rtp_salt,
                enc: provider
                    .srtp_provider
                    .aead_aes_256_gcm()
                    .create_cipher(rtp_key, true),
                dec: provider
                    .srtp_provider
                    .aead_aes_256_gcm()
                    .create_cipher(rtp_key, false),
            },
            rtcp: Derived::AeadAes256Gcm {
                salt: rtcp_salt,
                enc: provider
                    .srtp_provider
                    .aead_aes_256_gcm()
                    .create_cipher(rtcp_key, true),
                dec: provider
                    .srtp_provider
                    .aead_aes_256_gcm()
                    .create_cipher(rtcp_key, false),
            },
            srtcp_index,
            sha1_hmac_provider: provider.sha1_hmac_provider,
            rx_scratch: Vec::with_capacity(RX_SCRATCH_CAPACITY),
            decrypt_warnings: DecryptWarnings::default(),
        }
    }
}

#[derive(Debug)]
pub struct SrtpContext {
    /// Encryption/decryption derived from srtp_key for RTP.
    rtp: Derived,
    /// Encryption/decryption derived from srtp_key for RTCP.
    rtcp: Derived,
    /// Counter for outgoing SRTCP packets.
    srtcp_index: u32,
    /// SHA1-HMAC provider for AES_128_CM_SHA1_80 profile.
    sha1_hmac_provider: &'static dyn Sha1HmacProvider,
    /// Reusable scratch buffer for RTP decryption output. Reused across calls
    /// to `unprotect_rtp` so we don't allocate a fresh `Vec` per packet.
    rx_scratch: Vec<u8>,
    decrypt_warnings: DecryptWarnings,
}

/// SrtpContext contains cipher contexts that can't observe broken invariants after a panic.
impl UnwindSafe for SrtpContext {}
impl RefUnwindSafe for SrtpContext {}

/// Shares one warning budget across all SRTP and SRTCP streams in a receive context.
/// Reports the first failure immediately, then at exponentially increasing counts.
#[derive(Debug, Default)]
struct DecryptWarnings {
    failures: u64,
    suppressed_srtp: u64,
    suppressed_srtcp: u64,
}

impl DecryptWarnings {
    fn warn(&mut self, profile: SrtpProfile, is_rtcp: bool) {
        self.failures = self.failures.saturating_add(1);
        let should_warn = self.failures == 1
            || (self.failures >= DECRYPT_WARN_INITIAL_COUNT && self.failures.is_power_of_two());
        if !should_warn {
            let count = if is_rtcp {
                &mut self.suppressed_srtcp
            } else {
                &mut self.suppressed_srtp
            };
            *count = count.saturating_add(1);
            return;
        }

        let protocol = if is_rtcp { "SRTCP" } else { "SRTP" };
        warn!(
            failures = self.failures,
            suppressed_srtp = self.suppressed_srtp,
            suppressed_srtcp = self.suppressed_srtcp,
            "Failed to decrypt {protocol} {profile}"
        );
        self.suppressed_srtp = 0;
        self.suppressed_srtcp = 0;
    }
}

impl SrtpContext {
    pub fn protect_rtp(
        &mut self,
        buf: &[u8],
        header: &RtpHeader,
        srtp_index: u64, // same as ext_seq
    ) -> Vec<u8> {
        // SRTP layout
        // [header, [rtp, (padding + pad_count)], tag]

        //     0                   1                   2                   3
        //     0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
        //    +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+<+
        //    |V=2|P|X|  CC   |M|     PT      |       sequence number         | |
        //    +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+ |
        //    |                           timestamp                           | |
        //    +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+ |
        //    |           synchronization source (SSRC) identifier            | |
        //    +=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+ |
        //    |            contributing source (CSRC) identifiers             | |
        //    |                               ....                            | |
        //    +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+ |
        //    |                   RTP extension (OPTIONAL)                    | |
        //  +>+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+ |
        //  | |                          payload  ...                         | |
        //  | |                               +-------------------------------+ |
        //  | |                               | RTP padding   | RTP pad count | |
        //  +>+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+<+
        //  | ~                     SRTP MKI (OPTIONAL)                       ~ |
        //  | +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+ |
        //  | :                 authentication tag (RECOMMENDED)              : |
        //  | +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+ |
        //  |                                                                   |
        //  +- Encrypted Portion*                      Authenticated Portion ---+
        let hlen = header.header_len;
        let input = &buf[hlen..];

        match &mut self.rtp {
            Derived::Aes128CmSha1_80 { key, salt, enc, .. } => {
                // arbitrary body lengths are fine: the output buffer below is
                // allocated with the CTR scratch the ciphers need and truncated
                // after encryption, so no 16-byte alignment is required.
                let iv = Aes128CmSha1_80::rtp_iv(*salt, *header.ssrc, srtp_index);

                // Allocate buffer with CTR padding (aws-lc-rs requirement)
                // HMAC_TAG_LEN is 10, CTR_BUFFER_PADDING is 15, so we need 5 extra bytes
                let padding_needed =
                    CTR_BUFFER_PADDING.saturating_sub(Aes128CmSha1_80::HMAC_TAG_LEN);
                let mut output =
                    vec![0_u8; buf.len() + Aes128CmSha1_80::HMAC_TAG_LEN + padding_needed];

                enc.encrypt(&iv, input, &mut output[hlen..])
                    .expect("rtp encrypt");

                output[..hlen].copy_from_slice(&buf[..hlen]);

                let hmac_start = buf.len();
                let sha1_hmac = |k: &[u8], p: &[&[u8]]| self.sha1_hmac_provider.sha1_hmac(k, p);
                Aes128CmSha1_80::rtp_hmac(sha1_hmac, key, &mut output, srtp_index, hmac_start);

                output.truncate(buf.len() + Aes128CmSha1_80::HMAC_TAG_LEN);
                output
            }
            Derived::AeadAes128Gcm { salt, enc, .. } => {
                let roc = (srtp_index >> 16) as u32;

                let iv = AeadAes128Gcm::rtp_iv(*salt, *header.ssrc, roc, header.sequence_number);
                let aad = &buf[..hlen];

                // Input and output lengths for encryption:
                // https://www.rfc-editor.org/rfc/rfc7714#section-5.2.1
                let mut output = vec![0_u8; buf.len() + AeadAes128Gcm::TAG_LEN];
                enc.encrypt(&iv, aad, input, &mut output[hlen..])
                    .expect("rtp encrypt");

                output[..hlen].copy_from_slice(aad);

                output
            }
            Derived::AeadAes256Gcm { salt, enc, .. } => {
                let roc = (srtp_index >> 16) as u32;

                let iv = AeadAes256Gcm::rtp_iv(*salt, *header.ssrc, roc, header.sequence_number);
                let aad = &buf[..hlen];

                // Input and output lengths for encryption:
                // https://www.rfc-editor.org/rfc/rfc7714#section-5.2.1
                let mut output = vec![0_u8; buf.len() + AeadAes256Gcm::TAG_LEN];
                enc.encrypt(&iv, aad, input, &mut output[hlen..])
                    .expect("rtp encrypt");

                output[..hlen].copy_from_slice(aad);

                output
            }
        }
    }

    /// Decrypts an SRTP packet into the internal reusable scratch buffer and
    /// returns a slice of the plaintext bytes. The returned slice borrows from
    /// `self` until the next mutable use; the scratch buffer is reused across
    /// calls to avoid per-packet allocation.
    pub fn unprotect_rtp(
        &mut self,
        buf: &[u8],
        header: &RtpHeader,
        srtp_index: u64, // same as ext_seq
    ) -> Option<&[u8]> {
        match &mut self.rtp {
            Derived::Aes128CmSha1_80 { key, salt, dec, .. } => {
                if buf.len() < header.header_len + Aes128CmSha1_80::HMAC_TAG_LEN {
                    return None;
                }

                let hmac_start = buf.len() - Aes128CmSha1_80::HMAC_TAG_LEN;

                let sha1_hmac = |k: &[u8], p: &[&[u8]]| self.sha1_hmac_provider.sha1_hmac(k, p);
                if !Aes128CmSha1_80::rtp_verify(
                    sha1_hmac,
                    key,
                    &buf[..hmac_start],
                    srtp_index,
                    &buf[hmac_start..],
                ) {
                    trace!("unprotect_rtp hmac verify fail");
                    return None;
                }

                let iv = Aes128CmSha1_80::rtp_iv(*salt, *header.ssrc, srtp_index);

                let input = &buf[header.header_len..hmac_start];
                // Sized with CTR padding (aws-lc-rs requirement).
                self.rx_scratch.resize(input.len() + CTR_BUFFER_PADDING, 0);

                if let Err(e) = dec.decrypt(&iv, input, &mut self.rx_scratch) {
                    trace!(
                        "Failed to decrypt SRTP {} ({}): {}",
                        self.rtp.profile(),
                        error_details(header, srtp_index),
                        e
                    );
                    self.decrypt_warnings.warn(self.rtp.profile(), false);
                    return None;
                };

                Some(&self.rx_scratch[..input.len()])
            }
            Derived::AeadAes128Gcm { salt, dec, .. } => {
                if buf.len() < header.header_len + AeadAes128Gcm::TAG_LEN {
                    return None;
                }

                let roc: u32 = (srtp_index >> 16) as u32;
                let seq = header.sequence_number;

                let iv = AeadAes128Gcm::rtp_iv(*salt, *header.ssrc, roc, seq);

                let (aad, input) = buf.split_at(header.header_len);
                // Input and output lengths for decryption:
                // https://www.rfc-editor.org/rfc/rfc7714#section-5.2.2
                let out_len = input.len() - AeadAes128Gcm::TAG_LEN;
                self.rx_scratch.resize(out_len, 0);

                match dec.decrypt(&iv, &[aad], input, &mut self.rx_scratch) {
                    Ok(v) => v,
                    Err(e) => {
                        trace!(
                            "Failed to decrypt SRTP {} ({}): {}",
                            self.rtp.profile(),
                            error_details(header, srtp_index),
                            e
                        );
                        self.decrypt_warnings.warn(self.rtp.profile(), false);
                        return None;
                    }
                };

                Some(&self.rx_scratch[..out_len])
            }
            Derived::AeadAes256Gcm { salt, dec, .. } => {
                if buf.len() < header.header_len + AeadAes256Gcm::TAG_LEN {
                    return None;
                }

                let roc: u32 = (srtp_index >> 16) as u32;
                let seq = header.sequence_number;

                let iv = AeadAes256Gcm::rtp_iv(*salt, *header.ssrc, roc, seq);

                let (aad, input) = buf.split_at(header.header_len);
                // Input and output lengths for decryption:
                // https://www.rfc-editor.org/rfc/rfc7714#section-5.2.2
                let out_len = input.len() - AeadAes256Gcm::TAG_LEN;
                self.rx_scratch.resize(out_len, 0);

                match dec.decrypt(&iv, &[aad], input, &mut self.rx_scratch) {
                    Ok(v) => v,
                    Err(e) => {
                        trace!(
                            "Failed to decrypt SRTP {} ({}): {}",
                            self.rtp.profile(),
                            error_details(header, srtp_index),
                            e
                        );
                        self.decrypt_warnings.warn(self.rtp.profile(), false);
                        return None;
                    }
                };

                Some(&self.rx_scratch[..out_len])
            }
        }
    }

    pub fn protect_rtcp(&mut self, buf: &[u8]) -> Vec<u8> {
        let srtcp_index = self.srtcp_index;

        // https://tools.ietf.org/html/rfc3711#page-15
        // The SRTCP index MUST be set to zero before the first SRTCP
        // packet is sent, and MUST be incremented by one,
        // modulo 2^31, after each SRTCP packet is sent.
        self.srtcp_index = (self.srtcp_index + 1) % 2_u32.pow(31);

        // e is always encrypted, rest is 31 byte index.
        let e_and_si = 0x8000_0000 | srtcp_index;
        let ssrc = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);

        if ssrc == 0 {
            warn!("SSRC 0 does not make a good SRTCP IV");
        }

        match &mut self.rtcp {
            Derived::Aes128CmSha1_80 { key, salt, enc, .. } => {
                let iv = Aes128CmSha1_80::rtp_iv(*salt, ssrc, srtcp_index as u64);

                let final_size = buf.len() + SRTCP_INDEX_LEN + Aes128CmSha1_80::HMAC_TAG_LEN;
                // Allocate buffer with CTR padding (aws-lc-rs requirement)
                // SRTCP_INDEX_LEN + HMAC_TAG_LEN is 14, CTR_BUFFER_PADDING is 15, so we need 1 extra byte
                let padding_needed = CTR_BUFFER_PADDING
                    .saturating_sub(SRTCP_INDEX_LEN + Aes128CmSha1_80::HMAC_TAG_LEN);
                let mut output = vec![0_u8; final_size + padding_needed];

                output[0..8].copy_from_slice(&buf[0..8]);
                let input = &buf[8..];
                enc.encrypt(&iv, input, &mut output[8..])
                    .expect("rtcp encrypt");

                let to = &mut output[buf.len()..];
                to[0..4].copy_from_slice(&e_and_si.to_be_bytes());

                let hmac_index = final_size - Aes128CmSha1_80::HMAC_TAG_LEN;
                let sha1_hmac = |k: &[u8], p: &[&[u8]]| self.sha1_hmac_provider.sha1_hmac(k, p);
                Aes128CmSha1_80::rtcp_hmac(sha1_hmac, key, &mut output, hmac_index);

                output.truncate(final_size);
                output
            }
            Derived::AeadAes128Gcm { salt, enc, .. } => {
                let iv = AeadAes128Gcm::rtcp_iv(*salt, ssrc, srtcp_index);

                let mut aad = [0; AeadAes128Gcm::RTCP_AAD_LEN];
                aad[..8].copy_from_slice(&buf[..8]);
                aad[8..12].copy_from_slice(&e_and_si.to_be_bytes());

                let mut output = vec![0_u8; buf.len() + SRTCP_INDEX_LEN + AeadAes128Gcm::TAG_LEN];
                output[0..8].copy_from_slice(&buf[0..8]);
                let input = &buf[8..];

                let enc_start = 8;
                let enc_end = input.len() + 8 + AeadAes128Gcm::TAG_LEN;
                let encout = &mut output[enc_start..enc_end];

                enc.encrypt(&iv, &aad, input, encout).expect("rtcp encrypt");

                let to = &mut output[enc_end..];
                to[0..4].copy_from_slice(&e_and_si.to_be_bytes());

                output
            }
            Derived::AeadAes256Gcm { salt, enc, .. } => {
                let iv = AeadAes256Gcm::rtcp_iv(*salt, ssrc, srtcp_index);

                let mut aad = [0; AeadAes256Gcm::RTCP_AAD_LEN];
                aad[..8].copy_from_slice(&buf[..8]);
                aad[8..12].copy_from_slice(&e_and_si.to_be_bytes());

                let mut output = vec![0_u8; buf.len() + SRTCP_INDEX_LEN + AeadAes256Gcm::TAG_LEN];
                output[0..8].copy_from_slice(&buf[0..8]);
                let input = &buf[8..];

                let enc_start = 8;
                let enc_end = input.len() + 8 + AeadAes256Gcm::TAG_LEN;
                let encout = &mut output[enc_start..enc_end];

                enc.encrypt(&iv, &aad, input, encout).expect("rtcp encrypt");

                let to = &mut output[enc_end..];
                to[0..4].copy_from_slice(&e_and_si.to_be_bytes());

                output
            }
        }
    }

    // SRTCP layout
    // ["header", ssrc, payload, ["header", ssrc, payload], ...], ssrtcp_index, tag]
    //
    // |----------------------------------------------------------------------|
    //                          authenticated
    //
    //                  |--------------------------------------|
    //                              encrypted (aes)
    pub fn unprotect_rtcp(&mut self, buf: &[u8]) -> Option<Vec<u8>> {
        match &mut self.rtcp {
            Derived::Aes128CmSha1_80 { key, salt, dec, .. } => {
                if buf.len() < SRTCP_HEADER_LEN + SRTCP_INDEX_LEN + Aes128CmSha1_80::HMAC_TAG_LEN {
                    return None;
                }

                let hmac_start = buf.len() - Aes128CmSha1_80::HMAC_TAG_LEN;

                let sha1_hmac = |k: &[u8], p: &[&[u8]]| self.sha1_hmac_provider.sha1_hmac(k, p);
                if !Aes128CmSha1_80::rtcp_verify(
                    sha1_hmac,
                    key,
                    &buf[..hmac_start],
                    &buf[hmac_start..],
                ) {
                    trace!("unprotect_rtcp hmac verify fail");
                    return None;
                }

                let idx_start = hmac_start - SRTCP_INDEX_LEN;

                let srtcp_index_be = [
                    buf[idx_start],
                    buf[idx_start + 1],
                    buf[idx_start + 2],
                    buf[idx_start + 3],
                ];

                // E-flag and SRTCP index.
                let e_and_si = u32::from_be_bytes(srtcp_index_be);

                let is_encrypted = e_and_si & 0x8000_0000 > 0;

                if !is_encrypted {
                    // Non-encrypted we can just return
                    return Some(buf[0..idx_start].to_vec());
                }

                // The SRTCP index is a 31-bit counter for the SRTCP packet.
                let srtcp_index = e_and_si & 0x7fff_ffff;
                let ssrc = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);

                let iv = Aes128CmSha1_80::rtp_iv(*salt, ssrc, srtcp_index as u64);

                // The Encrypted Portion of an SRTCP packet consists of the encryption
                // of the RTCP payload of the equivalent compound RTCP packet, from the
                // first RTCP packet, i.e., from the ninth (9) octet to the end of the
                // compound packet.
                let input = &buf[8..idx_start];
                // Allocate buffer with CTR padding (aws-lc-rs requirement)
                let mut output = vec![0_u8; input.len() + 8 + CTR_BUFFER_PADDING];
                output[0..8].copy_from_slice(&buf[0..8]);

                if let Err(e) = dec.decrypt(&iv, input, &mut output[8..]) {
                    trace!("Failed to decrypt SRTCP {}: {}", self.rtcp.profile(), e);
                    self.decrypt_warnings.warn(self.rtcp.profile(), true);
                    return None;
                }

                output.truncate(input.len() + 8);
                Some(output)
            }
            Derived::AeadAes128Gcm { salt, dec, .. } => {
                if buf.len() < SRTCP_HEADER_LEN + SRTCP_INDEX_LEN + AeadAes128Gcm::TAG_LEN {
                    // Too short
                    return None;
                }

                let idx_start = buf.len() - SRTCP_INDEX_LEN;

                // Assume no MKI
                let e_and_si = u32::from_be_bytes(
                    buf[idx_start..buf.len()]
                        .try_into()
                        // This is ok because SRTCP_INDEX_LEN is 4 bytes and the buffer is at least
                        // that long.
                        .expect("SRTCP_INDEX_LEN to be 4"),
                );
                let is_encrypted = e_and_si & 0x8000_0000 > 0;

                // The Encrypted Portion of an SRTCP packet consists of the encryption
                // of the RTCP payload of the equivalent compound RTCP packet, from the
                // first RTCP packet, i.e., from the ninth (9) octet to the end of the
                // compound packet.
                let input = if is_encrypted {
                    &buf[8..idx_start]
                } else {
                    // No, encryption but we still pass the tag down to decrypt so it can verify
                    // it.
                    &buf[idx_start - AeadAes128Gcm::TAG_LEN..idx_start]
                };

                // The SRTCP index is a 31-bit counter for the SRTCP packet.
                let srtcp_index = e_and_si & 0x7fff_ffff;
                let ssrc = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);

                let iv = AeadAes128Gcm::rtcp_iv(*salt, ssrc, srtcp_index);
                // Declared out here for lifetime purposes, only used in the first branch of the if.
                let mut encrypted_aad = [0; AeadAes128Gcm::RTCP_AAD_LEN];
                let mut aads: [&[u8]; 2] = [&[], &[]];

                if is_encrypted {
                    encrypted_aad[0..8].copy_from_slice(&buf[0..8]);
                    encrypted_aad[8..12].copy_from_slice(&e_and_si.to_be_bytes());

                    aads[0] = encrypted_aad.as_slice();
                } else {
                    // The whole packet is AAD
                    aads[0] = &buf[0..idx_start - AeadAes128Gcm::TAG_LEN];
                    aads[1] = &buf[idx_start..];
                };

                let mut output = vec![0_u8; buf.len() - AeadAes128Gcm::TAG_LEN - SRTCP_INDEX_LEN];
                output[0..8].copy_from_slice(&buf[0..8]);

                let count = match dec.decrypt(&iv, &aads, input, &mut output[8..]) {
                    Ok(c) => c,
                    Err(e) => {
                        trace!("Failed to decrypt SRTCP {}: {}", self.rtcp.profile(), e);
                        self.decrypt_warnings.warn(self.rtcp.profile(), true);
                        return None;
                    }
                };

                if is_encrypted {
                    output.truncate(8 + count);
                } else {
                    // decrypt didn't error, the data is authenticated.
                    output.copy_from_slice(
                        &buf[0..buf.len() - SRTCP_INDEX_LEN - AeadAes128Gcm::TAG_LEN],
                    )
                }

                Some(output)
            }
            Derived::AeadAes256Gcm { salt, dec, .. } => {
                if buf.len() < SRTCP_HEADER_LEN + SRTCP_INDEX_LEN + AeadAes256Gcm::TAG_LEN {
                    // Too short
                    return None;
                }

                let idx_start = buf.len() - SRTCP_INDEX_LEN;

                // Assume no MKI
                let e_and_si = u32::from_be_bytes(
                    buf[idx_start..buf.len()]
                        .try_into()
                        // This is ok because SRTCP_INDEX_LEN is 4 bytes and the buffer is at least
                        // that long.
                        .expect("SRTCP_INDEX_LEN to be 4"),
                );
                let is_encrypted = e_and_si & 0x8000_0000 > 0;

                // The Encrypted Portion of an SRTCP packet consists of the encryption
                // of the RTCP payload of the equivalent compound RTCP packet, from the
                // first RTCP packet, i.e., from the ninth (9) octet to the end of the
                // compound packet.
                let input = if is_encrypted {
                    &buf[8..idx_start]
                } else {
                    // No, encryption but we still pass the tag down to decrypt so it can verify
                    // it.
                    &buf[idx_start - AeadAes256Gcm::TAG_LEN..idx_start]
                };

                // The SRTCP index is a 31-bit counter for the SRTCP packet.
                let srtcp_index = e_and_si & 0x7fff_ffff;
                let ssrc = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);

                let iv = AeadAes256Gcm::rtcp_iv(*salt, ssrc, srtcp_index);
                // Declared out here for lifetime purposes, only used in the first branch of the if.
                let mut encrypted_aad = [0; AeadAes256Gcm::RTCP_AAD_LEN];
                let mut aads: [&[u8]; 2] = [&[], &[]];

                if is_encrypted {
                    encrypted_aad[0..8].copy_from_slice(&buf[0..8]);
                    encrypted_aad[8..12].copy_from_slice(&e_and_si.to_be_bytes());

                    aads[0] = encrypted_aad.as_slice();
                } else {
                    // The whole packet is AAD
                    aads[0] = &buf[0..idx_start - AeadAes256Gcm::TAG_LEN];
                    aads[1] = &buf[idx_start..];
                };

                let mut output = vec![0_u8; buf.len() - AeadAes256Gcm::TAG_LEN - SRTCP_INDEX_LEN];
                output[0..8].copy_from_slice(&buf[0..8]);

                let count = match dec.decrypt(&iv, &aads, input, &mut output[8..]) {
                    Ok(c) => c,
                    Err(e) => {
                        trace!("Failed to decrypt SRTCP {}: {}", self.rtcp.profile(), e);
                        self.decrypt_warnings.warn(self.rtcp.profile(), true);
                        return None;
                    }
                };

                if is_encrypted {
                    output.truncate(8 + count);
                } else {
                    // decrypt didn't error, the data is authenticated.
                    output.copy_from_slice(
                        &buf[0..buf.len() - SRTCP_INDEX_LEN - AeadAes256Gcm::TAG_LEN],
                    )
                }

                Some(output)
            }
        }
    }
}

/// SrtpKeys created from DTLS SrtpKeyMaterial.
#[derive(Debug)]
struct SrtpKey<const ML: usize, const SL: usize> {
    master: [u8; ML],
    salt: [u8; SL],
}

impl<const ML: usize, const SL: usize> SrtpKey<ML, SL> {
    pub fn new(mat: &KeyingMaterial, left: bool) -> Self {
        // layout in SrtpKeyMaterial is [key_input, key_output, salt_input, salt_output]

        // Invariant
        assert!(
            mat.len() == ML * 2 + SL * 2,
            "The KeyingMaterial provided to SrtpKey::new must be ML * 2 + SL * 2 in length"
        );

        // offset 0, offset 1
        let (o0, o1) = if left { (0, 0) } else { (ML, SL) };

        let mut master = [0; ML];
        let mut salt = [0; SL];

        master[0..ML].copy_from_slice(&mat[o0..(o0 + ML)]);
        salt[0..SL].copy_from_slice(&mat[(ML + ML + o1)..(ML + ML + o1 + SL)]);

        SrtpKey { master, salt }
    }

    fn derive(&self, crypto: &dyn SrtpProvider, label: u8, out: &mut [u8]) {
        // AES-CM (128 or 256 bits) defined in RFC3711
        assert!(ML == 16 || ML == 32, "Only valid for 128 bit master keys");
        assert!(SL <= 14, "Only valid for 128 bit master keys");
        let mut i = 0; // index in out

        // input layout: [salt[SL] || label, round[2]] (|| is xor 7th byte)
        let mut input = [0; 16];

        input[0..SL].copy_from_slice(&self.salt[..]);
        input[7] ^= label;

        let mut buf = [0; 16 + 16]; // output from each AES
        let mut round: u16 = 0; // counter for each AES round

        // loop each AES round
        loop {
            if i == out.len() {
                break;
            }

            // splice in round at bottom of input
            input[14..].copy_from_slice(&round.to_be_bytes()[..]);

            // default key derivation function, which uses AES-128 in Counter Mode
            match ML {
                16 => crypto.srtp_aes_128_ecb_round(&self.master, &input[..], &mut buf[..]),
                32 => crypto.srtp_aes_256_ecb_round(&self.master, &input[..], &mut buf[..]),
                _ => panic!("Only valid for 128 or 256 bit master keys"),
            }

            // Copy to output. Even if we get 32 bytes of output with AES 128 ECB, we
            // only use the first 16. That matches the tests in the RFC.
            for j in buf.iter().take(16) {
                if i == out.len() {
                    break;
                }
                out[i] = *j;
                i += 1;
            }

            round += 1;
        }
    }
}

/// Encryption/decryption derived from the SrtpKey.
enum Derived {
    Aes128CmSha1_80 {
        key: [u8; 20],
        salt: [u8; 14],
        enc: Box<dyn Aes128CmSha1_80Cipher>,
        dec: Box<dyn Aes128CmSha1_80Cipher>,
    },
    AeadAes128Gcm {
        salt: [u8; 12],
        enc: Box<dyn AeadAes128GcmCipher>,
        dec: Box<dyn AeadAes128GcmCipher>,
    },
    AeadAes256Gcm {
        salt: [u8; 12],
        enc: Box<dyn AeadAes256GcmCipher>,
        dec: Box<dyn AeadAes256GcmCipher>,
    },
}

impl Derived {
    fn aes_128_cm_sha1_80(
        crypto: &dyn SrtpProvider,
        srtp_key: &SrtpKey<{ Aes128CmSha1_80::KEY_LEN }, { Aes128CmSha1_80::SALT_LEN }>,
    ) -> (Self, Self) {
        // RTP AES Counter
        let mut rtp_aes = [0; Aes128CmSha1_80::KEY_LEN];
        srtp_key.derive(crypto, LABEL_RTP_AES, &mut rtp_aes[..]);

        // RTP SHA1 HMAC
        let rtp_hmac = {
            let mut hmac = [0; Aes128CmSha1_80::HMAC_KEY_LEN];
            srtp_key.derive(crypto, LABEL_RTP_AUTHENTICATION_KEY, &mut hmac[..]);
            hmac
        };

        // RTP IV SALT
        let mut rtp_salt = [0; Aes128CmSha1_80::SALT_LEN];
        srtp_key.derive(crypto, LABEL_RTP_SALT, &mut rtp_salt[..]);

        // RTCP AES Counter
        let mut rtcp_aes = [0; Aes128CmSha1_80::KEY_LEN];
        srtp_key.derive(crypto, LABEL_RTCP_AES, &mut rtcp_aes[..]);

        // RTCP SHA1 HMAC
        let rtcp_hmac = {
            let mut hmac = [0; Aes128CmSha1_80::HMAC_KEY_LEN];
            srtp_key.derive(crypto, LABEL_RTCP_AUTHENTICATION_KEY, &mut hmac[..]);
            hmac
        };

        // RTCP IV SALT
        let mut rtcp_salt = [0; Aes128CmSha1_80::SALT_LEN];
        srtp_key.derive(crypto, LABEL_RTCP_SALT, &mut rtcp_salt[..]);

        let rtp = Derived::Aes128CmSha1_80 {
            key: rtp_hmac,
            salt: rtp_salt,
            enc: crypto.aes_128_cm_sha1_80().create_cipher(rtp_aes, true),
            dec: crypto.aes_128_cm_sha1_80().create_cipher(rtp_aes, false),
        };

        let rtcp = Derived::Aes128CmSha1_80 {
            key: rtcp_hmac,
            salt: rtcp_salt,
            enc: crypto.aes_128_cm_sha1_80().create_cipher(rtcp_aes, true),
            dec: crypto.aes_128_cm_sha1_80().create_cipher(rtcp_aes, false),
        };

        (rtp, rtcp)
    }

    fn aead_aes_128_gcm(
        crypto: &dyn SrtpProvider,
        srtp_key: &SrtpKey<{ AeadAes128Gcm::KEY_LEN }, { AeadAes128Gcm::SALT_LEN }>,
    ) -> (Derived, Derived) {
        // RTP session key
        let mut rtp_aes = [0; AeadAes128Gcm::KEY_LEN];
        srtp_key.derive(crypto, LABEL_RTP_AES, &mut rtp_aes[..]);

        // RTP session salt
        let mut rtp_salt = [0; AeadAes128Gcm::SALT_LEN];
        srtp_key.derive(crypto, LABEL_RTP_SALT, &mut rtp_salt[..]);

        // RTCP session key
        let mut rtcp_aes = [0; AeadAes128Gcm::KEY_LEN];
        srtp_key.derive(crypto, LABEL_RTCP_AES, &mut rtcp_aes[..]);

        // RTCP session salt
        let mut rtcp_salt = [0; AeadAes128Gcm::SALT_LEN];
        srtp_key.derive(crypto, LABEL_RTCP_SALT, &mut rtcp_salt[..]);

        let rtp = Derived::AeadAes128Gcm {
            salt: rtp_salt,
            enc: crypto.aead_aes_128_gcm().create_cipher(rtp_aes, true),
            dec: crypto.aead_aes_128_gcm().create_cipher(rtp_aes, false),
        };

        let rtcp = Derived::AeadAes128Gcm {
            salt: rtcp_salt,
            enc: crypto.aead_aes_128_gcm().create_cipher(rtcp_aes, true),
            dec: crypto.aead_aes_128_gcm().create_cipher(rtcp_aes, false),
        };

        (rtp, rtcp)
    }

    fn aead_aes_256_gcm(
        crypto: &dyn SrtpProvider,
        srtp_key: &SrtpKey<{ AeadAes256Gcm::KEY_LEN }, { AeadAes256Gcm::SALT_LEN }>,
    ) -> (Derived, Derived) {
        // RTP session key
        let mut rtp_aes = [0; AeadAes256Gcm::KEY_LEN];
        srtp_key.derive(crypto, LABEL_RTP_AES, &mut rtp_aes[..]);

        // RTP session salt
        let mut rtp_salt = [0; AeadAes256Gcm::SALT_LEN];
        srtp_key.derive(crypto, LABEL_RTP_SALT, &mut rtp_salt[..]);

        // RTCP session key
        let mut rtcp_aes = [0; AeadAes256Gcm::KEY_LEN];
        srtp_key.derive(crypto, LABEL_RTCP_AES, &mut rtcp_aes[..]);

        // RTCP session salt
        let mut rtcp_salt = [0; AeadAes256Gcm::SALT_LEN];
        srtp_key.derive(crypto, LABEL_RTCP_SALT, &mut rtcp_salt[..]);

        let rtp = Derived::AeadAes256Gcm {
            salt: rtp_salt,
            enc: crypto.aead_aes_256_gcm().create_cipher(rtp_aes, true),
            dec: crypto.aead_aes_256_gcm().create_cipher(rtp_aes, false),
        };

        let rtcp = Derived::AeadAes256Gcm {
            salt: rtcp_salt,
            enc: crypto.aead_aes_256_gcm().create_cipher(rtcp_aes, true),
            dec: crypto.aead_aes_256_gcm().create_cipher(rtcp_aes, false),
        };

        (rtp, rtcp)
    }

    fn profile(&self) -> SrtpProfile {
        match self {
            Derived::Aes128CmSha1_80 { .. } => SrtpProfile::AES128_CM_SHA1_80,
            Derived::AeadAes128Gcm { .. } => SrtpProfile::AEAD_AES_128_GCM,
            Derived::AeadAes256Gcm { .. } => SrtpProfile::AEAD_AES_256_GCM,
        }
    }
}

impl fmt::Debug for Derived {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Derived")
    }
}

fn error_details(header: &RtpHeader, srtp_index: u64) -> String {
    format!(
        "SSRC: {} seq_no: {} pt: {} mid: {:?} rid: {:?} rid_repair: {:?} srtp_index: {}",
        header.ssrc,
        header.sequence_number,
        header.payload_type,
        header.ext_vals.mid,
        header.ext_vals.rid,
        header.ext_vals.rid_repair,
        srtp_index
    )
}

#[cfg(test)]
mod test {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone, Default)]
    struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

    impl Write for CapturedLogs {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(buf)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl CapturedLogs {
        fn lines(&self) -> Vec<String> {
            String::from_utf8(self.0.lock().unwrap().clone())
                .unwrap()
                .lines()
                .map(str::to_owned)
                .collect()
        }
    }

    #[test]
    fn aead_128_decryption_warnings_are_rate_limited() {
        for rtcp_first in [false, true] {
            check_decryption_warning_rate_limit(SrtpProfile::AEAD_AES_128_GCM, rtcp_first);
        }
    }

    #[test]
    fn aead_256_decryption_warnings_are_rate_limited() {
        for rtcp_first in [false, true] {
            check_decryption_warning_rate_limit(SrtpProfile::AEAD_AES_256_GCM, rtcp_first);
        }
    }

    fn check_decryption_warning_rate_limit(profile: SrtpProfile, rtcp_first: bool) {
        let logs = CapturedLogs::default();
        let writer = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::WARN)
            .with_writer(move || writer.clone())
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            let crypto = crate::crypto::test_default_provider();
            let mat = KeyingMaterial::new(&vec![0; profile.keying_material_len()]);
            let mut context = SrtpContext::new(crypto, profile, &mat, true);
            let plaintext_rtp = [0x80, 111, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 42];
            let header =
                RtpHeader::parse(&plaintext_rtp, &crate::rtp_::ExtensionMap::empty()).unwrap();
            let valid_rtp = context.protect_rtp(&plaintext_rtp, &header, 1);
            let plaintext_rtcp = [0x80, 201, 0, 2, 0, 0, 0, 1, 0, 0, 0, 0];
            let valid_rtcp = context.protect_rtcp(&plaintext_rtcp);

            assert_eq!(
                context.unprotect_rtp(&valid_rtp, &header, 1).unwrap(),
                &[42]
            );
            assert_eq!(context.unprotect_rtcp(&valid_rtcp).unwrap(), plaintext_rtcp);
            assert!(logs.lines().is_empty());

            // Corrupt both authentication tags.
            let mut rtp = valid_rtp.clone();
            let mut rtcp = valid_rtcp.clone();
            *rtp.last_mut().unwrap() ^= 1;
            let rtcp_tag_end = rtcp.len() - SRTCP_INDEX_LEN - 1;
            rtcp[rtcp_tag_end] ^= 1;
            if rtcp_first {
                assert!(context.unprotect_rtcp(&rtcp).is_none());
            } else {
                assert!(context.unprotect_rtp(&rtp, &header, 1).is_none());
            }
            assert_eq!(logs.lines().len(), 1, "the first failure warns immediately");
            assert!(logs.lines()[0].contains("suppressed_srtp=0 suppressed_srtcp=0"));

            for ssrc in 1u32..=2_046 {
                // Changing SSRCs must not create additional warning budgets.
                rtp[8..12].copy_from_slice(&ssrc.to_be_bytes());
                rtcp[4..8].copy_from_slice(&ssrc.to_be_bytes());
                let header = RtpHeader::parse(&rtp, &crate::rtp_::ExtensionMap::empty()).unwrap();
                assert!(context.unprotect_rtp(&rtp, &header, 1).is_none());
                assert!(context.unprotect_rtcp(&rtcp).is_none());
            }
            rtp[8..12].copy_from_slice(&plaintext_rtp[8..12]);
            rtcp[4..8].copy_from_slice(&plaintext_rtcp[4..8]);

            // Successful packets do not reset the warning budget or failure counts.
            assert!(context.unprotect_rtp(&valid_rtp, &header, 1).is_some());
            assert!(context.unprotect_rtcp(&valid_rtcp).is_some());
            assert!(context.unprotect_rtp(&rtp, &header, 1).is_none());
            assert!(context.unprotect_rtcp(&rtcp).is_none());
            assert_eq!(
                logs.lines().len(),
                1,
                "4,095 failures still yield one warning"
            );

            // Either protocol can trigger the next report at failure 4,096.
            if rtcp_first {
                assert!(context.unprotect_rtp(&rtp, &header, 1).is_none());
            } else {
                assert!(context.unprotect_rtcp(&rtcp).is_none());
            }
            let lines = logs.lines();
            assert_eq!(lines.len(), 2);
            assert!(lines[1].contains("suppressed_srtp=2047 suppressed_srtcp=2047"));

            for failure in 0..4_095 {
                if failure % 2 == 0 {
                    assert!(context.unprotect_rtp(&rtp, &header, 1).is_none());
                } else {
                    assert!(context.unprotect_rtcp(&rtcp).is_none());
                }
            }
            assert_eq!(
                logs.lines().len(),
                2,
                "the next report waits until failure 8,192"
            );
            assert!(context.unprotect_rtcp(&rtcp).is_none());
            let lines = logs.lines();
            assert_eq!(lines.len(), 3);
            assert!(lines[2].contains("suppressed_srtp=2048 suppressed_srtcp=2047"));

            for failure in 0..8_191 {
                if failure % 2 == 0 {
                    assert!(context.unprotect_rtp(&rtp, &header, 1).is_none());
                } else {
                    assert!(context.unprotect_rtcp(&rtcp).is_none());
                }
                if failure == 4_095 {
                    assert_eq!(logs.lines().len(), 3, "the reporting gap doubles");
                }
            }
            assert_eq!(
                logs.lines().len(),
                3,
                "16,383 failures yield three warnings"
            );
            assert!(context.unprotect_rtcp(&rtcp).is_none());
            let lines = logs.lines();
            assert_eq!(lines.len(), 4);
            assert!(lines[3].contains("suppressed_srtp=4096 suppressed_srtcp=4095"));

            let mut independent = SrtpContext::new(crypto, profile, &mat, true);
            assert!(independent.unprotect_rtcp(&rtcp).is_none());
            let lines = logs.lines();
            assert_eq!(lines.len(), 5, "independent contexts warn immediately");
            assert!(lines[4].contains("suppressed_srtp=0 suppressed_srtcp=0"));
            for (line, count) in lines.iter().zip([1, 4_096, 8_192, 16_384, 1]) {
                assert_eq!(line.split_whitespace().next(), Some("WARN"));
                assert!(line.contains(&format!("failures={count} ")));
            }
        });
    }

    #[test]
    fn derive_key() {
        crate::init_crypto_default();
        let provider = crate::crypto::from_feature_flags();
        let crypto = provider.srtp_provider;

        // https://tools.ietf.org/html/rfc3711#appendix-B.3
        //
        // Key Derivation Test Vectors.

        let master = [
            0xE1, 0xF9, 0x7A, 0x0D, 0x3E, 0x01, 0x8B, 0xE0, //
            0xD6, 0x4F, 0xA3, 0x2C, 0x06, 0xDE, 0x41, 0x39,
        ];

        let salt = [
            0x0E, 0xC6, 0x75, 0xAD, 0x49, 0x8A, 0xFE, //
            0xEB, 0xB6, 0x96, 0x0B, 0x3A, 0xAB, 0xE6,
        ];

        let sk = SrtpKey { master, salt };

        // aes crypto key
        let mut out = [0_u8; 16];
        sk.derive(crypto, 0, &mut out[..]);

        assert_eq!(
            out,
            [
                0xC6, 0x1E, 0x7A, 0x93, 0x74, 0x4F, 0x39, 0xEE, //
                0x10, 0x73, 0x4A, 0xFE, 0x3F, 0xF7, 0xA0, 0x87
            ]
        );

        // hmac
        let mut out = [0_u8; 20];
        sk.derive(crypto, 1, &mut out[..]);

        assert_eq!(
            out,
            [
                0xCE, 0xBE, 0x32, 0x1F, 0x6F, 0xF7, 0x71, 0x6B, //
                0x6F, 0xD4, 0xAB, 0x49, 0xAF, 0x25, 0x6A, 0x15, //
                0x6D, 0x38, 0xBA, 0xA4
            ]
        );

        // salt
        let mut out = [0_u8; 14];
        sk.derive(crypto, 2, &mut out[..]);

        assert_eq!(
            out,
            [
                0x30, 0xCB, 0xBC, 0x08, 0x86, 0x3D, 0x8C, //
                0x85, 0xD4, 0x9D, 0xB3, 0x4A, 0x9A, 0xE1
            ]
        );
    }

    mod test_aes128_cm_sha1_80 {
        use super::*;

        const MAT: [u8; 60] = [
            0x2C, 0xB0, 0x23, 0x46, 0xB4, 0x22, 0x76, 0xA6, 0x72, 0xCF, 0xD1, 0x43, 0xAE, 0xC2,
            0xD5, 0xEE, 0xDD, 0xDE, 0x55, 0xF0, 0xAD, 0x7B, 0xCA, 0xC2, 0x26, 0x66, 0xF1, 0xC6,
            0x38, 0x61, 0x73, 0xED, 0x6E, 0xB2, 0x5C, 0xB7, 0xD2, 0x6A, 0x61, 0xA1, 0xEE, 0x2C,
            0x21, 0x0A, 0xDA, 0xE7, 0x60, 0xAA, 0xA2, 0xFD, 0x67, 0xB6, 0x72, 0xC4, 0x1A, 0xED,
            0x10, 0x5F, 0x9D, 0x36,
        ];

        const SRTCP: &[u8] = &[
            // header
            0x80, 0xC8, 0x00, 0x06, //
            // ssrc
            0x3C, 0xD7, 0xCC, 0x13, //
            // encrypted payload
            0xB7, 0xC8, 0x31, 0xDC, 0xB7, 0x76, 0xCD, 0x8D, 0xC2, 0x6F, 0xDA, 0x1D, 0x9B, 0xFC,
            0x8E, 0xE6, 0x58, 0x9A, 0x1A, 0x8A, 0x49, 0x28, 0x9C, 0xAE, 0xB2, 0x64, 0x20, 0x0C,
            0x37, 0xD2, 0xD0, 0xA4, 0xAF, 0xAC, 0x63, 0x85, 0xFF, 0xC6, 0x0D, 0xEC, 0x7D, 0x06,
            0xD4, 0x87, 0x3D, 0xD3, 0xA8, 0xCC, //
            // E flag and srtcp index (1)
            0x80, 0x00, 0x00, 0x01, //
            // hmac
            0xB7, 0xBB, 0x52, 0x65, 0x21, 0xD1, 0xE7, 0x3C, 0x0F, 0xC0,
        ];

        const DECRYPTED_PAYLOAD: &[u8] = &[
            0x80, 0xc8, 0x00, 0x06, 0x3c, 0xd7, 0xcc, 0x13, 0xe2, 0xee, 0x35, 0xc8, 0x60, 0x4e,
            0x61, 0x8c, 0x26, 0xf3, 0x27, 0x34, 0x00, 0x00, 0x00, 0x43, 0x00, 0x00, 0x14, 0x07,
            0x81, 0xca, 0x00, 0x06, 0x3c, 0xd7, 0xcc, 0x13, 0x01, 0x10, 0x38, 0x6e, 0x46, 0x75,
            0x32, 0x68, 0x57, 0x66, 0x72, 0x4d, 0x44, 0x72, 0x47, 0x66, 0x34, 0x6f, 0x00, 0x00,
        ];

        #[test]
        fn unprotect_rtcp() {
            let key_mat = KeyingMaterial::new(&MAT);
            let crypto = crate::crypto::test_default_provider();
            let mut ctx_rx =
                SrtpContext::new(crypto, SrtpProfile::AES128_CM_SHA1_80, &key_mat, true);
            ctx_rx.srtcp_index = 1;

            let decrypted = ctx_rx.unprotect_rtcp(SRTCP).unwrap();

            assert_eq!(ctx_rx.srtcp_index, 1);
            // check srtcp_index in incoming was indeed 1
            let srtcp_index = SRTCP.len() - Aes128CmSha1_80::HMAC_TAG_LEN - SRTCP_INDEX_LEN;
            let e_and_i = &SRTCP[srtcp_index..(srtcp_index + 4)];
            assert_eq!(e_and_i, &0x8000_0001_u32.to_be_bytes());
            assert_eq!(decrypted, DECRYPTED_PAYLOAD);

            // Take us back to where we started.
            let encrypted = ctx_rx.protect_rtcp(&decrypted);
            assert_eq!(encrypted, SRTCP);
        }
    }

    mod test_aead_aes_128_gcm {
        use crate::crypto::AeadAes128Gcm;
        use crate::rtp_::ExtensionMap;

        use super::*;

        const TAG_LEN: usize = AeadAes128Gcm::TAG_LEN;

        mod rfc7714 {
            // Test vectors from RFC7714

            // Session Key (RTP and RTCP)
            pub(super) const KEY: [u8; 16] = [
                0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, //
                0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
            ];

            // Session Salt (RTP and RTCP)
            pub(super) const SALT: [u8; 12] = [
                0x51, 0x75, 0x69, 0x64, 0x20, 0x70, 0x72, 0x6f, 0x20, 0x71, 0x75, 0x6f,
            ];

            /// Full plaintext RTP packet. First 12 octets is the header
            pub(super) const PLAINTEXT_RTP_PACKET: &[u8] = &[
                0x80, 0x40, 0xf1, 0x7b, 0x80, 0x41, 0xf8, 0xd3, 0x55, 0x01, 0xa0, 0xb2, 0x47, 0x61,
                0x6c, 0x6c, 0x69, 0x61, 0x20, 0x65, 0x73, 0x74, 0x20, 0x6f, 0x6d, 0x6e, 0x69, 0x73,
                0x20, 0x64, 0x69, 0x76, 0x69, 0x73, 0x61, 0x20, 0x69, 0x6e, 0x20, 0x70, 0x61, 0x72,
                0x74, 0x65, 0x73, 0x20, 0x74, 0x72, 0x65, 0x73,
            ];

            /// Full encrypted RTP packet. First 12 octets is the header.
            pub(super) const PROTECTED_RTP_PACKET: &[u8] = &[
                0x80, 0x40, 0xf1, 0x7b, 0x80, 0x41, 0xf8, 0xd3, 0x55, 0x01, 0xa0, 0xb2, 0xf2, 0x4d,
                0xe3, 0xa3, 0xfb, 0x34, 0xde, 0x6c, 0xac, 0xba, 0x86, 0x1c, 0x9d, 0x7e, 0x4b, 0xca,
                0xbe, 0x63, 0x3b, 0xd5, 0x0d, 0x29, 0x4e, 0x6f, 0x42, 0xa5, 0xf4, 0x7a, 0x51, 0xc7,
                0xd1, 0x9b, 0x36, 0xde, 0x3a, 0xdf, 0x88, 0x33, 0x89, 0x9d, 0x7f, 0x27, 0xbe, 0xb1,
                0x6a, 0x91, 0x52, 0xcf, 0x76, 0x5e, 0xe4, 0x39, 0x0c, 0xce,
            ];

            // Full plaintext RTCP packet
            pub(super) const PLAINTEXT_RTCP_PACKET: &[u8] = &[
                0x81, 0xc8, 0x00, 0x0d, 0x4d, 0x61, 0x72, 0x73, 0x4e, 0x54, 0x50, 0x31, 0x4e, 0x54,
                0x50, 0x32, 0x52, 0x54, 0x50, 0x20, 0x00, 0x00, 0x04, 0x2a, 0x00, 0x00, 0xe9, 0x30,
                0x4c, 0x75, 0x6e, 0x61, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad,
                0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef,
            ];

            /// Full encrypted RTCP packet
            pub(super) const PROTECTED_RTCP_PACKET: &[u8] = &[
                0x81, 0xc8, 0x00, 0x0d, 0x4d, 0x61, 0x72, 0x73, 0x63, 0xe9, 0x48, 0x85, 0xdc, 0xda,
                0xb6, 0x7c, 0xa7, 0x27, 0xd7, 0x66, 0x2f, 0x6b, 0x7e, 0x99, 0x7f, 0xf5, 0xc0, 0xf7,
                0x6c, 0x06, 0xf3, 0x2d, 0xc6, 0x76, 0xa5, 0xf1, 0x73, 0x0d, 0x6f, 0xda, 0x4c, 0xe0,
                0x9b, 0x46, 0x86, 0x30, 0x3d, 0xed, 0x0b, 0xb9, 0x27, 0x5b, 0xc8, 0x4a, 0xa4, 0x58,
                0x96, 0xcf, 0x4d, 0x2f, 0xc5, 0xab, 0xf8, 0x72, 0x45, 0xd9, 0xea, 0xde, 0x80, 0x00,
                0x05, 0xd4,
            ];

            // A RTCP packet that hasn't been encrypted, only authenticated.
            pub(super) const TAGGED_RTCP_PACKET: &[u8] = &[
                // RTCP Packet
                0x81, 0xc8, 0x00, 0x0d, 0x4d, 0x61, 0x72, 0x73, 0x4e, 0x54, 0x50, 0x31, 0x4e, 0x54,
                0x50, 0x32, 0x52, 0x54, 0x50, 0x20, 0x00, 0x00, 0x04, 0x2a, 0x00, 0x00, 0xe9, 0x30,
                0x4c, 0x75, 0x6e, 0x61, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad,
                0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef, //
                // Tag
                0x84, 0x1d, 0xd9, 0x68, 0x3d, 0xd7, 0x8e, 0xc9, 0x2a, 0xe5, 0x87, 0x90, 0x12, 0x5f,
                0x62, 0xb3, //
                // SRTCP Index
                0x00, 0x00, 0x05, 0xd4,
            ];
        }

        #[test]
        fn protect_rtp_rfc_7714_test() {
            let mut context = make_rtp_context();

            let header =
                RtpHeader::parse(&rfc7714::PLAINTEXT_RTP_PACKET[..12], &ExtensionMap::empty())
                    .expect("header to parse");
            let out = context.protect_rtp(rfc7714::PLAINTEXT_RTP_PACKET, &header, 0);

            assert_eq!(
                out,
                rfc7714::PROTECTED_RTP_PACKET,
                "failed to encrypted packet.\n{:02x?}\n{:02x?}",
                out,
                rfc7714::PLAINTEXT_RTP_PACKET
            );
        }

        #[test]
        fn unprotect_rtp_rfc_7714_test() {
            let mut context = make_rtp_context();
            let header =
                RtpHeader::parse(&rfc7714::PROTECTED_RTP_PACKET[..12], &ExtensionMap::empty())
                    .expect("header to parse");

            let out = context
                .unprotect_rtp(rfc7714::PROTECTED_RTP_PACKET, &header, 0)
                .expect("decrypt rtp");

            assert_eq!(
                out,
                &rfc7714::PLAINTEXT_RTP_PACKET[12..],
                "failed to decrypt packet.\n{:02x?}\n{:02x?}",
                out,
                rfc7714::PLAINTEXT_RTP_PACKET
            );
        }

        #[test]
        fn symmetry_rtp_rfc_7714_test() {
            let mut context = make_rtp_context();

            // First we encrypt
            let header =
                RtpHeader::parse(&rfc7714::PLAINTEXT_RTP_PACKET[..12], &ExtensionMap::empty())
                    .expect("header to parse");
            let encrypted = context.protect_rtp(rfc7714::PLAINTEXT_RTP_PACKET, &header, 0);

            // Then we decrypt the resulting cipher text
            let header = RtpHeader::parse(&encrypted[..12], &ExtensionMap::empty())
                .expect("header to parse");
            let decrypted = context
                .unprotect_rtp(&encrypted, &header, 0)
                .expect("rtp unprotect");

            // And verify we get the input back.
            assert_eq!(decrypted, &rfc7714::PLAINTEXT_RTP_PACKET[12..]);
        }

        #[test]
        fn unprotect_rtp_should_fail_with_broken_tag_data() {
            let mut context = make_rtp_context();

            let header_buf = {
                let mut buf = rfc7714::PROTECTED_RTP_PACKET[..12].to_vec();
                // Mess with part of the sequence number, since this makes up part of the
                // authenticated additional data(AAD) the resulting authenticity tag should not
                // match.
                buf[3] ^= 0xFF;

                buf
            };

            let header =
                RtpHeader::parse(&header_buf, &ExtensionMap::empty()).expect("header to parse");

            let result = context.unprotect_rtp(rfc7714::PROTECTED_RTP_PACKET, &header, 0);
            assert!(
                result.is_none(),
                "Should fail to decrypt a SRTP packet that has mismatched \
                authenicated additional data"
            );
        }

        #[test]
        fn unprotect_rtp_should_fail_with_broken_null_tag() {
            let mut context = make_rtp_context();

            let input = {
                let mut input = rfc7714::PROTECTED_RTP_PACKET.to_vec();
                let len = input.len();
                input[len - TAG_LEN..].copy_from_slice(&[0; TAG_LEN]);

                input
            };

            let header =
                RtpHeader::parse(&input[..12], &ExtensionMap::empty()).expect("header to parse");

            let result = context.unprotect_rtp(&input, &header, 0);
            assert!(
                result.is_none(),
                "Should fail to decrypt a SRTP packet with null tag"
            );
        }

        #[test]
        fn protect_rtcp_rfc_7714_test() {
            let mut context = make_rtcp_context();

            let out = context.protect_rtcp(rfc7714::PLAINTEXT_RTCP_PACKET);

            assert!(
                out == rfc7714::PROTECTED_RTCP_PACKET,
                "Expected encrypted and tagged RTCP packet:\n{:02x?}\nGot:\n{:02x?}",
                rfc7714::PROTECTED_RTCP_PACKET,
                out
            );
        }

        #[test]
        fn unprotect_rtcp_rfc_auth_only_7714_test() {
            let mut context = make_rtcp_context();

            let out = context
                .unprotect_rtcp(rfc7714::TAGGED_RTCP_PACKET)
                .expect("Unprotect RTCP");

            assert_eq!(out, rfc7714::PLAINTEXT_RTCP_PACKET);
        }

        fn make_rtp_context() -> SrtpContext {
            crate::init_crypto_default();
            SrtpContext::new_aead_aes_128_gcm(
                rfc7714::KEY,
                rfc7714::SALT,
                rfc7714::KEY,
                rfc7714::SALT,
                0,
            )
        }

        fn make_rtcp_context() -> SrtpContext {
            crate::init_crypto_default();
            SrtpContext::new_aead_aes_128_gcm(
                rfc7714::KEY,
                rfc7714::SALT,
                rfc7714::KEY,
                rfc7714::SALT,
                0x000005d4,
            )
        }
    }

    mod test_aead_aes_256_gcm {
        use crate::crypto::AeadAes256Gcm;
        use crate::rtp_::ExtensionMap;

        use super::*;

        const TAG_LEN: usize = AeadAes256Gcm::TAG_LEN;

        mod rfc7714 {
            // Test vectors from RFC7714

            // Session Key (RTP and RTCP)
            pub(super) const KEY: [u8; 32] = [
                0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, //
                0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, //
                0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, //
                0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
            ];

            // Session Salt (RTP and RTCP)
            pub(super) const SALT: [u8; 12] = [
                0x51, 0x75, 0x69, 0x64, 0x20, 0x70, 0x72, 0x6f, 0x20, 0x71, 0x75, 0x6f,
            ];

            /// Full plaintext RTP packet. First 12 octets is the header
            pub(super) const PLAINTEXT_RTP_PACKET: &[u8] = &[
                0x80, 0x40, 0xf1, 0x7b, 0x80, 0x41, 0xf8, 0xd3, 0x55, 0x01, 0xa0, 0xb2, 0x47, 0x61,
                0x6c, 0x6c, 0x69, 0x61, 0x20, 0x65, 0x73, 0x74, 0x20, 0x6f, 0x6d, 0x6e, 0x69, 0x73,
                0x20, 0x64, 0x69, 0x76, 0x69, 0x73, 0x61, 0x20, 0x69, 0x6e, 0x20, 0x70, 0x61, 0x72,
                0x74, 0x65, 0x73, 0x20, 0x74, 0x72, 0x65, 0x73,
            ];

            /// Full encrypted RTP packet. First 12 octets is the header.
            pub(super) const PROTECTED_RTP_PACKET: &[u8] = &[
                0x80, 0x40, 0xf1, 0x7b, 0x80, 0x41, 0xf8, 0xd3, 0x55, 0x01, 0xa0, 0xb2, 0x32, 0xb1,
                0xde, 0x78, 0xa8, 0x22, 0xfe, 0x12, 0xef, 0x9f, 0x78, 0xfa, 0x33, 0x2e, 0x33, 0xaa,
                0xb1, 0x80, 0x12, 0x38, 0x9a, 0x58, 0xe2, 0xf3, 0xb5, 0x0b, 0x2a, 0x02, 0x76, 0xff,
                0xae, 0x0f, 0x1b, 0xa6, 0x37, 0x99, 0xb8, 0x7b, 0x7a, 0xa3, 0xdb, 0x36, 0xdf, 0xff,
                0xd6, 0xb0, 0xf9, 0xbb, 0x78, 0x78, 0xd7, 0xa7, 0x6c, 0x13,
            ];

            // Full plaintext RTCP packet
            pub(super) const PLAINTEXT_RTCP_PACKET: &[u8] = &[
                0x81, 0xc8, 0x00, 0x0d, 0x4d, 0x61, 0x72, 0x73, 0x4e, 0x54, 0x50, 0x31, 0x4e, 0x54,
                0x50, 0x32, 0x52, 0x54, 0x50, 0x20, 0x00, 0x00, 0x04, 0x2a, 0x00, 0x00, 0xe9, 0x30,
                0x4c, 0x75, 0x6e, 0x61, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad,
                0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef,
            ];

            /// Full encrypted RTCP packet
            pub(super) const PROTECTED_RTCP_PACKET: &[u8] = &[
                0x81, 0xc8, 0x00, 0x0d, 0x4d, 0x61, 0x72, 0x73, 0xd5, 0x0a, 0xe4, 0xd1, 0xf5, 0xce,
                0x5d, 0x30, 0x4b, 0xa2, 0x97, 0xe4, 0x7d, 0x47, 0x0c, 0x28, 0x2c, 0x3e, 0xce, 0x5d,
                0xbf, 0xfe, 0x0a, 0x50, 0xa2, 0xea, 0xa5, 0xc1, 0x11, 0x05, 0x55, 0xbe, 0x84, 0x15,
                0xf6, 0x58, 0xc6, 0x1d, 0xe0, 0x47, 0x6f, 0x1b, 0x6f, 0xad, 0x1d, 0x1e, 0xb3, 0x0c,
                0x44, 0x46, 0x83, 0x9f, 0x57, 0xff, 0x6f, 0x6c, 0xb2, 0x6a, 0xc3, 0xbe, 0x80, 0x00,
                0x05, 0xd4,
            ];

            // A RTCP packet that hasn't been encrypted, only authenticated.
            pub(super) const TAGGED_RTCP_PACKET: &[u8] = &[
                // RTCP Packet
                0x81, 0xc8, 0x00, 0x0d, 0x4d, 0x61, 0x72, 0x73, 0x4e, 0x54, 0x50, 0x31, 0x4e, 0x54,
                0x50, 0x32, 0x52, 0x54, 0x50, 0x20, 0x00, 0x00, 0x04, 0x2a, 0x00, 0x00, 0xe9, 0x30,
                0x4c, 0x75, 0x6e, 0x61, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad,
                0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef, //
                // Tag
                0x91, 0xdb, 0x4a, 0xfb, 0xfe, 0xee, 0x5a, 0x97, 0x8f, 0xab, 0x43, 0x93, 0xed, 0x26,
                0x15, 0xfe, //
                // SRTCP Index
                0x00, 0x00, 0x05, 0xd4,
            ];
        }

        #[test]
        fn protect_rtp_rfc_7714_test() {
            let mut context = make_rtp_context();

            let header =
                RtpHeader::parse(&rfc7714::PLAINTEXT_RTP_PACKET[..12], &ExtensionMap::empty())
                    .expect("header to parse");
            let out = context.protect_rtp(rfc7714::PLAINTEXT_RTP_PACKET, &header, 0);

            assert_eq!(
                out,
                rfc7714::PROTECTED_RTP_PACKET,
                "failed to encrypted packet.\n{:02x?}\n{:02x?}",
                out,
                rfc7714::PLAINTEXT_RTP_PACKET
            );
        }

        #[test]
        fn unprotect_rtp_rfc_7714_test() {
            let mut context = make_rtp_context();
            let header =
                RtpHeader::parse(&rfc7714::PROTECTED_RTP_PACKET[..12], &ExtensionMap::empty())
                    .expect("header to parse");

            let out = context
                .unprotect_rtp(rfc7714::PROTECTED_RTP_PACKET, &header, 0)
                .expect("decrypt rtp");

            assert_eq!(
                out,
                &rfc7714::PLAINTEXT_RTP_PACKET[12..],
                "failed to decrypt packet.\n{:02x?}\n{:02x?}",
                out,
                rfc7714::PLAINTEXT_RTP_PACKET
            );
        }

        #[test]
        fn symmetry_rtp_rfc_7714_test() {
            let mut context = make_rtp_context();

            // First we encrypt
            let header =
                RtpHeader::parse(&rfc7714::PLAINTEXT_RTP_PACKET[..12], &ExtensionMap::empty())
                    .expect("header to parse");
            let encrypted = context.protect_rtp(rfc7714::PLAINTEXT_RTP_PACKET, &header, 0);

            // Then we decrypt the resulting cipher text
            let header = RtpHeader::parse(&encrypted[..12], &ExtensionMap::empty())
                .expect("header to parse");
            let decrypted = context
                .unprotect_rtp(&encrypted, &header, 0)
                .expect("rtp unprotect");

            // And verify we get the input back.
            assert_eq!(decrypted, &rfc7714::PLAINTEXT_RTP_PACKET[12..]);
        }

        #[test]
        fn unprotect_rtp_should_fail_with_broken_tag_data() {
            let mut context = make_rtp_context();

            let header_buf = {
                let mut buf = rfc7714::PROTECTED_RTP_PACKET[..12].to_vec();
                // Mess with part of the sequence number, since this makes up part of the
                // authenticated additional data(AAD) the resulting authenticity tag should not
                // match.
                buf[3] ^= 0xFF;

                buf
            };

            let header =
                RtpHeader::parse(&header_buf, &ExtensionMap::empty()).expect("header to parse");

            let result = context.unprotect_rtp(rfc7714::PROTECTED_RTP_PACKET, &header, 0);
            assert!(
                result.is_none(),
                "Should fail to decrypt a SRTP packet that has mismatched \
                authenicated additional data"
            );
        }

        #[test]
        fn unprotect_rtp_should_fail_with_broken_null_tag() {
            let mut context = make_rtp_context();

            let input = {
                let mut input = rfc7714::PROTECTED_RTP_PACKET.to_vec();
                let len = input.len();
                input[len - TAG_LEN..].copy_from_slice(&[0; TAG_LEN]);

                input
            };

            let header =
                RtpHeader::parse(&input[..12], &ExtensionMap::empty()).expect("header to parse");

            let result = context.unprotect_rtp(&input, &header, 0);
            assert!(
                result.is_none(),
                "Should fail to decrypt a SRTP packet with null tag"
            );
        }

        #[test]
        fn protect_rtcp_rfc_7714_test() {
            let mut context = make_rtcp_context();

            let out = context.protect_rtcp(rfc7714::PLAINTEXT_RTCP_PACKET);

            assert!(
                out == rfc7714::PROTECTED_RTCP_PACKET,
                "Expected encrypted and tagged RTCP packet:\n{:02x?}\nGot:\n{:02x?}",
                rfc7714::PROTECTED_RTCP_PACKET,
                out
            );
        }

        #[test]
        fn unprotect_rtcp_rfc_auth_only_7714_test() {
            let mut context = make_rtcp_context();

            let out = context
                .unprotect_rtcp(rfc7714::TAGGED_RTCP_PACKET)
                .expect("Unprotect RTCP");

            assert_eq!(out, rfc7714::PLAINTEXT_RTCP_PACKET);
        }

        fn make_rtp_context() -> SrtpContext {
            crate::init_crypto_default();
            SrtpContext::new_aead_aes_256_gcm(
                rfc7714::KEY,
                rfc7714::SALT,
                rfc7714::KEY,
                rfc7714::SALT,
                0,
            )
        }

        fn make_rtcp_context() -> SrtpContext {
            crate::init_crypto_default();
            SrtpContext::new_aead_aes_256_gcm(
                rfc7714::KEY,
                rfc7714::SALT,
                rfc7714::KEY,
                rfc7714::SALT,
                0x000005d4,
            )
        }
    }
}
