use std::{
	fmt::Debug,
	task::{Poll, ready},
};

use bytes::{Buf, Bytes};

use crate::{
	Error, StreamError,
	coding::*,
	transport::poll::{Lease, RecvStream},
};

/// A reader for decoding messages from a stream.
///
/// The `poll_*` methods are the implementation; the `async` methods are thin
/// wrappers, so the reader can be driven from another poll function without
/// pinning a future. Unconsumed bytes stay leased in the transport, which counts them
/// against its flow control until each unit is complete, so a `Pending` (or a cancelled
/// wrapper) never desynchronizes the stream and a parked reader holds no memory of its own.
pub struct Reader<S: RecvStream, V> {
	stream: S,
	version: V,
	/// How many bytes the last short decode needed, so the retry waits for all of them
	/// instead of waking once per chunk.
	want: usize,
	/// A contiguous copy of a message that spans transport chunks, empty between decodes.
	scratch: Vec<u8>,
	stats: crate::stats::Session,
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

/// The largest [`Reader::scratch`] kept between decodes; one past this is freed.
const SCRATCH_KEEP: usize = 64 * 1024;

impl<S: RecvStream, V: StreamCodes> Reader<S, V> {
	pub fn new(stream: S, version: V) -> Self {
		Self {
			stream,
			version,
			want: 0,
			scratch: Vec::new(),
			stats: Default::default(),
		}
	}

	pub(crate) fn with_stats(mut self, stats: &crate::stats::Session) -> Self {
		self.stats = stats.clone();
		self
	}

	/// Where control messages read here are counted.
	pub(crate) fn stats(&self) -> &crate::stats::Session {
		&self.stats
	}

	/// Poll the stream for a lease of at least `want` bytes.
	fn poll_lease(&mut self, waiter: &kio::Waiter, want: usize) -> Poll<Result<S::Lease<'_>, Error>> {
		let version = &self.version;
		self.stream
			.poll_lease(waiter, want)
			.map_err(|err| version.transport_error(err))
	}

