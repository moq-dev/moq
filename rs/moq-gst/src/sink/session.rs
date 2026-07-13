//! The MoQ session: connect, transport lifecycle, and the observable status the element exposes.
//!
//! The producers are created here (so the broadcast/catalog exist before connect, buffering early
//! frames) but handed back to the element, which writes into them synchronously from each pad's
//! streaming thread. This task only owns connect, the transport's lifetime, and stats; it touches no
//! media.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use anyhow::{Result, ensure};
use gst::glib;
use gst::prelude::*;

use hang::moq_net;

use super::MoqSink as Element;

pub(crate) static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
	tokio::runtime::Builder::new_multi_thread()
		.enable_all()
		.build()
		.expect("spawn tokio runtime")
});

pub(crate) static CAT: LazyLock<gst::DebugCategory> =
	LazyLock::new(|| gst::DebugCategory::new("moq-sink", gst::DebugColorFlags::empty(), Some("MoQ Sink Element")));

/// The observable surface behind the read-only properties. One per session: the element swaps in a
/// fresh `Arc` on every start, so a previous session's task (which may still be unwinding) writes only
/// its own detached copy and can never clobber the live status. No generation bookkeeping needed.
#[derive(Default)]
struct StatusInner {
	connected: bool,
	version: Option<String>,
	send_bitrate: u64,
}

/// Shared session status, read by the element's property getters and written by the session task.
#[derive(Default)]
pub struct Status {
	inner: Mutex<StatusInner>,
}

impl Status {
	fn set_connected(&self, value: bool) {
		self.inner.lock().unwrap().connected = value;
	}

	fn set_version(&self, value: Option<String>) {
		self.inner.lock().unwrap().version = value;
	}

	fn set_send_bitrate(&self, bits_per_sec: u64) {
		self.inner.lock().unwrap().send_bitrate = bits_per_sec;
	}

	fn reset(&self) {
		*self.inner.lock().unwrap() = StatusInner::default();
	}

	/// Whether the session is currently connected.
	pub fn connected(&self) -> bool {
		self.inner.lock().unwrap().connected
	}

	/// The negotiated MoQ version, or None when disconnected.
	pub fn version(&self) -> Option<String> {
		self.inner.lock().unwrap().version.clone()
	}

	/// The congestion controller's send estimate in bits per second, 0 when unavailable.
	pub fn send_bitrate(&self) -> u64 {
		self.inner.lock().unwrap().send_bitrate
	}
}

/// The connection settings, validated out of the GObject properties.
#[derive(Clone)]
pub struct ResolvedSettings {
	/// Relay URL to connect to.
	pub url: url::Url,
	/// Name to publish the broadcast under.
	pub broadcast: String,
	/// Disable TLS certificate verification (local/dev use).
	pub tls_disable_verify: bool,
}

/// A running session: the connect/lifecycle task plus the status it writes. Dropping the producers
/// (held by the element) and calling [`Session::stop`] tears it down.
pub(crate) struct Session {
	join: tokio::task::JoinHandle<()>,
	status: Arc<Status>,
	/// Set by the task on a fatal transport error so the pad streaming threads stop feeding a dead session.
	errored: Arc<AtomicBool>,
}

impl Session {
	/// Create the broadcast/catalog producers and spawn the connect task. Returns the producers for the
	/// element to write into; the session task owns only the origin, the connection, and the status.
	pub fn start(
		settings: ResolvedSettings,
		element: glib::WeakRef<Element>,
	) -> Result<(Self, moq_net::BroadcastProducer, moq_mux::catalog::Producer)> {
		// Producer setup may touch tokio time (group eviction), so run it inside the runtime context.
		let _rt = RUNTIME.enter();

		let origin = moq_net::Origin::random().produce();
		let mut broadcast = moq_net::Broadcast::new().produce();
		let broadcast_consumer = broadcast.consume();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast)?;
		ensure!(
			origin.publish_broadcast(&settings.broadcast, broadcast_consumer),
			"failed to publish broadcast {}",
			settings.broadcast
		);

