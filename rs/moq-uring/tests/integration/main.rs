//! Every integration test, linked into one binary.
//!
//! Each `tests/*.rs` file is its own crate that links this package and its
//! dependencies again, so add new integration tests here as a module.
//! `allocations.rs` stays apart because its counting global allocator would
//! apply to every test in the binary.
//!
//! moq-uring is Linux-only, so off Linux the binary builds to nothing rather
//! than compiling io_uring code.

#![cfg(target_os = "linux")]

// `transport` includes the adapter source, which names `crate::quic`.
#[cfg(feature = "quic")]
use moq_uring::quic;

mod credit;
mod echo;
mod ecn;
mod endpoint;
mod identity;
mod local_addr;
mod mtu;
mod qlog;
mod receive_credit;
mod session;
mod support;
mod teardown;
mod transport;
mod web;
mod workers;
