use std::fmt;

use self::aes_128_cm_sha1_80::AesKey;

use super::CryptoProvider;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SrtpProfile {
    #[cfg(feature = "_internal_test_exports")]
    PassThrough,
    Aes128CmSha1_80,
    AeadAes128Gcm,
    AeadAes256Gcm,
}

#[allow(dead_code)]
impl SrtpProfile {
    // All the profiles we support, ordered from most preferred to least.
    pub(crate) const ALL: &'static [SrtpProfile] = &[
        SrtpProfile::AeadAes256Gcm,
        SrtpProfile::AeadAes128Gcm,
        SrtpProfile::Aes128CmSha1_80,
    ];

    /// The length of keying material to extract from the DTLS session in bytes.
    #[rustfmt::skip]
    pub(crate) fn keying_material_len(&self) -> usize {
        match self {
            #[cfg(feature = "_internal_test_exports")]
            SrtpProfile::PassThrough => 0,
             // MASTER_KEY_LEN * 2 + MASTER_SALT * 2
             // TODO: This is a duplication of info that is held in srtp.rs, because we
             // don't want a dependency in that direction.
            SrtpProfile::Aes128CmSha1_80 => 16 * 2 + 14 * 2,
            SrtpProfile::AeadAes128Gcm   => 16 * 2 + 12 * 2,
            SrtpProfile::AeadAes256Gcm   => 32 * 2 + 12 * 2,
        }
    }
}

pub enum SrtpCrypto {
    #[cfg(any(feature = "openssl", feature = "openssl-dimpl"))]
    OpenSsl(super::ossl::OsslSrtpCryptoImpl),
    #[cfg(not(any(feature = "openssl", feature = "openssl-dimpl")))]
    OpenSsl(DummySrtpCryptoImpl),
    #[cfg(all(feature = "wincrypto", target_os = "windows"))]
    WinCrypto(super::wincrypto::WinCryptoSrtpCryptoImpl),
    #[cfg(not(all(feature = "wincrypto", target_os = "windows")))]
    WinCrypto(DummySrtpCryptoImpl),
    #[cfg(feature = "dimpl")]
    AwsLc(super::aws_lc::AwsLcImpl),
    #[cfg(not(feature = "dimpl"))]
    AwsLc(DummySrtpCryptoImpl),
}

#[allow(clippy::unit_arg)]
impl SrtpCrypto {
    #[cfg(any(feature = "openssl", feature = "openssl-dimpl"))]
    pub fn new_openssl() -> SrtpCrypto {
        Self::OpenSsl(super::ossl::OsslSrtpCryptoImpl)
    }

    #[cfg(not(any(feature = "openssl", feature = "openssl-dimpl")))]
    pub fn new_openssl() -> SrtpCrypto {
        Self::OpenSsl(DummySrtpCryptoImpl(CryptoProvider::OpenSsl))
    }

    #[cfg(all(feature = "wincrypto", target_os = "windows"))]
    pub fn new_wincrypto() -> SrtpCrypto {
        Self::WinCrypto(super::wincrypto::WinCryptoSrtpCryptoImpl)
    }

    #[cfg(not(all(feature = "wincrypto", target_os = "windows")))]
    pub fn new_wincrypto() -> SrtpCrypto {
        Self::WinCrypto(DummySrtpCryptoImpl(CryptoProvider::WinCrypto))
    }

    #[cfg(feature = "dimpl")]
    pub fn new_rust_crypto() -> SrtpCrypto {
        Self::AwsLc(super::aws_lc::AwsLcImpl)
    }

    #[cfg(not(feature = "dimpl"))]
    pub fn new_rust_crypto() -> SrtpCrypto {
        Self::AwsLc(DummySrtpCryptoImpl(CryptoProvider::Dimpl))
    }

