use std::{
	cmp,
	fmt::Debug,
	io,
	task::{Context, Poll, ready},
};

use bytes::{Buf, Bytes, BytesMut};

use crate::{Error, StreamError, coding::*};

/// A reader for decoding messages from a stream.
///
/// The `poll_*` methods are the implementation; the `async` methods are thin
/// wrappers, so the reader can be driven from another poll function without
/// pinning a future. Partial bytes accumulate in the internal buffer, so a
/// `Pending` (or a cancelled wrapper) never desynchronizes the stream.
pub struct Reader<S: crate::transport::poll::RecvStream, V> {
	stream: S,
	buffer: BytesMut,
	version: V,
}

/// How much payload may pile up behind a parked consumer before
/// [`Reader::poll_read_frame`] wakes it, even while the transport keeps handing over
/// ready chunks.
///
/// The loop's natural boundary is `Pending`, but that is a bound on the transport, not
/// on time: a sender at line rate can keep it `Ready` for as long as it likes, and on a
/// multi-threaded runtime a consumer on another worker would sit idle behind bytes that
/// have already arrived. The value is a trade-off rather than a derived limit, big
/// enough that an ordinary burst is still one wake.
const WAKE_BUDGET: usize = 64 * 1024;

impl<S: crate::transport::poll::RecvStream, V> Reader<S, V> {
	pub fn new(stream: S, version: V) -> Self {
		Self {
			stream,
			buffer: Default::default(),
			version,
		}
	}

