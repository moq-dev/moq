/// Test algorithm implementations for the CryptoProvider
///
/// This contains tests run against the installed crypto provider to verify
/// hash, cipher and signature implementations against known test vectors.
use str0m::crypto::CryptoProvider;

mod common;
use common::init_crypto_default;

fn get_provider() -> &'static CryptoProvider {
    init_crypto_default();
    CryptoProvider::get_default().unwrap()
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

mod sha1 {
    fn hash(input: &[u8]) -> String {
        super::get_provider()
            .sha1_provider
            .sha1(input)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    #[test]
    fn test_abc() {
        let provider = super::get_provider().sha1_provider;
        assert_eq!(
            provider.sha1(b"abc"),
            hex_as_bytes!(b"a9993e364706816aba3e25717850c26c9cd0d89d")
        );
    }

    #[test]
    fn fips_180_vectors() {
        assert_eq!(hash(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(hash(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            hash(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
        assert_eq!(
            hash(&[b'a'; 1_000_000]),
            "34aa973cd4c4daa4f61eeb2bdbad27316534016f"
        );
    }

    #[test]
    fn padding_boundaries() {
        let expected = [
            (55, "c1c8bbdc22796e28c0e15163d20899b65621d65a"),
            (56, "c2db330f6083854c99d4b5bfb6e8f29f201be699"),
            (63, "03f09f5b158a7a8cdad920bddc29b81c18a551f5"),
            (64, "0098ba824b5c16427bd7a1122a5a442a25ec644d"),
            (65, "11655326c708d70319be2610e8a57d9a5b959d3b"),
        ];
        for (len, digest) in expected {
            assert_eq!(hash(&vec![b'a'; len]), digest, "length {len}");
        }
    }
}

mod sha256 {
    fn verify_test_vector(input: &[u8], expected: &[u8; 32]) {
        let sha256_provider = super::get_provider().sha256_provider;
        let hash = sha256_provider.sha256(input);
        assert_eq!(hash, *expected);
    }

    #[test]
    fn test_hello_world() {
        verify_test_vector(
            b"hello world",
            &hex_as_bytes!(b"b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"),
        );
    }

    #[test]
    fn test_empty() {
        // SHA-256 of "" - NIST test vector
        verify_test_vector(
            b"",
            &hex_as_bytes!(b"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
        );
    }

    #[test]
    fn test_abc() {
        // SHA-256 of "abc" - NIST test vector
        verify_test_vector(
            b"abc",
            &hex_as_bytes!(b"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
        );
    }
}

mod sha1_hmac {
    fn verify_test_vector(key: &[u8], payloads: &[&[u8]], expected: &[u8; 20]) {
        let sha1_hmac_provider = super::get_provider().sha1_hmac_provider;
        let hmac = sha1_hmac_provider.sha1_hmac(key, payloads);
        assert_eq!(hmac, *expected);
    }

    #[test]
    fn test_rfc2202_test_case_1() {
        verify_test_vector(
            &[0x0b; 20],
            &[b"Hi There"],
            &hex_as_bytes!(b"b617318655057264e28bc0b6fb378c8ef146be00"),
        );
    }

    #[test]
    fn test_rfc2202_test_case_2() {
        verify_test_vector(
            b"Jefe",
            &[b"what do ya want for nothing?"],
            &hex_as_bytes!(b"effcdf6ae5eb2fa2d27416d5f184df9c259a7c79"),
        );
    }

    #[test]
    fn test_rfc2202_test_case_3() {
        verify_test_vector(
            &[0xaa; 20],
            &[&[0xddu8; 50]],
            &hex_as_bytes!(b"125d7342b9ac11cd91a39af48aa17b4f63f175d3"),
        );
    }

    #[test]
    fn test_rfc2202_test_case_4() {
        verify_test_vector(
            &[
                1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
                24, 25,
            ],
            &[&[0xcdu8; 50]],
            &hex_as_bytes!(b"4c9007f4026250c6bc8414f9bf50c86c2d7235da"),
        );
    }

    #[test]
    fn test_rfc2202_test_case_5() {
        verify_test_vector(
            &[0x0c; 20],
            &[b"Test With Truncation"],
            &hex_as_bytes!(b"4c1a03424b55e07fe7f27be1d58bb9324a9a5a04"),
        );
    }

    #[test]
    fn test_rfc2202_test_case_6() {
        verify_test_vector(
            &[0xaa; 80],
            &[b"Test Using Larger Than Block-Size Key - Hash Key First"],
            &hex_as_bytes!(b"aa4ae5e15272d00e95705637ce8a3b55ed402112"),
        );
    }

    #[test]
    fn test_rfc2202_test_case_7() {
        verify_test_vector(
            &[0xaa; 80],
            &[
                b"Test Using Larger Than Block-Size Key and Larger ",
                b"Than One Block-Size Data",
            ],
            &hex_as_bytes!(b"e8e99d0f45237d786d6bbaa7965c7808bbff1a91"),
        );
    }
}

mod aes_128_cm_sha1_80 {
    fn verify_test_vector(
        key: [u8; 16],
        iv: [u8; 16],
        encrypt: bool,
        input: &[u8],
        expected: &[u8],
    ) {
        let srtp_provider = super::get_provider().srtp_provider;
        let cipher_provider = srtp_provider.aes_128_cm_sha1_80();
        let mut cipher = cipher_provider.create_cipher(key, encrypt);

        let mut output = vec![0u8; input.len()];
        if encrypt {
            cipher.encrypt(&iv, input, &mut output).unwrap();
        } else {
            cipher.decrypt(&iv, input, &mut output).unwrap();
        }
        assert_eq!(expected, &output)
    }

    // AES-128-CTR Test Vectors from NIST SP 800-38A
    // https://nvlpubs.nist.gov/nistpubs/Legacy/SP/nistspecialpublication800-38a.pdf
    // Section F.5.1

    #[test]
    fn test_ctr_encrypt() {
        verify_test_vector(
            hex_as_bytes!(b"2b7e151628aed2a6abf7158809cf4f3c"),
            hex_as_bytes!(b"f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff"),
            true,
            &hex_as_bytes!(b"6bc1bee22e409f96e93d7e117393172a"),
            &hex_as_bytes!(b"874d6191b620e3261bef6864990db6ce"),
        );
    }

    #[test]
    fn test_decrypt() {
        verify_test_vector(
            hex_as_bytes!(b"2b7e151628aed2a6abf7158809cf4f3c"),
            hex_as_bytes!(b"f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff"),
            false,
            &hex_as_bytes!(b"874d6191b620e3261bef6864990db6ce"),
            &hex_as_bytes!(b"6bc1bee22e409f96e93d7e117393172a"),
        );
    }

    #[test]
    fn test_multiple_blocks() {
        verify_test_vector(
            hex_as_bytes!(b"2b7e151628aed2a6abf7158809cf4f3c"),
            hex_as_bytes!(b"f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff"),
            true,
            &hex_as_bytes!(
                b"6bc1bee22e409f96e93d7e117393172a\
             ae2d8a571e03ac9c9eb76fac45af8e51\
             30c81c46a35ce411e5fbc1191a0a52ef"
            ),
            &hex_as_bytes!(
                b"874d6191b620e3261bef6864990db6ce\
             9806f66b7970fdff8617187bb9fffdff\
             5ae4df3edbd5d35e5b4f09020db03eab"
            ),
        );
    }
}

mod aead_aes_128_gcm {
    fn verify_test_vector(
        key: [u8; 16],
        iv: [u8; 12],
        encrypt: bool,
        aads: &[&[u8]],
        input: &[u8],
        expected: &[u8],
    ) {
        let srtp_provider = super::get_provider().srtp_provider;
        let cipher_provider = srtp_provider.aead_aes_128_gcm();
        let mut cipher = cipher_provider.create_cipher(key, encrypt);

        let output_len = if encrypt {
            input.len() + 16
        } else {
            input.len() - 16
        };
        let mut output = vec![0u8; output_len];
        if encrypt {
            cipher.encrypt(&iv, aads[0], input, &mut output).unwrap();
        } else {
            cipher.decrypt(&iv, aads, input, &mut output).unwrap();
        }
        assert_eq!(expected, &output)
    }

    // AES-128-GCM Test Vectors from NIST SP 800-38D
    // https://csrc.nist.gov/publications/detail/sp/800-38d/final
    // Test Case 4

    #[test]
    fn test_encrypt() {
        verify_test_vector(
            hex_as_bytes!(b"feffe9928665731c6d6a8f9467308308"),
            hex_as_bytes!(b"cafebabefacedbaddecaf888"),
            true,
            &[&hex_as_bytes!(
                b"feedfacedeadbeeffeedfacedeadbeef\
                abaddad2"
            )],
            &hex_as_bytes!(
                b"d9313225f88406e5a55909c5aff5269a\
                86a7a9531534f7da2e4c303d8a318a72\
                1c3c0c95956809532fcf0e2449a6b525\
                b16aedf5aa0de657ba637b39"
            ),
            &hex_as_bytes!(
                b"42831ec2217774244b7221b784d0d49c\
                e3aa212f2c02a4e035c17e2329aca12e\
                21d514b25466931c7d8f6a5aac84aa05\
                1ba30b396a0aac973d58e091\
                5bc94fbc3221a5db94fae95ae7121a47"
            ),
        );
    }

    #[test]
    fn test_decrypt() {
        verify_test_vector(
            hex_as_bytes!(b"feffe9928665731c6d6a8f9467308308"),
            hex_as_bytes!(b"cafebabefacedbaddecaf888"),
            false,
            &[&hex_as_bytes!(
                b"feedfacedeadbeeffeedfacedeadbeef\
                abaddad2"
            )],
            &hex_as_bytes!(
                b"42831ec2217774244b7221b784d0d49c\
                e3aa212f2c02a4e035c17e2329aca12e\
                21d514b25466931c7d8f6a5aac84aa05\
                1ba30b396a0aac973d58e091\
                5bc94fbc3221a5db94fae95ae7121a47"
            ),
            &hex_as_bytes!(
                b"d9313225f88406e5a55909c5aff5269a\
                86a7a9531534f7da2e4c303d8a318a72\
                1c3c0c95956809532fcf0e2449a6b525\
                b16aedf5aa0de657ba637b39"
            ),
        );
    }

    #[test]
    fn test_decrypt_invalid_tag() {
        let srtp_provider = super::get_provider().srtp_provider;
        let cipher_provider = srtp_provider.aead_aes_128_gcm();
        let mut cipher = cipher_provider
            .create_cipher(hex_as_bytes!(b"feffe9928665731c6d6a8f9467308308"), false);

        let mut output = vec![0u8; 1024];
        let result = cipher.decrypt(
            &hex_as_bytes!(b"cafebabefacedbaddecaf888"),
            &[&hex_as_bytes!(
                b"feedfacedeadbeeffeedfacedeadbeef\
                abaddad2"
            )],
            &hex_as_bytes!(
                b"42831ec2217774244b7221b784d0d49c\
                e3aa212f2c02a4e035c17e2329aca12e\
                21d514b25466931c7d8f6a5aac84aa05\
                1ba30b396a0aac973d58e091\
                000000000000000000000000000000"
            ),
            &mut output,
        );
        assert!(result.is_err());
    }
}

mod aead_aes_256_gcm {
    fn verify_test_vector(
        key: [u8; 32],
        iv: [u8; 12],
        encrypt: bool,
        aads: &[&[u8]],
        input: &[u8],
        expected: &[u8],
    ) {
        let srtp_provider = super::get_provider().srtp_provider;
        let cipher_provider = srtp_provider.aead_aes_256_gcm();
        let mut cipher = cipher_provider.create_cipher(key, encrypt);

        let output_len = if encrypt {
            input.len() + 16
        } else {
            input.len() - 16
        };
        let mut output = vec![0u8; output_len];
        if encrypt {
            cipher.encrypt(&iv, aads[0], input, &mut output).unwrap();
        } else {
            cipher.decrypt(&iv, aads, input, &mut output).unwrap();
        }
        assert_eq!(expected, &output)
    }

    // AES-256-GCM Test Vectors from NIST SP 800-38D
    // Test Case 16

    #[test]
    fn test_encrypt() {
        verify_test_vector(
            hex_as_bytes!(
                b"feffe9928665731c6d6a8f9467308308\
                feffe9928665731c6d6a8f9467308308"
            ),
            hex_as_bytes!(b"cafebabefacedbaddecaf888"),
            true,
            &[&hex_as_bytes!(
                b"feedfacedeadbeeffeedfacedeadbeef\
                abaddad2"
            )],
            &hex_as_bytes!(
                b"d9313225f88406e5a55909c5aff5269a\
                86a7a9531534f7da2e4c303d8a318a72\
                1c3c0c95956809532fcf0e2449a6b525\
                b16aedf5aa0de657ba637b39"
            ),
            &hex_as_bytes!(
                b"522dc1f099567d07f47f37a32a84427d\
                643a8cdcbfe5c0c97598a2bd2555d1aa\
                8cb08e48590dbb3da7b08b1056828838\
                c5f61e6393ba7a0abcc9f662\
                76fc6ece0f4e1768cddf8853bb2d551b"
            ),
        );
    }

    #[test]
    fn test_decrypt() {
        verify_test_vector(
            hex_as_bytes!(
                b"feffe9928665731c6d6a8f9467308308\
                feffe9928665731c6d6a8f9467308308"
            ),
            hex_as_bytes!(b"cafebabefacedbaddecaf888"),
            false,
            &[&hex_as_bytes!(
                b"feedfacedeadbeeffeedfacedeadbeef\
                abaddad2"
            )],
            &hex_as_bytes!(
                b"522dc1f099567d07f47f37a32a84427d\
                643a8cdcbfe5c0c97598a2bd2555d1aa\
                8cb08e48590dbb3da7b08b1056828838\
                c5f61e6393ba7a0abcc9f662\
                76fc6ece0f4e1768cddf8853bb2d551b"
            ),
            &hex_as_bytes!(
                b"d9313225f88406e5a55909c5aff5269a\
                86a7a9531534f7da2e4c303d8a318a72\
                1c3c0c95956809532fcf0e2449a6b525\
                b16aedf5aa0de657ba637b39"
            ),
        );
    }
}

mod srtp_aes_128_ecb_round {
    const TEST_KEY: [u8; 16] = hex_as_bytes!(b"2b7e151628aed2a6abf7158809cf4f3c");

    fn verify_test_vector(key: &[u8], input: &[u8], expected: &[u8]) {
        let srtp_provider = super::get_provider().srtp_provider;
        let mut out = [0u8; 2048];
        srtp_provider.srtp_aes_128_ecb_round(key, input, out.as_mut_slice());
        assert_eq!(expected, &out[0..input.len()])
    }

    // Test vectors from NIST SP 800-38A:
    // https://nvlpubs.nist.gov/nistpubs/Legacy/SP/nistspecialpublication800-38a.pdf

    #[test]
    fn test_vec_1() {
        verify_test_vector(
            &TEST_KEY,
            &hex_as_bytes!(b"6bc1bee22e409f96e93d7e117393172a"),
            &hex_as_bytes!(b"3ad77bb40d7a3660a89ecaf32466ef97"),
        );
    }

    #[test]
    fn test_vec_2() {
        verify_test_vector(
            &TEST_KEY,
            &hex_as_bytes!(b"ae2d8a571e03ac9c9eb76fac45af8e51"),
            &hex_as_bytes!(b"f5d3d58503b9699de785895a96fdbaaf"),
        );
    }

    #[test]
    fn test_vec_3() {
        verify_test_vector(
            &TEST_KEY,
            &hex_as_bytes!(b"30c81c46a35ce411e5fbc1191a0a52ef"),
            &hex_as_bytes!(b"43b1cd7f598ece23881b00e3ed030688"),
        );
    }

    #[test]
    fn test_vec_4() {
        verify_test_vector(
            &TEST_KEY,
            &hex_as_bytes!(b"f69f2445df4f9b17ad2b417be66c3710"),
            &hex_as_bytes!(b"7b0c785e27e8ad3f8223207104725dd4"),
        );
    }
}

mod srtp_aes_256_ecb_round {
    const TEST_KEY: [u8; 32] =
        hex_as_bytes!(b"603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4");

    fn verify_test_vector(key: &[u8], input: &[u8], expected: &[u8]) {
        let srtp_provider = super::get_provider().srtp_provider;
        let mut out = [0u8; 2048];
        srtp_provider.srtp_aes_256_ecb_round(key, input, out.as_mut_slice());
        assert_eq!(expected, &out[0..input.len()])
    }

    // Test vectors from NIST SP 800-38A:
    // https://nvlpubs.nist.gov/nistpubs/Legacy/SP/nistspecialpublication800-38a.pdf

    #[test]
    fn test_vec_1() {
        verify_test_vector(
            &TEST_KEY,
            &hex_as_bytes!(b"6bc1bee22e409f96e93d7e117393172a"),
            &hex_as_bytes!(b"f3eed1bdb5d2a03c064b5a7e3db181f8"),
        );
    }

    #[test]
    fn test_vec_2() {
        verify_test_vector(
            &TEST_KEY,
            &hex_as_bytes!(b"ae2d8a571e03ac9c9eb76fac45af8e51"),
            &hex_as_bytes!(b"591ccb10d410ed26dc5ba74a31362870"),
        );
    }

    #[test]
    fn test_vec_3() {
        verify_test_vector(
            &TEST_KEY,
            &hex_as_bytes!(b"30c81c46a35ce411e5fbc1191a0a52ef"),
            &hex_as_bytes!(b"b6ed21b99ca6f4f9f153e7b1beafed1d"),
        );
    }

    #[test]
    fn test_vec_4() {
        verify_test_vector(
            &TEST_KEY,
            &hex_as_bytes!(b"f69f2445df4f9b17ad2b417be66c3710"),
            &hex_as_bytes!(b"23304b7a39f9f3ff067d8d8f9e24ecc7"),
        );
    }
}
