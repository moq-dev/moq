//! Small MoQ-side helpers shared across endpoints.
//!
//! The dial and accept loops live in `moq-tokio` (`Client::publish`/`consume`
//! and `Server::serve_publish`/`serve_consume`); this module carries the systemd
//! readiness notification used by every endpoint plus the MoQ side of an import.

use hang::moq_net;

/// The MoQ side of an import: where a gateway publishes, under what name, and the
/// retention it declares on the media tracks it mints.
///
/// Every gateway needs all three regardless of protocol, so they travel together
/// rather than as three positional arguments per entry point.
#[derive(Clone)]
pub struct ImportTarget {
	/// The Origin the gateway publishes into.
	pub origin: moq_net::origin::Producer,

	/// The broadcast name to publish under.
	pub name: String,

	/// How long relays keep a non-latest group of the published media tracks fetchable
	/// (`--latency-max`), or `None` for the publisher's own default.
	pub max_age: Option<std::time::Duration>,
}

/// Notify systemd (if any) that the endpoint is up.
pub fn notify_ready() {
	#[cfg(unix)]
	let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);
}
