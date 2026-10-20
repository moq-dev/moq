//! An implementation of the IETF MoQ specification.
//!
//! Not all features are supported; just to provide compatibility with the crate API.
//!
//! You should not use this module directly; see [crate] for the high-level API.

// Private codec entry points remain testable when their session callers are absent.
#![cfg_attr(not(feature = "async"), allow(dead_code, unused_imports))]

#[macro_use]
mod parameters;
pub mod active_count;
#[cfg(feature = "async")]
mod adapter;
pub(crate) mod auth;
pub mod cluster;
#[cfg(feature = "async")]
mod control;
mod datagram;
pub(crate) mod epoch;
pub(crate) mod error;
mod fetch;
mod filter;
mod goaway;
mod group;
pub mod hidden;
mod location;
pub(crate) mod max_age;
pub mod message;
mod namespace;
mod object;
pub mod peer;
mod priority;
mod properties;
mod publish;
mod publish_namespace;
#[cfg(feature = "async")]
mod publisher;
mod request;
#[cfg(feature = "async")]
mod request_stream;
mod request_update;
#[cfg(feature = "async")]
mod session;
pub mod solicit;
mod subscribe;
mod subscribe_namespace;
#[cfg(feature = "async")]
mod subscriber;
pub(crate) mod token;
mod track;
mod version;

#[cfg(feature = "async")]
use control::Control;
#[cfg(feature = "async")]
pub(crate) use control::initial_max_request_id;
pub use datagram::*;
pub use fetch::*;
pub use filter::*;
pub use goaway::*;
pub use group::*;
pub use location::*;
pub use message::{Body, Message};
pub use parameters::*;
pub use properties::Properties;
pub use publish::*;
pub use publish_namespace::*;
#[cfg(feature = "async")]
use publisher::*;
pub use request::*;
#[cfg(feature = "async")]
pub use session::*;
pub use subscribe::*;
pub use subscribe_namespace::*;
#[cfg(feature = "async")]
use subscriber::*;
pub use track::*;
pub use version::Version;

#[cfg(all(feature = "async", any(test, feature = "fuzz")))]
pub(crate) fn request_ids(window: u64, ids: &[u64]) -> usize {
	let control = Control::new(None, false).with_window(window, false, Version::Draft16);
	let held: Vec<_> = ids
		.iter()
		.filter_map(|id| control.accept(RequestId(*id)).ok())
		.collect();
	held.len()
}

#[cfg(all(feature = "async", any(test, feature = "fuzz")))]
pub(crate) fn request_history(ids: &[u64]) -> usize {
	let control = Control::new(None, false);
	ids.iter().filter(|id| control.accept(RequestId(**id)).is_ok()).count()
}
