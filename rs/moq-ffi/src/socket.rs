//! Point-to-point socket endpoints and their track sessions.

use std::sync::Arc;
use std::task::{Poll, ready};

use crate::consumer::MoqTrackConsumer;
use crate::error::MoqError;
use crate::ffi::{Background, OneShot, Polled};
use crate::origin::MoqOriginProducer;
use crate::producer::{MoqTrackInfo, MoqTrackProducer};

/// A socket endpoint: its first dial announces it as a client, and its first accept as a
/// server.
#[derive(uniffi::Object)]
pub struct MoqSocketEndpoint {
	inner: moq_net::socket::Endpoint,
	driver: Background,
}

#[uniffi::export]
impl MoqSocketEndpoint {
	/// The endpoint `id` beneath `prefix`, using `origin` for publishing and finding peers.
	/// Nothing is announced until its first dial or accept.
	#[uniffi::constructor]
	pub fn new(origin: &MoqOriginProducer, prefix: String, id: String) -> Result<Arc<Self>, MoqError> {
		let _guard = crate::ffi::enter();
		let origin = origin.inner();
		origin.driver.check()?;
		let inner = moq_net::socket::Endpoint::new(origin, prefix, &id)?;
		origin.driver.check()?;
		Ok(Arc::new(Self {
			inner,
			driver: origin.driver.clone(),
		}))
	}

	/// This endpoint's id.
	pub fn id(&self) -> String {
		self.inner.id().into()
	}

	/// The prefix beneath which this endpoint finds peers.
	pub fn prefix(&self) -> String {
		self.inner.prefix().to_string()
	}

	/// Retract the endpoint's broadcasts and reset its pending dials and sessions.
	pub fn close(&self) {
		let _guard = crate::ffi::enter();
		self.inner.close();
	}

	/// Dial the server `peer` in a new `track` session; cancelling this call resets only this dial.
	pub async fn dial_track(
		&self,
		peer: String,
		info: Option<MoqTrackInfo>,
	) -> Result<Arc<MoqSocketTrackStream>, MoqError> {
		self.driver
			.watch(async {
				let stream = self
					.inner
					.dial_track(&peer, info.map(TryInto::try_into).transpose()?)?
					.await?;
				Ok(Arc::new(MoqSocketTrackStream::new(stream, self.driver.clone())))
			})
			.await
	}

	/// Start accepting `track` dials and await the next one.
	pub async fn accept_track(&self) -> Result<Arc<MoqSocketIncomingTrack>, MoqError> {
		self.driver
			.watch(async {
				let inner = self.inner.accept_track().await?;
				Ok(Arc::new(MoqSocketIncomingTrack {
					peer: inner.peer().into(),
					session: inner.session().into(),
					inner: OneShot::new(inner),
					driver: self.driver.clone(),
				}))
			})
			.await
	}
}

impl Drop for MoqSocketEndpoint {
	fn drop(&mut self) {
		self.close();
	}
}

/// An incoming `track` dial, consumed by accepting or rejecting it.
#[derive(uniffi::Object)]
pub struct MoqSocketIncomingTrack {
	inner: OneShot<moq_net::socket::Incoming>,
	peer: String,
	session: String,
	driver: Background,
}

#[uniffi::export]
impl MoqSocketIncomingTrack {
	/// The endpoint claiming this dial; accepting verifies it really dialed.
	pub fn peer(&self) -> String {
		self.peer.clone()
	}
	/// The random id of this session.
	pub fn session(&self) -> String {
		self.session.clone()
	}
	/// Verify return routability and accept; cancelling resets the dial.
	pub async fn accept(&self, info: Option<MoqTrackInfo>) -> Result<Arc<MoqSocketTrackStream>, MoqError> {
		self.driver.check()?;
		let info = info.map(TryInto::try_into).transpose()?;
		let (incoming, answering) = self.inner.take()?;
		let accepting = Polled::new(incoming.accept(info)).supervised(self.driver.clone());
		let stream = accepting
			.wait(|inner, waiter| Poll::Ready(Ok(ready!(inner.poll_ok(waiter))?)))
			.await;
		answering.settle(stream.map(|stream| Arc::new(MoqSocketTrackStream::new(stream, self.driver.clone()))))
	}
	/// Refuse the dial as not found.
	pub fn reject(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		self.driver.check()?;
		self.inner.answer(|incoming| {
			incoming.reject();
			Ok(())
		})
	}
}

/// A `track` session exposing its existing producer and subscribed consumer handles.
#[derive(uniffi::Object)]
pub struct MoqSocketTrackStream {
	producer: Arc<MoqTrackProducer>,
	consumer: Arc<MoqTrackConsumer>,
	peer: String,
	session: String,
}

impl MoqSocketTrackStream {
	fn new(inner: moq_net::socket::TrackStream, driver: Background) -> Self {
		let peer = inner.peer().into();
		let session = inner.session().into();
		Self {
			producer: Arc::new(MoqTrackProducer::from_socket(inner.producer)),
			consumer: Arc::new(MoqTrackConsumer::new(inner.consumer, driver)),
			peer,
			session,
		}
	}
}

