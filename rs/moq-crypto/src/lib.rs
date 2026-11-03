//! Content signing and end-to-end encryption for MoQ.
//!
//! [`sign`] signs every grouped frame so a relay can proxy and cache a broadcast without
//! forging it. [`e2ee`] encrypts groups, datagrams, and track names so a relay forwards
//! ciphertext it cannot read. Both wrap `moq-net` and check a frame's timestamp with
//! [`moq_net::track::Info::rescale`], so neither accepts a frame the net track would refuse.

#![warn(missing_docs)]

pub mod e2ee;
pub mod sign;
