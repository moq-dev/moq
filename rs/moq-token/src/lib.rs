//! JWT token generation and validation for MoQ authentication.
//!
//! Create and verify JWT tokens used for authorizing publish/subscribe operations in MoQ.
//! Tokens specify which broadcast paths a client can publish to and consume from.
//!
//! See [`Claims`] for the JWT claims structure and [`Key`] for key management.
//! Path grants use [`Pattern`] from [`moq-pattern`](moq_pattern): the same grammar
//! `moq-net` re-exports. New minting should construct patterns rather than prefixes;
//! missing `v` on persisted claims stays v0 prefix semantics.

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
