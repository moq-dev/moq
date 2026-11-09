//! Small MoQ-side helpers shared across endpoints.
//!
//! The dial and accept loops live in `moq-tokio` (`Client::publish`/`consume`
//! and `Server::serve_publish`/`serve_consume`); this module carries the systemd
//! readiness notification used by every endpoint plus the MoQ side of an import.

/// The MoQ side of an import: where a gateway publishes, under what name, and how
/// (`--max-age`, `--linger`, and the uplink's allocator).
///
/// They travel together rather than as loose arguments per entry point.
#[derive(Clone)]
pub struct ImportTarget {
	/// The Origin the gateway publishes into.
	pub origin: moq_net::origin::Producer,

	/// The broadcast name to publish under.
	pub name: String,

	/// How the gateway publishes the broadcast.
	pub ingest: moq_mux::Ingest,
}

/// Set once the process takes a shutdown signal: it is draining from then on, not starting.
static STOPPING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Record that the process is shutting down, so no endpoint reports ready after it.
pub fn stop() {
	STOPPING.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Notify systemd (if any) that the endpoint is up, unless the process is shutting down.
pub fn notify_ready() {
	#[cfg(unix)]
	if !STOPPING.load(std::sync::atomic::Ordering::Relaxed) {
		let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);
	}
}
