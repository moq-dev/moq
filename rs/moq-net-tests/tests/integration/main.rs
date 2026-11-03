//! Tests that need JSON consumers or moq-net's hidden fuzz hooks.

mod ietf_conformance;
mod refresh;
mod restart;
mod snapshot;

#[path = "../../../moq-net/tests/integration/support/mod.rs"]
mod support;
