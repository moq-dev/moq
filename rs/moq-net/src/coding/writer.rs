use std::{
	fmt::Debug,
	task::{Poll, ready},
};

use crate::{Error, StreamError, coding::*, ietf};

/// A wrapper around a [crate::transport::poll::SendStream] that will reset on Drop.
///
/// The `poll_*` methods are the implementation; the `async` methods are thin
/// wrappers, so the writer can be driven from another poll function without
/// pinning a future. Messages are encoded into an internal buffer with
/// [`Self::buffer`] and drained by [`Self::poll_flush`], so a `Pending` (or a
/// cancelled wrapper) resumes mid-message instead of desynchronizing the stream.
pub struct Writer<S: crate::transport::poll::SendStream, V> {
	stream: Option<S>,
	buffer: Vec<u8>,
	/// How much of `buffer` has already hit the stream.
	flushed: usize,
	version: V,
	stats: crate::stats::Session,
	/// Bytes the stream took through this writer.
	written: u64,
	/// The payload this stream carries, billed as the transport delivers it. See [`Self::bill`].
	delivery: Option<Box<crate::stats::Delivery>>,
}

impl<S: crate::transport::poll::SendStream, V: StreamCodes> Writer<S, V> {
	/// Create a new writer for the given stream and version.
	pub fn new(stream: S, version: V) -> Self {
		Self {
			stream: Some(stream),
			buffer: Default::default(),
			flushed: 0,
			version,
			stats: Default::default(),
			written: 0,
			delivery: None,
		}
	}

	/// Bill the frames announced through [`Self::frame`] to `meter`, each byte once the
	/// transport delivered it.
	/// A stream carrying several groups bills all of them to the first meter: a stream serves one
	/// track, so they count alike.
	pub(crate) fn bill(&mut self, meter: crate::stats::Meter) {
		if self.delivery.is_none() && meter.is_tracked() {
			self.delivery = Some(Box::new(crate::stats::Delivery::new(meter)));
		}
	}

	/// The next `size` bytes written after what is buffered are one frame's payload.
	pub(crate) fn frame(&mut self, size: u64) {
		self.settle();
		let start = self.written + (self.buffer.len() - self.flushed) as u64;
		if let Some(delivery) = &mut self.delivery {
			delivery.frame(start, size);
		}
	}

	/// Bill the payload the transport has delivered so far.
	fn settle(&mut self) {
		if let (Some(delivery), Some(stream)) = (&mut self.delivery, &self.stream) {
			delivery.deliver(delivered(stream, self.written));
		}
	}

	/// [`Self::settle`], then wake `waiter` once more of what the stream took is delivered, so
	/// a stream idle after its last frame still bills that frame as it lands. A serve loop
	/// calls this whenever it parks, since a write it just made may be waiting on delivery.
	pub(crate) fn poll_settle(&mut self, waiter: &kio::Waiter) {
		if let (Some(delivery), Some(stream)) = (&mut self.delivery, &self.stream) {
			poll_settle(delivery, stream, self.written, waiter);
		}
	}

	pub(crate) fn with_stats(mut self, stats: &crate::stats::Session) -> Self {
		self.stats = stats.clone();
		self
	}

	/// Encode the given message into the write buffer, to be sent by
	/// [`Self::poll_flush`]. An encode error leaves the buffer untouched.
	pub fn buffer<T: Encode<V> + Debug>(&mut self, msg: &T) -> Result<(), Error>
	where
		V: Into<Form> + Copy,
	{
		let version = self.version;
		self.buffer_with(|w| msg.encode(w, version))
	}

	/// Encode a varint into the write buffer, to be sent by [`Self::poll_flush`].
	pub fn buffer_varint(&mut self, value: u64) -> Result<(), Error>
	where
		V: Into<Form> + Copy,
	{
		self.buffer_with(|w| w.varint(value))
	}

