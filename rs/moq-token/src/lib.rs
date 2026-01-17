//! JWT token generation and validation for MoQ authentication.
//!
//! Create and verify JWT tokens used for authorizing publish/subscribe operations in MoQ.
//! Tokens specify which broadcast paths a client can publish to and consume from.
//!
//! See [`Claims`] for the JWT claims structure and [`Key`] for key management.

mod algorithm;
mod claims;
mod generate;
mod key;
mod set;

pub use algorithm::*;
pub use claims::*;
pub use key::*;
pub use set::*;
