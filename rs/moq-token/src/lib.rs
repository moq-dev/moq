//! JWT token generation and validation for MoQ authentication.
//!
//! Create and verify JWT tokens used for authorizing publish/subscribe operations in MoQ.
//! Tokens specify which broadcast paths a client can publish to and consume from.
//!
//! See [`Claims`] for the JWT claims structure and [`Key`] for key management.
//! Pattern types from [`moq-pattern`](moq_pattern) are re-exported for standalone use.
//! Token claims and authorization still use path prefixes.

mod algorithm;
mod claims;
mod error;
mod fs;
mod generate;
mod key;
mod key_id;
mod path;
mod set;

pub use algorithm::*;
pub use claims::*;
pub use error::*;
pub use key::*;
pub use key_id::*;
pub use moq_pattern::{InvalidPattern, Pattern, Patterns, Segment, Specificity};
pub use set::*;
