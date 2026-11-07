//! PRF and HKDF key derivation built on top of [`HmacProvider`].
//!
//! Both TLS 1.2 PRF and TLS 1.3 HKDF are pure compositions of HMAC calls.
//! This module provides generic implementations so that crypto backends only
//! need to implement [`HmacProvider`] — no separate PRF or HKDF providers.

use crate::CryptoError;
use crate::buffer::Buf;
use crate::types::HashAlgorithm;

use super::HmacProvider;

/// Maximum HMAC output size we support (SHA-384 = 48 bytes).
const MAX_HASH_LEN: usize = 48;

// ============================================================================
// TLS 1.2 PRF (RFC 5246 Section 5)
// ============================================================================

/// TLS 1.2 PRF: `PRF(secret, label, seed)` writing `output_len` bytes to `out`.
///
/// Uses `scratch` for temporary concatenation of label+seed.
#[allow(clippy::too_many_arguments)]
pub fn prf_tls12(
    hmac: &dyn HmacProvider,
    secret: &[u8],
    label: &str,
    seed: &[u8],
    out: &mut Buf,
    output_len: usize,
    scratch: &mut Buf,
    hash: HashAlgorithm,
) -> Result<(), CryptoError> {
    let mut hmac_a = [0u8; MAX_HASH_LEN];

    // Build label + seed
    scratch.clear();
    scratch.extend_from_slice(label.as_bytes());
    scratch.extend_from_slice(seed);
    let label_seed = scratch.as_ref();

    // A(1) = HMAC(secret, label_seed)
    let hash_len = hmac.hmac(hash, secret, label_seed, &mut hmac_a)?;

    // Build payload = A(i) || label || seed
    scratch.clear();
    scratch.extend_from_slice(&hmac_a[..hash_len]);
    scratch.extend_from_slice(label.as_bytes());
    scratch.extend_from_slice(seed);
    let payload = scratch.as_mut();

    out.clear();
    while out.len() < output_len {
        // P(i) = HMAC(secret, A(i) || label || seed)
        let mut hmac_block = [0u8; MAX_HASH_LEN];
        let block_len = hmac.hmac(hash, secret, payload, &mut hmac_block)?;

        let remaining = output_len - out.len();
        let to_copy = remaining.min(block_len);
        out.extend_from_slice(&hmac_block[..to_copy]);

        if out.len() < output_len {
            // A(i+1) = HMAC(secret, A(i))
            hmac.hmac(hash, secret, &payload[..hash_len], &mut hmac_a)?;
            payload[..hash_len].copy_from_slice(&hmac_a[..hash_len]);
        }
    }

    Ok(())
}

// ============================================================================
// HKDF (RFC 5869)
// ============================================================================

/// HKDF-Extract: `PRK = HMAC-Hash(salt, IKM)`.
pub fn hkdf_extract(
    hmac: &dyn HmacProvider,
    hash: HashAlgorithm,
    salt: &[u8],
    ikm: &[u8],
    out: &mut Buf,
) -> Result<(), CryptoError> {
    out.clear();

    let hash_len = hash.output_len();
    let zero_salt: Vec<u8>;
    let actual_salt = if salt.is_empty() {
        zero_salt = vec![0u8; hash_len];
        &zero_salt[..]
    } else {
        salt
    };

    let mut prk = [0u8; MAX_HASH_LEN];
    let prk_len = hmac.hmac(hash, actual_salt, ikm, &mut prk)?;
    out.extend_from_slice(&prk[..prk_len]);
    Ok(())
}

/// HKDF-Expand: expand `prk` to `output_len` bytes.
pub fn hkdf_expand(
    hmac: &dyn HmacProvider,
    hash: HashAlgorithm,
    prk: &[u8],
    info: &[u8],
    out: &mut Buf,
    output_len: usize,
) -> Result<(), CryptoError> {
    let hash_len = hash.output_len();
    let n = output_len.div_ceil(hash_len);
    if n > 255 {
        return Err(CryptoError::HkdfOutputTooLong);
    }

    let mut t_prev = [0u8; MAX_HASH_LEN];
    let mut t_prev_len = 0usize;

    out.clear();
    for i in 1..=n {
        let mut input = Vec::with_capacity(t_prev_len + info.len() + 1);
        input.extend_from_slice(&t_prev[..t_prev_len]);
        input.extend_from_slice(info);
        input.push(i as u8);

        t_prev_len = hmac.hmac(hash, prk, &input, &mut t_prev)?;

        let remaining = output_len - out.len();
        let to_copy = remaining.min(t_prev_len);
        out.extend_from_slice(&t_prev[..to_copy]);
    }

    Ok(())
}