	/// Run `encode` into the write buffer, dropping its partial bytes if it fails.
	fn buffer_with(&mut self, encode: impl FnOnce(&mut Encoder<'_>) -> Result<(), EncodeError>) -> Result<(), Error>
	where
		V: Into<Form> + Copy,
	{
		let start = self.buffer.len();
		let mut w = Encoder::new(&mut self.buffer, self.version.into());
		if let Err(err) = encode(&mut w) {
			// Drop the partial encode: flushing it would corrupt the stream.
			self.buffer.truncate(start);
			return Err(err.into());
		}
		if let Some(class) = w.control {
			self.stats.control(class, self.buffer.len() - start, false);
		}
		Ok(())
	}

	/// Append raw pre-encoded bytes to the write buffer, to be sent by
	/// [`Self::poll_flush`].
	pub fn buffer_raw(&mut self, bytes: &[u8]) {
		self.buffer.extend_from_slice(bytes);
	}

	/// Poll until the write buffer has fully hit the stream.
	///
	/// A failed stream never takes another byte, so a write error drops the buffer too.
	pub fn poll_flush(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		while self.flushed < self.buffer.len() {
			let stream = self.stream.as_mut().unwrap();
			match ready!(stream.poll_write(waiter, &self.buffer[self.flushed..])) {
				Ok(n) => {
					self.flushed += n;
					self.written += n as u64;
				}
				Err(err) => {
					self.buffer.clear();
					self.flushed = 0;
					return Poll::Ready(Err(self.version.transport_error(err)));
				}
			}
		}
		self.buffer.clear();
		self.flushed = 0;
		Poll::Ready(Ok(()))
	}

	/// Encode the given message to the stream.
	///
	/// Cancelling this future never desynchronizes the stream: the message is
	/// already buffered and a later flush completes it. That also means a
	/// cancelled `encode` must not be retried with the same message, or two
	/// copies go on the wire; resume with [`Self::poll_flush`] (or any later
	/// write) instead.
	#[cfg(test)]
	pub async fn encode<T: Encode<V> + Debug>(&mut self, msg: &T) -> Result<(), Error>
	where
		V: Into<Form> + Copy,
	{
		self.buffer(msg)?;
		kio::wait(|waiter| self.poll_flush(waiter)).await
	}

	/// Encode a varint to the stream, with the same cancellation rules as [`Self::encode`].
	#[cfg(test)]
	pub async fn varint(&mut self, value: u64) -> Result<(), Error>
	where
		V: Into<Form> + Copy,
	{
		self.buffer_varint(value)?;
		kio::wait(|waiter| self.poll_flush(waiter)).await
	}

	/// Poll a write of `buf`, flushing any buffered message bytes first so the
	/// stream never reorders around the buffer.
	pub fn poll_write<Buf: bytes::Buf>(&mut self, waiter: &kio::Waiter, buf: &mut Buf) -> Poll<Result<usize, Error>> {
		ready!(self.poll_flush(waiter))?;
		let written = ready!(self.stream.as_mut().unwrap().poll_write_buf(waiter, buf))
			.map_err(|err| self.version.transport_error(err))?;
		self.written += written as u64;
		Poll::Ready(Ok(written))
	}

	/// Write the entire `Buf` to the stream.
	///
	/// NOTE: This can avoid performing a copy when using `Bytes`.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_write_all<Buf: bytes::Buf>(&mut self, waiter: &kio::Waiter, buf: &mut Buf) -> Poll<Result<(), Error>> {
		while buf.has_remaining() {
			ready!(self.poll_write(waiter, buf))?;
		}
		Poll::Ready(Ok(()))
	}

	/// Whether every buffered byte has hit the stream, so a FIN would not cut a message short.
	pub fn is_flushed(&self) -> bool {
		self.buffer.is_empty()
	}

	/// Mark the stream as finished.
	///
	/// Only valid once the buffer has flushed; the callers that finish
	/// immediately after an `encode` are safe because `encode` flushes fully.
	pub fn finish(&mut self) -> Result<(), Error> {
		debug_assert!(self.buffer.is_empty(), "finish with unflushed bytes");
		self.stream
			.as_mut()
			.unwrap()
			.finish()
			.map_err(|err| self.version.transport_error(err))
	}

	/// Abort the stream with the given error.
	///
	/// Consumes the writer: a later write won't compile, and the [`Drop`] fallback can't
	/// reset a second time and overwrite the reason with a plain [`Error::Cancel`].
	pub fn abort(mut self, err: &Error) {
		self.settle();
		if let Some(mut stream) = self.stream.take() {
			// The code comes from the negotiated protocol's registry: the same number means
			// different things on the two wires, so the version is what picks the table.
			stream.reset(self.version.encode_stream_code(&StreamError::from(err)));
		}
	}

	/// Reset the stream with `code`, for a condition no [`StreamError`] names, which the
	/// caller has already looked up in the negotiated protocol's registry.
	pub(crate) fn reset(mut self, code: u32) {
		self.settle();
		if let Some(mut stream) = self.stream.take() {
			stream.reset(code);
		}
	}

	/// Finish the stream and wait for the peer to acknowledge everything written.
	///
	/// [`Self::finish`] alone is not enough to deliver a final message. A stream that has sent
	/// its FIN is still retransmitting unacknowledged data, and a RESET_STREAM from that state
	/// discards it, so the [`Drop`] fallback below can throw away bytes the peer never read.
	/// Consuming the writer is what removes that fallback, and waiting for the acknowledgement
	/// is what makes the bytes safe.
	///
	/// A message still partly buffered resets the stream with [`Error::Cancel`] instead: a FIN
	/// would hand the peer a truncated message, and flushing the rest could wait forever on
	/// credit the peer withholds.
	#[cfg(test)]
	pub async fn close(self) -> Result<(), Error>
	where
		V: Clone,
	{
		let mut closing = self.closing()?;
		kio::wait(|waiter| closing.poll(waiter)).await
	}

	/// Finish the stream, returning the wait for the peer's acknowledgement: [`Self::close`]
	/// for a caller that polls.
	pub fn closing(mut self) -> Result<Closing<S, V>, Error>
	where
		V: Clone,
	{
		if !self.is_flushed() {
			self.abort(&Error::Cancel);
			return Err(Error::Cancel);
		}
		self.settle();
		let version = self.version.clone();
		let mut stream = self.stream.take();
		if let Some(stream) = &mut stream {
			stream.finish().map_err(|err| version.transport_error(err))?;
		}
		Ok(Closing {
			stream,
			version,
			written: self.written,
			delivery: self.delivery.take(),
		})
	}

	/// Resolves once the stream is closed, or the [Self::finish] is acknowledged by the peer.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_closed(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		self.poll_settle(waiter);
		let closed = ready!(self.stream.as_mut().unwrap().poll_closed(waiter));
		self.settle();
		Poll::Ready(closed.map_err(|err| self.version.transport_error(err)))
	}

