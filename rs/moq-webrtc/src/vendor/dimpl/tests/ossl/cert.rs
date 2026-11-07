use std::fmt;
use std::time::SystemTime;

use openssl::asn1::{Asn1Integer, Asn1Time, Asn1Type};
use openssl::bn::BigNum;
use openssl::ec::{EcGroup, EcKey};
use openssl::hash::MessageDigest;
use openssl::nid::Nid;
use openssl::pkey::{PKey, Private};
use openssl::rsa::Rsa;
use openssl::x509::{X509, X509Name};

use super::CryptoError;
use super::OsslDtlsImpl;

const RSA_F4: u32 = 0x10001;

/// Certificate used for DTLS.
#[derive(Debug, Clone)]
pub struct OsslDtlsCert {
    pub pkey: PKey<Private>,
    pub x509: X509,
}

/// Defines the type of key pair to generate for the DTLS certificate.
#[derive(Clone, Debug, Default)]
pub enum DtlsPKeyType {
    /// Generate an RSA key pair
    Rsa2048,
    /// Generate an EC-DSA key pair using the NIST P-256 curve
    #[default]
    EcDsaP256,
    /// Generate an EC-DSA key pair using the NIST P-384 curve
    EcDsaP384,
}

/// Controls certificate generation options.
#[derive(Clone, Debug)]
pub struct DtlsCertOptions {
    /// The common name for the certificate.
    pub common_name: String,
    /// The type of key to generate.
    pub pkey_type: DtlsPKeyType,
}

const DTLS_CERT_IDENTITY: &str = "WebRTC";

impl Default for DtlsCertOptions {
    fn default() -> Self {
        Self {
            common_name: DTLS_CERT_IDENTITY.into(),
            pkey_type: Default::default(),
        }
    }
}

impl OsslDtlsCert {
    /// Creates a new (self signed) DTLS certificate.
    pub fn new(options: DtlsCertOptions) -> Self {
        Self::self_signed(options).expect("create dtls cert")
    }

    // The libWebRTC code we try to match is at:
    // https://webrtc.googlesource.com/src/+/1568f1b1330f94494197696fe235094e6293b258/rtc_base/openssl_certificate.cc#58
    fn self_signed(options: DtlsCertOptions) -> Result<Self, CryptoError> {
        let f4 = BigNum::from_u32(RSA_F4)?;
        let pkey = match options.pkey_type {
            DtlsPKeyType::Rsa2048 => {
                let key = Rsa::generate_with_e(2048, &f4)?;
                PKey::from_rsa(key)?
            }
            DtlsPKeyType::EcDsaP256 => {
                let nid = Nid::X9_62_PRIME256V1; // NIST P-256 curve
                let group = EcGroup::from_curve_name(nid)?;
                let key = EcKey::generate(&group)?;
                PKey::from_ec_key(key)?
            }
            DtlsPKeyType::EcDsaP384 => {
                let nid = Nid::SECP384R1;
                let group = EcGroup::from_curve_name(nid)?;
                let key = EcKey::generate(&group)?;
                PKey::from_ec_key(key)?
            }
        };

        let mut x509b = X509::builder()?;
        x509b.set_version(2)?; // X509.V3 (zero indexed)

        // For Firefox, the serial number must be unique across all certificates, including those of other
        // processes/machines! See https://github.com/versatica/mediasoup/issues/127#issuecomment-474460153
        // and https://github.com/algesten/str0m/issues/517
        let mut serial_buf = [0u8; 16];
        openssl::rand::rand_bytes(&mut serial_buf)?;

        let serial_bn = BigNum::from_slice(&serial_buf)?;
        let serial = Asn1Integer::from_bn(&serial_bn)?;
        x509b.set_serial_number(&serial)?;
        let before = Asn1Time::from_unix(unix_time() - 3600)?;
        x509b.set_not_before(&before)?;
        let after = Asn1Time::days_from_now(7)?;
        x509b.set_not_after(&after)?;
        x509b.set_pubkey(&pkey)?;

        // The libWebRTC code for this is:
        //
        // !X509_NAME_add_entry_by_NID(name.get(), NID_commonName, MBSTRING_UTF8,
        // (unsigned char*)params.common_name.c_str(), -1, -1, 0) ||
        //
        // libWebRTC allows this name to be configured by the user of the library.
        // That's a future TODO for str0m.
        let mut nameb = X509Name::builder()?;
        nameb.append_entry_by_nid_with_type(
            Nid::COMMONNAME,
            options.common_name.as_str(),
            Asn1Type::UTF8STRING,
        )?;

        let name = nameb.build();

        x509b.set_subject_name(&name)?;
        x509b.set_issuer_name(&name)?;

        x509b.sign(&pkey, MessageDigest::sha256())?;
        let x509 = x509b.build();

        Ok(OsslDtlsCert { pkey, x509 })
    }

    /// Produce a (public) fingerprint of the cert.
    ///
    /// This is sent via SDP to the other peer to lock down the DTLS
    /// to this specific certificate.
    pub fn fingerprint(&self) -> Fingerprint {
        let digest: &[u8] = &self
            .x509
            .digest(MessageDigest::sha256())
            .expect("digest to fingerprint");

        Fingerprint {
            hash_func: "sha-256".into(),
            bytes: digest.to_vec(),
        }
    }

    pub fn new_dtls_impl(&self) -> Result<OsslDtlsImpl, CryptoError> {
        OsslDtlsImpl::new(self.clone())
    }

    pub fn new_dtls_impl_with_groups(
        &self,
        groups_list: &str,
    ) -> Result<OsslDtlsImpl, CryptoError> {
        OsslDtlsImpl::new_with_groups(self.clone(), Some(groups_list))
    }
}

#[derive(Debug)]
pub struct Fingerprint {
    pub hash_func: String,
    pub bytes: Vec<u8>,
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ", self.hash_func)?;
        for (i, b) in self.bytes.iter().enumerate() {
            if i > 0 {
                write!(f, ":")?;
            }
            write!(f, "{:02X}", b)?;
        }
        Ok(())
    }
}

// TODO: Refactor away this use of System::now, to instead go via InstantExt
// and base the time on the first Instant. This would require lazy init of
// Dtls, or that we pass a first ever Instant into the creation of Rtc.
//
// This is not a super high priority since it's only used for setting a before
// time in the generated certificate, and one hour back from that.
pub fn unix_time() -> libc::time_t {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs() as libc::time_t
}
