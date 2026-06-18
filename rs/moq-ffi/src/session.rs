use std::sync::Arc;

use moq_net::Session;
use url::Url;

use crate::error::MoqError;
use crate::ffi::Task;
use crate::origin::{MoqOriginConsumer, MoqOriginProducer};

struct Client {
	config: moq_native::ClientConfig,
	publish: Option<Arc<MoqOriginProducer>>,
	consume: Option<Arc<MoqOriginProducer>>,
}

impl Client {
	async fn connect(&self, url: Url) -> Result<Arc<MoqSession>, MoqError> {
		let client = self.config.clone().init().map_err(map_connect_error)?;

		// Materialize both origin sides (reusing the caller's if wired, else a
		// fresh one) so the session can publish/subscribe and the FFI can always
		// hand back a publisher/consumer.
		let publish = self
			.publish
			.as_ref()
			.map(|p| p.inner().clone())
			.unwrap_or_else(|| moq_net::Origin::random().produce());
		let subscribe = self
			.consume
			.as_ref()
			.map(|p| p.inner().clone())
			.unwrap_or_else(|| moq_net::Origin::random().produce());

		let session = client
			.with_publisher(&publish)
			.with_subscriber(subscribe.clone())
			.connect(url)
			.await
			.map_err(map_connect_error)?;

		Ok(Arc::new(MoqSession::new(session, publish, subscribe)))
	}
}

fn map_connect_error(err: moq_native::Error) -> MoqError {
	match err.connect_error() {
		Some(moq_native::ConnectError::Unauthorized) => MoqError::Unauthorized,
		Some(moq_native::ConnectError::Forbidden) => MoqError::Forbidden,
		_ => MoqError::Connect(format!("{err}")),
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
				config: moq_native::ClientConfig::default(),
				publish: None,
				consume: None,
			}),
		})
	}

	/// Disable TLS certificate verification (for development only).
	pub fn set_tls_disable_verify(&self, disable: bool) {
		if let Some(mut state) = self.task.lock() {
			state.config.tls.disable_verify = Some(disable);
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

	/// Set the local UDP socket bind address. Defaults to `[::]:0`.
	///
	/// Returns an error if the address cannot be parsed.
	pub fn set_bind(&self, addr: String) -> Result<(), MoqError> {
		let parsed: std::net::SocketAddr = addr
			.parse()
			.map_err(|err| MoqError::Bind(format!("invalid bind address: {err}")))?;
		if let Some(mut state) = self.task.lock() {
			state.config.bind = parsed;
		}
		Ok(())
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
	/// Any side not wired via [`set_publish`](Self::set_publish) /
	/// [`set_consume`](Self::set_consume) gets a fresh origin, so the producer
	/// and consumer sides are always accessible via [`MoqSession::publisher`]
	/// and [`MoqSession::consumer`] without the caller constructing a
	/// [`MoqOriginProducer`] themselves.
	///
	/// Can be cancelled by calling `cancel()`.
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

#[derive(uniffi::Object)]
pub struct MoqSession {
	inner: Option<moq_net::Session>,
	closed: Task<Session>,
	publisher: Arc<MoqOriginProducer>,
	consumer: Arc<MoqOriginConsumer>,
}

impl MoqSession {
	pub(crate) fn new(
		session: moq_net::Session,
		publish: moq_net::OriginProducer,
		subscribe: moq_net::OriginProducer,
	) -> Self {
		// Eagerly wrap the wired origin sides so each publisher()/consumer()
		// call hands back the same Arc. `publish` is published into; `subscribe`
		// is where the remote's broadcasts land (read via its consumer view).
		let publisher = Arc::new(MoqOriginProducer::from_inner(publish));
		let consumer = Arc::new(MoqOriginConsumer::from_inner(subscribe.consume()));
		Self {
			inner: Some(session.clone()),
			closed: Task::new(session),
			publisher,
			consumer,
		}
	}
}

impl Drop for MoqSession {
	fn drop(&mut self) {
		let _guard = crate::ffi::RUNTIME.enter();
		self.inner.take();
	}
}

#[uniffi::export]
impl MoqSession {
	/// Wait until the session is closed.
	pub async fn closed(&self) -> Result<(), MoqError> {
		// We have a task to run all of the closed calls juuuuust so they use the same tokio runtime.
		self.closed
			.run(|session| async move { session.closed().await.map_err(Into::into) })
			.await
	}

	/// Close the session with the given error code.
	pub fn cancel(&self, code: u32) {
		let _guard = crate::ffi::RUNTIME.enter();
		if let Some(inner) = &self.inner {
			inner.clone().close(moq_net::Error::Remote(code));
		}
		// NOTE: we don't abort the closed Task because it will be aborted via above ^
		// We'll get a slightly better error message instead of Cancelled.
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
	/// them; see [`MoqConnectionStats`].
	pub fn stats(&self) -> MoqConnectionStats {
		let _guard = crate::ffi::RUNTIME.enter();
		self.inner
			.as_ref()
			.map(moq_net::Session::stats)
			.unwrap_or_default()
			.into()
	}
}