		let status = Arc::new(Status::default());
		let errored = Arc::new(AtomicBool::new(false));

		// Publish through a background reconnect loop (connect, wait for close, reconnect with
		// backoff) rather than a one-shot connect that died on the first transport drop. `timeout = 0`
		// retries transport/connection failures indefinitely so an unattended publisher outlives
		// relay/QUIC outages; non-retryable errors (e.g. auth) stay terminal. During an outage the pad
		// threads keep writing — bounded by moq-net's per-group eviction — and the relay catches up
		// from a group boundary on reconnect. A bounded policy is available via `ClientConfig::backoff`.
		let mut config = moq_native::ClientConfig::default();
		config.tls.disable_verify = Some(settings.tls_disable_verify);
		config.backoff.timeout = std::time::Duration::ZERO;
		let client = config.init()?.with_publish(origin.consume());
		let reconnect = client.reconnect(settings.url.clone());

		let join = RUNTIME.spawn(forward(reconnect, origin, status.clone(), errored.clone(), element));

		Ok((Self { join, status, errored }, broadcast, catalog))
	}

	/// The live status, read by the element's property getters.
	pub fn status(&self) -> &Arc<Status> {
		&self.status
	}

	/// Whether the transport has hit a fatal error (the pad streaming threads stop feeding it on this).
	pub fn errored(&self) -> bool {
		self.errored.load(Ordering::Relaxed)
	}

	/// Abort the task: a clean local close, never an error. The in-flight connect or idle loop is
	/// cancelled at its next await point and the connection drops.
	pub fn stop(self) {
		self.join.abort();
	}
}

/// Mirror the reconnect loop's observable state into the element's [`Status`] until the loop stops.
///
/// The reconnect loop owns the session, so this task forwards each [`moq_native::Snapshot`]
/// (connected/version/send-bitrate) into the status the property getters read, and notifies
/// `connected` on the connect/disconnect edges. The loop stops only on a terminal error — a
/// non-retryable auth failure, or (with a bounded backoff) a give-up — which the `Err` arm posts as
/// a bus error, matching the old one-shot path. [`Session::stop`] aborts this task, which drops the
/// `Reconnect` handle and quietly tears the loop down.
async fn forward(
	mut reconnect: moq_native::Reconnect,
	origin: moq_net::OriginProducer,
	status: Arc<Status>,
	errored: Arc<AtomicBool>,
	element: glib::WeakRef<Element>,
) {
	// Hold the origin producer for the task's lifetime so the published broadcast stays alive: the
	// reconnecting client owns the consumer (taken once, via `origin.consume()` at start) and
	// re-publishes it on each connect.
	let _origin = origin;

	let mut was_connected = false;
	loop {
		match reconnect.changed().await {
			Ok(snapshot) => {
				let connected = snapshot.status == Some(moq_native::Status::Connected);
				status.set_connected(connected);
				status.set_version(snapshot.version);
				status.set_send_bitrate(snapshot.send_bitrate);
				if connected != was_connected {
					if connected {
						gst::info!(CAT, "session connected");
					} else {
						gst::warning!(CAT, "session disconnected, reconnecting");
					}
					notify_connected(&element);
					was_connected = connected;
				}
			}
			Err(err) => {
				// The reconnect loop stopped on a terminal error (a non-retryable auth failure, or a
				// bounded backoff's give-up). Reset the observable surface, flag `errored` so the pad
				// threads stop feeding a dead session, and post a fatal element error.
				status.reset();
				if was_connected {
					notify_connected(&element);
				}
				errored.store(true, Ordering::Relaxed);
				if let Some(obj) = element.upgrade() {
					gst::element_error!(obj, gst::CoreError::Failed, ("session error"), ["{err:?}"]);
				}
				return;
			}
		}
	}
}

/// Notify the `connected` property on the connect/disconnect edges, never per sample.
fn notify_connected(element: &glib::WeakRef<Element>) {
	if let Some(obj) = element.upgrade() {
		obj.notify("connected");
	}
}
