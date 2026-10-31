//! End-to-end encryption for MoQ groups, datagrams, and track names.
//!
//! Implements profile `moq-e2ee-00` from [draft-lcurley-moq-e2ee]. Relays forward
//! ciphertext; content keys never enter `moq-net`.
//!
//! [draft-lcurley-moq-e2ee]: https://datatracker.ietf.org/doc/draft-lcurley-moq-e2ee/
//!
//! A [`Credential`] is what the application distributes out of band. Each publisher
//! instance calls [`Credential::produce`] to mint a [`generation::Producer`] and wraps its
//! `moq-net` broadcast in a [`broadcast::Producer`]. Subscribers follow the path, take each
//! instance's epoch from the broadcast it resolved, bind a [`generation::Consumer`] with
//! [`Credential::consume`], and wrap the broadcast in a [`broadcast::Consumer`]. Both sides name tracks by their semantic names; only
//! the derived physical names reach `moq-net`.
//!
//! A caller cannot reopen an explicit epoch for production:
//!
//! ```compile_fail,E0624
//! fn reopen(credential: moq_e2ee::Credential, epoch: moq_net::Epoch) {
//!     let _ = moq_e2ee::generation::Producer::new(credential, epoch);
//! }
//! ```
//!
//! AES-128-GCM is inline and synchronous: a 1 KiB frame costs about a microsecond
//! on one core (see the `protect` bench), so there is no async encryption pump.

#![warn(missing_docs)]

pub mod broadcast;
pub mod credential;
pub mod datagram;
pub mod group;
pub mod track;

mod error;
pub mod generation;
mod key;
mod limits;
mod name;
mod protect;
mod terminal;
mod window;

pub use credential::Credential;
pub use error::{Error, Result};
pub use limits::{MAX_DATAGRAM_PLAINTEXT, MAX_GROUPED_PLAINTEXT, SECRET_LEN};
pub use name::Name;

#[cfg(test)]
mod tests;
