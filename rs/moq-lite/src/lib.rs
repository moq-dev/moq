//! # moq-lite (deprecated)
//!
//! This crate has been renamed to [`moq-net`](https://crates.io/crates/moq-net) to clarify
//! that it is the networking layer for Media over QUIC. Under the hood it negotiates one
//! of two wire protocols at session setup: the simplified `moq-lite` protocol or the full
//! IETF `moq-transport` protocol.
//!
//! `moq-lite` now re-exports `moq-net` so existing code keeps building. It will not
//! receive future updates. Migrate by replacing the dependency and changing `moq_lite::`
//! to `moq_net::`.

pub use moq_net::*;
