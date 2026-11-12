//! CPU support for rind cryptography, without a TLS dependency.

/// This CPU lacks a feature that rind requires, named in the message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("unsupported CPU: {0}")]
pub struct UnsupportedCpu(pub(crate) rind::cpu::MissingFeature);

/// Holds data-independent timing on for this thread until dropped.
///
/// rind otherwise turns DIT on and off around every call that handles a secret, which costs
/// about 50 ns a call on Apple silicon, so a transport holds one across each batch of packets.
pub use rind::cpu::Dit;

/// Refuse a CPU that rind cannot run on, naming the missing feature.
///
/// Call this before using rind's primitives directly; the rustls provider checks it too.
pub fn check() -> Result<(), UnsupportedCpu> {
	rind::cpu::check().map_err(UnsupportedCpu)
}
