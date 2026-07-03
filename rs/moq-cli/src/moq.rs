//! Small MoQ-side helpers shared across endpoints.
//!
//! The dial and accept loops live in `moq-native` (`Client::publish`/`consume`
//! and `Server::serve_publish`/`serve_consume`); this module just carries the
//! systemd readiness notification used by every endpoint.

/// Notify systemd (if any) that the endpoint is up.
pub fn notify_ready() {
	#[cfg(unix)]
	let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);
}