    // TODO: Can we avoice dynamic dispatch in this signature? The parameters are:
    //       1. As few "touch points" beteen rtp/srtp.rs and here as possible.
    //       2. Clear contract towards the actual impl.
    //       3. Choice of impl passed all the way from RtcConfig.
    pub fn new_aes_128_cm_sha1_80(
        &self,
        key: AesKey,
        encrypt: bool,
    ) -> Box<dyn aes_128_cm_sha1_80::CipherCtx> {
        match self {
            SrtpCrypto::OpenSsl(v) => Box::new(v.new_aes_128_cm_sha1_80(key, encrypt)),
            SrtpCrypto::WinCrypto(v) => Box::new(v.new_aes_128_cm_sha1_80(key, encrypt)),
            SrtpCrypto::AwsLc(v) => Box::new(v.new_aes_128_cm_sha1_80(key, encrypt)),
        }
    }

    pub fn new_aead_aes_128_gcm(
        &self,
        key: aead_aes_128_gcm::AeadKey,
        encrypt: bool,
    ) -> Box<dyn aead_aes_128_gcm::CipherCtx> {
        match self {
            SrtpCrypto::OpenSsl(v) => Box::new(v.new_aead_aes_128_gcm(key, encrypt)),
            SrtpCrypto::WinCrypto(v) => Box::new(v.new_aead_aes_128_gcm(key, encrypt)),
            SrtpCrypto::AwsLc(v) => Box::new(v.new_aead_aes_128_gcm(key, encrypt)),
        }
    }

    pub fn new_aead_aes_256_gcm(
        &self,
        key: aead_aes_256_gcm::AeadKey,
        encrypt: bool,
    ) -> Box<dyn aead_aes_256_gcm::CipherCtx> {
        match self {
            SrtpCrypto::OpenSsl(v) => Box::new(v.new_aead_aes_256_gcm(key, encrypt)),
            SrtpCrypto::WinCrypto(v) => Box::new(v.new_aead_aes_256_gcm(key, encrypt)),
            SrtpCrypto::AwsLc(v) => Box::new(v.new_aead_aes_256_gcm(key, encrypt)),
        }
    }

    pub fn srtp_aes_128_ecb_round(&self, key: &[u8], input: &[u8], output: &mut [u8]) {
        match self {
            SrtpCrypto::OpenSsl(v) => v.srtp_aes_128_ecb_round(key, input, output),
            SrtpCrypto::WinCrypto(v) => v.srtp_aes_128_ecb_round(key, input, output),
            SrtpCrypto::AwsLc(v) => v.srtp_aes_128_ecb_round(key, input, output),
        }
    }

    pub fn srtp_aes_256_ecb_round(&self, key: &[u8], input: &[u8], output: &mut [u8]) {
        match self {
            SrtpCrypto::OpenSsl(v) => v.srtp_aes_256_ecb_round(key, input, output),
            SrtpCrypto::WinCrypto(v) => v.srtp_aes_256_ecb_round(key, input, output),
            SrtpCrypto::AwsLc(v) => v.srtp_aes_256_ecb_round(key, input, output),
        }
    }
}

pub trait SrtpCryptoImpl {
    type Aes128CmSha1_80: aes_128_cm_sha1_80::CipherCtx;
    type AeadAes128Gcm: aead_aes_128_gcm::CipherCtx;
    type AeadAes256Gcm: aead_aes_256_gcm::CipherCtx;

    fn new_aes_128_cm_sha1_80(&self, key: AesKey, encrypt: bool) -> Self::Aes128CmSha1_80 {
        <Self::Aes128CmSha1_80 as aes_128_cm_sha1_80::CipherCtx>::new(key, encrypt)
    }

    fn new_aead_aes_128_gcm(
        &self,
        key: aead_aes_128_gcm::AeadKey,
        encrypt: bool,
    ) -> Self::AeadAes128Gcm {
        <Self::AeadAes128Gcm as aead_aes_128_gcm::CipherCtx>::new(key, encrypt)
    }

    fn new_aead_aes_256_gcm(
        &self,
        key: aead_aes_256_gcm::AeadKey,
        encrypt: bool,
    ) -> Self::AeadAes256Gcm {
        <Self::AeadAes256Gcm as aead_aes_256_gcm::CipherCtx>::new(key, encrypt)
    }

    fn srtp_aes_128_ecb_round(&self, key: &[u8], input: &[u8], output: &mut [u8]);

