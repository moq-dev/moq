//! Cryptography the MoQ crates share: endpoint keys, the provider, and the CPU check.
//!
//! [`endpoint`] holds the Ed25519 key that names an endpoint, which a relay proves to its peers
//! as its cluster route key.
//! [`provider`] is rind's rustls provider; [`cpu`] holds the CPU check and [`cpu::UnsupportedCpu`]
//! error that moq-quic, moq-tokio, and the other transport crates report.
//!
//! The `endpoint` feature, the default, gates the secret key; `provider`, off by default, gates
//! the rustls provider and enables `cpu`, which needs only rind. Without any of these features
//! the crate holds only [`endpoint::PublicKey`], the endpoint ID, and its canonical z-base-32
//! text, with no `moq-net` or crypto backend; `serde` adds its string serialization.

#![warn(missing_docs)]

#[cfg(feature = "cpu")]
pub mod cpu;
pub mod endpoint;
#[cfg(feature = "provider")]
pub mod provider;
