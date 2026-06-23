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

		let join = RUNTIME.spawn(run(settings, origin, status.clone(), errored.clone(), element));

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

/// Connect, then idle on the transport until it closes or dies. A remote death is surfaced as an
/// element error on the bus; [`Session::stop`] aborts this task instead, which is quiet.
async fn run(
	settings: ResolvedSettings,
	origin: moq_net::OriginProducer,
	status: Arc<Status>,
	errored: Arc<AtomicBool>,
	element: glib::WeakRef<Element>,
) {
	// `origin` is held for the task's lifetime so the published broadcast stays alive across the session.
	let result = connect_and_run(&settings, &origin, &status, &element).await;

	// Reset the observable surface on exit. The Status arc is private to this session, so this never
	// touches a newer session's status even if a new one started before this task unwound.
	status.reset();
	notify_connected(&element);

	if let Err(err) = result {
		errored.store(true, Ordering::Relaxed);
		if let Some(obj) = element.upgrade() {
			gst::element_error!(obj, gst::CoreError::Failed, ("session error"), ["{err:?}"]);
		}
	}
}

async fn connect_and_run(
	settings: &ResolvedSettings,
	origin: &moq_net::OriginProducer,
	status: &Status,
	element: &glib::WeakRef<Element>,
) -> Result<()> {
	let mut config = moq_native::ClientConfig::default();
	config.tls.disable_verify = Some(settings.tls_disable_verify);
	let client = config.init()?.with_publish(origin.consume());

	// A stop() during connect aborts this task, cancelling the connect future cleanly.
	let session = client.connect(settings.url.clone()).await?;
	status.set_connected(true);
	status.set_version(Some(session.version().to_string()));
	notify_connected(element);
	gst::info!(CAT, "session connected to {}", settings.url);

	// Congestion-controller send estimate; None when unavailable, then this arm parks forever.
	let mut send_bandwidth = session.send_bandwidth();
	// Resolves to Err when the transport dies; pinned so the select polls it each iteration.
	let closed = session.closed();
	tokio::pin!(closed);

	loop {
		tokio::select! {
			// Remote death: propagate the Err so the wrapper posts an element error to the bus.
			result = &mut closed => return Ok(result?),
			bitrate = async {
				match send_bandwidth.as_mut() {
					Some(bw) => bw.changed().await,
					None => std::future::pending::<Option<u64>>().await,
				}
			} => match bitrate {
				Some(rate) => status.set_send_bitrate(rate),
				None => {
					// Estimate gone: report 0 (the documented "unavailable" value) and stop polling.
					status.set_send_bitrate(0);
					send_bandwidth = None;
				}
			},
		}
	}
}

/// Notify the `connected` property on the connect/disconnect edges, never per sample.
fn notify_connected(element: &glib::WeakRef<Element>) {
	if let Some(obj) = element.upgrade() {
		obj.notify("connected");
	}
}
