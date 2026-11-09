//! Tests that need JSON consumers or moq-net's `fuzz` module.

mod hop_limit;
mod ietf_conformance;
mod refresh;
mod restart;
mod serve;
mod snapshot;
mod wide_hop;

#[path = "../../../moq-net/tests/integration/support/mod.rs"]
mod support;
