//! Every integration test, linked into one binary.
//!
//! Each `tests/*.rs` file is its own crate that links this package and its
//! dependencies again, so add new integration tests here as a module.
#![expect(clippy::disallowed_methods, reason = "tests read the host clock directly")]

mod import;
mod playback;
mod roundtrip;
