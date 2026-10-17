//! Every integration test, linked into one binary.
//!
//! Each `tests/*.rs` file is its own crate that links this package and its
//! dependencies again, so add new integration tests here as a module.
//! `allocations.rs` stays apart because its counting global allocator would
//! apply to every test in the binary.

mod credit;
mod echo;
mod ecn;
mod endpoint;
mod identity;
mod qlog;
mod session;
mod support;
mod teardown;
mod transport;
mod web;
mod workers;
