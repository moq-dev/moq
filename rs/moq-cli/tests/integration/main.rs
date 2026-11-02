//! Every integration test, linked into one binary.
//!
//! Each `tests/*.rs` file is its own crate that links this package and its
//! dependencies again, so add new integration tests here as a module.

mod auth_serve;
mod epochs;
mod export;
mod import;
mod listen;
mod report;
mod rtsp;
