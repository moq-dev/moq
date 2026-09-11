//! Small MoQ-side helpers shared across endpoints.
//!
//! The dial and accept loops live in `moq-tokio` (`Client::publish`/`consume`
//! and `Server::serve_publish`/`serve_consume`); this module carries the systemd
//! readiness notification used by every endpoint plus the MoQ side of an import.

use hang::moq_net;

/// The MoQ side of an import: where a gateway publishes, under what name, the
/// retention it declares on the media tracks it mints, and the connection
/// allocator those tracks claim on.
///
/// Every gateway needs all four regardless of protocol, so they travel together
/// rather than as loose arguments per entry point.
#[derive(Clone)]
pub struct ImportTarget {
	/// The Origin the gateway publishes into.
	pub origin: moq_net::origin::Producer,

	/// The broadcast name to publish under.
	pub name: String,

	/// How long relays keep a non-latest group of the published media tracks fetchable
	/// (`--max-age`), or `None` for the publisher's own default.
	pub max_age: Option<std::time::Duration>,

	/// Connection allocator each passthrough track claims its peak-hold bitrate on,
	/// so a co-resident encoder targets what is left. Unlimited when there is no
	/// uplink estimate.
	pub bandwidth: moq_net::bandwidth::Allocator,
}

/// Notify systemd (if any) that the endpoint is up.
pub fn notify_ready() {
	#[cfg(unix)]
	let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);
}
