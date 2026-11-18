//! Cryptography the MoQ crates share: the provider and the CPU check.
//!
//! [`provider`] is rind's rustls provider; [`cpu`] holds the CPU check and [`cpu::UnsupportedCpu`]
//! error that moq-quic, moq-tokio, and the other transport crates report.
//!
//! Both are off by default: `provider` gates the rustls provider and enables `cpu`, which needs
//! only rind.

#![warn(missing_docs)]

#[cfg(feature = "cpu")]
pub mod cpu;
#[cfg(feature = "provider")]
pub mod provider;