    fn srtp_aes_256_ecb_round(&self, key: &[u8], input: &[u8], output: &mut [u8]);
}

pub mod aes_128_cm_sha1_80 {
    use std::panic::UnwindSafe;

    use subtle::ConstantTimeEq;

    use crate::crypto::CryptoError;

    pub const KEY_LEN: usize = 16;
    pub const SALT_LEN: usize = 14;
    pub const HMAC_KEY_LEN: usize = 20;
    pub const HMAC_TAG_LEN: usize = 10;
    pub type AesKey = [u8; 16];
    pub type RtpSalt = [u8; 14];
    pub type RtpIv = [u8; 16];

    pub trait CipherCtx: UnwindSafe + Send + Sync {
        fn new(key: AesKey, encrypt: bool) -> Self
        where
            Self: Sized;

        fn encrypt(
            &mut self,
            iv: &RtpIv,
            input: &[u8],
            output: &mut [u8],
        ) -> Result<(), CryptoError>;

        fn decrypt(
            &mut self,
            iv: &RtpIv,
            input: &[u8],
            output: &mut [u8],
        ) -> Result<(), CryptoError>;
    }

    pub fn rtp_hmac(key: &[u8], buf: &mut [u8], srtp_index: u64, hmac_start: usize) {
        let roc = (srtp_index >> 16) as u32;
        let tag = crate::crypto::sha1_hmac(key, &[&buf[..hmac_start], &roc.to_be_bytes()]);
        buf[hmac_start..(hmac_start + HMAC_TAG_LEN)].copy_from_slice(&tag[0..HMAC_TAG_LEN]);
    }

    pub fn rtp_verify(key: &[u8], buf: &[u8], srtp_index: u64, cmp: &[u8]) -> bool {
        let roc = (srtp_index >> 16) as u32;
        let tag = crate::crypto::sha1_hmac(key, &[buf, &roc.to_be_bytes()]);

        tag[0..HMAC_TAG_LEN].ct_eq(cmp).into()
    }

    pub fn rtp_iv(salt: RtpSalt, ssrc: u32, srtp_index: u64) -> RtpIv {
        let mut iv = [0; 16];
        let ssrc_be = ssrc.to_be_bytes();
        let srtp_be = srtp_index.to_be_bytes();
        iv[4..8].copy_from_slice(&ssrc_be);
        for i in 0..8 {
            iv[i + 6] ^= srtp_be[i];
        }
        for i in 0..14 {
            iv[i] ^= salt[i];
        }
        iv
    }

    pub fn rtcp_hmac(key: &[u8], buf: &mut [u8], hmac_index: usize) {
        let tag = crate::crypto::sha1_hmac(key, &[&buf[0..hmac_index]]);

        buf[hmac_index..(hmac_index + HMAC_TAG_LEN)].copy_from_slice(&tag[0..HMAC_TAG_LEN]);
    }

    pub fn rtcp_verify(key: &[u8], buf: &[u8], cmp: &[u8]) -> bool {
        let tag = crate::crypto::sha1_hmac(key, &[buf]);

        tag[0..HMAC_TAG_LEN].ct_eq(cmp).into()
    }
}

pub mod aead_aes_128_gcm {
    use std::panic::UnwindSafe;

    use crate::crypto::CryptoError;

    pub const KEY_LEN: usize = 16;
    pub const SALT_LEN: usize = 12;
    pub const RTCP_AAD_LEN: usize = 12;
    pub const TAG_LEN: usize = 16;
    pub const IV_LEN: usize = 12;
    pub type AeadKey = [u8; KEY_LEN];
    pub type RtpSalt = [u8; SALT_LEN];
    pub type RtpIv = [u8; SALT_LEN];

    pub trait CipherCtx: UnwindSafe + Send + Sync {
        fn new(key: AeadKey, encrypt: bool) -> Self
        where
            Self: Sized;

        fn encrypt(
            &mut self,
            iv: &[u8; IV_LEN],
            aad: &[u8],
            input: &[u8],
            output: &mut [u8],
        ) -> Result<(), CryptoError>;

