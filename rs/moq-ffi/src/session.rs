use std::sync::Arc;

use url::Url;

use crate::error::MoqError;
use crate::ffi::Task;
use crate::origin::{MoqOriginConsumer, MoqOriginProducer};

struct Client {
	config: moq_native::connect::Config,
	publish: Option<Arc<MoqOriginProducer>>,
	consume: Option<Arc<MoqOriginProducer>>,
}

impl Client {
	async fn connect(&self, url: Url) -> Result<Arc<MoqSession>, MoqError> {
		let client = self
			.config
			.clone()
			.init(Default::default())
			.map_err(map_connect_error)?;

		// Materialize both origin sides so the session can publish/subscribe and the FFI can
		// always hand back a publisher/consumer.
		let (publish, subscribe) = crate::origin::resolve_pair(self.publish.as_ref(), self.consume.as_ref());

		let connection = client
			.with_publisher(&publish)
			.with_subscriber(subscribe.clone())
			.connect(url);

		// Wait for the first session so auth errors surface here; later drops are the
		// connection's to ride out. `MoqClient::cancel` unblocks a dial stuck retrying.
		// `established` hands the connection back, which is what keeps it reconnecting.
		let connection = connection.established().await.map_err(map_connect_error)?;

		Ok(Arc::new(MoqSession::connected(connection, publish, subscribe)))
	}
}

fn map_connect_error(err: moq_native::Error) -> MoqError {
	match err.connect_error() {
		Some(moq_native::ConnectError::Unauthorized) => MoqError::Unauthorized,
		Some(moq_native::ConnectError::Forbidden) => MoqError::Forbidden,
		_ => MoqError::Connect(format!("{err}")),
	}
}

