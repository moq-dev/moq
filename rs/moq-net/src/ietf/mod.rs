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
pub mod active_count;
pub(crate) mod auth;
pub mod cluster;
#[cfg(feature = "async")]
mod control;
mod datagram;
pub(crate) mod epoch;
pub(crate) mod error;
mod fetch;
mod filter;
pub(crate) mod flate;
mod goaway;
mod group;
pub mod hidden;
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
pub mod solicit;
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

#[cfg(test)]
mod tests {
	use super::*;

	/// Our Setup Options, pinned to the values the extension drafts register. A renumber
	/// still round trips against ourselves but breaks every other implementation.
	#[test]
	fn setup_option_code_points() {
		for (name, key, spec) in [
			("HOP_ID", cluster::HOP_ID, 0x40B54),
			("RELAY_COST", cluster::RELAY_COST, 0x40B56),
			("SOLICIT", solicit::SOLICIT, 0x40B5A),
			("HIDDEN", hidden::HIDDEN, 0x40B5C),
			("AUTH", auth::AUTH, 0x40B70),
			("ACTIVE_COUNT", active_count::ACTIVE_COUNT, 0x40B64),
			("EPOCH", epoch::EPOCH, 0x40B68),
		] {
			assert_eq!(key, spec, "{name}");
		}
	}

	/// Our message parameters, pinned to the values the extension drafts register.
	#[test]
	fn message_parameter_code_points() {
		for (name, key, spec) in [
			("HOP_PATH", cluster::HOP_PATH, 0x40B57),
			("ROUTE_COST", cluster::ROUTE_COST, 0x40B58),
			("HIDDEN", hidden::HIDDEN_PARAM, 0x40B5E),
			("ACTIVE_COUNT", active_count::ACTIVE_COUNT_PARAM, 0x40B66),
			("EPOCH", epoch::EPOCH_PARAM, 0x40B69),
		] {
			assert_eq!(key, spec, "{name}");
		}
	}
}
