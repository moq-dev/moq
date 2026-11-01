//! Per-frame Ed25519 signatures for MoQ broadcasts.
//!
//! Implements profile `moq-sign-00` from [draft-lcurley-moq-sign]. A publisher signs every
//! grouped frame with its endpoint's [`SecretKey`], binding the broadcast path, its publisher
//! epoch, the track, the group, the frame index, and the timestamp. A subscriber hands a frame
//! to the application only after it verifies against the [`PublicKey`] it expects, so a relay
//! can proxy and cache without forging, retiming, relocating, or splicing content. Signatures
//! authenticate content, not liveness: a relay that recorded an instance can replay it whole under
//! its epoch.
//!
//! [draft-lcurley-moq-sign]: https://datatracker.ietf.org/doc/draft-lcurley-moq-sign/
//!
//! A publisher wraps its `moq-net` broadcast in a [`broadcast::Producer`], which mints a fresh
//! epoch to announce on the broadcast's route. A subscriber wraps the broadcast an origin
//! resolved in a [`broadcast::Consumer`], which takes the epoch from that route. Both sides name
//! the broadcast by the same application path, independent of where a relay mounts it.
//!
//! A broadcast that needs no signature, the identity level, uses `moq-net` directly.

#![warn(missing_docs)]

pub mod broadcast;
pub mod group;
pub mod track;

mod error;
mod key;
mod scope;

pub use error::{Error, Result};
pub use key::{InvalidKey, PublicKey, SecretKey};

#[cfg(test)]
mod tests;
