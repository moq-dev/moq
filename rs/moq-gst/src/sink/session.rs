//! The MoQ session: connect, transport lifecycle, and the observable status the element exposes.
//!
//! The producers are created here (so the broadcast/catalog exist before connect, buffering early
//! frames) but handed back to the element, which writes into them synchronously from each pad's
//! streaming thread. This task only owns connect, the transport's lifetime, and stats; it touches no
//! media.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use anyhow::Result;
use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;

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
/// `Disconnected` (a drop the reconnect loop is still retrying), so a consumer watching
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
	/// The reconnect loop gave up permanently (an auth rejection, or a CONNECT status that isn't an
	/// invitation to retry). Terminal.
	#[enum_value(name = "Failed: connection rejected, gave up", nick = "failed")]
	Failed,
}

/// The connect/version surface behind the `status`, `connected`, and `moq-version` properties. One per
/// session: the element swaps in a fresh `Arc` on every start, so a previous session's task (which may
/// still be unwinding) writes only its own detached copy and can never clobber the live status. The
/// bitrate properties read a [`moq_net::bandwidth::Consumer`] directly, so they aren't mirrored here.
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
	/// QUIC idle timeout override.
	pub quic_idle_timeout: Option<std::time::Duration>,
	/// QUIC keep-alive override, including zero to disable it.
	pub quic_keep_alive: Option<std::time::Duration>,
}

/// Builds the connect configuration with the sink's TLS and backoff overrides.
pub(super) fn connect_config(settings: &ResolvedSettings) -> moq_tokio::connect::Config {
	let mut config = moq_tokio::connect::Config::default();
	config.tls.insecure = Some(settings.tls_disable_verify);
	config.backoff.timeout = std::time::Duration::ZERO.into();
	config
}

/// The QUIC transport overrides the sink exposes as properties.
pub(super) fn quic_config(settings: &ResolvedSettings) -> moq_tokio::quic::Config {
	let mut config = moq_tokio::quic::Config::default();
	// The properties are optional and the config fields are not: an unset property
	// leaves the library default rather than overriding it with one of its own.
	if let Some(idle_timeout) = settings.quic_idle_timeout {
		config.idle_timeout = idle_timeout.into();
	}
	if let Some(keep_alive) = settings.quic_keep_alive {
		config.keep_alive = keep_alive.into();
	}
	config
}

/// Whether the publication is still open, and if not, how it ended.
///
/// Monotonic: `Open` moves once, and the first terminal transition wins. `Eos` beating a later error
/// is deliberate, not a tie-break: by then the producers and the catalog were consumed cleanly, so the
/// failure happened after the publication was already complete and must not rewrite it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Completion {
	/// Request pads, negotiation, publication and flow resets are allowed.
	Open,
	/// Every pad ended cleanly and the producers were consumed.
	Eos,
	/// A session or finalize error ended the publication.
	Failed,
}

/// The atomic behind a [`CompletionHandle`], reached only through the typed methods so no caller can
/// write a state the transition rules forbid.
pub(crate) struct CompletionState(AtomicU8);

/// One publication's completion, shared by its session task and by its pads.
///
/// Every clone belongs to one exact session, so this doubles as the session's identity: a task that
/// outlived its session still holds only that session's handle, and `Arc::ptr_eq` answers whether a
/// deferred message still belongs to the live publication. A terminal error staying inside its own
/// session falls out of the topology rather than being checked against a separate id.
pub(crate) type CompletionHandle = Arc<CompletionState>;

impl CompletionState {
	const OPEN: u8 = 0;
	const EOS: u8 = 1;
	const FAILED: u8 = 2;

	pub(crate) fn new() -> CompletionHandle {
		Arc::new(Self(AtomicU8::new(Self::OPEN)))
	}

	pub(crate) fn get(&self) -> Completion {
		match self.0.load(Ordering::Relaxed) {
			Self::EOS => Completion::Eos,
			Self::FAILED => Completion::Failed,
			_ => Completion::Open,
		}
	}

