//! Cryptography the MoQ crates share: the provider and the DIT guard.
//!
//! [`provider`] is rind's rustls provider; [`cpu`] re-exports rind's DIT guard.
//!
//! Both are off by default: `provider` gates the rustls provider and enables `cpu`, which needs
//! only rind.

#![warn(missing_docs)]

#[cfg(feature = "cpu")]
pub mod cpu;
#[cfg(feature = "provider")]
pub mod provider;
