//! The MoQ session: connect, transport lifecycle, and the observable status the element exposes.
//!
//! The producers are created here (so the broadcast/catalog exist before connect, buffering early
//! frames) but handed back to the element, which writes into them synchronously from each pad's
//! streaming thread. This task only owns connect, the transport's lifetime, and stats; it touches no
//! media.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use anyhow::Result;
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

/// The publish connection's lifecycle, surfaced as the `status` property.
///
/// Bundles what a bare `connected` bool can't: `Failed` (a terminal give-up) is distinct from
/// `Disconnected` (a transient drop the reconnect loop is still retrying), so a consumer watching
/// `notify::status` learns when a connection is newly established or permanently rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, glib::Enum)]
#[enum_type(name = "GstMoqSinkConnectionStatus")]
pub enum ConnectionStatus {
	/// No live session: either the first connect is still pending or an established one dropped and a
	/// reconnect is in flight.
	#[default]
	#[enum_value(name = "Disconnected: no live session, (re)connecting", nick = "disconnected")]
	Disconnected,
	/// A session is connected and publishing.
	#[enum_value(name = "Connected: session established", nick = "connected")]
	Connected,
	/// The reconnect loop gave up permanently (a non-retryable error, e.g. auth rejection). Terminal.
	#[enum_value(name = "Failed: connection rejected, gave up", nick = "failed")]
	Failed,
}

/// The connect/version surface behind the `status`, `connected`, and `moq-version` properties. One per
/// session: the element swaps in a fresh `Arc` on every start, so a previous session's task (which may
/// still be unwinding) writes only its own detached copy and can never clobber the live status. No
/// generation bookkeeping needed. The bitrate properties read a [`moq_net::bandwidth::Consumer`] directly,
/// so they aren't mirrored here.
#[derive(Default)]
struct StatusInner {
	status: ConnectionStatus,
	version: Option<String>,
}

/// Shared session status, read by the element's property getters and written by the session task.
#[derive(Default)]
pub struct Status {
	inner: Mutex<StatusInner>,
}

impl Status {
	/// Set the connection status and negotiated version together, so a `notify::status` handler that
	/// re-reads `moq-version` sees the two consistent.
	fn set(&self, status: ConnectionStatus, version: Option<String>) {
		let mut inner = self.inner.lock().unwrap();
		inner.status = status;
		inner.version = version;
	}

	/// The current connection lifecycle status.
	pub fn status(&self) -> ConnectionStatus {
		self.inner.lock().unwrap().status
	}

	/// Whether a session is currently connected.
	pub fn connected(&self) -> bool {
		self.inner.lock().unwrap().status == ConnectionStatus::Connected
	}

