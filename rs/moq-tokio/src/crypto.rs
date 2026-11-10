//! rind cryptography shared by native MoQ applications.
//!
//! This module re-exports `moq_crypto::provider`, so moq-quic, qmux, and moq-uring share one provider.

pub use moq_crypto::provider::{Provider, install_default, provider};

/// Hash certificate bytes independently of the configured TLS cipher suites.
pub(crate) fn sha256(data: &[u8]) -> rind::digest::Digest {
	rind::digest::digest(&rind::digest::SHA256, data)
}
