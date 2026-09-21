//! End-to-end encryption for MoQ groups, datagrams, and track names.
//!
//! Implements profile `moq-e2ee-00` from [draft-lcurley-moq-e2ee]. Relays forward
//! ciphertext; content keys never enter `moq-net`.
//!
//! [draft-lcurley-moq-e2ee]: https://datatracker.ietf.org/doc/draft-lcurley-moq-e2ee/
//!
//! A [`Credential`] is what the application distributes out of band. Each publisher
//! instance mints an [`Epoch`] and binds it as a [`Generation`], which derives opaque
//! track names and protects `moq-net` tracks. Subscribers discover the epoch from the
//! broadcast path and bind the same generation.
//!
//! AES-128-GCM is inline and synchronous: a 1 KiB frame costs about a microsecond
//! on one core (see the `protect` bench), so there is no async encryption pump.

#![warn(missing_docs)]

pub mod credential;
pub mod datagram;
pub mod group;
pub mod track;

mod epoch;
mod error;
mod generation;
mod key;
mod limits;
mod name;
mod protect;
mod window;

pub use credential::Credential;
pub use epoch::Epoch;
pub use error::{Error, Result};
pub use generation::Generation;
pub use limits::{MAX_DATAGRAM_PLAINTEXT, MAX_GROUPED_PLAINTEXT, SECRET_LEN};
pub use name::Name;

#[cfg(test)]
mod tests;