	pub(crate) fn is_open(&self) -> bool {
		self.0.load(Ordering::Relaxed) == Self::OPEN
	}

	/// Take the clean terminal state, returning whether this call is the one that took it.
	pub(crate) fn finish(&self) -> bool {
		self.take(Self::EOS)
	}

	/// Take the failed terminal state, returning whether this call is the one that took it.
	pub(crate) fn fail(&self) -> bool {
		self.take(Self::FAILED)
	}

	/// Compare-exchange rather than a store: a loser that overwrote the winner would turn an EOS the
	/// element already earned into a bus error.
	fn take(&self, terminal: u8) -> bool {
		self.0
			.compare_exchange(Self::OPEN, terminal, Ordering::Relaxed, Ordering::Relaxed)
			.is_ok()
	}
}

/// Permission for one session's task to report a terminal failure on the bus.
///
/// Held between creating the session and completing `READY -> PAUSED`. Marking it releases the task;
/// dropping it unmarked leaves the task parked, which is what a rolled-back transition wants, since
/// the session it belonged to is torn down with it.
pub(crate) struct SessionRegistration {
	gate: Arc<tokio::sync::Notify>,
}

impl SessionRegistration {
	/// Release the task: the element installed this session, so its errors now have somewhere to land.
	pub(crate) fn mark_registered(self) {
		self.gate.notify_one();
	}
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
	/// This publication's completion. The task moves it to `Failed` on a fatal transport error, so the
	/// pad streaming threads stop feeding a dead session without consulting the element.
	completion: CompletionHandle,
}

impl Session {
	/// Create the broadcast/catalog producers and spawn the connect task. Returns the producers for the
	/// element to write into; the session task owns only the origin, the connection, and the status.
	pub fn start(
		settings: ResolvedSettings,
		element: glib::WeakRef<Element>,
	) -> Result<(
		Self,
		SessionRegistration,
		moq_net::broadcast::Producer,
		moq_mux::catalog::Producer,
	)> {
		// Producer setup may touch tokio time (group eviction), so run it inside the runtime context.
		let _rt = RUNTIME.enter();

		let origin = moq_tokio::origin::spawn(moq_net::Hop::random());
		let mut broadcast = origin.create_broadcast(
			&settings.broadcast,
			moq_net::broadcast::Route::new().with_announce(true),
		)?;
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast)?;

		let status = Arc::new(Status::default());
		let completion = CompletionState::new();

		// Publish through a background reconnect loop: connect, wait for close, reconnect with backoff.
		// `timeout = 0` drops the give-up deadline so an unattended publisher outlives relay/QUIC
		// outages of any length, which is the trade this element wants: a pipeline nobody is watching
		// should still be publishing when the relay comes back. The loop still ends on the two answers
		// a server states outright (an auth rejection, or a CONNECT status that isn't an invitation to
		// retry), posting the bus error below. During an outage the pad threads keep writing (bounded
		// by moq-net's per-group eviction) and the relay catches up from a group boundary on
		// reconnect. A bounded policy is available via `ClientConfig::backoff`.
		let client = connect_config(&settings)
			.init(quic_config(&settings))?
			.with_publisher(origin.consume());
		let reconnect = client.connect(settings.url.clone());
		// Persistent handles that survive reconnects; the getters read them without touching the loop.
		let send_bandwidth = reconnect.send_bandwidth();
		let recv_bandwidth = reconnect.recv_bandwidth();

		// The task is spawned parked. An immediate auth rejection would otherwise race the element
		// installing this session, and its bus error would be discarded for belonging to no live one.
		let gate = Arc::new(tokio::sync::Notify::new());
		let join = RUNTIME.spawn(forward(
			reconnect,
			origin,
			status.clone(),
			completion.clone(),
			element,
			gate.clone(),
		));

