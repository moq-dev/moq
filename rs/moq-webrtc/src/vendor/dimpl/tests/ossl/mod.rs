#![allow(unused)]

//! OpenSSL implementation of cryptographic functions.

/// Errors that can arise in DTLS.
#[derive(Debug)]
pub enum CryptoError {
    /// Some error from OpenSSL layer (used for DTLS).
    OpenSsl(openssl::error::ErrorStack),

    /// Other IO errors.
    Io(io::Error),
}

impl From<openssl::error::ErrorStack> for CryptoError {
    fn from(value: openssl::error::ErrorStack) -> Self {
        CryptoError::OpenSsl(value)
    }
}

impl From<io::Error> for CryptoError {
    fn from(value: io::Error) -> Self {
        CryptoError::Io(value)
    }
}

mod cert;
use std::io;

pub use cert::{DtlsCertOptions, DtlsPKeyType, Fingerprint, OsslDtlsCert};

pub mod io_buf;
mod stream;

mod dtls;
use dimpl::SrtpProfile;
pub use dtls::{OsslDtlsImpl, dtls_ssl_create};

pub use io_buf::DatagramSend;
use std::collections::VecDeque;

/// Targeted MTU
pub(crate) const DATAGRAM_MTU: usize = 1150;

/// Warn if any packet we are about to send is above this size.
pub(crate) const DATAGRAM_MTU_WARN: usize = 1280;

/// Events arising from a [`Dtls`] instance.
#[derive(Debug)]
pub enum DtlsEvent {
    /// When the DTLS has finished handshaking.
    Connected,

    /// Keying material for SRTP encryption master key and the selected SRTP profile.
    SrtpKeyingMaterial(Vec<u8>, SrtpProfile),

    /// The fingerprint of the remote peer.
    ///
    /// This should be checked against the fingerprint communicated in the SDP.
    RemoteFingerprint(Fingerprint),

    /// Decrypted data from incoming DTLS traffic.
    Data(Vec<u8>),
}
