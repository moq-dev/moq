//! End-to-end encryption for MoQ groups, datagrams, catalogs, and opaque track names.
//!
//! Implements profile `moq-e2ee-01` from [draft-lcurley-moq-e2ee]. Relays forward
//! ciphertext; content keys never enter `moq-net`.
//!
//! [draft-lcurley-moq-e2ee]: https://datatracker.ietf.org/doc/draft-lcurley-moq-e2ee/
//!
//! # Processing
//!
//! AES-128-GCM is inline and synchronous. A 20 ms Opus datagram and a 1 KiB grouped
//! frame both finish far faster than real time on one core (see the `protect` bench),
//! so there is no async encryption pump. Memory is bounded by
//! [`GROUP_DUPLICATE_GROUPS`] (current plus previous group) and
//! [`DATAGRAM_DUPLICATE_WINDOW`] (1024 sequences). Those defaults match the profile
//! recommendation and do not change the wire.

#![warn(missing_docs)]

mod credential;
mod error;
mod key;
mod limits;
mod protect;
mod publication;
mod window;

pub mod catalog;
pub mod datagram;
pub mod group;
pub mod track;

pub use credential::{Credential, Domain, PhysicalName, Pin};
pub use datagram::{Datagram, Event as DatagramEvent};
pub use error::{Error, Result};
pub use group::Frame;
pub use key::TrackKey;
pub use limits::{
	DATAGRAM_DUPLICATE_WINDOW, DOMAIN_DATAGRAM, DOMAIN_GROUP, GROUP_DUPLICATE_GROUPS, KEY_LABEL, KEY_LEN,
	MAX_DATAGRAM_BODY, MAX_GROUPED_PAYLOAD, MAX_GROUPED_PLAINTEXT, MAX_INVOCATIONS, MAX_PLAINTEXT_BYTES, MAX_U32,
	MAX_U53, MIN_DATAGRAM_HEADER, NAME_LABEL, NAME_LEN, PHYSICAL_NAME_LEN, PROFILE, SALT, SECRET_LEN, TAG_LEN,
	datagram_payload_limit, varint_len,
};
pub use protect::{nonce, open, protect};
pub use publication::Publication;

#[cfg(test)]
mod tests;
