//! An implementation of the IETF MoQ specification.
//!
//! Not all features are supported; just to provide compatibility with the crate API.
//!
//! You should not use this module directly; see [crate] for the high-level API.

#![cfg_attr(
	not(feature = "async"),
	allow(
		dead_code,
		unused_imports,
		reason = "private codec entry points remain testable when their session callers are absent"
	)
)]

#[macro_use]
mod parameters;
#[cfg(feature = "async")]
mod control;
mod datagram;
pub(crate) mod error;
mod fetch;
mod filter;
pub(crate) mod flate;
mod goaway;
mod group;
mod location;
pub(crate) mod max_age;
pub mod message;
pub(crate) mod namespace;
pub(crate) mod object;
pub mod peer;
mod priority;
pub(crate) mod properties;
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
mod subscribe;
mod subscribe_namespace;
mod subscribe_tracks;
#[cfg(feature = "async")]
mod subscriber;
pub(crate) mod token;
mod track;
mod version;

#[cfg(feature = "async")]
use control::Control;
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
pub use subscribe_tracks::*;
#[cfg(feature = "async")]
use subscriber::*;
// The fuzz harness drives a subgroup stream's ingest directly.
pub use track::*;
pub use version::Version;
#[cfg(all(feature = "async", any(test, feature = "fuzz")))]
pub(crate) use {
	request_stream::{Framed, NamespaceUpdate, Next, Update},
	subscriber::GroupIngest,
};
