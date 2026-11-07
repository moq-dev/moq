//! CPU support for rind cryptography, without a TLS dependency.

/// This CPU lacks a feature that rind requires, named in the message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("unsupported CPU: {0}")]
pub struct UnsupportedCpu(pub(crate) rind::cpu::MissingFeature);

/// Refuse a CPU that rind cannot run on, naming the missing feature.
///
/// Call this before using rind's primitives directly; the rustls provider checks it too.
pub fn check() -> Result<(), UnsupportedCpu> {
	rind::cpu::check().map_err(UnsupportedCpu)
}