/// HKDF-Expand-Label for TLS 1.3 (RFC 8446 Section 7.1).
///
/// Uses the `"tls13 "` prefix.
pub fn hkdf_expand_label(
    hmac: &dyn HmacProvider,
    hash: HashAlgorithm,
    secret: &[u8],
    label: &[u8],
    context: &[u8],
    out: &mut Buf,
    output_len: usize,
) -> Result<(), CryptoError> {
    let info = build_hkdf_label(b"tls13 ", label, context, output_len)?;
    hkdf_expand(hmac, hash, secret, &info, out, output_len)
}

/// HKDF-Expand-Label for DTLS 1.3 (RFC 9147).
///
/// Uses the `"dtls13"` prefix (no trailing space).
pub fn hkdf_expand_label_dtls13(
    hmac: &dyn HmacProvider,
    hash: HashAlgorithm,
    secret: &[u8],
    label: &[u8],
    context: &[u8],
    out: &mut Buf,
    output_len: usize,
) -> Result<(), CryptoError> {
    let info = build_hkdf_label(b"dtls13", label, context, output_len)?;
    hkdf_expand(hmac, hash, secret, &info, out, output_len)
}

/// Build the HkdfLabel structure.
///
/// ```text
/// struct {
///     uint16 length;
///     opaque label<6..255> = prefix + Label;
///     opaque context<0..255> = Context;
/// } HkdfLabel;
/// ```
fn build_hkdf_label(
    prefix: &[u8],
    label: &[u8],
    context: &[u8],
    output_len: usize,
) -> Result<Vec<u8>, CryptoError> {
    let full_label_len = prefix.len() + label.len();

    if full_label_len > 255 {
        return Err(CryptoError::HkdfLabelTooLong);
    }
    if context.len() > 255 {
        return Err(CryptoError::HkdfContextTooLong);
    }
    if output_len > 65535 {
        return Err(CryptoError::HkdfOutputLengthTooLarge);
    }

    let info_len = 2 + 1 + full_label_len + 1 + context.len();
    let mut info = Vec::with_capacity(info_len);

    // uint16 length
    info.extend_from_slice(&(output_len as u16).to_be_bytes());
    // opaque label
    info.push(full_label_len as u8);
    info.extend_from_slice(prefix);
    info.extend_from_slice(label);
    // opaque context
    info.push(context.len() as u8);
    info.extend_from_slice(context);

    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex_to_vec(hex: &str) -> Vec<u8> {
        let hex = hex.replace([' ', '\n'], "");
        let mut v = Vec::new();
        for i in 0..hex.len() / 2 {
            // unwrap: test-only hex parsing
            let byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
            v.push(byte);
        }
        v
    }

    fn slice_to_hex(data: &[u8]) -> String {
        let mut s = String::new();
        for byte in data.iter() {
            s.push_str(&format!("{:02x}", byte));
        }
        s
    }

    /// Convert an ASCII hex array into a byte array at compile time.
    macro_rules! hex_as_bytes {
        ($input:expr) => {{
            const fn from_hex_char(c: u8) -> u8 {
                match c {
                    b'0'..=b'9' => c - b'0',
                    b'a'..=b'f' => c - b'a' + 10,
                    b'A'..=b'F' => c - b'A' + 10,
                    _ => panic!("Invalid hex character"),
                }
            }

            const INPUT: &[u8] = $input;
            const LEN: usize = INPUT.len();
            const OUTPUT_LEN: usize = LEN / 2;

            const fn convert() -> [u8; OUTPUT_LEN] {
                assert!(LEN % 2 == 0, "Hex string length must be even");

                let mut out = [0u8; OUTPUT_LEN];
                let mut i = 0;
                while i < LEN {
                    out[i / 2] = (from_hex_char(INPUT[i]) << 4) | from_hex_char(INPUT[i + 1]);
                    i += 2;
                }
                out
            }

            convert()
        }};
    }

    /// We need a concrete HmacProvider for tests. Use the default feature-gated one.
    fn hmac_provider() -> &'static dyn HmacProvider {
        #[cfg(feature = "aws-lc-rs")]
        {
            &crate::crypto::aws_lc_rs::hmac::HMAC_PROVIDER
        }
        #[cfg(all(not(feature = "aws-lc-rs"), feature = "rust-crypto"))]
        {
            &crate::crypto::rust_crypto::hmac::HMAC_PROVIDER
        }
    }

    // ========================================================================
    // HMAC-SHA-256 Test Vectors from RFC 4231
    // ========================================================================

    #[test]
    fn hmac_sha256_test_case_1() {
        let key = hex_to_vec("0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b");
        let data = b"Hi There";
        let expected = "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7";

        let result = hmac_provider().hmac_sha256(&key, data).unwrap();
        assert_eq!(slice_to_hex(&result), expected);
    }

    #[test]
    fn hmac_sha256_test_case_2() {
        let key = b"Jefe";
        let data = b"what do ya want for nothing?";
        let expected = "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843";

        let result = hmac_provider().hmac_sha256(key, data).unwrap();
        assert_eq!(slice_to_hex(&result), expected);
    }

    #[test]
    fn hmac_sha256_test_case_3() {
        let key = hex_to_vec("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let data = hex_to_vec(
            "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd\
             dddddddddddddddddddddddddddddddddddd",
        );
        let expected = "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe";

        let result = hmac_provider().hmac_sha256(&key, &data).unwrap();
        assert_eq!(slice_to_hex(&result), expected);
    }

    #[test]
    fn hmac_sha256_test_case_4() {
        let key = hex_to_vec("0102030405060708090a0b0c0d0e0f10111213141516171819");
        let data = hex_to_vec(
            "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd\
             cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
        );
        let expected = "82558a389a443c0ea4cc819899f2083a85f0faa3e578f8077a2e3ff46729665b";

        let result = hmac_provider().hmac_sha256(&key, &data).unwrap();
        assert_eq!(slice_to_hex(&result), expected);
    }

    #[test]
    fn hmac_sha256_test_case_6() {
        // Test with a key larger than block size (> 64 bytes)
        let key = vec![0xaa; 131];
        let data = b"Test Using Larger Than Block-Size Key - Hash Key First";
        let expected = "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54";

        let result = hmac_provider().hmac_sha256(&key, data).unwrap();
        assert_eq!(slice_to_hex(&result), expected);
    }

    #[test]
    fn hmac_sha256_test_case_7() {
        // Test with a key larger than block size and large data
        let key = vec![0xaa; 131];
        let data =
            b"This is a test using a larger than block-size key and a larger than block-size \
              data. The key needs to be hashed before being used by the HMAC algorithm.";
        let expected = "9b09ffa71b942fcb27635fbcd5b0e944bfdc63644f0713938a7f51535c3a35e2";

        let result = hmac_provider().hmac_sha256(&key, data).unwrap();
        assert_eq!(slice_to_hex(&result), expected);
    }

    // ========================================================================
    // TLS 1.2 PRF
    // ========================================================================

    #[test]
    fn prf_tls12_sha256() {
        // Test vector from https://github.com/xomexh/TLS-PRF
        let mut output = Buf::new();
        let mut scratch = Buf::new();
        prf_tls12(
            hmac_provider(),
            &hex_as_bytes!(b"9bbe436ba940f017b17652849a71db35"),
            "test label",
            &hex_as_bytes!(b"a0ba9f936cda311827a6f796ffd5198c"),
            &mut output,
            100,
            &mut scratch,
            HashAlgorithm::SHA256,
        )
        .unwrap();
        assert_eq!(
            output.as_ref(),
            &hex_as_bytes!(
                b"e3f229ba727be17b8d122620557cd453c2aab21d\
                  07c3d495329b52d4e61edb5a6b301791e90d35c9\
                  c9a46b4e14baf9af0fa022f7077def17abfd3797\
                  c0564bab4fbc91666e9def9b97fce34f796789ba\
                  a48082d122ee42c5a72e5a5110fff70187347b66"
            )
        );
    }

    // ========================================================================
    // HKDF Test Vectors from RFC 5869
    // ========================================================================

    #[test]
    fn hkdf_sha256_rfc5869_case1() {
        // Test Case 1 - Basic test case with SHA-256
        let ikm = hex_to_vec("0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b");
        let salt = hex_to_vec("000102030405060708090a0b0c");
        let info = hex_to_vec("f0f1f2f3f4f5f6f7f8f9");
        let expected_prk = "077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5";
        let expected_okm = "3cb25f25faacd57a90434f64d0362f2a\
                            2d2d0a90cf1a5a4c5db02d56ecc4c5bf\
                            34007208d5b887185865";

        let h = hmac_provider();

        let mut prk = Buf::new();
        hkdf_extract(h, HashAlgorithm::SHA256, &salt, &ikm, &mut prk).unwrap();
        assert_eq!(slice_to_hex(prk.as_ref()), expected_prk);

        let mut okm = Buf::new();
        hkdf_expand(h, HashAlgorithm::SHA256, prk.as_ref(), &info, &mut okm, 42).unwrap();
        assert_eq!(slice_to_hex(okm.as_ref()), expected_okm);
    }

    #[test]
    fn hkdf_sha256_rfc5869_case2() {
        // Test Case 2 - Longer inputs/outputs with SHA-256
        let ikm = hex_to_vec(
            "000102030405060708090a0b0c0d0e0f\
             101112131415161718191a1b1c1d1e1f\
             202122232425262728292a2b2c2d2e2f\
             303132333435363738393a3b3c3d3e3f\
             404142434445464748494a4b4c4d4e4f",
        );
        let salt = hex_to_vec(
            "606162636465666768696a6b6c6d6e6f\
             707172737475767778797a7b7c7d7e7f\
             808182838485868788898a8b8c8d8e8f\
             909192939495969798999a9b9c9d9e9f\
             a0a1a2a3a4a5a6a7a8a9aaabacadaeaf",
        );
        let info = hex_to_vec(
            "b0b1b2b3b4b5b6b7b8b9babbbcbdbebf\
             c0c1c2c3c4c5c6c7c8c9cacbcccdcecf\
             d0d1d2d3d4d5d6d7d8d9dadbdcdddedf\
             e0e1e2e3e4e5e6e7e8e9eaebecedeeef\
             f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff",
        );
        let expected_prk = "06a6b88c5853361a06104c9ceb35b45cef760014904671014a193f40c15fc244";
        let expected_okm = "b11e398dc80327a1c8e7f78c596a4934\
                            4f012eda2d4efad8a050cc4c19afa97c\
                            59045a99cac7827271cb41c65e590e09\
                            da3275600c2f09b8367793a9aca3db71\
                            cc30c58179ec3e87c14c01d5c1f3434f\
                            1d87";

        let h = hmac_provider();

        let mut prk = Buf::new();
        hkdf_extract(h, HashAlgorithm::SHA256, &salt, &ikm, &mut prk).unwrap();
        assert_eq!(slice_to_hex(prk.as_ref()), expected_prk);

        let mut okm = Buf::new();
        hkdf_expand(h, HashAlgorithm::SHA256, prk.as_ref(), &info, &mut okm, 82).unwrap();
        assert_eq!(slice_to_hex(okm.as_ref()), expected_okm);
    }

    #[test]
    fn hkdf_sha256_rfc5869_case3() {
        // Test Case 3 - Zero-length salt and info with SHA-256
        let ikm = hex_to_vec("0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b");
        let salt = vec![];
        let info = vec![];
        let expected_prk = "19ef24a32c717b167f33a91d6f648bdf96596776afdb6377ac434c1c293ccb04";
        let expected_okm = "8da4e775a563c18f715f802a063c5a31\
                            b8a11f5c5ee1879ec3454e5f3c738d2d\
                            9d201395faa4b61a96c8";

        let h = hmac_provider();

        let mut prk = Buf::new();
        hkdf_extract(h, HashAlgorithm::SHA256, &salt, &ikm, &mut prk).unwrap();
        assert_eq!(slice_to_hex(prk.as_ref()), expected_prk);

        let mut okm = Buf::new();
        hkdf_expand(h, HashAlgorithm::SHA256, prk.as_ref(), &info, &mut okm, 42).unwrap();
        assert_eq!(slice_to_hex(okm.as_ref()), expected_okm);
    }

    // ========================================================================
    // HKDF-Expand-Label
    // ========================================================================

    #[test]
    fn hkdf_expand_label_basic() {
        let h = hmac_provider();
        let secret = [0u8; 32];
        let mut out = Buf::new();

        hkdf_expand_label(h, HashAlgorithm::SHA256, &secret, b"key", &[], &mut out, 16).unwrap();
        assert_eq!(out.len(), 16);

        hkdf_expand_label(
            h,
            HashAlgorithm::SHA256,
            &secret,
            b"iv",
            &[1, 2, 3, 4],
            &mut out,
            12,
        )
        .unwrap();
        assert_eq!(out.len(), 12);
    }

    #[test]
    fn hkdf_expand_label_dtls13_basic() {
        let h = hmac_provider();
        let secret = [0u8; 32];
        let mut out = Buf::new();

        hkdf_expand_label_dtls13(h, HashAlgorithm::SHA256, &secret, b"key", &[], &mut out, 16)
            .unwrap();
        assert_eq!(out.len(), 16);

        // TLS 1.3 and DTLS 1.3 with same inputs should produce different outputs
        let mut tls_out = Buf::new();
        let mut dtls_out = Buf::new();

        hkdf_expand_label(
            h,
            HashAlgorithm::SHA256,
            &secret,
            b"key",
            &[],
            &mut tls_out,
            16,
        )
        .unwrap();
        hkdf_expand_label_dtls13(
            h,
            HashAlgorithm::SHA256,
            &secret,
            b"key",
            &[],
            &mut dtls_out,
            16,
        )
        .unwrap();

        assert_ne!(tls_out.as_ref(), dtls_out.as_ref());
    }
}
