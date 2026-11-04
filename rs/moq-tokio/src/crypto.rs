//! rind cryptography shared by native MoQ applications.
//!
//! [`provider`] and [`install_default`] refuse a CPU that rind cannot run on with
//! [`UnsupportedCpu`], naming the missing feature. This module re-exports
//! `moq_crypto::provider`, so moq-quic, qmux, and moq-uring report the same type.

pub use moq_crypto::provider::{Provider, UnsupportedCpu, check, install_default, provider};

/// Hash certificate bytes independently of the configured TLS cipher suites.
pub(crate) fn sha256(data: &[u8]) -> rind::digest::Digest {
	rind::digest::digest(&rind::digest::SHA256, data)
}