	/// Poll-friendly [`Self::close`] for a finished writer: once the peer acknowledges
	/// (or the stream dies), the stream is released so the [`Drop`] fallback cannot
	/// reset an acknowledged stream with a spurious Cancel.
	pub fn poll_close(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let res = ready!(self.poll_closed(waiter));
		self.stream = None;
		Poll::Ready(res)
	}

	/// The stream, until the writer finishes, aborts, or closes it.
	pub(crate) fn stream(&self) -> Option<&S> {
		self.stream.as_ref()
	}

	/// Set the priority within the stream's send group, or session-wide when ungrouped.
	///
	/// Streams with HIGHER values are transmitted first. This is the transport trait's
	/// convention (matching W3C `sendOrder` and quinn's scheduler) and the model's
	/// [`Subscription::priority`](crate::track::Subscription), where higher values preempt
	/// lower ones. The lite priority queue's rank is the opposite (0 = most urgent); rank
	/// holders convert via `PriorityHandle::send_order`.
	pub fn set_priority(&mut self, priority: i64) {
		self.stream.as_mut().unwrap().set_priority(priority);
	}

	/// Cast the writer to a different version, used during version negotiation.
	pub fn with_version<O>(mut self, version: O) -> Writer<S, O> {
		Writer {
			// We need to use an Option so Drop doesn't reset the stream.
			stream: self.stream.take(),
			buffer: std::mem::take(&mut self.buffer),
			flushed: self.flushed,
			version,
			stats: self.stats.clone(),
			written: self.written,
			delivery: self.delivery.take(),
		}
	}
}

/// How much of the `written` bytes the transport delivered: what it reports, or what it took
/// when it cannot tell.
fn delivered<S: crate::transport::poll::SendStream>(stream: &S, written: u64) -> u64 {
	stream.delivered().unwrap_or(written).min(written)
}

