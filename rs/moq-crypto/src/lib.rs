//! Cryptography for MoQ: content signing, end-to-end encryption, and the shared provider.
//!
//! [`sign`] signs every grouped frame so a relay can proxy and cache a broadcast without
//! forging it. [`e2ee`] encrypts groups, datagrams, and track names so a relay forwards
//! ciphertext it cannot read. Both wrap `moq-net` and check a frame's timestamp with
//! `moq_net::track::Info::rescale`, so neither accepts a frame the net track would refuse.
//! [`provider`] is rind's rustls provider and CPU check, with the [`provider::UnsupportedCpu`]
//! error that moq-quic, moq-tokio, and the other transport crates report.
//!
//! The `sign` and `e2ee` features, both default, gate those halves; `provider`, off by default,
//! gates the provider and needs no `moq-net`. Without any of them the crate holds only
//! [`sign::PublicKey`], the endpoint ID, and its canonical z-base-32 text, with no `moq-net`
//! or crypto backend; `serde` adds its string serialization.

#![warn(missing_docs)]

#[cfg(feature = "e2ee")]
pub mod e2ee;
#[cfg(feature = "provider")]
pub mod provider;
pub mod sign;