/// The terminal error of a connection that had already established a session.
///
/// Same auth rejections as [`map_connect_error`], but a session that ended carries
/// a [`moq_net::Error`], and stringifying that into `Connect` would both lose the
/// variant a caller can match on and claim the dial failed when it had succeeded.
/// A server-accepted session reports its close the same way.
///
/// A local stop is not a failure at all, so it maps to `Closed`, which is what the
/// bindings' `is_shutdown` recognizes. `status()` reaches this path whenever
/// another handle shuts the connection down underneath it, and reporting that as a
/// connect error would make every status watcher treat an expected teardown as a
/// broken connection.
fn map_closed_error(err: moq_native::Error) -> MoqError {
	match err.connect_error() {
		Some(moq_native::ConnectError::Unauthorized) => MoqError::Unauthorized,
		Some(moq_native::ConnectError::Forbidden) => MoqError::Forbidden,
		_ => match err {
			moq_native::Error::Stopped => MoqError::Closed,
			moq_native::Error::MoqNet(err) => err.into(),
			err => MoqError::Connect(format!("{err}")),
		},
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn maps_native_auth_connect_errors() {
		assert!(matches!(
			map_connect_error(moq_native::ConnectError::Unauthorized.into()),
			MoqError::Unauthorized
		));
		assert!(matches!(
			map_connect_error(moq_native::ConnectError::Forbidden.into()),
			MoqError::Forbidden
		));
	}

	/// A session that ended must report the close, not a dial failure. Auth
	/// rejections keep their dedicated variants, which is what `is_auth` reads in
	/// every binding, but everything else keeps the `moq_net::Error` a caller can
	/// match on instead of being flattened into a `Connect` string.
	#[test]
	fn maps_closed_errors_without_flattening_the_reason() {
		assert!(matches!(
			map_closed_error(moq_net::Error::Remote(7).into()),
			MoqError::Protocol(moq_net::Error::Remote(7))
		));
		assert!(matches!(
			map_closed_error(moq_net::Error::Cancel.into()),
			MoqError::Protocol(moq_net::Error::Cancel)
		));

		// A local stop is an expected teardown, not a failed connection: the bindings'
		// `is_shutdown` reads `Closed`, and `Connect` would read as a broken dial.
		assert!(matches!(map_closed_error(moq_native::Error::Stopped), MoqError::Closed));

		// Auth still wins, so `is_auth` keeps working on a rejection delivered as a close.
		assert!(matches!(
			map_closed_error(moq_net::Error::Unauthorized.into()),
			MoqError::Unauthorized
		));
		assert!(matches!(
			map_closed_error(moq_native::ConnectError::Forbidden.into()),
			MoqError::Forbidden
		));
	}

	#[test]
	fn sets_tls_system_roots() {
		let client = MoqClient::new();

		client.set_tls_system_roots(true);
		{
			let state = client.task.lock().expect("client state should be available");
			assert_eq!(state.config.tls.system_roots, Some(true));
		}

		client.set_tls_system_roots(false);
		let state = client.task.lock().expect("client state should be available");
		assert_eq!(state.config.tls.system_roots, Some(false));
	}

	#[test]
	fn sets_tls_client_cert_and_key() {
		let client = MoqClient::new();

		client.set_tls_cert(Some("cert.pem".into()));
		client.set_tls_key(Some("key.pem".into()));
		{
			let state = client.task.lock().expect("client state should be available");
			assert_eq!(state.config.tls.cert.as_deref(), Some(std::path::Path::new("cert.pem")));
			assert_eq!(state.config.tls.key.as_deref(), Some(std::path::Path::new("key.pem")));
		}

		client.set_tls_cert(None);
		client.set_tls_key(None);
		let state = client.task.lock().expect("client state should be available");
		assert_eq!(state.config.tls.cert, None);
		assert_eq!(state.config.tls.key, None);
	}
}

/// Retry pacing for the automatic reconnect (see [`MoqClient::set_backoff`]).
///
/// The delay starts at `initial_ms`, multiplies by `multiplier` after each failed
/// attempt, and caps at `max_ms`. After `timeout_ms` of consecutive failures the
/// connection gives up for good (0 retries forever); the window resets whenever a
/// session stays up past `initial_ms`.
#[derive(Clone, Debug, uniffi::Record)]
pub struct MoqBackoff {
	/// Delay before the first reconnect attempt, in milliseconds.
	#[uniffi(default = 1000)]
	pub initial_ms: u64,
	/// Multiplier applied to the delay after each failure.
	#[uniffi(default = 2)]
	pub multiplier: u32,
	/// Maximum delay between reconnect attempts, in milliseconds.
	#[uniffi(default = 30000)]
	pub max_ms: u64,
	/// Time spent retrying before giving up, in milliseconds. 0 retries forever.
	#[uniffi(default = 300000)]
	pub timeout_ms: u64,
}

#[derive(uniffi::Object)]
pub struct MoqClient {
	task: Task<Client>,
}

#[uniffi::export]
impl MoqClient {
	/// Create a new MoQ client with default configuration.
	#[uniffi::constructor]
	pub fn new() -> Arc<Self> {
		let _guard = crate::ffi::RUNTIME.enter();
		Arc::new(Self {
			task: Task::new(Client {
				config: moq_native::connect::Config::default(),
				publish: None,
				consume: None,
			}),
		})
	}

	/// Disable TLS certificate verification (for development only).
	pub fn set_tls_disable_verify(&self, disable: bool) {
		if let Some(mut state) = self.task.lock() {
			state.config.tls.insecure = Some(disable);
		}
	}

	/// Trust these PEM root certificate file(s) instead of the system roots.
	///
	/// Pass the paths to PEM-encoded CA certificates. An empty list restores the
	/// default behavior of using the platform's native root store.
	pub fn set_tls_roots(&self, paths: Vec<String>) {
		if let Some(mut state) = self.task.lock() {
			state.config.tls.root = paths.into_iter().map(Into::into).collect();
		}
	}

	/// Configure whether to also trust the platform's native root certificates.
	///
	/// By default, system roots are trusted only when no custom roots are configured.
	/// Set this to `true` to trust system roots in addition to roots from
	/// `set_tls_roots`, or `false` to trust only custom roots.
	pub fn set_tls_system_roots(&self, system_roots: bool) {
		if let Some(mut state) = self.task.lock() {
			state.config.tls.system_roots = Some(system_roots);
		}
	}

	/// Pin the peer to a certificate with one of these SHA-256 fingerprints, encoded as hex.
	///
	/// This is the native equivalent of the browser's WebTransport `serverCertificateHashes`
	/// and accepts the same values a server reports (see `MoqServer.cert_fingerprints`). Use it
	/// to trust a self-signed certificate without disabling verification. An empty list clears
	/// any pinned fingerprints.
	pub fn set_tls_fingerprints(&self, fingerprints: Vec<String>) {
		if let Some(mut state) = self.task.lock() {
			state.config.tls.fingerprint = fingerprints;
		}
	}

	/// Present this PEM certificate chain when the relay requires mTLS.
	///
	/// Only certificates are read from the file; any private keys are ignored. Must be
	/// paired with `set_tls_key`, otherwise `connect` fails with an incomplete-auth error.
	/// Pass `None` to clear a previously set path.
	pub fn set_tls_cert(&self, path: Option<String>) {
		if let Some(mut state) = self.task.lock() {
			state.config.tls.cert = path.map(Into::into);
		}
	}

	/// Present this PEM private key when the relay requires mTLS.
	///
	/// Only the private key is read from the file; any certificates are ignored. Must be
	/// paired with `set_tls_cert`, otherwise `connect` fails with an incomplete-auth error.
	/// Pass `None` to clear a previously set path.
	pub fn set_tls_key(&self, path: Option<String>) {
		if let Some(mut state) = self.task.lock() {
			state.config.tls.key = path.map(Into::into);
		}
	}

	/// Set the local UDP socket bind address. Defaults to `[::]:0`.
	///
	/// Returns an error if the address cannot be parsed.
	pub fn set_bind(&self, addr: String) -> Result<(), MoqError> {
		let parsed: std::net::SocketAddr = addr
			.parse()
			.map_err(|err| MoqError::Bind(format!("invalid bind address: {err}")))?;
		if let Some(mut state) = self.task.lock() {
			state.config.bind = Some(parsed);
		}
		Ok(())
	}

	/// Enable or disable automatic reconnecting. Enabled by default.
	///
	/// When enabled, the session returned by [`connect`](Self::connect) redials with
	/// backoff whenever the transport drops, and broadcasts consumed through it survive
	/// the gap. Disable for a one-shot dial: the transport's close then ends the session
	/// (surfaced via [`MoqSession::closed`]).
	pub fn set_reconnect(&self, enabled: bool) {
		if let Some(mut state) = self.task.lock() {
			state.config.once = Some(!enabled);
		}
	}

	/// Configure retry pacing for the automatic reconnect (see [`MoqBackoff`]).
	pub fn set_backoff(&self, backoff: MoqBackoff) {
		if let Some(mut state) = self.task.lock() {
			let mut out = moq_native::Backoff::default();
			out.initial = Some(std::time::Duration::from_millis(backoff.initial_ms));
			out.multiplier = Some(backoff.multiplier);
			out.max = Some(std::time::Duration::from_millis(backoff.max_ms));
			out.timeout = Some(std::time::Duration::from_millis(backoff.timeout_ms));
			state.config.backoff = out;
		}
	}

	/// Set the origin to publish local broadcasts to the remote.
	pub fn set_publish(&self, origin: Option<Arc<MoqOriginProducer>>) {
		if let Some(mut state) = self.task.lock() {
			state.publish = origin;
		}
	}

	/// Set the origin to consume remote broadcasts from the remote.
	pub fn set_consume(&self, origin: Option<Arc<MoqOriginProducer>>) {
		if let Some(mut state) = self.task.lock() {
			state.consume = origin;
		}
	}

	/// Connect to a MoQ server and wait for the session to be established.
	///
	/// The returned session automatically reconnects with backoff when the transport
	/// drops (unless disabled via [`set_reconnect`](Self::set_reconnect)), and broadcasts
	/// consumed through it ride out the gap. Watch [`MoqSession::status`] for the
	/// connect/disconnect transitions and [`MoqSession::closed`] for the connection
	/// giving up for good.
	///
	/// Both origin sides are always accessible via [`MoqSession::publisher`] and
	/// [`MoqSession::consumer`], without the caller constructing a [`MoqOriginProducer`]
	/// themselves. With neither [`set_publish`](Self::set_publish) nor
	/// [`set_consume`](Self::set_consume) wired, the two sides share one origin, so a broadcast
	/// announced on this session is also discoverable through it. Wiring either side opts out of
	/// that and gives the other side its own fresh origin.
	///
	/// Can be cancelled by calling `cancel()`, including while the initial dial is retrying.
	pub async fn connect(&self, url: String) -> Result<Arc<MoqSession>, MoqError> {
		let url = Url::parse(&url)?;

		self.task.run(|state| async move { state.connect(url).await }).await
	}

	/// Cancel all current and future `connect()` calls.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}

/// A snapshot of connection statistics for a [`MoqSession`].
///
/// Each field is `None` when the transport backend doesn't report that metric (native QUIC
/// reports all of them; the browser WebTransport reports few or none), or when it isn't yet
/// available (e.g. `send_rate_bps` before the congestion controller has a window). A `None` is
/// not the same as a zero value.
#[derive(uniffi::Record)]
pub struct MoqConnectionStats {
	/// Smoothed round-trip time, in microseconds.
	pub rtt_us: Option<u64>,
	/// Estimated send bandwidth from the congestion controller, in bits per second.
	pub send_rate_bps: Option<u64>,
	/// Estimated receive bandwidth from MoQ PROBE, in bits per second.
	pub recv_rate_bps: Option<u64>,
	/// Total bytes sent, including retransmissions and overhead.
	pub bytes_sent: Option<u64>,
	/// Total bytes received, including duplicates and overhead.
	pub bytes_received: Option<u64>,
	/// Total bytes lost (detected via retransmission or acknowledgement).
	pub bytes_lost: Option<u64>,
	/// Total datagrams sent.
	pub packets_sent: Option<u64>,
	/// Total datagrams received.
	pub packets_received: Option<u64>,
	/// Total datagrams detected as lost.
	pub packets_lost: Option<u64>,
}

impl From<moq_net::ConnectionStats> for MoqConnectionStats {
	fn from(stats: moq_net::ConnectionStats) -> Self {
		Self {
			rtt_us: stats.rtt.map(|d| d.as_micros() as u64),
			send_rate_bps: stats.estimated_send_rate,
			recv_rate_bps: stats.estimated_recv_rate,
			bytes_sent: stats.bytes_sent,
			bytes_received: stats.bytes_received,
			bytes_lost: stats.bytes_lost,
			packets_sent: stats.packets_sent,
			packets_received: stats.packets_received,
			packets_lost: stats.packets_lost,
		}
	}
}

/// A connection lifecycle transition reported by [`MoqSession::status`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MoqConnectionStatus {
	/// A session connected (the first connect, or a reconnect after a drop).
	Connected,
	/// The session dropped; a reconnect attempt follows.
	Disconnected,
	/// The peer sent a GOAWAY; the replacement is being dialed while the old
	/// session keeps serving.
	Migrating,
}

impl From<moq_native::Status> for MoqConnectionStatus {
	fn from(status: moq_native::Status) -> Self {
		match status {
			moq_native::Status::Connected => Self::Connected,
			moq_native::Status::Disconnected => Self::Disconnected,
			// A future unknown status means the loop is between the known states;
			// Migrating is the "still served, in flux" bucket.
			_ => Self::Migrating,
		}
	}
}

/// What backs a [`MoqSession`]: a client connection (a loop that may redial) or
/// a server-accepted session (one transport; the peer redialing yields a fresh
/// `accept()`).
#[derive(Clone)]
enum Inner {
	Connection(moq_native::Connection),
	Session(moq_net::Session),
}

#[derive(uniffi::Object)]
pub struct MoqSession {
	inner: Inner,
	/// Serializes `closed()` calls onto the FFI runtime; holds its own `Inner` clone
	/// so a parked `closed()` doesn't block the rest of the surface.
	closed: Task<Inner>,
	/// Serializes `status()` calls, which need `&mut` for per-handle change tracking.
	status: Task<Inner>,
	publisher: Arc<MoqOriginProducer>,
	consumer: Arc<MoqOriginConsumer>,
}

impl MoqSession {
	/// Wrap a client connection (see [`MoqClient::connect`]).
	pub(crate) fn connected(
		connection: moq_native::Connection,
		publish: moq_net::origin::Producer,
		subscribe: moq_net::origin::Producer,
	) -> Self {
		Self::build(Inner::Connection(connection), publish, subscribe)
	}

	/// Wrap a server-accepted session (see `MoqServer::accept`).
	pub(crate) fn accepted(
		session: moq_net::Session,
		publish: moq_net::origin::Producer,
		subscribe: moq_net::origin::Producer,
	) -> Self {
		Self::build(Inner::Session(session), publish, subscribe)
	}

	fn build(inner: Inner, publish: moq_net::origin::Producer, subscribe: moq_net::origin::Producer) -> Self {
		// Eagerly wrap the wired origin sides so each publisher()/consumer()
		// call hands back the same Arc. `publish` is published into; `subscribe`
		// is where the remote's broadcasts land (read via its consumer view).
		let publisher = Arc::new(MoqOriginProducer::from_inner(publish));
		let consumer = Arc::new(MoqOriginConsumer::from_inner(subscribe.consume()));
		Self {
			inner: inner.clone(),
			closed: Task::new(inner.clone()),
			status: Task::new(inner),
			publisher,
			consumer,
		}
	}

	/// Abort the live transport (if any) with `err` and stop any reconnect loop.
	fn teardown(&self, err: moq_net::Error) {
		match &self.inner {
			Inner::Connection(connection) => connection.abort(err),
			Inner::Session(session) => session.abort(err),
		}
	}
}

impl Drop for MoqSession {
	fn drop(&mut self) {
		let _guard = crate::ffi::RUNTIME.enter();
		// Close the transport while the runtime is entered. The backend spawns a
		// lingering CLOSE task, which panics (aborting under panic=abort) if no reactor
		// is in context. We can't leave this to the last `Session` clone's drop: clones
		// live in the `closed`/`status` tasks and the connection state, released after
		// this guard, off-runtime. Close-once dedup then makes those trailing drops no-ops.
		self.teardown(moq_net::Error::Cancel);
	}
}

#[uniffi::export]
impl MoqSession {
	/// Wait until the session is over.
	///
	/// A client session resolves when its connection stops for good: `Err` with the
	/// terminal error when it gave up (retries exhausted, or the session's close reason
	/// with reconnecting disabled), `Ok` after a local [`shutdown`](Self::shutdown) /
	/// [`cancel`](Self::cancel). Transient drops the reconnect loop rides out do not
	/// resolve this; watch [`status`](Self::status) for those. A server-accepted
	/// session resolves with the session's close reason.
	pub async fn closed(&self) -> Result<(), MoqError> {
		// We have a task to run all of the closed calls juuuuust so they use the same tokio runtime.
		self.closed
			.run(|inner| async move {
				match &*inner {
					Inner::Connection(connection) => connection.closed().await.map_err(map_closed_error),
					Inner::Session(session) => Err(session.closed().await.into()),
				}
			})
			.await
	}

	/// Wait for the connection status to differ from the one this handle last reported.
	///
	/// A client session reports `Connected` first (the connect it was built from), then
	/// follows the reconnect loop: `Disconnected` while redialing, `Connected` again on
	/// success, `Migrating` during a GOAWAY handover. It returns an error once the
	/// connection stops for good (same terminal result as [`closed`](Self::closed)).
	/// A server-accepted session is a single transport, so its only transition is
	/// terminal: this waits for the close and returns its reason.
	///
	/// This is the current status, not a queue of every edge: a drop that reconnects
	/// before you ask again is coalesced away, so the outages it hides are the ones
	/// that already healed. Don't count outages with it.
	pub async fn status(&self) -> Result<MoqConnectionStatus, MoqError> {
		self.status
			.run(|mut inner| async move {
				match &mut *inner {
					Inner::Connection(connection) => Ok(connection.status().await.map_err(map_closed_error)?.into()),
					Inner::Session(session) => Err(session.closed().await.into()),
				}
			})
			.await
	}

	/// Close the session with the given error code, stopping any reconnect loop.
	pub fn cancel(&self, code: u32) {
		let _guard = crate::ffi::RUNTIME.enter();
		self.teardown(moq_net::Error::Remote(code));
		// NOTE: we don't abort the closed Task; the teardown above resolves it
		// (with the close reason, or Ok once the connection loop stops).
	}

	/// Graceful shutdown. Equivalent to `cancel(0)`. Documents the
	/// convention that code 0 means "no error" so callers don't have to
	/// pick one. Named `shutdown` (not `close`) because UniFFI's Kotlin
	/// generator already emits an `AutoCloseable.close()` that releases
	/// the FFI handle, and shadowing it would silently mean a different
	/// thing per binding.
	pub fn shutdown(&self) {
		self.cancel(0);
	}

	/// The publish-side origin: where local broadcasts get advertised
	/// to the remote. Either the producer the caller wired via
	/// `set_publish` / `set_consume` before connect/accept, or one
	/// auto-created if neither was set.
	pub fn publisher(&self) -> Arc<MoqOriginProducer> {
		self.publisher.clone()
	}

	/// The subscribe-side origin: a read handle for receiving
	/// announcements pushed by the remote. Either derived from the
	/// origin the caller wired via `set_consume`, or auto-created if
	/// neither was set.
	pub fn consumer(&self) -> Arc<MoqOriginConsumer> {
		self.consumer.clone()
	}

	/// Snapshot the current connection statistics (RTT, bandwidth estimates,
	/// byte/packet counters). Cheap to call; intended for periodic polling.
	///
	/// Individual fields are `None` when the transport backend doesn't report
	/// them, or (on a client session) while the connection is between sessions;
	/// see [`MoqConnectionStats`].
	pub fn stats(&self) -> MoqConnectionStats {
		let _guard = crate::ffi::RUNTIME.enter();
		match &self.inner {
			Inner::Connection(connection) => connection.stats().stats(),
			Inner::Session(session) => Some(session.stats()),
		}
		.unwrap_or_default()
		.into()
	}
}
