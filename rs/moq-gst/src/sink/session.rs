//! The MoQ session: connect and keep the publish alive across transport drops.
//!
//! The producers are created here (so the broadcast/catalog exist before connect, buffering early
//! frames) but handed back to the element, which writes into them synchronously from each pad's
//! streaming thread. This task only owns the origin and the background reconnect loop; it touches no
//! media.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

use anyhow::{Result, ensure};
use gst::glib;

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

/// A running session: the background reconnect loop plus the fatal-error flag it sets. Dropping the
/// producers (held by the element) and calling [`Session::stop`] tears it down.
pub(crate) struct Session {
	join: tokio::task::JoinHandle<()>,
	/// Set by the task if the reconnect loop permanently gives up, so the pad streaming threads stop
	/// feeding a dead session.
	errored: Arc<AtomicBool>,
}

impl Session {
	/// Create the broadcast/catalog producers and spawn the reconnect task. Returns the producers for
	/// the element to write into; the session task owns only the origin and the reconnect loop.
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

		let errored = Arc::new(AtomicBool::new(false));

		// Hand the publish origin to a background reconnect loop: connect, wait for the session to
		// close, then reconnect with exponential backoff. This replaces the previous one-shot connect
		// that posted a fatal bus error on the first transport death. A relay restart or QUIC idle
		// timeout left the publish permanently dead until the pipeline was rebuilt. `timeout = 0` means
		// never give up, so an unattended publisher outlives arbitrary relay/transport outages; the
		// pad threads keep writing across an outage (bounded by moq-net's per-group eviction) and the
		// relay catches up from a group boundary on reconnect. A bounded retry policy is available via
		// `ClientConfig::backoff` if a terminal error is preferred.
		let mut config = moq_native::ClientConfig::default();
		config.tls.disable_verify = Some(settings.tls_disable_verify);
		config.backoff.timeout = std::time::Duration::ZERO;
		let client = config.init()?.with_publish(origin.consume());
		let reconnect = client.reconnect(settings.url.clone());

		let join = RUNTIME.spawn(run(reconnect, origin, errored.clone(), element));

		Ok((Self { join, errored }, broadcast, catalog))
	}

	/// Whether the reconnect loop has permanently given up (the pad streaming threads stop feeding it
	/// on this).
	pub fn errored(&self) -> bool {
		self.errored.load(Ordering::Relaxed)
	}

	/// Abort the task: a clean local close, never an error. Dropping the [`moq_native::Reconnect`]
	/// handle tears the loop down and drops the connection.
	pub fn stop(self) {
		self.join.abort();
	}
}

/// Hold the origin alive and wait for the reconnect loop to stop.
///
/// The reconnect loop owns the session and reconnects forever, so this task only keeps `origin`
/// (hence the published broadcast) alive and waits. With `backoff.timeout = 0` the loop never gives
/// up, so [`Reconnect::closed`](moq_native::Reconnect::closed) resolving `Err` is a safety net: a
/// bounded backoff would land here on final give-up, where we flag `errored` and post a fatal element
/// error, matching the old one-shot failure path. [`Session::stop`] aborts this task, dropping the
/// `Reconnect` handle and quietly tearing the loop down.
async fn run(
	reconnect: moq_native::Reconnect,
	origin: moq_net::OriginProducer,
	errored: Arc<AtomicBool>,
	element: glib::WeakRef<Element>,
) {
	// `origin` is held for the task's lifetime so the published broadcast stays alive across every
	// reconnect; the loop re-consumes it (a fresh publish leg) for each connection.
	let _origin = origin;

	if let Err(err) = reconnect.closed().await {
		errored.store(true, Ordering::Relaxed);
		if let Some(obj) = element.upgrade() {
			gst::element_error!(obj, gst::CoreError::Failed, ("session error"), ["{err:?}"]);
		}
	}
}
