//! Live datagram tracks, independent of stream tracks with the same name.

use std::sync::Arc;
use std::task::{Poll, ready};

use crate::consumer::MoqBroadcastConsumer;
use crate::error::MoqError;
use crate::ffi::{Background, Polled};
use crate::media::{MoqDatagram, MoqTimestamp};
use crate::producer::{Child, Children, MoqBroadcastProducer, MoqBroadcastRequests};

/// Publisher properties of a datagram track, fixed at creation.
#[derive(Clone, Default, uniffi::Record)]
pub struct MoqDatagramInfo {
	/// Publisher priority, breaking ties between equal subscriber priorities.
	#[uniffi(default = 0)]
	pub priority: i8,
	/// Timestamp ticks per second, or null for an untimed track.
	#[uniffi(default = None)]
	pub timescale: Option<u64>,
}

impl TryFrom<MoqDatagramInfo> for moq_net::datagram::Info {
	type Error = MoqError;

	fn try_from(info: MoqDatagramInfo) -> Result<Self, Self::Error> {
		let timescale = info
			.timescale
			.map(moq_net::Timescale::new)
			.transpose()
			.map_err(|_| MoqError::Codec {
				reason: "invalid datagram timescale".into(),
			})?;
		Ok(Self::default().with_priority(info.priority).with_timescale(timescale))
	}
}

impl From<moq_net::datagram::Info> for MoqDatagramInfo {
	fn from(info: moq_net::datagram::Info) -> Self {
		Self {
			priority: info.priority,
			timescale: info.timescale.map(|scale| scale.as_u64()),
		}
	}
}

#[uniffi::export]
impl MoqBroadcastProducer {
	/// Create a live datagram track in its own namespace.
	pub fn create_datagrams(
		&self,
		name: String,
		info: Option<MoqDatagramInfo>,
	) -> Result<Arc<MoqDatagramProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		let info = info.unwrap_or_default().try_into()?;
		self.with_state(|state| {
			let producer = state.broadcast.create_datagrams(name, info)?;
			MoqDatagramProducer::child(producer, &state.children)
		})
	}
}

#[uniffi::export]
impl MoqBroadcastConsumer {
	/// Subscribe to a live datagram track, receiving only datagrams written from now on.
	pub fn datagrams(&self, name: String) -> Result<Arc<MoqDatagramConsumer>, MoqError> {
		let _guard = crate::ffi::enter();
		self.driver.check()?;
		Ok(Arc::new(MoqDatagramConsumer::new(
			self.inner.datagrams(&name)?,
			self.driver.clone(),
		)))
	}
}

/// Writes datagrams with a shared sequence counter and no groups or cache.
#[derive(uniffi::Object)]
pub struct MoqDatagramProducer {
	inner: Polled<moq_net::datagram::Producer>,
}

impl MoqDatagramProducer {
	fn child(producer: moq_net::datagram::Producer, children: &Children) -> Result<Arc<Self>, MoqError> {
		children.adopt(Arc::new(Self {
			inner: Polled::new(producer),
		}))
	}
}

impl Child for MoqDatagramProducer {
	fn abort(&self, err: moq_net::Error) {
		let _guard = crate::ffi::enter();
		if let Ok(producer) = self.inner.take(|_| Ok(())) {
			producer.abort(err);
		}
	}
}

impl Drop for MoqDatagramProducer {
	fn drop(&mut self) {
		Child::abort(self, moq_net::Error::Unroutable);
	}
}

#[uniffi::export]
impl MoqDatagramProducer {
	/// The datagram track's name.
	pub fn name(&self) -> Result<String, MoqError> {
		self.inner
			.peek(|producer| producer.name().to_owned())
			.ok_or(MoqError::Closed)
	}

	/// The publisher properties fixed when this track was created.
	pub fn info(&self) -> Result<MoqDatagramInfo, MoqError> {
		self.inner
			.peek(|producer| (*producer.info()).into())
			.ok_or(MoqError::Closed)
	}

	/// Append a datagram, returning its assigned sequence; its timestamp must match the track.
	pub fn append(&self, timestamp: Option<MoqTimestamp>, payload: Vec<u8>) -> Result<u64, MoqError> {
		let _guard = crate::ffi::enter();
		let timestamp = timestamp.map(TryInto::try_into).transpose()?;
		Ok(self
			.inner
			.peek_mut(|producer| producer.append(timestamp, payload))
			.ok_or(MoqError::Closed)??)
	}

	/// Insert a datagram at its explicit sequence; later appends continue past the highest one.
	pub fn insert(&self, datagram: MoqDatagram) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let datagram = moq_net::Datagram {
			sequence: datagram.sequence,
			timestamp: datagram.timestamp.map(TryInto::try_into).transpose()?,
			payload: datagram.payload.into(),
		};
		Ok(self
			.inner
			.peek_mut(|producer| producer.insert(datagram))
			.ok_or(MoqError::Closed)??)
	}

	/// Create a local consumer, starting with the next datagram written.
	pub fn consume(&self) -> Result<Arc<MoqDatagramConsumer>, MoqError> {
		let _guard = crate::ffi::enter();
		let consumer = self.inner.peek(|producer| producer.consume()).ok_or(MoqError::Closed)?;
		Ok(Arc::new(MoqDatagramConsumer::new(consumer, Background::default())))
	}

	/// The highest subscriber priority, or null while nobody consumes this track.
	pub fn priority(&self) -> Result<Option<i8>, MoqError> {
		self.inner.peek(|producer| producer.priority()).ok_or(MoqError::Closed)
	}

	/// Wait until the aggregate subscriber priority differs from `last`.
	pub async fn priority_changed(&self, last: Option<i8>) -> Result<Option<i8>, MoqError> {
		self.inner
			.wait(|producer, waiter| Poll::Ready(Ok(ready!(producer.poll_priority(waiter, last)))))
			.await
	}

	/// Finish the track; existing consumers drain their unread datagrams before its end.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		self.inner.take(|_| Ok(()))?.finish();
		Ok(())
	}

	/// Abort the track with an application error, discarding unread datagrams.
	pub fn abort(&self, error_code: u16) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		self.inner.take(|_| Ok(()))?.abort(moq_net::Error::App(error_code));
		Ok(())
	}
}

