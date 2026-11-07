//! Exported things with feature `_internal_test_exports`.

use crate::Rtc;
use crate::crypto::CryptoProvider;
use crate::crypto::dtls::{KeyingMaterial, SrtpProfile};
use crate::format::PayloadParams;
use crate::media::Media;
use crate::media::Mid;
use crate::rtp::{ExtensionMap, RtpHeader};
use is::IceCreds;

pub mod fuzz;
mod rng;
use rng::Rng;

mod setup;

impl Rtc {
    /// UNSTABLE: not public API!
    pub fn _mids(&self) -> Vec<Mid> {
        self.session.medias.iter().map(Media::mid).collect()
    }

    /// UNSTABLE: not public API!
    pub fn _exts(&self) -> &ExtensionMap {
        &self.session.exts
    }

    /// UNSTABLE: not public API!
    pub fn _local_ice_creds(&self) -> IceCreds {
        self.ice.local_credentials().clone()
    }

    /// Install known keys to exercise individual SRTP profiles in integration tests.
    pub fn _set_srtp_keys(&mut self, mat: KeyingMaterial, profile: SrtpProfile, active: bool) {
        self.session
            .set_keying_material(mat, &self.crypto_provider, profile, active);
    }
}

impl RtpHeader {
    /// UNSTABLE: not public API!
    pub fn _parse(buf: &[u8], exts: &ExtensionMap) -> Option<RtpHeader> {
        Self::parse(buf, exts)
    }
}

impl PayloadParams {
    /// UNSTABLE: not public API!
    pub fn _is_locked(&self) -> bool {
        self.locked
    }
}

/// UNSTABLE: not public API!
pub use crate::bwe_::ProbeClusterConfig;

/// Construct authenticated RTP packets with controlled headers and payloads in integration tests.
pub struct TestSrtp(crate::rtp_::SrtpContext);

impl TestSrtp {
    /// Use the same keys and profile as the test receiver's incoming SRTP context.
    pub fn new(crypto: &CryptoProvider, profile: SrtpProfile, mat: &KeyingMaterial) -> Self {
        Self(crate::rtp_::SrtpContext::new(crypto, profile, mat, true))
    }

    /// Serialize and encrypt a supplied packet, including deliberately malformed payloads.
    pub fn protect(
        &mut self,
        mut header: RtpHeader,
        seq: u64,
        payload: &[u8],
        exts: &ExtensionMap,
    ) -> Vec<u8> {
        let mut buf = vec![0; 1500];
        let header_len = header.write_to(&mut buf, exts);
        buf[header_len..header_len + payload.len()].copy_from_slice(payload);
        header.header_len = header_len;
        let packet = &buf[..header_len + payload.len()];
        self.0.protect_rtp(packet, &header, seq)
    }
}
