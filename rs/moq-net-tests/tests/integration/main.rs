//! Tests that need JSON consumers or moq-net's hidden fuzz hooks.

mod hop_limit;
mod ietf_conformance;
mod refresh;
mod restart;
mod serve;
mod snapshot;
mod wide_hop;

#[path = "../../../moq-net/tests/integration/support/mod.rs"]
mod support;