        fn decrypt(
            &mut self,
            iv: &[u8; IV_LEN],
            aads: &[&[u8]],
            input: &[u8],
            output: &mut [u8],
        ) -> Result<usize, CryptoError>;
    }

    pub fn rtp_iv(salt: RtpSalt, ssrc: u32, roc: u32, seq: u16) -> RtpIv {
        // See: https://www.rfc-editor.org/rfc/rfc7714#section-8.1

        // TODO: See if this is faster if rewritten for u128
        let mut iv = [0; SALT_LEN];

        let ssrc_be = ssrc.to_be_bytes();
        let roc_be = roc.to_be_bytes();
        let seq_be = seq.to_be_bytes();

        iv[2..6].copy_from_slice(&ssrc_be);
        iv[6..10].copy_from_slice(&roc_be);
        iv[10..12].copy_from_slice(&seq_be);

        // XOR with salt
        for i in 0..SALT_LEN {
            iv[i] ^= salt[i];
        }

        iv
    }

    pub fn rtcp_iv(salt: RtpSalt, ssrc: u32, srtp_index: u32) -> RtpIv {
        // See: https://www.rfc-editor.org/rfc/rfc7714#section-9.1
        // TODO: See if this is faster if rewritten for u128
        let mut iv = [0; SALT_LEN];

        let ssrc_be = ssrc.to_be_bytes();
        let srtp_be = srtp_index.to_be_bytes();

        iv[2..6].copy_from_slice(&ssrc_be);
        iv[8..12].copy_from_slice(&srtp_be);

        // XOR with salt
        for i in 0..SALT_LEN {
            iv[i] ^= salt[i];
        }

        iv
    }
}

pub mod aead_aes_256_gcm {
    use std::panic::UnwindSafe;

    use crate::crypto::CryptoError;

    pub const KEY_LEN: usize = 32;
    pub const SALT_LEN: usize = 12;
    pub const RTCP_AAD_LEN: usize = 12;
    pub const TAG_LEN: usize = 16;
    pub const IV_LEN: usize = 12;
    pub type AeadKey = [u8; KEY_LEN];
    pub type RtpSalt = [u8; SALT_LEN];
    pub type RtpIv = [u8; SALT_LEN];

    pub trait CipherCtx: UnwindSafe + Send + Sync {
        fn new(key: AeadKey, encrypt: bool) -> Self
        where
            Self: Sized;

        fn encrypt(
            &mut self,
            iv: &[u8; IV_LEN],
            aad: &[u8],
            input: &[u8],
            output: &mut [u8],
        ) -> Result<(), CryptoError>;

        fn decrypt(
            &mut self,
            iv: &[u8; IV_LEN],
            aads: &[&[u8]],
            input: &[u8],
            output: &mut [u8],
        ) -> Result<usize, CryptoError>;
    }

    pub fn rtp_iv(salt: RtpSalt, ssrc: u32, roc: u32, seq: u16) -> RtpIv {
        // See: https://www.rfc-editor.org/rfc/rfc7714#section-8.1

        // TODO: See if this is faster if rewritten for u128
        let mut iv = [0; SALT_LEN];

        let ssrc_be = ssrc.to_be_bytes();
        let roc_be = roc.to_be_bytes();
        let seq_be = seq.to_be_bytes();

        iv[2..6].copy_from_slice(&ssrc_be);
        iv[6..10].copy_from_slice(&roc_be);
        iv[10..12].copy_from_slice(&seq_be);

        // XOR with salt
        for i in 0..SALT_LEN {
            iv[i] ^= salt[i];
        }

        iv
    }

    pub fn rtcp_iv(salt: RtpSalt, ssrc: u32, srtp_index: u32) -> RtpIv {
        // See: https://www.rfc-editor.org/rfc/rfc7714#section-9.1
        // TODO: See if this is faster if rewritten for u128
        let mut iv = [0; SALT_LEN];

        let ssrc_be = ssrc.to_be_bytes();
        let srtp_be = srtp_index.to_be_bytes();

        iv[2..6].copy_from_slice(&ssrc_be);
        iv[8..12].copy_from_slice(&srtp_be);

        // XOR with salt
        for i in 0..SALT_LEN {
            iv[i] ^= salt[i];
        }

        iv
    }
}

