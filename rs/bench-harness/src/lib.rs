//! Criterion, or CodSpeed's instrumented Criterion when `cargo codspeed` sets `cfg(codspeed)`.
//!
//! The workspace depends on this as `criterion`, so benches stay plain Criterion. CodSpeed's
//! own compat crate is not used directly because its uninstrumented build is a Criterion 0.5
//! fork, which would replace Criterion 0.8 in every local and nightly run.

#[cfg(not(codspeed))]
pub use criterion::*;

#[cfg(codspeed)]
pub use codspeed_criterion_compat::*;
