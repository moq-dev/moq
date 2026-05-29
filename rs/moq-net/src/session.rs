use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use web_transport_trait::Stats;

use crate::{BandwidthConsumer, BandwidthProducer, Error, OriginConsumer, OriginProducer, Version};

/// A MoQ transport session, wrapping a WebTransport connection.
///
/// Created via:
/// - [`crate::Client::connect`] for clients.
/// - [`crate::Server::accept`] for servers.
///
/// If the caller didn't wire its own origin via
/// [`Client::with_publish`](crate::Client::with_publish) /
/// [`Client::with_consume`](crate::Client::with_consume) /
/// [`Client::with_origin`](crate::Client::with_origin) (or the matching
/// methods on [`Server`](crate::Server)), connect/accept auto-create a
/// fresh [`Origin`](crate::Origin) and surface the producer and consumer
/// sides as [`publisher`](Self::publisher) and [`consumer`](Self::consumer).
/// Callers that wired their own origin see both as `None` and continue
/// to drive things through what they already hold.
#[derive(Clone)]
pub struct Session {
	session: Arc<dyn SessionInner>,
	version: Version,
	send_bandwidth: Option<BandwidthConsumer>,
	recv_bandwidth: Option<BandwidthConsumer>,
	publisher: Option<OriginProducer>,
	consumer: Option<OriginConsumer>,
	closed: bool,
}

impl Session {
	pub(super) fn new<S: web_transport_trait::Session>(
		session: S,
		version: Version,
		recv_bandwidth: Option<BandwidthConsumer>,
	) -> Self {
		// Send bandwidth is version-agnostic: it depends on QUIC backend support.
		let send_bandwidth = if session.stats().estimated_send_rate().is_some() {
			let producer = BandwidthProducer::new();
			let consumer = producer.consume();

			let session = session.clone();
			web_async::spawn(async move {
				run_send_bandwidth(&session, producer).await;
			});

			Some(consumer)
		} else {
			None
		};

		Self {
			session: Arc::new(session),
			version,
			send_bandwidth,
			recv_bandwidth,
			publisher: None,
			consumer: None,
			closed: false,
		}
	}

	/// Attach the auto-created origin sides. Called by `Client::connect` /
	/// `Server::accept` on the no-config path so the caller can publish
	/// broadcasts and read announcements without constructing an Origin
	/// themselves.
	pub(crate) fn with_origins(mut self, publisher: Option<OriginProducer>, consumer: Option<OriginConsumer>) -> Self {
		self.publisher = publisher;
		self.consumer = consumer;
		self
	}

	/// Returns the negotiated protocol version.
	pub fn version(&self) -> Version {
		self.version
	}

	/// Returns a consumer for the estimated send bitrate (from the congestion controller).
	///
	/// Returns `None` if the QUIC backend doesn't support bandwidth estimation.
	pub fn send_bandwidth(&self) -> Option<BandwidthConsumer> {
		self.send_bandwidth.clone()
	}

	/// Returns a consumer for the estimated receive bitrate (from PROBE).
	///
	/// Returns `None` if the MoQ version doesn't support PROBE (requires moq-lite-03+).
	pub fn recv_bandwidth(&self) -> Option<BandwidthConsumer> {
		self.recv_bandwidth.clone()
	}

	/// The auto-created origin producer side. `Some` only when the caller
	/// didn't wire its own publish/consume origin before connect/accept.
	pub fn publisher(&self) -> Option<&OriginProducer> {
		self.publisher.as_ref()
	}

	/// The auto-created origin consumer side. `Some` only when the caller
	/// didn't wire its own publish/consume origin before connect/accept.
	pub fn consumer(&self) -> Option<&OriginConsumer> {
		self.consumer.as_ref()
	}

	/// Close the underlying transport session.
	pub fn close(&mut self, err: Error) {
		if self.closed {
			return;
		}
		self.closed = true;
		self.session.close(err.to_code(), err.to_string().as_ref());
	}

	/// Block until the transport session is closed.
	pub async fn closed(&self) -> Result<(), Error> {
		let err = self.session.closed().await;
		Err(Error::Transport(err))
	}
}

impl Drop for Session {
	fn drop(&mut self) {
		if !self.closed {
			self.session.close(Error::Cancel.to_code(), "dropped");
		}
	}
}

/// Polls the QUIC congestion controller for estimated send rate.
///
/// Exits as soon as the session closes so we don't pin the underlying connection
/// after the wrapping [`Session`] is dropped.
async fn run_send_bandwidth<S: web_transport_trait::Session>(session: &S, producer: BandwidthProducer) {
	tokio::select! {
		_ = session.closed() => {}
		_ = producer.closed() => {}
		_ = run_send_bandwidth_inner(session, &producer) => {}
	}
}

/// Toggles between waiting for a consumer and polling stats while one exists.
/// Returns when the producer channel errors (closed by the consumer side).
async fn run_send_bandwidth_inner<S: web_transport_trait::Session>(session: &S, producer: &BandwidthProducer) {
	const POLL_INTERVAL: Duration = Duration::from_millis(100);

	loop {
		if producer.used().await.is_err() {
			return;
		}

		let mut interval = tokio::time::interval(POLL_INTERVAL);
		loop {
			tokio::select! {
				biased;
				res = producer.unused() => {
					if res.is_err() {
						return;
					}
					// No more consumers, pause polling.
					break;
				}
				_ = interval.tick() => {
					let bitrate = session.stats().estimated_send_rate();
					if producer.set(bitrate).is_err() {
						return;
					}
				}
			}
		}
	}
}

// We use a wrapper type that is dyn-compatible to remove the generic bounds from Session.
trait SessionInner: Send + Sync {
	fn close(&self, code: u32, reason: &str);
	fn closed(&self) -> Pin<Box<dyn Future<Output = String> + Send + '_>>;
}

impl<S: web_transport_trait::Session> SessionInner for S {
	fn close(&self, code: u32, reason: &str) {
		S::close(self, code, reason);
	}

	fn closed(&self) -> Pin<Box<dyn Future<Output = String> + Send + '_>> {
		Box::pin(async move { S::closed(self).await.to_string() })
	}
}
