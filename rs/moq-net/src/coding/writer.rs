use std::{
	fmt::Debug,
	task::{Context, Poll, ready},
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
	buffer: bytes::BytesMut,
	version: V,
}

impl<S: crate::transport::poll::SendStream, V> Writer<S, V> {
	/// Create a new writer for the given stream and version.
	pub fn new(stream: S, version: V) -> Self {
		Self {
			stream: Some(stream),
			buffer: Default::default(),
			version,
		}
	}

	/// Encode the given message into the write buffer, to be sent by
	/// [`Self::poll_flush`]. An encode error leaves the buffer untouched.
	pub fn buffer<T: Encode<V> + Debug>(&mut self, msg: &T) -> Result<(), Error>
	where
		V: Clone,
	{
		let start = self.buffer.len();
		if let Err(err) = msg.encode(&mut self.buffer, self.version.clone()) {
			// Drop the partial encode: flushing it would corrupt the stream.
			self.buffer.truncate(start);
			return Err(err.into());
		}
		Ok(())
	}

	/// Append raw pre-encoded bytes to the write buffer, to be sent by
	/// [`Self::poll_flush`].
	pub fn buffer_raw(&mut self, bytes: &[u8]) {
		self.buffer.extend_from_slice(bytes);
	}

	/// Poll until the write buffer has fully hit the stream.
	pub fn poll_flush(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
		while !self.buffer.is_empty() {
			ready!(self.stream.as_mut().unwrap().poll_write_buf(cx, &mut self.buffer))
				.map_err(Error::from_transport)?;
		}
		Poll::Ready(Ok(()))
	}

	/// Encode the given message to the stream.
	///
	/// Cancelling this future never desynchronizes the stream: the message is
	/// already buffered and a later flush completes it. That also means a
	/// cancelled `encode` must not be retried with the same message, or two
	/// copies go on the wire; resume with [`Self::poll_flush`] (or any later
	/// write) instead.
	pub async fn encode<T: Encode<V> + Debug>(&mut self, msg: &T) -> Result<(), Error>
	where
		V: Clone,
	{
		self.buffer(msg)?;
		std::future::poll_fn(|cx| self.poll_flush(cx)).await
	}

	/// Poll a write of `buf`, flushing any buffered message bytes first so the
	/// stream never reorders around the buffer.
	pub fn poll_write<Buf: bytes::Buf>(&mut self, cx: &mut Context<'_>, buf: &mut Buf) -> Poll<Result<usize, Error>> {
		ready!(self.poll_flush(cx))?;
		self.stream
			.as_mut()
			.unwrap()
			.poll_write_buf(cx, buf)
			.map_err(Error::from_transport)
	}

	/// Poll until the entire `Buf` has been written to the stream.
	///
	/// NOTE: This can avoid performing a copy when using `Bytes`.
	pub fn poll_write_all<Buf: bytes::Buf>(&mut self, cx: &mut Context<'_>, buf: &mut Buf) -> Poll<Result<(), Error>> {
		while buf.has_remaining() {
			ready!(self.poll_write(cx, buf))?;
		}
		Poll::Ready(Ok(()))
	}

	/// Write the entire `Buf` to the stream.
	///
	/// NOTE: This can avoid performing a copy when using `Bytes`.
	pub async fn write_all<Buf: bytes::Buf + Send>(&mut self, buf: &mut Buf) -> Result<(), Error> {
		std::future::poll_fn(|cx| self.poll_write_all(cx, buf)).await
	}

	/// Mark the stream as finished.
	///
	/// Only valid once the buffer has flushed; the callers that finish
	/// immediately after an `encode` are safe because `encode` flushes fully.
	pub fn finish(&mut self) -> Result<(), Error> {
		debug_assert!(self.buffer.is_empty(), "finish with unflushed bytes");
		self.stream.as_mut().unwrap().finish().map_err(Error::from_transport)
	}

	/// Abort the stream with the given error.
	///
	/// Consumes the writer: a later write won't compile, and the [`Drop`] fallback can't
	/// reset a second time and overwrite the reason with a plain [`Error::Cancel`].
	pub fn abort(mut self, err: &Error) {
		if let Some(mut stream) = self.stream.take() {
			stream.reset(StreamError::from(err).to_code());
		}
	}

	/// Finish the stream and wait for the peer to acknowledge everything written.
	///
	/// [`Self::finish`] alone is not enough to deliver a final message. A stream that has sent
	/// its FIN is still retransmitting unacknowledged data, and a RESET_STREAM from that state
	/// discards it, so the [`Drop`] fallback below can throw away bytes the peer never read.
	/// Consuming the writer is what removes that fallback, and waiting for the acknowledgement
	/// is what makes the bytes safe.
	pub async fn close(mut self) -> Result<(), Error> {
		let Some(mut stream) = self.stream.take() else {
			return Ok(());
		};

		stream.finish().map_err(Error::from_transport)?;
		std::future::poll_fn(|cx| stream.poll_closed(cx).map_err(Error::from_transport)).await
	}