#[uniffi::export]
impl MoqSocketTrackStream {
	/// This side's write half.
	pub fn producer(&self) -> Arc<MoqTrackProducer> {
		self.producer.clone()
	}
	/// The peer's read half, retaining its reliable ascending subscription.
	pub fn consumer(&self) -> Arc<MoqTrackConsumer> {
		self.consumer.clone()
	}
	/// The peer endpoint's id.
	pub fn peer(&self) -> String {
		self.peer.clone()
	}
	/// The random id of this session.
	pub fn session(&self) -> String {
		self.session.clone()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::error::MoqProtocolKind;
	use crate::media::MoqFrame;
	use std::future::Future;
	use std::pin::Pin;
	use std::task::{Context, Waker};

	fn pending(future: Pin<&mut impl Future>) {
		assert!(future.poll(&mut Context::from_waker(Waker::noop())).is_pending());
	}

	fn endpoints() -> (Arc<MoqSocketEndpoint>, Arc<MoqSocketEndpoint>) {
		let origin = MoqOriginProducer::new(Default::default());
		let a = MoqSocketEndpoint::new(&origin, "room".into(), "a".into()).unwrap();
		let b = MoqSocketEndpoint::new(&origin, "room".into(), "b".into()).unwrap();
		(a, b)
	}

	fn frame(payload: &[u8]) -> MoqFrame {
		MoqFrame {
			payload: payload.into(),
			timestamp: None,
		}
	}

	async fn connect(
		a: &MoqSocketEndpoint,
		b: &MoqSocketEndpoint,
	) -> (Arc<MoqSocketTrackStream>, Arc<MoqSocketTrackStream>) {
		let mut incoming = Box::pin(b.accept_track());
		pending(incoming.as_mut());
		let (dialed, accepted) = tokio::join!(a.dial_track(b.id(), None), async {
			incoming.await.unwrap().accept(None).await
		});
		(dialed.unwrap(), accepted.unwrap())
	}

	#[tokio::test]
	async fn track_subscription_survives_stream_release_and_has_no_default_age_limit() {
		let (a, b) = endpoints();
		let (client, server) = connect(&a, &b).await;
		assert_eq!((client.peer(), server.peer()), ("b".into(), "a".into()));
		assert_eq!(client.session(), server.session());
		let producer = client.producer();
		let reverse = server.producer();
		let consumer = server.consumer();
		drop(server);
		assert!(consumer.info().max_age_unlimited);
		// Create the later group first: the reliable ascending reader must wait for 0.
		let later = producer.create_group(1).unwrap();
		later.write_frame(frame(b"later")).unwrap();
		later.finish().unwrap();
		let first = producer.create_group(0).unwrap();
		first.write_frame(frame(b"first")).unwrap();
		first.finish().unwrap();
		producer.finish().unwrap();
		assert_eq!(consumer.read_frame().await.unwrap().unwrap().payload, b"first");
		assert_eq!(consumer.read_frame().await.unwrap().unwrap().payload, b"later");
		assert!(consumer.read_frame().await.unwrap().is_none());
		reverse.finish().unwrap();
		assert!(client.consumer().read_frame().await.unwrap().is_none());
	}

	#[tokio::test]
	async fn an_abort_is_not_a_finish_and_does_not_close_another_session() {
		let (a, b) = endpoints();
		let (client, server) = connect(&a, &b).await;
		let (other, peer) = connect(&a, &b).await;
		assert_ne!(client.session(), other.session());
		client.producer().abort(42).unwrap();
		let error = server.consumer().read_frame().await.err().unwrap();
		assert!(matches!(error, MoqError::Protocol { details } if details.kind == MoqProtocolKind::App));
		other.producer().write_frame(frame(b"independent")).unwrap();
		other.producer().finish().unwrap();
		assert_eq!(
			peer.consumer().read_frame().await.unwrap().unwrap().payload,
			b"independent"
		);
		assert!(peer.consumer().read_frame().await.unwrap().is_none());
	}

	#[tokio::test]
	async fn cancelled_accept_wait_leaves_the_queue_and_rejection_is_one_shot() {
		let (a, b) = endpoints();
		let mut abandoned = Box::pin(b.accept_track());
		pending(abandoned.as_mut());
		drop(abandoned);
		let (dialed, ()) = tokio::join!(a.dial_track(b.id(), None), async {
			let incoming = b.accept_track().await.unwrap();
			assert_eq!(incoming.peer(), "a");
			incoming.reject().unwrap();
			assert!(matches!(incoming.reject(), Err(MoqError::AlreadyResponded)));
		});
		assert!(matches!(dialed, Err(MoqError::NotFound)));
		let _ = connect(&a, &b).await;
	}

	#[tokio::test]
	async fn cancelling_a_dial_resets_its_incoming_but_the_endpoint_can_dial_again() {
		let (a, b) = endpoints();
		let mut incoming = Box::pin(b.accept_track());
		pending(incoming.as_mut());
		let mut dial = Box::pin(a.dial_track(b.id(), None));
		pending(dial.as_mut());
		let incoming = tokio::select! {
			incoming = incoming => incoming.unwrap(),
			_ = &mut dial => panic!("dial resolved without acceptance"),
		};
		drop(dial);
		assert!(incoming.accept(None).await.is_err());
		let _ = connect(&a, &b).await;
	}

	#[tokio::test]
	async fn endpoint_close_ends_its_sessions_and_wakes_a_pending_accept() {
		let (a, b) = endpoints();
		let (_client, server) = connect(&a, &b).await;
		let mut incoming = Box::pin(b.accept_track());
		pending(incoming.as_mut());
		a.close();
		assert!(server.consumer().read_frame().await.is_err());
		b.close();
		assert!(matches!(incoming.await, Err(MoqError::Closed)));
		assert!(matches!(b.dial_track(a.id(), None).await, Err(MoqError::Closed)));
	}
}