/// Bill what `stream` delivered of its `written` bytes, then register `waiter` for more while
/// a frame the stream took waits on delivery. A wake that brings nothing stops it, so a stream
/// that will deliver no more parks instead of spinning.
fn poll_settle<S: crate::transport::poll::SendStream>(
	delivery: &mut crate::stats::Delivery,
	stream: &S,
	written: u64,
	waiter: &kio::Waiter,
) {
	// Nothing waits, so nothing to ask the transport, which may lock to answer.
	if !delivery.waiting(written) {
		return;
	}
	let mut through = delivered(stream, written);
	delivery.deliver(through);
	while delivery.waiting(written) && stream.poll_delivered(waiter).is_ready() {
		let now = delivered(stream, written);
		if now <= through {
			return;
		}
		through = now;
		delivery.deliver(through);
	}
}

/// A [`Writer`] whose stream has sent its FIN, waiting for the peer to acknowledge it.
///
/// Dropping it never resets the stream, so what was written still arrives.
pub struct Closing<S: crate::transport::poll::SendStream, V> {
	stream: Option<S>,
	version: V,
	written: u64,
	delivery: Option<Box<crate::stats::Delivery>>,
}

impl<S: crate::transport::poll::SendStream, V: StreamCodes> Closing<S, V> {
	/// Resolves once the peer acknowledges everything written, or the stream dies.
	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let Some(stream) = &self.stream else {
			return Poll::Ready(Ok(()));
		};
		if let Some(delivery) = &mut self.delivery {
			poll_settle(delivery, stream, self.written, waiter);
		}
		let closed = ready!(stream.poll_closed(waiter));
		self.settle();
		Poll::Ready(closed.map_err(|err| self.version.transport_error(err)))
	}
}

impl<S: crate::transport::poll::SendStream, V> Closing<S, V> {
	fn settle(&mut self) {
		if let (Some(delivery), Some(stream)) = (&mut self.delivery, &self.stream) {
			delivery.deliver(delivered(stream, self.written));
		}
	}
}

impl<S: crate::transport::poll::SendStream, V> Drop for Closing<S, V> {
	fn drop(&mut self) {
		self.settle();
	}
}

impl<S: crate::transport::poll::SendStream> Writer<S, ietf::Version> {
	/// Encode `msg` into the write buffer as `[id][size][body]`, to be sent by
	/// [`Self::poll_flush`]. The type and body go in together or not at all, so a failed encode
	/// never leaves a bare type for the next write to complete. `id` is explicit for the drafts
	/// that send one body under several types.
	pub fn buffer_message<T: Encode<ietf::Version>>(&mut self, id: u64, msg: &T) -> Result<(), Error> {
		let start = self.buffer.len();
		self.buffer_varint(id)?;
		// The body is buffered on its own so control stats count it without its type selector.
		let version = self.version;
		if let Err(err) = self.buffer_with(|w| msg.encode(w, version)) {
			self.buffer.truncate(start);
			return Err(err);
		}
		Ok(())
	}

	/// Encode an IETF `Message` to the stream as `[type_id][size][body]`, with the same
	/// all-or-nothing buffering as [`Self::buffer_message`] and cancellation rules as [`Self::encode`].
	#[cfg(test)]
	pub async fn encode_message<'m, T: ietf::Message<'m>>(&mut self, msg: &T) -> Result<(), Error> {
		self.buffer_message(T::ID, msg)?;
		kio::wait(|waiter| self.poll_flush(waiter)).await
	}
}

impl<S: crate::transport::poll::SendStream, V> Drop for Writer<S, V> {
	fn drop(&mut self) {
		if let (Some(delivery), Some(stream)) = (&mut self.delivery, &self.stream) {
			delivery.deliver(delivered(stream, self.written));
		}
		if let Some(mut stream) = self.stream.take() {
			// Unlike the Quinn default, we abort the stream on drop.
			//
			// A `Drop` impl cannot add the bound that would reach the version's registry, and
			// it does not need one: CANCELLED is 0x1 in moq-lite and in every moq-transport
			// draft we negotiate, which `both_registries_agree_about_a_cancellation` pins.
			stream.reset(StreamError::Cancel.to_code());
		}
	}
}
#[cfg(test)]
mod tests {
	use super::*;
	use crate::lite::test_transport::{Log, SinkSend};