impl fmt::Display for SrtpProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(feature = "_internal_test_exports")]
            SrtpProfile::PassThrough => write!(f, "PassThrough"),
            SrtpProfile::Aes128CmSha1_80 => write!(f, "SRTP_AES128_CM_SHA1_80"),
            SrtpProfile::AeadAes128Gcm => write!(f, "SRTP_AEAD_AES_128_GCM"),
            SrtpProfile::AeadAes256Gcm => write!(f, "SRTP_AEAD_AES_256_GCM"),
        }
    }
}

pub struct DummySrtpCryptoImpl(CryptoProvider);

impl SrtpCryptoImpl for DummySrtpCryptoImpl {
    type Aes128CmSha1_80 = ();
    type AeadAes128Gcm = ();
    type AeadAes256Gcm = ();

    fn new_aes_128_cm_sha1_80(&self, _: AesKey, _: bool) -> Self::Aes128CmSha1_80 {
        panic!("Must enable feature: {}", self.0)
    }

    fn new_aead_aes_128_gcm(&self, _: aead_aes_128_gcm::AeadKey, _: bool) -> Self::AeadAes128Gcm {
        panic!("Must enable feature: {}", self.0)
    }

    fn new_aead_aes_256_gcm(&self, _: aead_aes_256_gcm::AeadKey, _: bool) -> Self::AeadAes256Gcm {
        panic!("Must enable feature: {}", self.0)
    }

    fn srtp_aes_128_ecb_round(&self, _: &[u8], _: &[u8], _: &mut [u8]) {
        panic!("Must enable feature: {}", self.0)
    }

    fn srtp_aes_256_ecb_round(&self, _: &[u8], _: &[u8], _: &mut [u8]) {
        panic!("Must enable feature: {}", self.0)
    }
}

impl aes_128_cm_sha1_80::CipherCtx for () {
    fn new(_: AesKey, _: bool) -> Self
    where
        Self: Sized,
    {
        unreachable!()
    }

    fn encrypt(
        &mut self,
        _: &aes_128_cm_sha1_80::RtpIv,
        _: &[u8],
        _: &mut [u8],
    ) -> Result<(), super::CryptoError> {
        unreachable!()
    }

    fn decrypt(
        &mut self,
        _: &aes_128_cm_sha1_80::RtpIv,
        _: &[u8],
        _: &mut [u8],
    ) -> Result<(), super::CryptoError> {
        unreachable!()
    }
}

impl aead_aes_128_gcm::CipherCtx for () {
    fn new(_: aead_aes_128_gcm::AeadKey, _: bool) -> Self
    where
        Self: Sized,
    {
        unreachable!()
    }

    fn encrypt(
        &mut self,
        _: &[u8; aead_aes_128_gcm::IV_LEN],
        _: &[u8],
        _: &[u8],
        _: &mut [u8],
    ) -> Result<(), super::CryptoError> {
        unreachable!()
    }

    fn decrypt(
        &mut self,
        _: &[u8; aead_aes_128_gcm::IV_LEN],
        _: &[&[u8]],
        _: &[u8],
        _: &mut [u8],
    ) -> Result<usize, super::CryptoError> {
        unreachable!()
    }
}

impl aead_aes_256_gcm::CipherCtx for () {
    fn new(_: aead_aes_256_gcm::AeadKey, _: bool) -> Self
    where
        Self: Sized,
    {
        unreachable!()
    }

    fn encrypt(
        &mut self,
        _: &[u8; aead_aes_256_gcm::IV_LEN],
        _: &[u8],
        _: &[u8],
        _: &mut [u8],
    ) -> Result<(), super::CryptoError> {
        unreachable!()
    }

    fn decrypt(
        &mut self,
        _: &[u8; aead_aes_256_gcm::IV_LEN],
        _: &[&[u8]],
        _: &[u8],
        _: &mut [u8],
    ) -> Result<usize, super::CryptoError> {
        unreachable!()
    }
}