	/// Poll for the next message on the stream.
	pub fn poll_decode<T: Decode<V> + Debug>(&mut self, cx: &mut Context<'_>) -> Poll<Result<T, Error>>
	where
		V: Clone,
	{
		loop {
			let mut cursor = io::Cursor::new(&self.buffer);
			match T::decode(&mut cursor, self.version.clone()) {
				Ok(msg) => {
					self.buffer.advance(cursor.position() as usize);
					return Poll::Ready(Ok(msg));
				}
				// Stream closed while we still need more data.
				Err(DecodeError::Short) if !ready!(self.poll_read_more(cx))? => {
					return Poll::Ready(Err(DecodeError::Short.into()));
				}
				Err(DecodeError::Short) => {}
				Err(e) => return Poll::Ready(Err(e.into())),
			}
		}
	}

	/// Decode the next message from the stream.
	pub async fn decode<T: Decode<V> + Debug>(&mut self) -> Result<T, Error>
	where
		V: Clone,
	{
		std::future::poll_fn(|cx| self.poll_decode(cx)).await
	}

	/// Poll for the next message unless the stream is closed cleanly first.
	pub fn poll_decode_maybe<T: Decode<V> + Debug>(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<T>, Error>>
	where
		V: Clone,
	{
		if !ready!(self.poll_has_more(cx))? {
			return Poll::Ready(Ok(None));
		}

		self.poll_decode(cx).map_ok(Some)
	}

	/// Decode the next message unless the stream is closed.
	pub async fn decode_maybe<T: Decode<V> + Debug>(&mut self) -> Result<Option<T>, Error>
	where
		V: Clone,
	{
		std::future::poll_fn(|cx| self.poll_decode_maybe(cx)).await
	}

	/// Poll for the next message without consuming it.
	pub fn poll_decode_peek<T: Decode<V> + Debug>(&mut self, cx: &mut Context<'_>) -> Poll<Result<T, Error>>
	where
		V: Clone,
	{
		loop {
			let mut cursor = io::Cursor::new(&self.buffer);
			match T::decode(&mut cursor, self.version.clone()) {
				Ok(msg) => return Poll::Ready(Ok(msg)),
				Err(DecodeError::Short) if !ready!(self.poll_read_more(cx))? => {
					return Poll::Ready(Err(DecodeError::Short.into()));
				}
				Err(DecodeError::Short) => {}
				Err(e) => return Poll::Ready(Err(e.into())),
			}
		}
	}

	/// Decode the next message from the stream without consuming it.
	pub async fn decode_peek<T: Decode<V> + Debug>(&mut self) -> Result<T, Error>
	where
		V: Clone,
	{
		std::future::poll_fn(|cx| self.poll_decode_peek(cx)).await
	}

	/// Poll for the next message without consuming it unless the stream closes cleanly first.
	pub fn poll_decode_peek_maybe<T: Decode<V> + Debug>(
		&mut self,
		cx: &mut Context<'_>,
	) -> Poll<Result<Option<T>, Error>>
	where
		V: Clone,
	{
		if !ready!(self.poll_has_more(cx))? {
			return Poll::Ready(Ok(None));
		}

		self.poll_decode_peek(cx).map_ok(Some)
	}

	/// Peek the next message unless the stream is closed.
	pub async fn decode_peek_maybe<T: Decode<V> + Debug>(&mut self) -> Result<Option<T>, Error>
	where
		V: Clone,
	{
		std::future::poll_fn(|cx| self.poll_decode_peek_maybe(cx)).await
	}

	/// Poll for the next chunk, draining the reader's internal buffer first.
	pub fn poll_read_chunk(&mut self, cx: &mut Context<'_>, max: usize) -> Poll<Result<Option<Bytes>, Error>> {
		if !self.buffer.is_empty() {
			let n = cmp::min(self.buffer.len(), max);
			return Poll::Ready(Ok(Some(self.buffer.split_to(n).freeze())));
		}
		self.stream.poll_read_chunk(cx, max).map_err(Error::from_transport)
	}

	/// Fill a frame's payload from the stream, returning `Pending` once it would block.
	///
	/// `Ready(Ok(()))` means the frame is full and the caller should commit it; an error
	/// means the caller should abort it. Either way the frame is untouched otherwise, so
	/// the caller keeps ownership of the commit/abort decision.
	///
	/// Consumers are woken where the loop yields rather than once per chunk. The wire
	/// hands over everything it has already buffered in one turn, and each wake pays a
	/// group lock and a clock read, so the intermediate ones mostly publish bytes the
	/// consumer is about to be handed anyway. Pairing the deferred
	/// [`frame::ProducerOwned::write`] with its wake is this function's job precisely
	/// because it owns the only `Pending` it can return; the other two exits are woken
	/// by the caller's `finish` (which stamps and publishes the frame) or `abort` (which
	/// fails the group). [`WAKE_BUDGET`] bounds the wait in between.
	pub(crate) fn poll_read_frame(
		&mut self,
		cx: &mut Context<'_>,
		frame: &mut crate::frame::ProducerOwned,
	) -> Poll<Result<(), Error>> {
		let mut owed = 0;

		let result = loop {
			if frame.remaining() == 0 {
				break Ok(());
			}

			match self.poll_read_chunk(cx, frame.remaining()) {
				Poll::Pending => {
					if owed > 0 {
						frame.notify();
					}
					return Poll::Pending;
				}
				Poll::Ready(Ok(Some(chunk))) if !chunk.is_empty() => {
					owed += chunk.len();
					if let Err(err) = frame.write(chunk) {
						break Err(err);
					}
					if owed >= WAKE_BUDGET {
						frame.notify();
						owed = 0;
					}
				}
				// A FIN mid-payload: the declared size never arrived.
				Poll::Ready(Ok(_)) => break Err(Error::WrongSize),
				Poll::Ready(Err(err)) => break Err(err),
			}
		};

		Poll::Ready(result)
	}

	/// Poll for exactly `size` bytes, accumulating partial reads in the buffer.
	pub fn poll_read_exact(&mut self, cx: &mut Context<'_>, size: usize) -> Poll<Result<Bytes, Error>> {
		while self.buffer.len() < size {
			if !ready!(self.poll_read_more(cx))? {
				return Poll::Ready(Err(DecodeError::Short.into()));
			}
		}
		Poll::Ready(Ok(self.buffer.split_to(size).freeze()))
	}

	/// Read exactly the given number of bytes from the stream.
	pub async fn read_exact(&mut self, size: usize) -> Result<Bytes, Error> {
		std::future::poll_fn(|cx| self.poll_read_exact(cx, size)).await
	}

	/// Poll until the stream is closed, erroring if there are any additional bytes.
	pub fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
		if ready!(self.poll_has_more(cx))? {
			return Poll::Ready(Err(DecodeError::Short.into()));
		}

		Poll::Ready(Ok(()))
	}

	/// Poll for whether data is available in the buffer or stream.
	fn poll_has_more(&mut self, cx: &mut Context<'_>) -> Poll<Result<bool, Error>> {
		if !self.buffer.is_empty() {
			return Poll::Ready(Ok(true));
		}

		self.poll_read_more(cx)
	}

	/// Poll for more data from the stream. `true` if data was read, `false` if the
	/// stream finished.
	fn poll_read_more(&mut self, cx: &mut Context<'_>) -> Poll<Result<bool, Error>> {
		match ready!(self.stream.poll_read_buf(cx, &mut self.buffer)) {
			Ok(Some(_)) => Poll::Ready(Ok(true)),
			Ok(None) => Poll::Ready(Ok(false)),
			Err(e) => Poll::Ready(Err(Error::from_transport(e))),
		}
	}

	/// Abort the stream with the given error.
	pub fn abort(&mut self, err: &Error) {
		// STOP_SENDING is a stream operation, so it carries a stream code. Sending the
		// session code here would have the peer read it against the wrong registry.
		self.stream.stop(StreamError::from(err).to_code());
	}

	/// Abort the stream with a raw application code.
	///
	/// [`Self::abort`] encodes the moq-lite error space. A protocol with its own registry
	/// of stream reset codes has to name one directly, since the two spaces do not agree
	/// on what a given number means.
	pub fn stop(&mut self, code: u32) {
		self.stream.stop(code);
	}

	/// Cast the reader to a different version, used during version negotiation.
	pub fn with_version<V2>(self, version: V2) -> Reader<S, V2> {
		Reader {
			stream: self.stream,
			buffer: self.buffer,
			version,
		}
	}
}

#[cfg(test)]
mod tests {
	use std::sync::Arc;

	use futures::FutureExt;

	use super::*;
	use crate::StreamError;

	/// Records the STOP_SENDING code so a test can assert which registry it came from.
	#[derive(Default)]
	struct StopLog {
		stops: Vec<u32>,
	}

	impl web_transport_trait::poll::RecvStream for StopLog {
		type Error = crate::lite::test_transport::SinkError;

		fn poll_read(
			&mut self,
			_cx: &mut std::task::Context<'_>,
			_dst: &mut [u8],
		) -> std::task::Poll<Result<Option<usize>, Self::Error>> {
			std::task::Poll::Ready(Ok(None))
		}

		fn stop(&mut self, code: u32) {
			self.stops.push(code);
		}

		fn poll_closed(&mut self, _cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
			std::task::Poll::Ready(Ok(()))
		}
	}

	/// STOP_SENDING refuses a stream, so it must carry a stream code. Reusing the session
	/// table here would have the peer decode it against the wrong registry: `Cancel` would
	/// arrive as INTERNAL_ERROR, and `Unauthorized` as KEY_VALUE_FORMATTING_ERROR.
	#[test]
	fn abort_stops_with_a_stream_code() {
		for (err, expected) in [
			(Error::Cancel, StreamError::Cancel.to_code()),
			(Error::Lagged, StreamError::TooFarBehind.to_code()),
			(
				Error::Unauthorized,
				StreamError::Session(crate::SessionError::Unauthorized).to_code(),
			),
		] {
			let mut reader = Reader::new(StopLog::default(), ());
			reader.abort(&err);
			assert_eq!(reader.stream.stops, vec![expected], "{err:?} used the wrong registry");
		}

		// The two registries disagree about 0 and 1, which is what makes the mix-up silent.
		assert_ne!(StreamError::Cancel.to_code(), crate::SessionError::Cancel.to_code());
	}

	/// Counts wakes delivered to a parked consumer.
	#[derive(Default)]
	struct CountWaker(std::sync::atomic::AtomicUsize);

	impl std::task::Wake for CountWaker {
		fn wake(self: Arc<Self>) {
			self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
		}
	}

	impl CountWaker {
		fn count(&self) -> usize {
			self.0.load(std::sync::atomic::Ordering::SeqCst)
		}
	}

	/// A group with one open frame, plus a consumer parked on its payload.
	fn parked_frame(
		size: usize,
	) -> (
		crate::frame::ProducerOwned,
		crate::frame::Consumer,
		kio::Waiter,
		Arc<CountWaker>,
	) {
		let mut group = crate::group::Info { sequence: 0 }.produce();
		let mut consumer = group.consume();
		let frame = group
			.create_frame_owned(crate::frame::Info {
				size: size as u64,
				timestamp: crate::Timestamp::ZERO,
			})
			.unwrap();

		let payload = consumer.next_frame().now_or_never().unwrap().unwrap().unwrap();
		let wakes = Arc::new(CountWaker::default());
		let waiter = kio::Waiter::new(wakes.clone().into());

		(frame, payload, waiter, wakes)
	}

	/// Hands back one queued chunk per poll, then blocks: a stream whose socket has
	/// delivered a burst and has nothing more for this turn.
	#[derive(Default)]
	struct Chunks(std::collections::VecDeque<&'static [u8]>);

	impl web_transport_trait::poll::RecvStream for Chunks {
		type Error = crate::lite::test_transport::SinkError;

		fn poll_read(&mut self, _cx: &mut Context<'_>, dst: &mut [u8]) -> Poll<Result<Option<usize>, Self::Error>> {
			let Some(chunk) = self.0.pop_front() else {
				return Poll::Pending;
			};

			// The caller sizes `dst`, so a chunk larger than it spans two reads.
			let n = chunk.len().min(dst.len());
			dst[..n].copy_from_slice(&chunk[..n]);
			if n < chunk.len() {
				self.0.push_front(&chunk[n..]);
			}

			Poll::Ready(Ok(Some(n)))
		}

		fn stop(&mut self, _code: u32) {}

		fn poll_closed(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
			Poll::Pending
		}
	}

	/// A consumer parked on the group cannot run until this task yields, so a burst of
	/// chunks owes exactly one wake: at the boundary, from whoever wrote them. Every
	/// other exit hands the wake to the caller's `finish` or `abort`.
	#[test]
	fn frame_payload_wakes_once_per_poll_turn() {
		let (mut frame, mut payload, waiter, wakes) = parked_frame(9);
		assert!(payload.poll_read_chunk(&waiter).is_pending());

		let mut reader = Reader::new(Chunks([b"foo".as_slice(), b"bar"].into()), ());
		let mut cx = Context::from_waker(std::task::Waker::noop());

		assert!(reader.poll_read_frame(&mut cx, &mut frame).is_pending());
		assert_eq!(wakes.count(), 1, "two chunks in one turn owe one wake");

		let Poll::Ready(Ok(Some(chunk))) = payload.poll_read_chunk(&waiter) else {
			panic!("the boundary wake did not publish both chunks");
		};
		assert_eq!(chunk, Bytes::from_static(b"foobar"));

		// A turn that reads nothing must not touch the group at all.
		assert!(payload.poll_read_chunk(&waiter).is_pending());
		assert!(reader.poll_read_frame(&mut cx, &mut frame).is_pending());
		assert_eq!(wakes.count(), 1, "an empty poll turn woke a consumer");

		// The tail completes the frame, so the loop returns without a wake of its own.
		// The commit is what publishes it, and the commit is the caller's to make.
		reader.stream.0.push_back(b"baz");
		assert!(matches!(
			reader.poll_read_frame(&mut cx, &mut frame),
			Poll::Ready(Ok(()))
		));
		assert_eq!(wakes.count(), 1, "a full frame woke before it was committed");

		frame.finish().unwrap();
		assert_eq!(wakes.count(), 2);
	}

	/// Stands in for an egress consumer on another worker: it catches up and re-parks
	/// before handing over each chunk, so every wake the drain loop issues while the
	/// burst is in flight lands on a parked waiter and is counted.
	struct Burst {
		chunks: Chunks,
		payload: crate::frame::Consumer,
		waiter: kio::Waiter,
	}

	impl web_transport_trait::poll::RecvStream for Burst {
		type Error = crate::lite::test_transport::SinkError;

		fn poll_read(&mut self, cx: &mut Context<'_>, dst: &mut [u8]) -> Poll<Result<Option<usize>, Self::Error>> {
			while self.payload.poll_read_chunk(&self.waiter).is_ready() {}
			self.chunks.poll_read(cx, dst)
		}

		fn stop(&mut self, _code: u32) {}

		fn poll_closed(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
			Poll::Pending
		}
	}

	/// A sender at line rate can keep the drain loop `Ready` for as long as it likes, so
	/// the loop's own boundary is not a bound on how long a parked consumer waits.
	/// [`WAKE_BUDGET`] is.
	#[test]
	fn a_burst_past_the_budget_wakes_before_the_boundary() {
		const CHUNK: &[u8] = &[0u8; 8 * 1024];
		let chunks = 2 * WAKE_BUDGET / CHUNK.len();

		// One byte short, so the loop can never reach its `Ok(())` exit.
		let (mut frame, mut payload, waiter, wakes) = parked_frame(chunks * CHUNK.len() + 1);
		assert!(payload.poll_read_chunk(&waiter).is_pending());

		let mut reader = Reader::new(
			Burst {
				chunks: Chunks(std::iter::repeat_n(CHUNK, chunks).collect()),
				payload,
				waiter,
			},
			(),
		);
		let mut cx = Context::from_waker(std::task::Waker::noop());
		assert!(reader.poll_read_frame(&mut cx, &mut frame).is_pending());

		// Two budgets' worth, and the boundary is owed nothing by the time it arrives.
		assert_eq!(wakes.count(), 2, "the burst was withheld past its budget");
	}
}
