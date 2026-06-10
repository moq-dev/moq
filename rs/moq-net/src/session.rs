use std::{future::Future, pin::Pin, sync::Arc, task::Poll, time::Duration};

use kio::{Consumer, Producer};
use web_transport_trait::Stats;

use crate::{BandwidthConsumer, BandwidthProducer, Error, OriginConsumer, OriginProducer, Version};

/// A MoQ transport session, wrapping a WebTransport connection.
///
/// Created via:
/// - [`crate::Client::connect`] for clients.
/// - [`crate::Server::accept`] for servers.
///
/// Both [`publisher`](Self::publisher) and [`consumer`](Self::consumer)
/// are always populated: by whatever the caller wired via
/// [`Client::with_publisher`](crate::Client::with_publisher) /
/// [`Client::with_consumer`](crate::Client::with_consumer) /
/// [`Client::with_origin`](crate::Client::with_origin) (or the matching
/// methods on [`Server`](crate::Server)), or by an auto-created fresh
/// [`Origin`](crate::Origin) for any side the caller left unset. Use
/// `publisher()` to publish broadcasts and `consumer()` to read
/// announcements without ever having to construct an Origin yourself.
#[derive(Clone)]
pub struct Session {
	session: Arc<dyn SessionInner>,
	version: Version,
	send_bandwidth: Option<BandwidthConsumer>,
	recv_bandwidth: Option<BandwidthConsumer>,
	publisher: OriginProducer,
	consumer: OriginConsumer,
	goaway: GoawayTrigger,
	closed: bool,
}

impl Session {
	pub(super) fn new<S: web_transport_trait::Session>(
		session: S,
		version: Version,
		recv_bandwidth: Option<BandwidthConsumer>,
		publisher: OriginProducer,
		consumer: OriginConsumer,
		goaway: GoawayTrigger,
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
			publisher,
			consumer,
			goaway,
			closed: false,
		}
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

	/// The publish-side origin: where local broadcasts get advertised
	/// to the remote. Either the producer the caller passed via
	/// [`Client::with_publisher`](crate::Client::with_publisher) /
	/// [`Server::with_publisher`](crate::Server::with_publisher) /
	/// `with_origin`, or one auto-created at connect/accept time.
	pub fn publisher(&self) -> &OriginProducer {
		&self.publisher
	}

	/// The subscribe-side origin: a cheap read handle for receiving
	/// announcements pushed by the remote. Either derived from the
	/// producer the caller passed via
	/// [`Client::with_consumer`](crate::Client::with_consumer) /
	/// [`Server::with_consumer`](crate::Server::with_consumer) /
	/// `with_origin`, or auto-created at connect/accept time.
	pub fn consumer(&self) -> &OriginConsumer {
		&self.consumer
	}

	/// Begin a graceful drain of this session.
	///
	/// Returns a [`Drain`] handle: [`Drain::start`] sends a GOAWAY asking the peer to
	/// migrate away (without closing the session), and [`Drain::complete`] awaits its
	/// departure.
	pub fn drain(&self) -> Drain {
		Drain {
			goaway: self.goaway.clone(),
			session: self.session.clone(),
		}
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

/// A handle to gracefully drain a [`Session`], obtained via [`Session::drain`].
///
/// `start` asks the peer to migrate away (GOAWAY) without closing the session;
/// `complete` waits until it actually leaves. Cheaply clonable.
#[derive(Clone)]
pub struct Drain {
	goaway: GoawayTrigger,
	session: Arc<dyn SessionInner>,
}

impl Drain {
	/// Send a GOAWAY asking the peer to migrate away, optionally to `uri` (`None`
	/// just asks them to leave). The session stays open so in-flight groups can
	/// finish; call [`complete`](Self::complete) to await departure. Calling more
	/// than once, or on a protocol version that predates GOAWAY, is harmless.
	pub fn start<'a>(&self, uri: impl Into<Option<&'a str>>) {
		let uri: Option<&str> = uri.into();
		// A closed channel means the protocol task already exited (session gone),
		// so there's nothing left to GOAWAY.
		if let Ok(mut value) = self.goaway.write() {
			*value = Some(Arc::from(uri.unwrap_or("")));
		}
	}

	/// Wait until the session has fully closed: the peer left, or it was forced.
	pub async fn complete(&self) {
		self.session.closed().await;
	}
}

/// Trigger half of a session's GOAWAY signal, held by [`Session`] / [`Drain`].
/// `None` means "not yet requested"; `Some(uri)` carries the (possibly empty) URI.
pub(crate) type GoawayTrigger = Producer<Option<Arc<str>>>;

/// Signal half handed to the per-protocol session task spawned by `lite::start` /
/// `ietf::start`, which writes the actual GOAWAY frame when fired.
pub(crate) type GoawaySignal = Consumer<Option<Arc<str>>>;

/// Create a linked [`GoawayTrigger`] / [`GoawaySignal`] pair for one session.
pub(crate) fn goaway_channel() -> (GoawayTrigger, GoawaySignal) {
	let trigger = Producer::new(None);
	let signal = trigger.consume();
	(trigger, signal)
}

/// Resolve once a GOAWAY is requested, yielding the (possibly empty) redirect URI,
/// or `None` if the trigger was dropped without firing (the session is going away).
pub(crate) async fn goaway_triggered(signal: GoawaySignal) -> Option<Arc<str>> {
	// Map inside the closure so the `Ref` lock guard (not `Send`) never lands in the
	// returned future, keeping it spawnable.
	kio::wait(|waiter| {
		match signal.poll(waiter, |uri| match &**uri {
			Some(uri) => Poll::Ready(uri.clone()),
			None => Poll::Pending,
		}) {
			Poll::Ready(Ok(uri)) => Poll::Ready(Some(uri)),
			Poll::Ready(Err(_closed)) => Poll::Ready(None),
			Poll::Pending => Poll::Pending,
		}
	})
	.await
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