/// Reads a live datagram track from its own arrival cursor.
#[derive(uniffi::Object)]
pub struct MoqDatagramConsumer {
	inner: Polled<moq_net::datagram::Consumer>,
}

impl MoqDatagramConsumer {
	fn new(consumer: moq_net::datagram::Consumer, driver: Background) -> Self {
		Self {
			inner: Polled::new(consumer).supervised(driver),
		}
	}
}

#[uniffi::export]
impl MoqDatagramConsumer {
	/// The datagram track's name.
	pub fn name(&self) -> Result<String, MoqError> {
		self.inner
			.peek(|consumer| consumer.name().to_owned())
			.ok_or(MoqError::Closed)
	}

	/// Wait for the publisher to accept this track and return its properties.
	pub async fn info(&self) -> Result<MoqDatagramInfo, MoqError> {
		self.inner
			.wait(|consumer, waiter| Poll::Ready(Ok(ready!(consumer.poll_info(waiter))?.into())))
			.await
	}

	/// Receive the next datagram in arrival order, or null after the track finishes.
	pub async fn recv(&self) -> Result<Option<MoqDatagram>, MoqError> {
		self.inner
			.wait(|consumer, waiter| {
				Poll::Ready(Ok(ready!(consumer.poll_recv(waiter))?.map(|datagram| MoqDatagram {
					sequence: datagram.sequence,
					timestamp: datagram.timestamp.map(Into::into),
					payload: datagram.payload.to_vec(),
				})))
			})
			.await
	}

	/// This consumer's subscriber priority.
	pub fn priority(&self) -> Result<i8, MoqError> {
		self.inner.peek(|consumer| consumer.priority()).ok_or(MoqError::Closed)
	}

	/// Change this consumer's subscriber priority, higher first.
	pub fn set_priority(&self, priority: i8) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		self.inner
			.peek_mut(|consumer| consumer.set_priority(priority))
			.ok_or(MoqError::Closed)
	}

	/// Cancel current and future reads and release this consumer's demand.
	pub fn cancel(&self) {
		self.inner.cancel();
	}
}

/// Handles requests for datagram tracks separately from stream tracks.
#[derive(uniffi::Object)]
pub struct MoqDatagramRequests {
	inner: Polled<moq_net::broadcast::DatagramRequests>,
	children: Children,
}

#[uniffi::export]
impl MoqBroadcastRequests {
	/// Create an on-demand handler for datagram tracks.
	pub fn datagrams(&self) -> Result<Arc<MoqDatagramRequests>, MoqError> {
		let _guard = crate::ffi::enter();
		let requests = self
			.reads
			.peek(|inner| inner.requests.datagrams())
			.ok_or(MoqError::Closed)?;
		Ok(Arc::new(MoqDatagramRequests {
			inner: Polled::new(requests).supervised(self.driver.clone()),
			children: self.children.clone(),
		}))
	}
}

#[uniffi::export]
impl MoqDatagramRequests {
	/// Wait for the next datagram track requested by a consumer.
	pub async fn requested(&self) -> Result<Arc<MoqDatagramRequest>, MoqError> {
		let request = self
			.inner
			.wait(|requests, waiter| Poll::Ready(Ok(ready!(requests.poll_requested(waiter))?)))
			.await?;
		Ok(Arc::new(MoqDatagramRequest {
			inner: std::sync::Mutex::new(Some(request)),
			children: self.children.clone(),
		}))
	}

	/// Cancel the handler and reject requests it has not handed out.
	pub fn cancel(&self) {
		self.inner.cancel();
	}
}

/// A requested datagram track awaiting acceptance or rejection.
#[derive(uniffi::Object)]
pub struct MoqDatagramRequest {
	inner: std::sync::Mutex<Option<moq_net::datagram::Request>>,
	children: Children,
}

#[uniffi::export]
impl MoqDatagramRequest {
	/// The requested datagram track's name.
	pub fn name(&self) -> Result<String, MoqError> {
		Ok(self
			.inner
			.lock()
			.unwrap()
			.as_ref()
			.ok_or(MoqError::Closed)?
			.name()
			.to_owned())
	}

	/// The highest priority among the consumers waiting for this track.
	pub fn priority(&self) -> Result<Option<i8>, MoqError> {
		Ok(self.inner.lock().unwrap().as_ref().ok_or(MoqError::Closed)?.priority())
	}

	/// Accept the request with its fixed publisher properties.
	pub fn accept(&self, info: Option<MoqDatagramInfo>) -> Result<Arc<MoqDatagramProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		let info = info.unwrap_or_default().try_into()?;
		let request = self.inner.lock().unwrap().take().ok_or(MoqError::Closed)?;
		MoqDatagramProducer::child(request.accept(info), &self.children)
	}

	/// Reject this request with an application error code.
	pub fn abort(&self, error_code: u16) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let request = self.inner.lock().unwrap().take().ok_or(MoqError::Closed)?;
		request.reject(moq_net::Error::App(error_code));
		Ok(())
	}
}
