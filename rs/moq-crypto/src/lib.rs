//! Cryptography for MoQ: end-to-end encryption and the shared provider.
//!
//! [`e2ee`] encrypts groups, datagrams, and track names so a relay forwards ciphertext it cannot
//! read. It wraps `moq-net` and checks a frame's timestamp with `moq_net::track::Info::rescale`,
//! so it accepts no frame the net track would refuse.
//! [`provider`] is rind's rustls provider; [`cpu`] holds the CPU check and [`cpu::UnsupportedCpu`]
//! error that moq-quic, moq-tokio, and the other transport crates report.
//!
//! The `e2ee` feature, on by default, gates encryption; `provider`, off by default, gates the
//! rustls provider and enables `cpu`, which needs only rind.
//!
//! The `poll_*` methods are always available. Enable the `async` feature for their generated
//! async wrappers (such as `read_frame` and `recv_group`); it is off by default.

#![warn(missing_docs)]

#[cfg(feature = "cpu")]
pub mod cpu;
#[cfg(feature = "e2ee")]
pub mod e2ee;
#[cfg(feature = "provider")]
pub mod provider;