		Ok((
			Self {
				join,
				status,
				send_bandwidth,
				recv_bandwidth,
				completion,
			},
			SessionRegistration { gate },
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
		self.send_bandwidth.peek().map_or(0, moq_net::bandwidth::Rate::as_bps)
	}

	/// The estimated receive bitrate in bits per second, 0 when disconnected or unavailable.
	pub fn recv_bitrate(&self) -> u64 {
		self.recv_bandwidth.peek().map_or(0, moq_net::bandwidth::Rate::as_bps)
	}

	/// Share this publication's completion with a pad's buffer path.
	pub fn completion(&self) -> CompletionHandle {
		self.completion.clone()
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
/// The reconnect loop owns the session; this task follows [`moq_tokio::Connection`] to mirror
/// status/version into the `Status` the getters read, and watches the persistent bandwidth consumers
/// only to `notify` the bitrate properties (the getters read the estimates directly). Each source is
/// notified on its own change: a status edge notifies `status`/`connected`/`moq-version` together, a
/// bitrate change notifies just that bitrate. The loop stops only on a terminal error (a non-retryable
/// auth failure, or a bounded backoff's give-up), which the `Err` arm posts as a bus error.
/// [`Session`]'s `Drop` aborts this task, which drops the `Connection` handle and quietly tears the loop
/// down.
async fn forward(
	reconnect: moq_tokio::Connection,
	origin: moq_net::origin::Producer,
	status: Arc<Status>,
	completion: CompletionHandle,
	element: glib::WeakRef<Element>,
	registered: Arc<tokio::sync::Notify>,
) {
	wait_for_registration(registered).await;
	forward_registered(reconnect, origin, status, completion, element).await;
}

async fn wait_for_registration(registered: Arc<tokio::sync::Notify>) {
	// `Notify` keeps the permit, so marking before the task parks here is not a lost wakeup. A session
	// rolled back instead of marked is aborted by `Session`'s `Drop`, which unparks nothing.
	registered.notified().await;
}

async fn forward_registered(
	mut reconnect: moq_tokio::Connection,
	origin: moq_net::origin::Producer,
	status: Arc<Status>,
	completion: CompletionHandle,
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
						moq_tokio::Status::Connected => ConnectionStatus::Connected,
						_ => ConnectionStatus::Disconnected,
					};
					status.set(connection, reconnect.version().map(|v| v.to_string()));
					match state {
						moq_tokio::Status::Connected => gst::info!(CAT, "session connected"),
						moq_tokio::Status::Disconnected => gst::warning!(CAT, "session disconnected, reconnecting"),
						_ => {}
					}
					notify(&element, &["status", "connected", "moq-version"]);
				}
				Err(err) => {
					// The reconnect loop stopped on a terminal error (a non-retryable auth failure, or a
					// bounded backoff's give-up). Ending the publication stops the pad threads feeding a
					// dead session; losing that race means it already ended, so there is nothing to report.
					let won = completion.fail();
					status.set(ConnectionStatus::Failed, None);
					notify(&element, &["status", "connected", "moq-version"]);
					if won && let Some(obj) = element.upgrade() {
						obj.imp().post_session_error(&completion, format!("{err:?}"));
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

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn a_terminal_result_waits_until_the_session_is_registered() {
		let gate = Arc::new(tokio::sync::Notify::new());
		let registration = SessionRegistration { gate: gate.clone() };
		let completion = CompletionState::new();
		let task_completion = completion.clone();
		let (entered, reached) = tokio::sync::oneshot::channel();

		let task = tokio::spawn(async move {
			entered.send(()).unwrap();
			wait_for_registration(gate).await;
			task_completion.fail();
		});
		reached.await.unwrap();
		assert_eq!(completion.get(), Completion::Open, "the terminal result stayed parked");

		registration.mark_registered();
		task.await.unwrap();
		assert_eq!(completion.get(), Completion::Failed, "registration released it");
	}
}