	/// Poll `decode` against the leased bytes, leasing more until it stops coming up
	/// short. `consume` releases what it read; otherwise the bytes stay for the next read.
	fn poll_with<T>(
		&mut self,
		waiter: &kio::Waiter,
		consume: bool,
		decode: impl FnMut(&mut Decoder<'_>) -> Result<T, DecodeError>,
	) -> Poll<Result<T, Error>>
	where
		V: Into<Form> + Copy,
	{
		self.poll_with_sized(waiter, consume, decode).map_ok(|(value, _)| value)
	}

	/// [`Self::poll_with`], also returning how many bytes the value took.
	fn poll_with_sized<T>(
		&mut self,
		waiter: &kio::Waiter,
		consume: bool,
		mut decode: impl FnMut(&mut Decoder<'_>) -> Result<T, DecodeError>,
	) -> Poll<Result<(T, usize), Error>>
	where
		V: Into<Form> + Copy,
	{
		let form = self.version.into();
		loop {
			let want = self.want.max(1);
			let version = &self.version;
			let mut lease = ready!(self.stream.poll_lease(waiter, want)).map_err(|err| version.transport_error(err))?;

			// Decode against the first chunk, or a copy of as much as the last attempt needed
			// when that spans chunks. Only a message cut across chunks pays for the copy.
			let mut size = lease.chunk().len().max(want.min(lease.remaining()));
			loop {
				let chunk = lease.chunk();
				let buf: &[u8] = match chunk.len() >= size {
					true => chunk,
					false => {
						self.scratch.clear();
						for chunk in lease.chunks() {
							let take = chunk.len().min(size - self.scratch.len());
							self.scratch.extend_from_slice(&chunk[..take]);
							if self.scratch.len() == size {
								break;
							}
						}
						&self.scratch
					}
				};

				let mut r = Decoder::new(buf, form);
				let need = match decode(&mut r) {
					Ok(value) => {
						let used = buf.len() - r.remaining();
						let control = r.control.map(|class| (class, used - r.uncounted));
						self.want = 0;
						if self.scratch.capacity() > SCRATCH_KEEP {
							self.scratch = Vec::new();
						}
						if consume {
							// Counted only once the bytes are released, so a peek or a retry
							// after a short read never counts the same message twice.
							if let Some((class, size)) = control {
								self.stats.control(class, size, true);
							}
							lease.advance(used);
							lease.release();
						}
						return Poll::Ready(Ok((value, used)));
					}
					// A short read inside a complete body is malformed, but reports what it
					// needed from the body, so always ask for at least one byte more.
					Err(DecodeError::Short) => r.need().max(buf.len() + 1),
					Err(e) => return Poll::Ready(Err(e.into())),
				};

				if need <= lease.remaining() {
					size = need;
					continue;
				}
				// Stream closed while we still need more data.
				if lease.is_fin() {
					return Poll::Ready(Err(DecodeError::Short.into()));
				}
				self.want = need;
				// The lease keeps every byte for the retry, which copies them again, so a
				// parked reader holds no more of its own than an idle one.
				if self.scratch.capacity() > SCRATCH_KEEP {
					self.scratch = Vec::new();
				}
				break;
			}
		}
	}

	/// [`Self::poll_with`], or `None` if the stream closes cleanly before any byte.
	fn poll_with_maybe<T>(
		&mut self,
		waiter: &kio::Waiter,
		consume: bool,
		decode: impl FnMut(&mut Decoder<'_>) -> Result<T, DecodeError>,
	) -> Poll<Result<Option<T>, Error>>
	where
		V: Into<Form> + Copy,
	{
		if !ready!(self.poll_has_more(waiter))? {
			return Poll::Ready(Ok(None));
		}

		self.poll_with(waiter, consume, decode).map_ok(Some)
	}

	/// Poll for the next message on the stream.
	pub fn poll_decode<T: for<'a> Decode<'a, V> + Debug>(&mut self, waiter: &kio::Waiter) -> Poll<Result<T, Error>>
	where
		V: Into<Form> + Copy,
	{
		let version = self.version;
		self.poll_with(waiter, true, |r| T::decode(r, version))
	}

	/// [`Self::poll_decode`] with `decode` in place of the type's own decoder, for a
	/// message whose bounds come from the session.
	pub(crate) fn poll_decode_with<T: Debug>(
		&mut self,
		waiter: &kio::Waiter,
		decode: impl FnMut(&mut Decoder<'_>) -> Result<T, DecodeError>,
	) -> Poll<Result<T, Error>>
	where
		V: Into<Form> + Copy,
	{
		self.poll_with(waiter, true, decode)
	}

	/// [`Self::poll_decode_maybe`] with `decode` in place of the type's own decoder, for a
	/// message read in place, whose borrowed fields `decode` resolves before the bytes go.
	pub(crate) fn poll_decode_maybe_with<T: Debug>(
		&mut self,
		waiter: &kio::Waiter,
		decode: impl FnMut(&mut Decoder<'_>) -> Result<T, DecodeError>,
	) -> Poll<Result<Option<T>, Error>>
	where
		V: Into<Form> + Copy,
	{
		self.poll_with_maybe(waiter, true, decode)
	}

	/// Decode the next message from the stream.
	pub async fn decode<T: for<'a> Decode<'a, V> + Debug>(&mut self) -> Result<T, Error>
	where
		V: Into<Form> + Copy,
	{
		kio::wait(|waiter| self.poll_decode(waiter)).await
	}

	/// [`Self::poll_decode`], also returning the bytes it consumed, for a handshake to
	/// count once the session's stats are known.
	pub(crate) fn poll_decode_sized<T: for<'a> Decode<'a, V> + Debug>(
		&mut self,
		waiter: &kio::Waiter,
	) -> Poll<Result<(T, usize), Error>>
	where
		V: Into<Form> + Copy,
	{
		let version = self.version;
		self.poll_with_sized(waiter, true, |r| T::decode(r, version))
	}

	/// [`Self::decode`], also returning the bytes it consumed.
	pub(crate) async fn decode_sized<T: for<'a> Decode<'a, V> + Debug>(&mut self) -> Result<(T, usize), Error>
	where
		V: Into<Form> + Copy,
	{
		kio::wait(|waiter| self.poll_decode_sized(waiter)).await
	}

	/// Poll for the next message unless the stream is closed cleanly first.
	pub fn poll_decode_maybe<T: for<'a> Decode<'a, V> + Debug>(
		&mut self,
		waiter: &kio::Waiter,
	) -> Poll<Result<Option<T>, Error>>
	where
		V: Into<Form> + Copy,
	{
		let version = self.version;
		self.poll_with_maybe(waiter, true, |r| T::decode(r, version))
	}

	/// Decode the next message unless the stream is closed.
	pub async fn decode_maybe<T: for<'a> Decode<'a, V> + Debug>(&mut self) -> Result<Option<T>, Error>
	where
		V: Into<Form> + Copy,
	{
		kio::wait(|waiter| self.poll_decode_maybe(waiter)).await
	}

	/// Poll for the next message without consuming it unless the stream closes cleanly first.
	pub fn poll_decode_peek_maybe<T: for<'a> Decode<'a, V> + Debug>(
		&mut self,
		waiter: &kio::Waiter,
	) -> Poll<Result<Option<T>, Error>>
	where
		V: Into<Form> + Copy,
	{
		let version = self.version;
		self.poll_with_maybe(waiter, false, |r| T::decode(r, version))
	}

	/// Peek the next message unless the stream is closed.
	pub async fn decode_peek_maybe<T: for<'a> Decode<'a, V> + Debug>(&mut self) -> Result<Option<T>, Error>
	where
		V: Into<Form> + Copy,
	{
		kio::wait(|waiter| self.poll_decode_peek_maybe(waiter)).await
	}

	/// Poll for the next varint on the stream.
	pub fn poll_varint(&mut self, waiter: &kio::Waiter) -> Poll<Result<u64, Error>>
	where
		V: Into<Form> + Copy,
	{
		self.poll_with(waiter, true, |r| r.varint())
	}

	/// Read the next varint from the stream.
	pub async fn varint(&mut self) -> Result<u64, Error>
	where
		V: Into<Form> + Copy,
	{
		kio::wait(|waiter| self.poll_varint(waiter)).await
	}

	/// Poll for the next varint unless the stream is closed cleanly first.
	pub fn poll_varint_maybe(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<u64>, Error>>
	where
		V: Into<Form> + Copy,
	{
		self.poll_with_maybe(waiter, true, |r| r.varint())
	}

	/// Read the next varint unless the stream is closed.
	pub async fn varint_maybe(&mut self) -> Result<Option<u64>, Error>
	where
		V: Into<Form> + Copy,
	{
		kio::wait(|waiter| self.poll_varint_maybe(waiter)).await
	}

	/// Poll for the next varint without consuming it.
	pub fn poll_varint_peek(&mut self, waiter: &kio::Waiter) -> Poll<Result<u64, Error>>
	where
		V: Into<Form> + Copy,
	{
		self.poll_with(waiter, false, |r| r.varint())
	}

	/// Read the next varint without consuming it.
	pub async fn varint_peek(&mut self) -> Result<u64, Error>
	where
		V: Into<Form> + Copy,
	{
		kio::wait(|waiter| self.poll_varint_peek(waiter)).await
	}

	/// Poll for the next chunk of up to `max` bytes, or `None` once the stream finished.
	/// Its credit returns as it is read.
	pub fn poll_read_chunk(&mut self, waiter: &kio::Waiter, max: usize) -> Poll<Result<Option<Bytes>, Error>> {
		self.want = 0;
		let version = &self.version;
		self.stream
			.poll_read_chunk(waiter, max)
			.map_err(|err| version.transport_error(err))
	}

	/// Fill a frame's payload from the stream, returning `Pending` once it would block.
	///
	/// The payload reaches the frame as it arrives, but keeps its receive credit until the
	/// segment it lands in seals, so an incomplete frame holds at most one segment of the
	/// receive window however large it is.
	///
	/// `Ready(Ok(()))` means the frame is full and the caller should commit it; an error
	/// means the caller should abort it. Either way the frame is untouched otherwise, so
	/// the caller keeps ownership of the commit/abort decision.
	///
	/// Consumers are woken where the loop yields rather than once per chunk. The wire
	/// hands over everything it has already buffered in one turn, and each wake pays a
	/// group lock and a clock read, so the intermediate ones mostly publish bytes the
	/// consumer is about to be handed anyway. Pairing the deferred
	/// [`frame::Producer::write_deferred`] with its wake is this function's job precisely
	/// because it owns the only `Pending` it can return; the other two exits are woken
	/// by the caller's `finish` (which stamps and publishes the frame) or `abort` (which
	/// fails the group). [`WAKE_BUDGET`] bounds the wait in between.
	pub(crate) fn poll_read_frame(
		&mut self,
		waiter: &kio::Waiter,
		frame: &mut crate::frame::Producer,
	) -> Poll<Result<(), Error>> {
		self.want = 0;
		let mut owed = 0;

		let result = loop {
			let mut lease = match self.poll_lease(waiter, frame.remaining().min(1)) {
				Poll::Pending => {
					if owed > 0 {
						frame.notify();
					}
					return Poll::Pending;
				}
				Poll::Ready(Ok(lease)) => lease,
				Poll::Ready(Err(err)) => break Err(err),
			};

			// One chunk per lease: a lease may hold the transport's lock, so the frame is
			// written, and its consumers woken, only once the lease is gone. A chunk holding
			// the rest of the frame is taken whole, so a frame delivered in one piece keeps
			// its allocation; any other stops where the segment being written seals.
			let remaining = frame.remaining();
			let take = match lease.chunk().len() >= remaining {
				true => remaining,
				false => lease.chunk().len().min(frame.unsealed()),
			};
			let chunk = lease.copy_to_bytes(take);
			let complete = chunk.len() == remaining;
			let ended = lease.is_fin() && !lease.has_remaining();
			if complete || chunk.len() == frame.unsealed() {
				// Every byte of the segment is in hand, so its credit returns.
				lease.release();
			} else {
				drop(lease);
			}

			if !chunk.is_empty() {
				owed += chunk.len();
				if let Err(err) = frame.write_deferred(chunk) {
					return Poll::Ready(Err(err));
				}
				if owed >= WAKE_BUDGET {
					frame.notify();
					owed = 0;
				}
			}
			if complete {
				break Ok(());
			}
			// A FIN mid-payload: the declared size never arrived.
			if ended {
				break Err(Error::WrongSize);
			}
		};

		Poll::Ready(result)
	}

	/// Poll for exactly `size` bytes.
	pub fn poll_read_exact(&mut self, waiter: &kio::Waiter, size: usize) -> Poll<Result<Bytes, Error>> {
		self.want = 0;
		let mut lease = ready!(self.poll_lease(waiter, size))?;
		if lease.remaining() < size {
			return Poll::Ready(Err(DecodeError::Short.into()));
		}
		let bytes = lease.copy_to_bytes(size);
		lease.release();
		Poll::Ready(Ok(bytes))
	}

	/// Poll until the stream is closed, erroring if there are any additional bytes.
	pub fn poll_closed(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		if ready!(self.poll_has_more(waiter))? {
			return Poll::Ready(Err(DecodeError::Short.into()));
		}

		Poll::Ready(Ok(()))
	}

	/// Poll for whether data is available on the stream: `false` once it finishes.
	pub(crate) fn poll_has_more(&mut self, waiter: &kio::Waiter) -> Poll<Result<bool, Error>> {
		let lease = ready!(self.poll_lease(waiter, 1))?;
		Poll::Ready(Ok(lease.has_remaining()))
	}

	/// Rank the stream for eviction under receive pressure. See [`RecvStream::set_priority`].
	pub fn set_priority(&mut self, priority: i64) {
		self.stream.set_priority(priority);
	}

	/// Abort the stream with the given error.
	pub fn abort(&mut self, err: impl Into<StreamError>) {
		// STOP_SENDING is a stream operation, so it carries a stream code from the
		// negotiated protocol's registry. A session code, or the other protocol's, would be
		// read against the wrong table and mean something else.
		self.stream.stop(self.version.encode_stream_code(&err.into()));
	}

	/// Cast the reader to a different version, used during version negotiation.
	pub fn with_version<V2>(self, version: V2) -> Reader<S, V2> {
		Reader {
			stream: self.stream,
			version,
			want: self.want,
			scratch: self.scratch,
			stats: self.stats,
		}
	}
}

#[cfg(test)]
mod tests {
	use std::{collections::VecDeque, sync::Arc};

	use futures::FutureExt;

	use super::*;
	use crate::StreamError;
	use crate::transport::poll::{Buffered, Chunks};

	/// Records the STOP_SENDING code so a test can assert which registry it came from.
	#[derive(Default)]
	struct StopLog {
		stops: Vec<u32>,
	}

	impl Chunks for StopLog {
		type Error = crate::lite::test_transport::SinkError;

		fn poll_read_chunk(&mut self, _: &kio::Waiter, _: usize) -> Poll<Result<Option<Bytes>, Self::Error>> {
			Poll::Ready(Ok(None))
		}

		fn stop(&mut self, code: u32) {
			self.stops.push(code);
		}
	}

	fn stop_log<V: StreamCodes>(version: V) -> Reader<Buffered<StopLog>, V> {
		Reader::new(Buffered::new(StopLog::default()), version)
	}

	/// A control message counts once, when it is consumed: never on a peek, a retry after
	/// a short read, or a failed decode, and in full when it spans chunks.
	#[test]
	fn control_counts_consumed_messages_once() {
		use crate::{
			lite,
			stats::{Message, Registry, Tier, Totals},
		};
		let registry = Registry::new(Default::default());
		let session = registry.tier(Tier::default()).session(crate::Path::from_static(""));
		let version = lite::Version::Lite07;
		let msg = lite::Probe {
			bitrate: Some(123),
			rtt: Some(4),
		};
		let bytes: &'static [u8] = msg.encode_bytes(version).unwrap().to_vec().leak();
		let size = bytes.len() as u64;
		let (head, tail) = bytes.split_at(2);
		let counted = || registry.snapshot().control();
		let probes = |n: u64| vec![(Tier::default(), Message::Probe, Totals { tx: 0, rx: size * n })];

		let mut reader = queue(&[bytes, head], version).with_stats(&session);
		let waiter = &kio::Waiter::noop();
		assert!(matches!(
			reader.poll_decode_peek_maybe::<lite::Probe>(waiter),
			Poll::Ready(Ok(Some(_)))
		));
		assert!(counted().is_empty());
		assert!(matches!(reader.poll_decode::<lite::Probe>(waiter), Poll::Ready(Ok(_))));
		assert_eq!(counted(), probes(1));

		// The second probe arrives in two chunks, so the reader parks and retries.
		assert!(reader.poll_decode::<lite::Probe>(waiter).is_pending());
		assert!(reader.poll_decode::<lite::Probe>(waiter).is_pending());
		assert_eq!(counted(), probes(1));
		reader.stream.get_mut().chunks.push_back(tail);
		assert!(matches!(reader.poll_decode::<lite::Probe>(waiter), Poll::Ready(Ok(_))));
		assert_eq!(counted(), probes(2));

		// A wrapped message counts its type too, as the sender's one encode does.
		let reply = lite::AuthReply::Error(lite::AuthError {
			code: 1,
			reason: "no".into(),
		});
		let reply: &'static [u8] = reply.encode_bytes(version).unwrap().to_vec().leak();
		reader.stream.get_mut().chunks.push_back(reply);
		assert!(matches!(
			reader.poll_decode::<lite::AuthReply>(waiter),
			Poll::Ready(Ok(_))
		));
		let auth = (
			Tier::default(),
			Message::Auth,
			Totals {
				tx: 0,
				rx: reply.len() as u64,
			},
		);
		assert_eq!(counted(), [vec![auth.clone()], probes(2)].concat());

		// A body cut short inside its declared size is malformed, and never counted.
		reader.stream.get_mut().chunks.push_back(&[1, 0x80]);
		assert!(matches!(reader.poll_decode::<lite::Probe>(waiter), Poll::Ready(Err(_))));
		assert_eq!(counted(), [vec![auth], probes(2)].concat());
	}

	/// STOP_SENDING refuses a stream, so it must carry a stream code. Reusing the session
	/// table here would have the peer decode it against the wrong registry: `Cancel` would
	/// arrive as INTERNAL_ERROR, and `Unauthorized` as DELIVERY_TIMEOUT.
	#[test]
	fn abort_stops_with_a_stream_code() {
		const VERSION: crate::lite::Version = crate::lite::Version::Lite05;

		for (err, expected) in [
			(Error::Cancel, StreamError::Cancel.to_code()),
			(Error::Lagged, StreamError::TooFarBehind.to_code()),
			(Error::Unauthorized, StreamError::Unauthorized.to_code()),
		] {
			let mut reader = stop_log(VERSION);
			reader.abort(&err);
			assert_eq!(
				reader.stream.get_ref().stops,
				vec![expected],
				"{err:?} used the wrong registry"
			);
		}

		// The two registries disagree about 0 and 1, which is what makes the mix-up silent.
		assert_ne!(StreamError::Cancel.to_code(), crate::SessionError::Cancel.to_code());
	}

	/// And the registry follows the negotiated protocol, not just the stream direction. A
	/// group dropped for being old is a moq-lite code with no moq-transport value, so
	/// the same abort has to leave a different number on each wire.
	#[test]
	fn abort_stops_with_the_negotiated_protocols_code() {
		let mut lite = stop_log(crate::lite::Version::Lite05);
		lite.abort(&Error::Old);

		let mut ietf = stop_log(crate::ietf::Version::Draft22);
		ietf.abort(&Error::Old);

		assert_eq!(lite.stream.get_ref().stops, vec![StreamError::Old.to_code()]);
		assert_eq!(ietf.stream.get_ref().stops, vec![crate::ietf::error::INTERNAL_ERROR]);
		assert_ne!(lite.stream.get_ref().stops, ietf.stream.get_ref().stops);
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
		crate::frame::Producer,
		crate::frame::Consumer,
		kio::Waiter,
		Arc<CountWaker>,
	) {
		let group = crate::group::Info { sequence: 0 }.produce();
		let mut consumer = group.consume();
		let frame = group
			.create_frame(crate::frame::Info {
				size: size as u64,
				timestamp: Some(crate::Timestamp::ZERO),
			})
			.unwrap();

		let payload = consumer.next_frame().now_or_never().unwrap().unwrap().unwrap();
		let wakes = Arc::new(CountWaker::default());
		let waiter = kio::Waiter::new(wakes.clone().into());

		(frame, payload, waiter, wakes)
	}

	/// Hands back one queued chunk per poll, then blocks: a stream whose socket has
	/// delivered a burst and has nothing more for this turn. Finishes once `fin` is set and
	/// the queue is empty.
	#[derive(Default)]
	struct Queue {
		chunks: VecDeque<&'static [u8]>,
		fin: bool,
	}

	impl Chunks for Queue {
		type Error = crate::lite::test_transport::SinkError;

		fn poll_read_chunk(&mut self, _: &kio::Waiter, max: usize) -> Poll<Result<Option<Bytes>, Self::Error>> {
			let Some(chunk) = self.chunks.pop_front() else {
				return match self.fin {
					true => Poll::Ready(Ok(None)),
					false => Poll::Pending,
				};
			};

			// The caller sizes the read, so a chunk larger than it spans two reads.
			let n = chunk.len().min(max);
			if n < chunk.len() {
				self.chunks.push_front(&chunk[n..]);
			}

			Poll::Ready(Ok(Some(Bytes::from_static(&chunk[..n]))))
		}

		fn stop(&mut self, _code: u32) {}
	}

	fn queue<V: StreamCodes>(chunks: &[&'static [u8]], version: V) -> Reader<Buffered<Queue>, V> {
		Reader::new(
			Buffered::new(Queue {
				chunks: chunks.iter().copied().collect(),
				fin: false,
			}),
			version,
		)
	}

	/// How many bytes the reader still has leased.
	fn leased<V: StreamCodes>(reader: &mut Reader<Buffered<Queue>, V>) -> usize {
		let waiter = &kio::Waiter::noop();
		match reader.stream.poll_lease(waiter, 0) {
			Poll::Ready(Ok(lease)) => lease.remaining(),
			_ => panic!("a lease of nothing never waits"),
		}
	}

	/// A message cut across chunks decodes from a copy, waiting for exactly what its
	/// prefix declared, and consumes only itself.
	#[test]
	fn a_message_across_chunks_waits_for_its_body() {
		// A size-prefixed goaway: size 3, then a 2 byte URI.
		let mut reader = queue(&[b"\x03", b"\x02a"], crate::lite::Version::Lite05);
		let waiter = &kio::Waiter::noop();
		let version = crate::lite::Version::Lite05;
		let mut goaway = |r: &mut Decoder<'_>| Ok(crate::lite::Goaway::decode(r, version)?.uri.into_owned());
		assert!(reader.poll_decode_with(waiter, &mut goaway).is_pending());
		assert_eq!(reader.want, 4, "the prefix declared the whole message");

		reader.stream.get_mut().chunks.push_back(b"bnext");
		assert!(matches!(
			reader.poll_decode_with(waiter, &mut goaway),
			Poll::Ready(Ok(uri)) if uri == "ab"
		));
		assert_eq!(leased(&mut reader), 4, "what follows the message stays leased");
		assert!(reader.scratch.is_empty() || reader.scratch.capacity() <= SCRATCH_KEEP);
	}

	/// A copy that spans chunks is freed when the reader parks for more: the lease still
	/// holds those bytes, so keeping the copy would double what a peer can make it hold.
	#[test]
	fn a_parked_decode_frees_its_copy() {
		const LEN: usize = 70_000;
		let mut head = Vec::new();
		Encoder::new(&mut head, crate::lite::Version::Lite05.into())
			.varint(LEN as u64)
			.unwrap();
		head.extend_from_slice(&[0; 10]);
		let head: &'static [u8] = head.leak();
		let body: &'static [u8] = vec![0; LEN - 10].leak();

		// Two length-prefixed fields: the first spans both chunks, the second never arrives.
		let mut reader = queue(&[head, body], crate::lite::Version::Lite05);
		let waiter = &kio::Waiter::noop();
		let res = reader.poll_with(waiter, true, |r| {
			r.bytes()?;
			r.bytes()?;
			Ok(())
		});
		assert!(res.is_pending());
		const { assert!(SCRATCH_KEEP < LEN) };
		assert!(reader.scratch.capacity() <= SCRATCH_KEEP);
		assert_eq!(
			leased(&mut reader),
			head.len() + body.len(),
			"the lease keeps the bytes"
		);
	}

	fn refuses_frame<T: for<'a> Decode<'a, V> + Debug, V: StreamCodes + Into<Form> + Copy>(
		prefix: &'static [u8],
		version: V,
		oversized: bool,
	) {
		refuses_frame_with(prefix, version, oversized, |r| T::decode(r, version).map(|_| ()));
	}

	/// [`refuses_frame`] for a message that borrows, which `decode` reads and drops.
	fn refuses_frame_with<V: StreamCodes + Into<Form> + Copy>(
		prefix: &'static [u8],
		version: V,
		oversized: bool,
		decode: impl FnMut(&mut Decoder<'_>) -> Result<(), DecodeError>,
	) {
		let mut reader = queue(&[prefix, b"following bytes"], version);
		let waiter = &kio::Waiter::noop();
		let result = reader.poll_decode_with(waiter, decode);
		if oversized {
			assert!(
				matches!(
					result,
					Poll::Ready(Err(Error::Decode(DecodeError::MessageTooLarge { .. })))
				),
				"{result:?}"
			);
		} else {
			assert!(
				matches!(result, Poll::Ready(Err(Error::Decode(DecodeError::InvalidValue)))),
				"{result:?}"
			);
		}
		assert_eq!(reader.stream.get_ref().chunks.len(), 1, "read past the complete frame");
		assert_eq!(leased(&mut reader), prefix.len());
	}

	#[test]
	fn malformed_complete_frames_are_refused() {
		use crate::{Version, ietf, lite, setup};
		refuses_frame::<lite::Setup, _>(&[0], lite::Version::Lite05, false);
		refuses_frame_with(&[0], lite::Version::Lite05, false, |r| {
			lite::Goaway::decode(r, lite::Version::Lite05).map(|_| ())
		});
		refuses_frame_with(&[0, 0], ietf::Version::Draft22, false, |r| {
			ietf::GoAway::decode(r, ietf::Version::Draft22).map(|_| ())
		});
		for version in [lite::Version::Lite01, lite::Version::Lite02] {
			refuses_frame::<setup::Client, _>(&[0x20, 0, 0], Version::Lite(version), false);
			refuses_frame::<setup::Server, _>(&[0x21, 0, 0], Version::Lite(version), false);
		}
	}

	#[test]
	fn oversized_setup_is_refused_at_the_prefix() {
		use crate::lite;
		for version in [lite::Version::Lite05, lite::Version::Lite06] {
			let mut prefix = Vec::new();
			Encoder::new(&mut prefix, version.into()).varint(65536).unwrap();
			refuses_frame::<lite::Setup, _>(prefix.leak(), version, true);
		}
	}

	#[test]
	fn setup_at_the_limit_waits_for_its_body() {
		use crate::{Version, lite, setup};
		for version in [lite::Version::Lite01, lite::Version::Lite02] {
			let mut prefix = vec![0x20];
			Encoder::new(&mut prefix, version.into()).varint(65535).unwrap();
			assert!(matches!(
				setup::Client::decode_slice(&prefix, Version::Lite(version)),
				Err(DecodeError::Short)
			));
			prefix[0] = 0x21;
			assert!(matches!(
				setup::Server::decode_slice(&prefix, Version::Lite(version)),
				Err(DecodeError::Short)
			));
		}
		for version in [lite::Version::Lite05, lite::Version::Lite06] {
			let mut prefix = Vec::new();
			Encoder::new(&mut prefix, version.into()).varint(65535).unwrap();
			assert!(matches!(
				lite::Setup::decode_slice(&prefix, version),
				Err(DecodeError::Short)
			));
		}
	}

	#[test]
	fn fragmented_setup_waits_for_its_body() {
		let mut reader = queue(&[b"\x01"], crate::lite::Version::Lite05);
		let waiter = &kio::Waiter::noop();
		assert!(reader.poll_decode::<crate::lite::Setup>(waiter).is_pending());
		reader.stream.get_mut().chunks.push_back(b"\x00");
		assert!(matches!(
			reader.poll_decode::<crate::lite::Setup>(waiter),
			Poll::Ready(Ok(_))
		));
		assert_eq!(leased(&mut reader), 0);
	}

	/// A FIN before the declared payload arrived is a wrong size, not a complete frame.
	#[test]
	fn a_fin_mid_frame_is_a_wrong_size() {
		let (mut frame, _payload, _waiter, _wakes) = parked_frame(9);
		let mut reader = queue(&[b"foo"], crate::lite::Version::Lite05);
		reader.stream.get_mut().fin = true;
		let waiter = &kio::Waiter::noop();
		assert!(matches!(
			reader.poll_read_frame(waiter, &mut frame),
			Poll::Ready(Err(Error::WrongSize))
		));
	}

	/// A consumer parked on the group cannot run until this task yields, so a burst of
	/// chunks owes exactly one wake: at the boundary, from whoever wrote them. Every
	/// other exit hands the wake to the caller's `finish` or `abort`.
	#[test]
	fn frame_payload_wakes_once_per_poll_turn() {
		let (mut frame, mut payload, waiter, wakes) = parked_frame(9);
		assert!(payload.poll_read_chunk(&waiter).is_pending());

		let mut reader = queue(&[b"foo", b"bar"], crate::lite::Version::Lite05);
		let noop = &kio::Waiter::noop();

		assert!(reader.poll_read_frame(noop, &mut frame).is_pending());
		assert_eq!(wakes.count(), 1, "two chunks in one turn owe one wake");

		let Poll::Ready(Ok(Some(chunk))) = payload.poll_read_chunk(&waiter) else {
			panic!("the boundary wake did not publish both chunks");
		};
		assert_eq!(chunk, Bytes::from_static(b"foobar"));

		// A turn that reads nothing must not touch the group at all.
		assert!(payload.poll_read_chunk(&waiter).is_pending());
		assert!(reader.poll_read_frame(noop, &mut frame).is_pending());
		assert_eq!(wakes.count(), 1, "an empty poll turn woke a consumer");

		// The tail completes the frame, so the loop returns without a wake of its own.
		// The commit is what publishes it, and the commit is the caller's to make.
		reader.stream.get_mut().chunks.push_back(b"baz");
		assert!(matches!(reader.poll_read_frame(noop, &mut frame), Poll::Ready(Ok(()))));
		assert_eq!(wakes.count(), 1, "a full frame woke before it was committed");

		let _group = frame.finish().unwrap();
		assert_eq!(wakes.count(), 2);
	}

	/// Stands in for an egress consumer on another worker: it catches up and re-parks
	/// before handing over each chunk, so every wake the drain loop issues while the
	/// burst is in flight lands on a parked waiter and is counted.
	struct Burst {
		chunks: Queue,
		payload: crate::frame::Consumer,
		waiter: kio::Waiter,
	}

	impl Chunks for Burst {
		type Error = crate::lite::test_transport::SinkError;

		fn poll_read_chunk(&mut self, waiter: &kio::Waiter, max: usize) -> Poll<Result<Option<Bytes>, Self::Error>> {
			// Start each turn with none of the last turn's registrations left.
			self.waiter = kio::Waiter::new(self.waiter.waker().clone());
			while self.payload.poll_read_chunk(&self.waiter).is_ready() {}
			self.chunks.poll_read_chunk(waiter, max)
		}

		fn stop(&mut self, _code: u32) {}
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
			Buffered::new(Burst {
				chunks: Queue {
					chunks: std::iter::repeat_n(CHUNK, chunks).collect(),
					fin: false,
				},
				payload,
				waiter,
			}),
			crate::lite::Version::Lite05,
		);
		let noop = &kio::Waiter::noop();
		assert!(reader.poll_read_frame(noop, &mut frame).is_pending());

		// Two budgets' worth, and the boundary is owed nothing by the time it arrives.
		assert_eq!(wakes.count(), 2, "the burst was withheld past its budget");
	}

	/// A leasing stream that keeps the credit of every byte read until a release, the way
	/// a QUIC connection window does, counting what is held and what came back.
	#[derive(Default)]
	struct Credited {
		chunks: VecDeque<Bytes>,
		held: usize,
		released: usize,
	}

	struct CreditedLease<'a>(&'a mut Credited);

	impl RecvStream for Credited {
		type Error = crate::lite::test_transport::SinkError;
		type Lease<'a> = CreditedLease<'a>;

		fn poll_lease(&mut self, _: &kio::Waiter, want: usize) -> Poll<Result<CreditedLease<'_>, Self::Error>> {
			match self.chunks.iter().map(Bytes::len).sum::<usize>() >= want {
				true => Poll::Ready(Ok(CreditedLease(self))),
				false => Poll::Pending,
			}
		}

		fn set_priority(&mut self, _: i64) {}

		fn stop(&mut self, _: u32) {}
	}

	impl Buf for CreditedLease<'_> {
		fn remaining(&self) -> usize {
			self.0.chunks.iter().map(Bytes::len).sum()
		}

		fn chunk(&self) -> &[u8] {
			self.0.chunks.front().map_or(&[], |chunk| &chunk[..])
		}

		fn advance(&mut self, n: usize) {
			let front = self.0.chunks.front_mut().expect("advanced past the lease");
			front.advance(n);
			if front.is_empty() {
				self.0.chunks.pop_front();
			}
			self.0.held += n;
		}
	}

	impl Lease for CreditedLease<'_> {
		fn chunks(&self) -> impl Iterator<Item = &[u8]> {
			self.0.chunks.iter().map(|chunk| &chunk[..])
		}

		fn is_fin(&self) -> bool {
			false
		}

		fn release(self) {
			self.0.released += std::mem::take(&mut self.0.held);
		}
	}

	/// A frame returns its receive credit as each segment seals, so however large it is,
	/// it never holds more than one segment of the window, even when a chunk straddles
	/// the boundary.
	#[test]
	fn a_frame_returns_credit_per_segment() {
		use crate::frame::SEGMENT;
		const SIZE: usize = 2 * SEGMENT + SEGMENT / 2;
		const CHUNK: usize = 100_000;

		let (mut frame, mut payload, waiter, _wakes) = parked_frame(SIZE);
		let data = Bytes::from((0..SIZE).map(|i| (i % 251) as u8).collect::<Vec<_>>());
		let mut reader = Reader::new(Credited::default(), crate::lite::Version::Lite05);
		let noop = &kio::Waiter::noop();

		let mut sent = 0;
		let done = loop {
			let next = (sent + CHUNK).min(SIZE);
			reader.stream.chunks.push_back(data.slice(sent..next));
			sent = next;
			let res = reader.poll_read_frame(noop, &mut frame);
			let stream = &reader.stream;
			if res.is_ready() {
				break res;
			}
			assert_eq!(stream.released, sent / SEGMENT * SEGMENT, "credit returned off a seal");
			assert!(stream.held < SEGMENT, "{} bytes held past a segment", stream.held);
		};
		assert!(matches!(done, Poll::Ready(Ok(()))));
		assert_eq!(reader.stream.released, SIZE);

		let _group = frame.finish().unwrap();
		let mut read = Vec::new();
		while let Poll::Ready(Ok(Some(chunk))) = payload.poll_read_chunk(&waiter) {
			read.extend_from_slice(&chunk);
		}
		assert_eq!(read, data);
	}
}