	#[derive(Debug, Clone, thiserror::Error)]
	#[error("stream stopped with {0}")]
	struct Stopped(u32);

	impl crate::transport::Error for Stopped {
		fn session_error(&self) -> Option<(u32, String)> {
			None
		}

		fn stream_error(&self) -> Option<u32> {
			Some(self.0)
		}
	}

	impl crate::transport::poll::SendStream for Stopped {
		type Error = Self;
		type Group = crate::transport::poll::Ungrouped;

		fn poll_write(&mut self, _: &kio::Waiter, _: &[u8]) -> Poll<Result<usize, Self::Error>> {
			Poll::Ready(Err(self.clone()))
		}

		fn set_priority(&mut self, _: i64) {}

		fn set_group(&mut self, group: &Self::Group) {
			match *group {}
		}

		fn finish(&mut self) -> Result<(), Self::Error> {
			Err(self.clone())
		}

		fn reset(&mut self, _: u32) {}

		fn poll_closed(&self, _: &kio::Waiter) -> Poll<Result<(), Self::Error>> {
			Poll::Ready(Err(self.clone()))
		}
	}

	#[kio::test]
	async fn raw_payload_stop_uses_the_negotiated_registry() {
		for (version, expected) in [
			(
				crate::Version::Ietf(crate::ietf::Version::Draft22),
				Error::Stream(crate::StreamError::GoingAway),
			),
			(
				crate::Version::Lite(crate::lite::Version::Lite05),
				Error::Stream(crate::StreamError::GoingAway),
			),
		] {
			let mut writer = Writer::new(Stopped(0x4), version);
			let err = writer.write_all(&mut b"payload".as_slice()).await.unwrap_err();
			assert!(
				matches!(
					(&err, &expected),
					(
						Error::Stream(crate::StreamError::Unknown(4)),
						Error::Stream(crate::StreamError::Unknown(4))
					) | (
						Error::Stream(crate::StreamError::GoingAway),
						Error::Stream(crate::StreamError::GoingAway)
					)
				),
				"{version} decoded the STOP_SENDING with the wrong registry: {err:?}"
			);
		}
	}

	#[test]
	fn set_priority_forwards_send_order() {
		let log = Log::default();
		let mut writer = Writer::new(SinkSend::new(log.clone()), crate::lite::Version::Lite05);

		for send_order in 0u8..=255 {
			writer.set_priority(send_order.into());
		}

		assert_eq!(log.priorities(), (0i64..=255).collect::<Vec<_>>());
	}

	/// A message the stream has not taken whole cannot be finished, so closing resets rather
	/// than hand the peer a truncated message.
	#[kio::test]
	async fn closing_an_unflushed_message_resets() {
		let log = Log::default();
		let budget = kio::Producer::new(0);
		let mut writer = Writer::new(
			SinkSend::stepped(log.clone(), budget.consume()),
			crate::lite::Version::Lite05,
		);
		writer.buffer_varint(5).unwrap();
		assert!(writer.poll_flush(&kio::Waiter::noop()).is_pending());
		assert!(!writer.is_flushed());

		assert!(matches!(writer.close().await, Err(Error::Cancel)));
		assert_eq!(log.resets(), vec![StreamError::Cancel.to_code()]);
		assert!(
			!log.trail()
				.iter()
				.any(|(_, sent)| matches!(sent, crate::lite::test_transport::Sent::Finish))
		);
	}

	/// A stream that takes every write, with a delivered prefix the test moves. It counts the
	/// polls waiting on delivery, which stay pending.
	#[derive(Clone, Default)]
	struct Acked {
		acked: std::sync::Arc<std::sync::atomic::AtomicU64>,
		waits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
	}

	impl Acked {
		fn ack(&self, through: u64) {
			self.acked.store(through, std::sync::atomic::Ordering::Relaxed);
		}

		fn waits(&self) -> usize {
			self.waits.swap(0, std::sync::atomic::Ordering::Relaxed)
		}
	}

	impl crate::transport::poll::SendStream for Acked {
		type Error = Stopped;
		type Group = crate::transport::poll::Ungrouped;

		fn poll_write(&mut self, _: &kio::Waiter, buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
			Poll::Ready(Ok(buf.len()))
		}

		fn set_priority(&mut self, _: i64) {}

