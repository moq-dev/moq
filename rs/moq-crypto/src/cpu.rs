//! CPU support for rind cryptography, without a TLS dependency.

/// Holds data-independent timing on for this thread until dropped.
///
/// rind otherwise turns DIT on and off around every call that handles a secret, which costs
/// about 50 ns a call on Apple silicon, so a transport holds one across each batch of packets.
pub use rind::cpu::Dit;