	/// Poll until the stream is closed, or the [Self::finish] is acknowledged by the peer.
	pub fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
		self.stream
			.as_mut()
			.unwrap()
			.poll_closed(cx)
			.map_err(Error::from_transport)
	}

	/// Poll-friendly [`Self::close`] for a finished writer: once the peer acknowledges
	/// (or the stream dies), the stream is released so the [`Drop`] fallback cannot
	/// reset an acknowledged stream with a spurious Cancel.
	pub fn poll_close(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
		let res = ready!(self.poll_closed(cx));
		self.stream = None;
		Poll::Ready(res)
	}

	/// Wait for the stream to be closed, or the [Self::finish] to be acknowledged by the peer.
	pub async fn closed(&mut self) -> Result<(), Error> {
		std::future::poll_fn(|cx| self.poll_closed(cx)).await
	}

	/// Set the stream's send order: streams with HIGHER values are transmitted first.
	///
	/// This is the transport trait's convention (matching W3C `sendOrder` and quinn's
	/// scheduler) and the model's [`Subscription::priority`](crate::track::Subscription),
	/// where higher values preempt lower ones. The lite priority queue's rank is the
	/// opposite (0 = most urgent); rank holders convert via `PriorityHandle::send_order`.
	pub fn set_priority(&mut self, send_order: u8) {
		self.stream.as_mut().unwrap().set_priority(send_order);
	}

	/// Cast the writer to a different version, used during version negotiation.
	pub fn with_version<O>(mut self, version: O) -> Writer<S, O> {
		Writer {
			// We need to use an Option so Drop doesn't reset the stream.
			stream: self.stream.take(),
			buffer: std::mem::take(&mut self.buffer),
			version,
		}
	}
}

impl<S: crate::transport::poll::SendStream> Writer<S, ietf::Version> {
	/// Encode an IETF `Message` to the stream, writing `[type_id][size][body]`.
	pub async fn encode_message<T: ietf::Message>(&mut self, msg: &T) -> Result<(), Error> {
		self.buffer(&T::ID)?;
		self.encode(msg).await
	}
}

impl<S: crate::transport::poll::SendStream, V> Drop for Writer<S, V> {
	fn drop(&mut self) {
		if let Some(mut stream) = self.stream.take() {
			// Unlike the Quinn default, we abort the stream on drop.
			stream.reset(StreamError::Cancel.to_code());
		}
	}
}
#[cfg(test)]
mod tests {
	use super::*;
	use crate::lite::test_transport::{Log, SinkSend};
	use std::task::Waker;

	#[test]
	fn set_priority_forwards_send_order() {
		let log = Log::default();
		let mut writer = Writer::new(SinkSend::new(log.clone()), crate::lite::Version::Lite05);

		for send_order in 0u8..=255 {
			writer.set_priority(send_order);
		}

		assert_eq!(log.priorities(), (0u8..=255).collect::<Vec<_>>());
	}

	/// Writes some bytes, then errors: the shape of a partial encode.
	#[derive(Debug)]
	struct Poison;

	impl Encode<()> for Poison {
		fn encode<W: bytes::BufMut>(&self, w: &mut W, _: ()) -> Result<(), EncodeError> {
			w.put_slice(b"junk");
			Err(EncodeError::BoundsExceeded)
		}
	}

	/// A failed encode must leave the buffer exactly as it was: flushing its
	/// partial bytes would desynchronize the stream for every later message.
	#[test]
	fn a_failed_encode_leaves_no_partial_bytes() {
		let mut writer = Writer::new(SinkSend::new(Log::default()), ());

		writer.buffer(&5u8).unwrap();
		writer.buffer(&Poison).unwrap_err();
		writer.buffer(&7u8).unwrap();

		let log = writer.stream.as_ref().unwrap().log.clone();
		let mut cx = std::task::Context::from_waker(Waker::noop());
		assert!(writer.poll_flush(&mut cx).is_ready());
		assert_eq!(*log.writes.lock().unwrap(), vec![5, 7], "the partial encode leaked");
	}

	/// A flush interrupted mid-message resumes where it left off, so an
	/// abandoned wrapper future never desynchronizes the stream.
	#[test]
	fn flush_resumes_after_pending() {
		let gate = kio::Producer::new(false);
		let mut writer = Writer::new(SinkSend::gated(Log::default(), gate.consume()), ());
		let log = writer.stream.as_ref().unwrap().log.clone();

		writer.buffer(&5u8).unwrap();
		let mut cx = std::task::Context::from_waker(Waker::noop());
		assert!(writer.poll_flush(&mut cx).is_pending());
		assert!(log.writes.lock().unwrap().is_empty());

		// The bytes survive the Pending (and a second message queued behind them).
		writer.buffer(&7u8).unwrap();
		let Ok(mut open) = gate.write() else {
			panic!("gate closed")
		};
		*open = true;
		drop(open);
		assert!(writer.poll_flush(&mut cx).is_ready());
		assert_eq!(
			*log.writes.lock().unwrap(),
			vec![5, 7],
			"the flush lost or reordered bytes"
		);
	}
}
