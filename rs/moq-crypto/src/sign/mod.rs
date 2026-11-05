//! Per-frame Ed25519 signatures for MoQ broadcasts.
//!
//! Implements profile `moq-sign-00` from [draft-lcurley-moq-sign]. A publisher signs every
//! grouped frame with its endpoint's [`SecretKey`], binding the broadcast path, its publisher
//! instance, the track, the group, the frame index, and the timestamp. A subscriber hands a frame
//! to the application only after it verifies against the [`PublicKey`] it expects, so a relay
//! can proxy and cache without forging, retiming, relocating, or splicing content. Signatures
//! authenticate content, not liveness: a relay that recorded an instance can replay it whole under
//! its instance.
//!
//! [draft-lcurley-moq-sign]: https://datatracker.ietf.org/doc/draft-lcurley-moq-sign/
//!
//! A publisher wraps its `moq-net` broadcast in a [`broadcast::Producer`], which mints a fresh
//! instance to announce on the broadcast's route. A subscriber wraps the broadcast an origin
//! resolved in a [`broadcast::Consumer`], which takes the instance from that route. Both sides name
//! the broadcast by the same application path, independent of where a relay mounts it.
//!
//! A broadcast that needs no signature, the identity level, uses `moq-net` directly.
//!
//! Without the `sign` feature, this module holds only [`PublicKey`] and its text codec.

#![warn(missing_docs)]

#[cfg(feature = "sign")]
pub mod broadcast;
#[cfg(feature = "sign")]
pub mod group;
#[cfg(feature = "sign")]
pub mod track;

#[cfg(feature = "sign")]
mod error;
mod key;
#[cfg(feature = "sign")]
mod scope;
#[cfg(feature = "sign")]
mod secret;

#[cfg(feature = "sign")]
pub use error::{Error, Result};
pub use key::{InvalidKey, PublicKey};
#[cfg(feature = "sign")]
pub use secret::SecretKey;

#[cfg(all(test, feature = "sign"))]
mod tests;
