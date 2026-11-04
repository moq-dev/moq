//! Content signing and end-to-end encryption for MoQ.
//!
//! [`sign`] signs every grouped frame so a relay can proxy and cache a broadcast without
//! forging it. [`e2ee`] encrypts groups, datagrams, and track names so a relay forwards
//! ciphertext it cannot read. Both wrap `moq-net` and check a frame's timestamp with
//! `moq_net::track::Info::rescale`, so neither accepts a frame the net track would refuse.
//!
//! The `sign` and `e2ee` features, both default, gate those halves. Without them the crate holds
//! only [`sign::PublicKey`], the endpoint ID, and its canonical z-base-32 text, with no `moq-net`
//! or crypto backend; `serde` adds its string serialization.
//!
//! The `poll_*` methods are always available. Enable the `async` feature for their generated
//! async wrappers (such as `read_frame` and `recv_group`); it is off by default.

#![warn(missing_docs)]

#[cfg(feature = "e2ee")]
pub mod e2ee;
pub mod sign;
