//! The Ed25519 key that names an endpoint.
//!
//! An endpoint's [`SecretKey`] persists in a file and is the key a relay proves to its peers (the
//! cluster route key); its [`PublicKey`] is the endpoint ID, shown as 52 z-base-32 characters.
//!
//! Without the `endpoint` feature, this module holds only [`PublicKey`] and its text codec.

#![warn(missing_docs)]

mod key;
#[cfg(feature = "endpoint")]
mod secret;

pub use key::{InvalidKey, PublicKey};
#[cfg(feature = "endpoint")]
pub use secret::SecretKey;

#[cfg(all(test, feature = "endpoint"))]
mod tests;
