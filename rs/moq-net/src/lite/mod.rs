//! The low-level encoding for the moq-lite specification.
//!
//! You should not use this module directly; see [crate] for the high-level API.
//!
//! Specification: [<https://github.com/moq-dev/drafts>]

#![cfg_attr(
	not(feature = "async"),
	allow(
		dead_code,
		unused_imports,
		reason = "private codec entry points remain testable when their session callers are absent"
	)
)]

mod announce;
mod auth;
mod compress;
mod datagram;
mod epoch;
pub(crate) mod error;
mod fetch;
mod goaway;
mod group;
pub(crate) mod ietf;
mod info;
pub mod message;
mod parameters;
pub(crate) mod priority;
mod probe;
#[cfg(feature = "async")]
mod publisher;
#[cfg(feature = "async")]
mod replay;
#[cfg(feature = "async")]
mod session;
mod setup;
mod stream;
mod subscribe;
#[cfg(feature = "async")]
mod subscriber;
#[cfg(all(test, feature = "async"))]
pub(crate) mod test_transport;
mod track;
mod tracks;
mod version;

pub use announce::*;
pub use auth::*;
pub(crate) use compress::*;
pub use datagram::*;
pub use fetch::*;
pub use goaway::*;
pub use group::*;
pub use info::*;
pub use message::Message;
pub use parameters::*;
pub use probe::*;
#[cfg(feature = "async")]
use publisher::*;
#[cfg(feature = "async")]
pub(super) use session::*;
pub use setup::*;
pub use stream::*;
pub use subscribe::*;
#[cfg(feature = "async")]
use subscriber::*;
// The fuzz harness drives a group stream's ingest directly.
#[cfg(all(feature = "async", any(test, feature = "fuzz")))]
pub(crate) use subscriber::FrameIngest;
pub use track::*;
pub use tracks::*;
pub use version::Version;