		fn set_group(&mut self, group: &Self::Group) {
			match *group {}
		}

		fn finish(&mut self) -> Result<(), Self::Error> {
			Ok(())
		}

		fn reset(&mut self, _: u32) {}

		fn poll_closed(&self, _: &kio::Waiter) -> Poll<Result<(), Self::Error>> {
			Poll::Pending
		}

		fn delivered(&self) -> Option<u64> {
			Some(self.acked.load(std::sync::atomic::Ordering::Relaxed))
		}

		fn poll_delivered(&self, _: &kio::Waiter) -> Poll<()> {
			self.waits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
			Poll::Pending
		}
	}

	/// A frame's bytes bill as the transport delivers them, not as the stream takes them,
	/// and the frame once its last byte is: a reset never bills what it discarded. A stream
	/// waiting on its peer waits on delivery too, so its last frame bills as it lands.
	#[test]
	fn egress_bills_delivered_payload() {
		let registry = crate::stats::Registry::new(crate::stats::Config::new());
		let billed = || {
			registry
				.snapshot()
				.traffic()
				.into_iter()
				.map(|(_, _, traffic)| (traffic.bytes, traffic.frames))
				.fold((0, 0), |(bytes, frames), (b, f)| (bytes + b, frames + f))
		};
		let stream = Acked::default();
		let mut writer = Writer::new(stream.clone(), crate::lite::Version::Lite05);
		writer.bill(registry.publisher_meter(crate::Path::from_static("demo"), &crate::stats::Tier::default()));
		let waiter = &kio::Waiter::noop();

		// A one-byte header, then two 100-byte frames, each behind a two-byte size.
		writer.buffer_varint(0).unwrap();
		for _ in 0..2 {
			writer.buffer_varint(100).unwrap();
			writer.frame(100);
			writer.buffer_raw(&[b'x'; 100]);
		}
		assert!(writer.poll_flush(waiter).is_ready());
		assert!(writer.poll_closed(waiter).is_pending());
		assert_eq!(billed(), (0, 0), "the stream took it all, the transport delivered none");
		assert_eq!(stream.waits(), 1, "the wait for the peer waits on delivery too");

		// Into the first frame's size: none of its payload.
		stream.ack(2);
		assert!(writer.poll_closed(waiter).is_pending());
		assert_eq!(billed(), (0, 0));

		// Through the first frame and half the second.
		stream.ack(3 + 100 + 2 + 50);
		assert!(writer.poll_closed(waiter).is_pending());
		assert_eq!(billed(), (150, 1));

		// All of it: nothing is left to wait on.
		stream.waits();
		stream.ack(3 + 100 + 2 + 100);
		assert!(writer.poll_closed(waiter).is_pending());
		assert_eq!(billed(), (200, 2));
		assert_eq!(stream.waits(), 0);

		// A reset bills what was delivered by then, and nothing after.
		writer.buffer_varint(100).unwrap();
		writer.frame(100);
		writer.buffer_raw(&[b'x'; 100]);
		assert!(writer.poll_flush(waiter).is_ready());
		stream.ack(1 + 2 * (2 + 100) + 2 + 30);
		writer.abort(&Error::Cancel);
		stream.ack(u64::MAX);
		assert_eq!(billed(), (230, 2));
	}

	/// Writes some bytes, then errors: the shape of a partial encode.
	#[derive(Debug)]
	struct Poison;

	impl<V> Encode<V> for Poison {
		fn encode(&self, w: &mut Encoder<'_>, _: V) -> Result<(), EncodeError> {
			w.control = Some(crate::stats::Message::Probe);
			w.slice(b"junk");
			Err(EncodeError::BoundsExceeded)
		}
	}