	/// The negotiated MoQ version, or None when not connected.
	pub fn version(&self) -> Option<String> {
		self.inner.lock().unwrap().version.clone()
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

/// A running session: the connect/lifecycle task plus the state the property getters read. Dropping the
/// `Session` (or the producers held by the element) tears it down.
pub(crate) struct Session {
	join: tokio::task::JoinHandle<()>,
	status: Arc<Status>,
	/// The live send-bitrate estimate, tracked across reconnects by the reconnect loop. Read directly
	/// by the `estimated-send-bitrate` getter.
	send_bandwidth: moq_net::bandwidth::Consumer,
	/// The live recv-bitrate estimate, tracked across reconnects by the reconnect loop. Read directly
	/// by the `estimated-recv-bitrate` getter.
	recv_bandwidth: moq_net::bandwidth::Consumer,
	/// Set by the task on a fatal transport error so the pad streaming threads stop feeding a dead session.
	errored: Arc<AtomicBool>,
}

impl Session {
	/// Create the broadcast/catalog producers and spawn the connect task. Returns the producers for the
	/// element to write into; the session task owns only the origin, the connection, and the status.
	pub fn start(
		settings: ResolvedSettings,
		element: glib::WeakRef<Element>,
	) -> Result<(Self, moq_net::broadcast::Producer, moq_mux::catalog::Producer)> {
		// Producer setup may touch tokio time (group eviction), so run it inside the runtime context.
		let _rt = RUNTIME.enter();

		let origin = moq_net::Origin::random().produce();
		let mut broadcast = origin.create_broadcast(
			&settings.broadcast,
			moq_net::broadcast::Route::new().with_announce(true),
		)?;
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast)?;

		let status = Arc::new(Status::default());
		let errored = Arc::new(AtomicBool::new(false));

		// Publish through a background reconnect loop: connect, wait for close, reconnect with backoff.
		// `timeout = 0` retries transport/connection failures indefinitely so an unattended publisher
		// outlives relay/QUIC outages; non-retryable errors (e.g. auth) stay terminal. During an outage
		// the pad threads keep writing (bounded by moq-net's per-group eviction) and the relay catches up
		// from a group boundary on reconnect. A bounded policy is available via `ClientConfig::backoff`.
		let mut config = moq_native::ClientConfig::default();
		config.tls.disable_verify = Some(settings.tls_disable_verify);
		config.backoff.timeout = std::time::Duration::ZERO;
		let client = config.init()?.with_publisher(origin.consume());
		let reconnect = client.reconnect(settings.url.clone());
		// Persistent handles that survive reconnects; the getters read them without touching the loop.
		let send_bandwidth = reconnect.send_bandwidth();
		let recv_bandwidth = reconnect.recv_bandwidth();

		let join = RUNTIME.spawn(forward(reconnect, origin, status.clone(), errored.clone(), element));

		Ok((
			Self {
				join,
				status,
				send_bandwidth,
				recv_bandwidth,
				errored,
			},
			broadcast,
			catalog,
		))
	}

	/// The live status, read by the element's property getters.
	pub fn status(&self) -> &Arc<Status> {
		&self.status
	}

	/// The congestion controller's send estimate in bits per second, 0 when disconnected or unavailable.
	pub fn send_bitrate(&self) -> u64 {
		self.send_bandwidth.peek().unwrap_or(0)
	}

	/// The estimated receive bitrate in bits per second, 0 when disconnected or unavailable.
	pub fn recv_bitrate(&self) -> u64 {
		self.recv_bandwidth.peek().unwrap_or(0)
	}

	/// Whether the transport has hit a fatal error (the pad streaming threads stop feeding it on this).
	pub fn errored(&self) -> bool {
		self.errored.load(Ordering::Relaxed)
	}

	/// Stop the session: a clean local close, never an error. [`Drop`] aborts the task, cancelling the
	/// in-flight connect or reconnect loop at its next await point and dropping the connection.
	pub fn stop(self) {}
}

impl Drop for Session {
	fn drop(&mut self) {
		// Abort on any teardown path (explicit `stop`, or the element dropped early) so the reconnect
		// loop can't outlive the element.
		self.join.abort();
	}
}

/// Track the reconnect loop's observable state into the element's [`Status`] and fire GObject
/// notifications until the loop stops.
///
/// The reconnect loop owns the session; this task follows [`moq_native::Reconnect`] to mirror
/// status/version into the `Status` the getters read, and watches the persistent bandwidth consumers
/// only to `notify` the bitrate properties (the getters read the estimates directly). Each source is
/// notified on its own change: a status edge notifies `status`/`connected`/`moq-version` together, a
/// bitrate change notifies just that bitrate. The loop stops only on a terminal error (a non-retryable
/// auth failure, or a bounded backoff's give-up), which the `Err` arm posts as a bus error.
/// [`Session`]'s `Drop` aborts this task, which drops the `Reconnect` handle and quietly tears the loop
/// down.
async fn forward(
	mut reconnect: moq_native::Reconnect,
	origin: moq_net::origin::Producer,
	status: Arc<Status>,
	errored: Arc<AtomicBool>,
	element: glib::WeakRef<Element>,
) {
	// Hold the origin producer for the task's lifetime so the broadcast created on it stays routable:
	// the reconnecting client owns the consumer (taken once, via `origin.consume()` at start) and
	// serves it on each connect.
	let _origin = origin;

	// Persistent across reconnects; watched only to fire property notifications.
	let mut send_bandwidth = reconnect.send_bandwidth();
	let mut recv_bandwidth = reconnect.recv_bandwidth();

	loop {
		tokio::select! {
			// Poll status first: on a terminal error it is ready immediately, so we exit rather than
			// spinning on the bandwidth channels the loop closes as it stops.
			biased;

			result = reconnect.status() => match result {
				Ok(state) => {
					let connection = match state {
						moq_native::Status::Connected => ConnectionStatus::Connected,
						_ => ConnectionStatus::Disconnected,
					};
					status.set(connection, reconnect.version().map(|v| v.to_string()));
					match state {
						moq_native::Status::Connected => gst::info!(CAT, "session connected"),
						moq_native::Status::Disconnected => gst::warning!(CAT, "session disconnected, reconnecting"),
						_ => {}
					}
					notify(&element, &["status", "connected", "moq-version"]);
				}
				Err(err) => {
					// The reconnect loop stopped on a terminal error (a non-retryable auth failure, or a
					// bounded backoff's give-up). Flag `errored` so the pad threads stop feeding a dead
					// session, and post a fatal element error.
					status.set(ConnectionStatus::Failed, None);
					notify(&element, &["status", "connected", "moq-version"]);
					errored.store(true, Ordering::Relaxed);
					if let Some(obj) = element.upgrade() {
						gst::element_error!(obj, gst::CoreError::Failed, ("session error"), ["{err:?}"]);
					}
					return;
				}
			},

				// A closed estimate means the reconnect loop is gone for good, so stop rather than spin on
				// a channel that is now always ready. The biased status arm above wins when it has the
				// reason, which is the usual way this loop ends.
				result = send_bandwidth.changed() => match result {
					Ok(_) => notify(&element, &["estimated-send-bitrate"]),
					Err(_) => return,
				},
				result = recv_bandwidth.changed() => match result {
					Ok(_) => notify(&element, &["estimated-recv-bitrate"]),
					Err(_) => return,
				},
		}
	}
}

/// Emit a GObject `notify` for each named property, on the connect/disconnect/bitrate edges, never per
/// sample.
fn notify(element: &glib::WeakRef<Element>, props: &[&str]) {
	if let Some(obj) = element.upgrade() {
		for prop in props {
			obj.notify(prop);
		}
	}
}