	/// A failed encode must leave the buffer exactly as it was: flushing its
	/// partial bytes would desynchronize the stream for every later message.
	#[test]
	fn a_failed_encode_leaves_no_partial_bytes() {
		let mut writer = Writer::new(SinkSend::new(Log::default()), crate::lite::Version::Lite05);

		writer.buffer_varint(5).unwrap();
		writer.buffer(&Poison).unwrap_err();
		writer.buffer_varint(7).unwrap();

		let log = writer.stream.as_ref().unwrap().log.clone();
		let waiter = &kio::Waiter::noop();
		assert!(writer.poll_flush(waiter).is_ready());
		assert_eq!(*log.writes.lock().unwrap(), vec![5, 7], "the partial encode leaked");

		// A control message's type goes with its body: a bare type would make the next
		// message, or a FIN, a torn one.
		let mut writer = Writer::new(SinkSend::new(Log::default()), crate::ietf::Version::Draft18);
		writer.buffer_varint(5).unwrap();
		writer.buffer_message(0x2, &Poison).unwrap_err();
		writer.buffer_varint(7).unwrap();

		let log = writer.stream.as_ref().unwrap().log.clone();
		assert!(writer.poll_flush(waiter).is_ready());
		assert_eq!(*log.writes.lock().unwrap(), vec![5, 7], "the message type leaked");
	}

	#[test]
	fn control_body_counts_successful_encodes_once_before_transport_delivery() {
		use crate::{
			lite,
			stats::{Message, Registry, Tier, Totals},
		};
		let stats = Registry::new(Default::default());
		let session = stats
			.tier(Tier::new(crate::Path::from_static("internal")))
			.session(crate::stats::SessionInfo::new(crate::Path::from_static("")));
		let gate = kio::Producer::new(false);
		let mut writer =
			Writer::new(SinkSend::gated(Log::default(), gate.consume()), lite::Version::Lite07).with_stats(&session);
		writer.buffer(&lite::ControlType::Probe).unwrap();
		let msg = lite::Probe {
			bitrate: Some(1_000_000),
			rtt: Some(12),
		};
		let size = msg.encode_bytes(lite::Version::Lite07).unwrap().len() as u64;
		writer.buffer(&msg).unwrap();
		writer.buffer(&Poison).unwrap_err();
		assert!(writer.poll_flush(&kio::Waiter::noop()).is_pending());
		assert!(writer.poll_flush(&kio::Waiter::noop()).is_pending());
		assert_eq!(
			stats.snapshot().control(),
			vec![(
				Tier::new(crate::Path::from_static("internal")),
				Message::Probe,
				Totals {
					tx: size,
					tx_messages: 1,
					..Default::default()
				}
			)]
		);
		writer
			.buffer(&lite::Track {
				broadcast: crate::Path::from_static("test"),
				instance: None,
				node: 0,
				track: "video".into(),
			})
			.unwrap();
		let track_info = || {
			stats
				.snapshot()
				.control()
				.into_iter()
				.find(|(_, class, _)| *class == Message::TrackInfo)
				.unwrap()
				.2
				.tx
		};
		let before = track_info();
		let properties = lite::ietf::TrackProperties::default();
		writer.buffer(&properties).unwrap();
		assert_eq!(
			track_info() - before,
			properties.encode_bytes(lite::Version::Lite07).unwrap().len() as u64,
			"negotiated track-property extensions are control bodies too"
		);

		writer
			.buffer(&lite::SubscribeStart {
				largest: None,
				starts: vec![Some(3)],
			})
			.unwrap();
		writer.buffer(&lite::AnnounceBroadcast::Synced).unwrap();
		assert_eq!(
			stats.snapshot().control().len(),
			4,
			"classes do not collapse into the same counter"
		);
	}

	/// A flush interrupted mid-message resumes where it left off, so an
	/// abandoned wrapper future never desynchronizes the stream.
	#[test]
	fn flush_resumes_after_pending() {
		let gate = kio::Producer::new(false);
		let mut writer = Writer::new(
			SinkSend::gated(Log::default(), gate.consume()),
			crate::lite::Version::Lite05,
		);
		let log = writer.stream.as_ref().unwrap().log.clone();

		writer.buffer_varint(5).unwrap();
		let waiter = &kio::Waiter::noop();
		assert!(writer.poll_flush(waiter).is_pending());
		assert!(log.writes.lock().unwrap().is_empty());

		// The bytes survive the Pending (and a second message queued behind them).
		writer.buffer_varint(7).unwrap();
		let Ok(mut open) = gate.write() else {
			panic!("gate closed")
		};
		*open = true;
		drop(open);
		assert!(writer.poll_flush(waiter).is_ready());
		assert_eq!(
			*log.writes.lock().unwrap(),
			vec![5, 7],
			"the flush lost or reordered bytes"
		);
	}
}
