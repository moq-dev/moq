use std::{
	cmp,
	fmt::Debug,
	ops::{Deref, DerefMut},
	sync::Arc,
	task::{Context, Poll, ready},
};

use bytes::{Bytes, BytesMut};

use crate::{
	Error, StreamError,
	coding::*,
	pressure::{Erase, Hold, Holder, Lent, Locked, Pressure, Rank},
};

/// A reader for decoding messages from a stream.
///
/// The `poll_*` methods are the implementation; the `async` methods are thin
/// wrappers, so the reader can be driven from another poll function without
/// pinning a future. Partial bytes accumulate in the internal buffer, so a
/// `Pending` (or a cancelled wrapper) never desynchronizes the stream.
///
/// A [tracked](Self::track) reader holds the transport's connection credit for its
/// buffered bytes, returning each unit's as it is decoded and handed off, so read-ahead
/// stays with the next unit. Its buffer sits beside that credit in its [`Hold`], so an
/// eviction frees the memory with the credit even while the reader is parked. A partial
/// frame's payload carries its own credit in a [`HeldFrame`], which returns it when the
/// frame goes. An untracked reader returns credit as soon as it reads.
pub struct Reader<S: crate::transport::poll::RecvStream, V> {
	stream: S,
	/// The buffer until tracked, then empty: tracking moves it into the hold.
	buffer: BytesMut,
	version: V,
	/// While tracked, the buffer and its credit.
	hold: Option<Arc<Hold<S>>>,
	/// The receive allowance filled and this stream was chosen to free it.
	evicted: bool,
}

/// A chunk of a unit, with the credit lent to it if tracked.
type LentChunk<S> = (Bytes, Option<Lent<S>>);

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

impl<S: crate::transport::poll::RecvStream, V: StreamCodes> Reader<S, V> {
	pub fn new(stream: S, version: V) -> Self {
		Self {
			stream,
			buffer: Default::default(),
			version,
			hold: None,
			evicted: false,
		}
	}

	/// Hold connection credit until each unit completes, evictable by `pressure` at `rank`.
	pub(crate) fn track<H: ?Sized + Holder>(&mut self, pressure: &Pressure<H>, rank: Rank)
	where
		Hold<S>: Erase<H>,
	{
		if self.hold.is_some() {
			return self.rank(rank);
		}
		let code = self.version.encode_stream_code(&StreamError::Evicted);
		let hold = pressure.track(rank, self.stream.stopper(), code);
		hold.lock().adopt(std::mem::take(&mut self.buffer));
		self.hold = Some(hold);
	}

	/// Re-rank a tracked stream once its header identifies it.
	pub(crate) fn rank(&self, rank: Rank) {
		if let Some(hold) = &self.hold {
			hold.rank(rank);
		}
	}

	/// Whether this stream was evicted, registering `cx` to hear when it is. The eviction
	/// already stopped the stream and took its buffer and credit.
	fn poll_evicted(&mut self, cx: &Context<'_>) -> bool {
		self.evicted |= self.hold.as_ref().is_some_and(|hold| hold.poll_evicted(cx));
		self.evicted
	}

	/// The buffered bytes, for tests.
	#[cfg(test)]
	fn buffered(&mut self) -> usize {
		Buffer::of(&mut self.buffer, &self.hold).len()
	}

	/// Poll `decode` against the buffered bytes, reading more until it stops coming up
	/// short. `consume` drops what it read; otherwise the bytes stay for the next read.
	fn poll_with<T>(
		&mut self,
		cx: &mut Context<'_>,
		consume: bool,
		decode: impl Fn(&mut Decoder<'_>) -> Result<T, DecodeError>,
	) -> Poll<Result<T, Error>>
	where
		V: Into<Form> + Copy,
	{
		loop {
			let mut buffer = Buffer::of(&mut self.buffer, &self.hold);
			let mut r = Decoder::new(&buffer, self.version.into());
			let decoded = decode(&mut r);
			let used = buffer.len() - r.remaining();
			match decoded {
				Ok(value) => {
					if consume {
						buffer.consume(used);
					}
					return Poll::Ready(Ok(value));
				}
				Err(DecodeError::Short) => drop(buffer),
				Err(e) => return Poll::Ready(Err(e.into())),
			}
			// Stream closed while we still need more data.
			if !ready!(self.poll_read_more(cx))? {
				return Poll::Ready(Err(DecodeError::Short.into()));
			}
		}
	}

	/// [`Self::poll_with`], or `None` if the stream closes cleanly before any byte.
	fn poll_with_maybe<T>(
		&mut self,
		cx: &mut Context<'_>,
		consume: bool,
		decode: impl Fn(&mut Decoder<'_>) -> Result<T, DecodeError>,
	) -> Poll<Result<Option<T>, Error>>
	where
		V: Into<Form> + Copy,
	{
		if !ready!(self.poll_has_more(cx))? {
			return Poll::Ready(Ok(None));
		}

		self.poll_with(cx, consume, decode).map_ok(Some)
	}

	/// Poll for the next message on the stream.
	pub fn poll_decode<T: Decode<V> + Debug>(&mut self, cx: &mut Context<'_>) -> Poll<Result<T, Error>>
	where
		V: Into<Form> + Copy,
	{
		let version = self.version;
		self.poll_with(cx, true, |r| T::decode(r, version))
	}

	/// Decode the next message from the stream.
	pub async fn decode<T: Decode<V> + Debug>(&mut self) -> Result<T, Error>
	where
		V: Into<Form> + Copy,
	{
		std::future::poll_fn(|cx| self.poll_decode(cx)).await
	}

	/// Poll for the next message unless the stream is closed cleanly first.
	pub fn poll_decode_maybe<T: Decode<V> + Debug>(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<T>, Error>>
	where
		V: Into<Form> + Copy,
	{
		let version = self.version;
		self.poll_with_maybe(cx, true, |r| T::decode(r, version))
	}

	/// Decode the next message unless the stream is closed.
	pub async fn decode_maybe<T: Decode<V> + Debug>(&mut self) -> Result<Option<T>, Error>
	where
		V: Into<Form> + Copy,
	{
		std::future::poll_fn(|cx| self.poll_decode_maybe(cx)).await
	}

	/// Poll for the next message without consuming it unless the stream closes cleanly first.
	pub fn poll_decode_peek_maybe<T: Decode<V> + Debug>(
		&mut self,
		cx: &mut Context<'_>,
	) -> Poll<Result<Option<T>, Error>>
	where
		V: Into<Form> + Copy,
	{
		let version = self.version;
		self.poll_with_maybe(cx, false, |r| T::decode(r, version))
	}

	/// Peek the next message unless the stream is closed.
	pub async fn decode_peek_maybe<T: Decode<V> + Debug>(&mut self) -> Result<Option<T>, Error>
	where
		V: Into<Form> + Copy,
	{
		std::future::poll_fn(|cx| self.poll_decode_peek_maybe(cx)).await
	}

	/// Poll for the next varint on the stream.
	pub fn poll_varint(&mut self, cx: &mut Context<'_>) -> Poll<Result<u64, Error>>
	where
		V: Into<Form> + Copy,
	{
		self.poll_with(cx, true, |r| r.varint())
	}

	/// Read the next varint from the stream.
	pub async fn varint(&mut self) -> Result<u64, Error>
	where
		V: Into<Form> + Copy,
	{
		std::future::poll_fn(|cx| self.poll_varint(cx)).await
	}

	/// Poll for the next varint unless the stream is closed cleanly first.
	pub fn poll_varint_maybe(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<u64>, Error>>
	where
		V: Into<Form> + Copy,
	{
		self.poll_with_maybe(cx, true, |r| r.varint())
	}

	/// Read the next varint unless the stream is closed.
	pub async fn varint_maybe(&mut self) -> Result<Option<u64>, Error>
	where
		V: Into<Form> + Copy,
	{
		std::future::poll_fn(|cx| self.poll_varint_maybe(cx)).await
	}

	/// Poll for the next varint without consuming it.
	pub fn poll_varint_peek(&mut self, cx: &mut Context<'_>) -> Poll<Result<u64, Error>>
	where
		V: Into<Form> + Copy,
	{
		self.poll_with(cx, false, |r| r.varint())
	}

	/// Read the next varint without consuming it.
	pub async fn varint_peek(&mut self) -> Result<u64, Error>
	where
		V: Into<Form> + Copy,
	{
		std::future::poll_fn(|cx| self.poll_varint_peek(cx)).await
	}

	/// Poll for the next chunk, draining the reader's internal buffer first. The chunk is
	/// a complete unit: its credit returns as it is handed over.
	pub fn poll_read_chunk(&mut self, cx: &mut Context<'_>, max: usize) -> Poll<Result<Option<Bytes>, Error>> {
		let chunk = ready!(self.poll_unit_chunk(cx, max))?;
		Poll::Ready(Ok(chunk.map(|(chunk, _credit)| chunk)))
	}

	/// The next chunk of a unit, draining the buffer first, with the credit lent to it:
	/// whoever keeps the bytes keeps the credit, so an eviction frees only the buffer.
	fn poll_unit_chunk(&mut self, cx: &mut Context<'_>, max: usize) -> Poll<Result<Option<LentChunk<S>>, Error>> {
		let mut buffer = Buffer::of(&mut self.buffer, &self.hold);
		if !buffer.is_empty() {
			let n = cmp::min(buffer.len(), max);
			return Poll::Ready(Ok(Some(buffer.lend_buffered(n))));
		}
		if self.evicted || buffer.is_evicted() {
			self.evicted = true;
			return Poll::Ready(Err(Error::Evicted));
		}
		let read = match self.stream.poll_read_chunk(cx, max) {
			Poll::Pending => {
				drop(buffer);
				return match self.poll_evicted(cx) {
					true => Poll::Ready(Err(Error::Evicted)),
					false => Poll::Pending,
				};
			}
			Poll::Ready(read) => read,
		};
		let Some((chunk, credit)) = read.map_err(|err| self.version.transport_error(err))? else {
			return Poll::Ready(Ok(None));
		};
		let credit = buffer.lend(credit);
		Poll::Ready(Ok(Some((chunk, credit))))
	}

	/// Fill a frame's payload from the stream, returning `Pending` once it would block.
	///
	/// `Ready(Ok(()))` means the frame is full and the caller should commit it; an error
	/// means the caller should abort it. Either way the frame is untouched otherwise, so
	/// the caller keeps ownership of the commit/abort decision. The payload's credit moves
	/// into the frame, so it returns only as the frame goes, whichever way that is.
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
		frame: &mut HeldFrame<S>,
	) -> Poll<Result<(), Error>> {
		let mut owed = 0;

		loop {
			if frame.frame.remaining() == 0 {
				return Poll::Ready(Ok(()));
			}

			match self.poll_unit_chunk(cx, frame.frame.remaining()) {
				Poll::Pending => {
					if owed > 0 {
						frame.frame.notify();
					}
					return Poll::Pending;
				}
				Poll::Ready(Ok(Some((chunk, credit)))) if !chunk.is_empty() => {
					frame.keep(credit);
					owed += chunk.len();
					frame.frame.write(chunk)?;
					if owed >= WAKE_BUDGET {
						frame.frame.notify();
						owed = 0;
					}
				}
				// A FIN mid-payload: the declared size never arrived.
				Poll::Ready(Ok(_)) => return Poll::Ready(Err(Error::WrongSize)),
				Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
			}
		}
	}

	/// Poll for exactly `size` bytes, accumulating partial reads in the buffer.
	pub fn poll_read_exact(&mut self, cx: &mut Context<'_>, size: usize) -> Poll<Result<Bytes, Error>> {
		loop {
			let mut buffer = Buffer::of(&mut self.buffer, &self.hold);
			if buffer.len() >= size {
				return Poll::Ready(Ok(buffer.hand_off(size)));
			}
			drop(buffer);
			if !ready!(self.poll_read_more(cx))? {
				return Poll::Ready(Err(DecodeError::Short.into()));
			}
		}
	}

	/// Poll to discard `remaining` bytes, counting it down as they arrive. Nothing is
	/// buffered past what one read brings, so a peer cannot make a skipped payload cost
	/// memory.
	pub(crate) fn poll_skip(&mut self, cx: &mut Context<'_>, remaining: &mut usize) -> Poll<Result<(), Error>> {
		loop {
			let mut buffer = Buffer::of(&mut self.buffer, &self.hold);
			let skipped = buffer.len().min(*remaining);
			buffer.consume(skipped);
			drop(buffer);
			*remaining -= skipped;
			if *remaining == 0 {
				return Poll::Ready(Ok(()));
			}
			if !ready!(self.poll_read_more(cx))? {
				return Poll::Ready(Err(DecodeError::Short.into()));
			}
		}
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

	/// Poll for whether data is available in the buffer or stream: `false` once it finishes.
	pub(crate) fn poll_has_more(&mut self, cx: &mut Context<'_>) -> Poll<Result<bool, Error>> {
		if !Buffer::of(&mut self.buffer, &self.hold).is_empty() {
			return Poll::Ready(Ok(true));
		}

		self.poll_read_more(cx)
	}

	/// Poll for more data from the stream. `true` if data was read, `false` if the
	/// stream finished.
	fn poll_read_more(&mut self, cx: &mut Context<'_>) -> Poll<Result<bool, Error>> {
		let mut buffer = Buffer::of(&mut self.buffer, &self.hold);
		if self.evicted || buffer.is_evicted() {
			self.evicted = true;
			return Poll::Ready(Err(Error::Evicted));
		}
		let read = match self.stream.poll_read_buf(cx, &mut *buffer) {
			Poll::Pending => {
				drop(buffer);
				return match self.poll_evicted(cx) {
					true => Poll::Ready(Err(Error::Evicted)),
					false => Poll::Pending,
				};
			}
			Poll::Ready(read) => read,
		};
		match read {
			Ok(Some(credit)) => {
				buffer.take(credit);
				Poll::Ready(Ok(true))
			}
			Ok(None) => Poll::Ready(Ok(false)),
			Err(e) => Poll::Ready(Err(self.version.transport_error(e))),
		}
	}

	/// Abort the stream with the given error, discarding what it buffered and returning
	/// the credit it holds.
	pub fn abort(&mut self, err: impl Into<StreamError>) {
		// An evicted stream was stopped with its own code already.
		if !self.hold.as_ref().is_some_and(|hold| hold.is_evicted()) {
			// STOP_SENDING is a stream operation, so it carries a stream code from the
			// negotiated protocol's registry. A session code, or the other protocol's, would
			// be read against the wrong table and mean something else.
			self.stream.stop(self.version.encode_stream_code(&err.into()));
		}
		// Nothing buffered is worth keeping, and a reader retained after its abort must not
		// keep memory whose credit returned. A partial frame's credit is its own, returned
		// as its owner aborts it.
		self.buffer = BytesMut::new();
		if let Some(hold) = &self.hold {
			hold.clear();
		}
	}

	/// Cast the reader to a different version, used during version negotiation.
	pub fn with_version<V2>(self, version: V2) -> Reader<S, V2> {
		Reader {
			stream: self.stream,
			buffer: self.buffer,
			version,
			hold: self.hold,
			evicted: self.evicted,
		}
	}
}

/// A reader's buffered bytes: its own while untracked, else locked in its hold beside
/// their credit, so no eviction runs between a check and a read.
enum Buffer<'a, S: crate::transport::poll::RecvStream> {
	Own(&'a mut BytesMut),
	Held(Locked<'a, S>),
}

impl<'a, S: crate::transport::poll::RecvStream> Buffer<'a, S> {
	fn of(own: &'a mut BytesMut, hold: &'a Option<Arc<Hold<S>>>) -> Self {
		match hold {
			Some(hold) => Self::Held(hold.lock()),
			None => Self::Own(own),
		}
	}

	/// Whether an eviction emptied the buffer, after which nothing may be read into it.
	fn is_evicted(&self) -> bool {
		matches!(self, Self::Held(held) if held.is_evicted())
	}

	/// Keep the credit for bytes just read into the buffer, if tracked; otherwise it
	/// returns now.
	fn take(&mut self, credit: S::Credit) {
		if let Self::Held(held) = self {
			held.take(credit);
		}
	}

	/// Discard the first `bytes`, a unit consumed whole, then return their credit.
	fn consume(&mut self, bytes: usize) {
		match self {
			Self::Own(buffer) => bytes::Buf::advance(buffer, bytes),
			Self::Held(held) => held.consume(bytes),
		}
	}

	/// Hand off the first `bytes` as a complete unit, whose credit returns as it leaves.
	fn hand_off(&mut self, bytes: usize) -> Bytes {
		match self {
			Self::Own(buffer) => buffer.split_to(bytes).freeze(),
			Self::Held(held) => held.hand_off(bytes),
		}
	}

	/// Move the first `bytes` into a partial unit, with their credit if tracked.
	fn lend_buffered(&mut self, bytes: usize) -> LentChunk<S> {
		match self {
			Self::Own(buffer) => (buffer.split_to(bytes).freeze(), None),
			Self::Held(held) => held.lend_buffered(bytes),
		}
	}

	/// The credit for bytes read straight into a partial unit, if tracked; otherwise it
	/// returns now.
	fn lend(&self, credit: S::Credit) -> Option<Lent<S>> {
		match self {
			Self::Own(_) => None,
			Self::Held(held) => Some(held.lend(credit)),
		}
	}
}

/// A frame filled from a stream by [`Reader::poll_read_frame`], with the connection credit
/// for the payload it holds so far. The credit is the frame's own, so it returns only as
/// the frame finishes, aborts or drops, on every exit alike.
pub(crate) struct HeldFrame<S: crate::transport::poll::RecvStream> {
	/// Declared before `credit`: fields drop in order, so a dropped frame frees its payload
	/// before the credit returns and lets the peer send more.
	frame: crate::frame::ProducerOwned,
	credit: Option<Lent<S>>,
}

impl<S: crate::transport::poll::RecvStream> HeldFrame<S> {
	pub fn new(frame: crate::frame::ProducerOwned) -> Self {
		Self { frame, credit: None }
	}

	/// Keep the credit for payload just written into the frame.
	fn keep(&mut self, credit: Option<Lent<S>>) {
		match (&mut self.credit, credit) {
			(Some(held), Some(credit)) => held.merge(credit),
			(held, credit @ Some(_)) => *held = credit,
			(_, None) => {}
		}
	}

	/// Publish the complete frame, returning its credit once it is handed off.
	pub fn finish(self) -> crate::Result<()> {
		// `_credit` lives to the end of the scope, past the frame `finish` consumes.
		let Self { frame, credit: _credit } = self;
		frame.finish()
	}

	/// Fail the frame with `err`, freeing its payload before its credit returns.
	pub fn abort(self, err: Error) -> crate::Result<()> {
		let Self { frame, credit: _credit } = self;
		frame.abort(err)
	}
}

impl<S: crate::transport::poll::RecvStream> Deref for Buffer<'_, S> {
	type Target = BytesMut;

	fn deref(&self) -> &BytesMut {
		match self {
			Self::Own(buffer) => buffer,
			Self::Held(held) => held.buffer(),
		}
	}
}

impl<S: crate::transport::poll::RecvStream> DerefMut for Buffer<'_, S> {
	fn deref_mut(&mut self) -> &mut BytesMut {
		match self {
			Self::Own(buffer) => buffer,
			Self::Held(held) => held.buffer_mut(),
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

	impl crate::transport::poll::RecvStream for StopLog {
		type Error = crate::lite::test_transport::SinkError;
		type Credit = crate::transport::Unheld;
		type Stopper = ();
		fn stopper(&self) {}

		fn poll_read(
			&mut self,
			_cx: &mut std::task::Context<'_>,
			_dst: &mut [u8],
		) -> std::task::Poll<Result<Option<Self::Credit>, Self::Error>> {
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
	/// arrive as INTERNAL_ERROR, and `Unauthorized` as DELIVERY_TIMEOUT.
	#[test]
	fn abort_stops_with_a_stream_code() {
		const VERSION: crate::lite::Version = crate::lite::Version::Lite05;

		for (err, expected) in [
			(Error::Cancel, StreamError::Cancel.to_code()),
			(Error::Lagged, StreamError::TooFarBehind.to_code()),
			(Error::Unauthorized, StreamError::Unauthorized.to_code()),
		] {
			let mut reader = Reader::new(StopLog::default(), VERSION);
			reader.abort(&err);
			assert_eq!(reader.stream.stops, vec![expected], "{err:?} used the wrong registry");
		}

		// The two registries disagree about 0 and 1, which is what makes the mix-up silent.
		assert_ne!(StreamError::Cancel.to_code(), crate::SessionError::Cancel.to_code());
	}

	/// And the registry follows the negotiated protocol, not just the stream direction. A
	/// group dropped for being old is a moq-lite code with no moq-transport value, so
	/// the same abort has to leave a different number on each wire.
	#[test]
	fn abort_stops_with_the_negotiated_protocols_code() {
		let mut lite = Reader::new(StopLog::default(), crate::lite::Version::Lite05);
		lite.abort(&Error::Old);

		let mut ietf = Reader::new(StopLog::default(), crate::ietf::Version::Draft20);
		ietf.abort(&Error::Old);

		assert_eq!(lite.stream.stops, vec![StreamError::Old.to_code()]);
		assert_eq!(ietf.stream.stops, vec![crate::ietf::error::INTERNAL_ERROR]);
		assert_ne!(lite.stream.stops, ietf.stream.stops);
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
	fn parked_frame<S: crate::transport::poll::RecvStream>(
		size: usize,
	) -> (HeldFrame<S>, crate::frame::Consumer, kio::Waiter, Arc<CountWaker>) {
		let mut group = crate::group::Info { sequence: 0 }.produce();
		let mut consumer = group.consume();
		let frame = group
			.create_frame_owned(
				crate::frame::Info {
					size: size as u64,
					timestamp: Some(crate::Timestamp::ZERO),
				},
				&Default::default(),
			)
			.unwrap();

		let payload = consumer.next_frame().now_or_never().unwrap().unwrap().unwrap();
		let wakes = Arc::new(CountWaker::default());
		let waiter = kio::Waiter::new(wakes.clone().into());

		(HeldFrame::new(frame), payload, waiter, wakes)
	}

	/// Hands back one queued chunk per poll, then blocks: a stream whose socket has
	/// delivered a burst and has nothing more for this turn.
	#[derive(Default)]
	struct Chunks(std::collections::VecDeque<&'static [u8]>);

	impl crate::transport::poll::RecvStream for Chunks {
		type Error = crate::lite::test_transport::SinkError;
		type Credit = crate::transport::Unheld;
		type Stopper = ();
		fn stopper(&self) {}

		fn poll_read(
			&mut self,
			_cx: &mut Context<'_>,
			dst: &mut [u8],
		) -> Poll<Result<Option<Self::Credit>, Self::Error>> {
			let Some(chunk) = self.0.pop_front() else {
				return Poll::Pending;
			};

			// The caller sizes `dst`, so a chunk larger than it spans two reads.
			let n = chunk.len().min(dst.len());
			dst[..n].copy_from_slice(&chunk[..n]);
			if n < chunk.len() {
				self.0.push_front(&chunk[n..]);
			}

			Poll::Ready(Ok(Some(crate::transport::Unheld(n as u64))))
		}

		fn stop(&mut self, _code: u32) {}

		fn poll_closed(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
			Poll::Pending
		}
	}

	/// A skipped payload is discarded across reads, leaving what follows it, and a skip
	/// waiting on more has counted down what it already discarded.
	#[test]
	fn skip_discards_across_reads() {
		let chunks = [b"abc".as_slice(), b"defg", b"hi"];
		let mut reader = Reader::new(Chunks(chunks.into()), crate::lite::Version::Lite05);
		let mut cx = Context::from_waker(std::task::Waker::noop());

		let mut remaining = 6;
		assert!(matches!(reader.poll_skip(&mut cx, &mut remaining), Poll::Ready(Ok(()))));
		assert_eq!(&reader.read_exact(3).now_or_never().unwrap().unwrap()[..], b"ghi");

		let mut reader = Reader::new(Chunks([b"ab".as_slice(), b"cd"].into()), crate::lite::Version::Lite05);
		let mut remaining = 5;
		assert!(reader.poll_skip(&mut cx, &mut remaining).is_pending());
		assert_eq!(remaining, 1);
		assert_eq!(reader.buffered(), 0, "nothing skipped is kept");
	}

	fn refuses_frame<T: Decode<V> + Debug, V: StreamCodes + Into<Form> + Copy>(
		prefix: &[u8],
		version: V,
		oversized: bool,
	) {
		let mut reader = Reader::new(Chunks([b"following bytes".as_slice()].into()), version);
		reader.buffer.extend_from_slice(prefix);
		let mut cx = Context::from_waker(std::task::Waker::noop());
		let result = reader.poll_decode::<T>(&mut cx);
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
		assert_eq!(reader.buffered(), prefix.len());
		assert_eq!(reader.stream.0.len(), 1, "read past the complete frame");
	}

	#[test]
	fn malformed_complete_frames_are_refused() {
		use crate::{Version, ietf, lite, setup};
		refuses_frame::<lite::Setup, _>(&[0], lite::Version::Lite05, false);
		refuses_frame::<lite::Goaway, _>(&[0], lite::Version::Lite05, false);
		refuses_frame::<ietf::GoAway, _>(&[0, 0], ietf::Version::Draft20, false);
		for version in [
			Version::Lite(lite::Version::Lite01),
			Version::Lite(lite::Version::Lite02),
			Version::Ietf(ietf::Version::Draft14),
		] {
			let prefix = if version.is_lite() {
				&[0x20, 0][..]
			} else {
				&[0x20, 0, 0][..]
			};
			refuses_frame::<setup::Client, _>(prefix, version, false);
			let prefix = if version.is_lite() {
				&[0x21, 0][..]
			} else {
				&[0x21, 0, 0][..]
			};
			refuses_frame::<setup::Server, _>(prefix, version, false);
		}
	}

	#[test]
	fn oversized_setup_is_refused_at_the_prefix() {
		use crate::{Version, lite, setup};
		for version in [lite::Version::Lite01, lite::Version::Lite02] {
			for size in [65536u64, (1 << 40) + 1] {
				let mut prefix = vec![0x20];
				Encoder::new(&mut prefix, version.into()).varint(size).unwrap();
				refuses_frame::<setup::Client, _>(&prefix, Version::Lite(version), true);
				prefix[0] = 0x21;
				refuses_frame::<setup::Server, _>(&prefix, Version::Lite(version), true);
			}
		}
		for version in [lite::Version::Lite05, lite::Version::Lite06] {
			let mut prefix = Vec::new();
			Encoder::new(&mut prefix, version.into()).varint(65536).unwrap();
			refuses_frame::<lite::Setup, _>(&prefix, version, true);
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
		let mut reader = Reader::new(Chunks([b"\x01".as_slice()].into()), crate::lite::Version::Lite05);
		let mut cx = Context::from_waker(std::task::Waker::noop());
		assert!(reader.poll_decode::<crate::lite::Setup>(&mut cx).is_pending());
		reader.stream.0.push_back(b"\x00");
		assert!(matches!(
			reader.poll_decode::<crate::lite::Setup>(&mut cx),
			Poll::Ready(Ok(_))
		));
		assert_eq!(reader.buffered(), 0);
	}

	/// A consumer parked on the group cannot run until this task yields, so a burst of
	/// chunks owes exactly one wake: at the boundary, from whoever wrote them. Every
	/// other exit hands the wake to the caller's `finish` or `abort`.
	#[test]
	fn frame_payload_wakes_once_per_poll_turn() {
		let (mut frame, mut payload, waiter, wakes) = parked_frame(9);
		assert!(payload.poll_read_chunk(&waiter).is_pending());

		let mut reader = Reader::new(Chunks([b"foo".as_slice(), b"bar"].into()), crate::lite::Version::Lite05);
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

	impl crate::transport::poll::RecvStream for Burst {
		type Error = crate::lite::test_transport::SinkError;
		type Credit = crate::transport::Unheld;
		type Stopper = ();
		fn stopper(&self) {}

		fn poll_read(
			&mut self,
			cx: &mut Context<'_>,
			dst: &mut [u8],
		) -> Poll<Result<Option<Self::Credit>, Self::Error>> {
			// Start each turn with none of the last turn's registrations left.
			self.waiter = kio::Waiter::new(self.waiter.waker().clone());
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
			crate::lite::Version::Lite05,
		);
		let mut cx = Context::from_waker(std::task::Waker::noop());
		assert!(reader.poll_read_frame(&mut cx, &mut frame).is_pending());

		// Two budgets' worth, and the boundary is owed nothing by the time it arrives.
		assert_eq!(wakes.count(), 2, "the burst was withheld past its budget");
	}

	mod credit {
		use std::task::Waker;

		use super::*;
		use crate::lite::Version;
		use crate::transport::{
			memory,
			poll::{SendStream as _, Session as _},
		};

		fn noop() -> Context<'static> {
			Context::from_waker(Waker::noop())
		}

		/// A registry of memory streams.
		type Tracked = Pressure<Hold<memory::RecvStream>>;

		/// A memory stream whose writer stays open, read by a reader the returned pressure
		/// tracks. `window` bounds what the reader's side accepts.
		fn tracked(
			window: Option<usize>,
		) -> (
			memory::SendStream,
			Reader<memory::RecvStream, Version>,
			Tracked,
			(memory::Session, memory::Session),
		) {
			let (mut a, mut b) = memory::pair(memory::Config {
				receive_window: window,
				..Default::default()
			});
			let Poll::Ready(Ok(send)) = a.poll_open_uni(&mut noop()) else {
				panic!("open")
			};
			let Poll::Ready(Ok(recv)) = b.poll_accept_uni(&mut noop()) else {
				panic!("accept")
			};
			let pressure = Tracked::default();
			let mut reader = Reader::new(recv, Version::Lite05);
			reader.track(&pressure, Rank::Unidentified);
			(send, reader, pressure, (a, b))
		}

		fn varint(poll: Poll<Result<u64, Error>>) -> u64 {
			match poll {
				Poll::Ready(Ok(value)) => value,
				other => panic!("{other:?}"),
			}
		}

		fn write(send: &mut memory::SendStream, bytes: &'static [u8]) -> Poll<usize> {
			send.poll_write(&mut noop(), bytes).map(|res| res.expect("write"))
		}

		/// An incomplete unit keeps its credit, and the bytes read past a unit stay held
		/// for the next one.
		#[test]
		fn credit_returns_per_complete_unit() {
			let (mut send, mut reader, pressure, _sessions) = tracked(None);

			// The first byte of a two-byte varint.
			assert_eq!(write(&mut send, &[0x40]), Poll::Ready(1));
			assert!(reader.poll_varint(&mut noop()).is_pending());
			assert_eq!(pressure.held(), 1);

			// The rest of it, plus a second varint read ahead in the same chunk.
			assert_eq!(write(&mut send, &[0x05, 0x07]), Poll::Ready(2));
			assert_eq!(varint(reader.poll_varint(&mut noop())), 5);
			assert_eq!(pressure.held(), 1);
			assert_eq!(varint(reader.poll_varint(&mut noop())), 7);
			assert_eq!(pressure.held(), 0);
		}

		/// A peek holds what it looked at until the unit is consumed.
		#[test]
		fn a_peek_keeps_the_credit() {
			let (mut send, mut reader, pressure, _sessions) = tracked(None);
			assert_eq!(write(&mut send, &[0x07, 0x00, 0x01]), Poll::Ready(3));
			assert_eq!(varint(reader.poll_varint_peek(&mut noop())), 7);
			assert_eq!(pressure.held(), 3);
			let bytes = reader.poll_read_exact(&mut noop(), 2);
			assert!(matches!(bytes, Poll::Ready(Ok(bytes)) if bytes.as_ref() == [0x07, 0x00]));
			assert_eq!(pressure.held(), 1);
		}

		/// Bytes buffered before tracking began already returned their credit.
		#[test]
		fn tracking_starts_from_what_is_buffered() {
			let (mut a, mut b) = memory::pair(memory::Config::default());
			let Poll::Ready(Ok(mut send)) = a.poll_open_uni(&mut noop()) else {
				panic!("open")
			};
			let Poll::Ready(Ok(recv)) = b.poll_accept_uni(&mut noop()) else {
				panic!("accept")
			};
			let mut reader = Reader::new(recv, Version::Lite05);
			let _sessions = (a, b);

			assert_eq!(write(&mut send, &[0x07, 0x40]), Poll::Ready(2));
			assert_eq!(varint(reader.poll_varint(&mut noop())), 7);
			let pressure = Tracked::default();
			reader.track(&pressure, Rank::Unidentified);
			assert_eq!(pressure.held(), 0);

			assert_eq!(write(&mut send, &[0x09, 0x01]), Poll::Ready(2));
			assert_eq!(varint(reader.poll_varint(&mut noop())), 9);
			assert_eq!(pressure.held(), 1);
		}

		/// An untracked reader returns credit as it reads, so an incomplete unit never
		/// stalls the peer.
		#[test]
		fn an_untracked_reader_holds_nothing() {
			let (mut a, mut b) = memory::pair(memory::Config {
				receive_window: Some(2),
				..Default::default()
			});
			let Poll::Ready(Ok(mut send)) = a.poll_open_uni(&mut noop()) else {
				panic!("open")
			};
			let Poll::Ready(Ok(recv)) = b.poll_accept_uni(&mut noop()) else {
				panic!("accept")
			};
			let mut reader = Reader::new(recv, Version::Lite05);
			let _sessions = (a, b);

			// An eight-byte varint, two bytes at a time.
			assert_eq!(write(&mut send, &[0xc0, 0x00]), Poll::Ready(2));
			assert!(reader.poll_varint(&mut noop()).is_pending());
			assert_eq!(write(&mut send, &[0x00, 0x00]), Poll::Ready(2));
		}

		/// A tracked reader's incomplete unit spends the window until it completes.
		#[test]
		fn a_tracked_reader_holds_the_window() {
			let (mut send, mut reader, _pressure, _sessions) = tracked(Some(2));
			assert_eq!(write(&mut send, &[0xc0, 0x00]), Poll::Ready(2));
			assert!(reader.poll_varint(&mut noop()).is_pending());
			assert!(write(&mut send, &[0x00]).is_pending());
		}

		/// Aborting lets go of the credit and of what was buffered.
		#[test]
		fn abort_returns_the_credit() {
			let (mut send, mut reader, pressure, _sessions) = tracked(None);
			assert_eq!(write(&mut send, &[0x07, 0x40]), Poll::Ready(2));
			assert!(reader.poll_has_more(&mut noop()).is_ready());
			assert_eq!(pressure.held(), 2);
			reader.abort(&Error::Cancel);
			assert_eq!(pressure.held(), 0);
			assert_eq!(reader.buffered(), 0);
		}

		/// Hands out `max`-byte reads of a looping buffer of units, so only the reader costs.
		struct Endless {
			data: Bytes,
			pos: usize,
			max: usize,
		}

		impl crate::transport::poll::RecvStream for Endless {
			type Error = crate::lite::test_transport::SinkError;
			type Credit = crate::transport::Unheld;
			type Stopper = ();
			fn stopper(&self) {}

			fn poll_read(
				&mut self,
				_: &mut Context<'_>,
				dst: &mut [u8],
			) -> Poll<Result<Option<Self::Credit>, Self::Error>> {
				let n = dst.len().min(self.max).min(self.data.len() - self.pos);
				dst[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
				self.pos = (self.pos + n) % self.data.len();
				Poll::Ready(Ok(Some(crate::transport::Unheld(n as u64))))
			}

			fn stop(&mut self, _code: u32) {}

			fn poll_closed(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
				Poll::Pending
			}
		}

		/// Decode size-prefixed units, untracked and tracked, as messages and as frames: the
		/// per-read and per-unit cost of holding credit in the reader. Run by nightly.
		#[test]
		#[ignore = "release benchmark, run by nightly"]
		fn benchmark_tracked_reads() {
			const UNIT: usize = 1000;
			const UNITS: usize = 1 << 16;
			const RUNS: usize = 5;
			let mut unit = Vec::new();
			Encoder::new(&mut unit, Version::Lite05.into())
				.varint(UNIT as u64)
				.unwrap();
			unit.resize(unit.len() + UNIT, 7);
			let data = Bytes::from(unit.repeat(1024));

			println!("max_read,untracked_ns_per_unit,tracked_ns_per_unit,untracked_ns_per_frame,tracked_ns_per_frame");
			for max in [1200usize, 16 * 1024, 64 * 1024] {
				let mut ns = [f64::MAX; 4];
				for _ in 0..RUNS {
					let modes = [(false, false), (true, false), (false, true), (true, true)];
					for (mode, (tracked, framed)) in modes.into_iter().enumerate() {
						let stream = Endless {
							data: data.clone(),
							pos: 0,
							max,
						};
						let mut reader = Reader::new(stream, Version::Lite05);
						let pressure = Pressure::<Hold<Endless>>::default();
						if tracked {
							reader.track(&pressure, Rank::Unidentified);
						}
						let mut group = crate::group::Info { sequence: 0 }.produce();
						let start = std::time::Instant::now();
						for i in 0..UNITS {
							let size = varint(reader.poll_varint(&mut noop()));
							if !framed {
								let Poll::Ready(Ok(payload)) = reader.poll_read_exact(&mut noop(), size as usize)
								else {
									panic!("short unit");
								};
								std::hint::black_box(payload);
								continue;
							}
							// A fresh group now and then, so the cache stays small.
							if i % 64 == 0 {
								group = crate::group::Info { sequence: i as u64 }.produce();
							}
							let info = crate::frame::Info {
								size,
								timestamp: Some(crate::Timestamp::ZERO),
							};
							let frame = group.create_frame_owned(info, &Default::default()).unwrap();
							let mut frame = HeldFrame::new(frame);
							let Poll::Ready(Ok(())) = reader.poll_read_frame(&mut noop(), &mut frame) else {
								panic!("short frame");
							};
							frame.finish().unwrap();
						}
						ns[mode] = ns[mode].min(start.elapsed().as_nanos() as f64 / UNITS as f64);
					}
				}
				println!("{max},{:.1},{:.1},{:.1},{:.1}", ns[0], ns[1], ns[2], ns[3]);
			}
		}

		/// An evicted reader stops its stream and fails its next read.
		#[test]
		fn eviction_stops_the_stream() {
			let (mut send, mut reader, pressure, _sessions) = tracked(None);
			assert_eq!(write(&mut send, &[0x40]), Poll::Ready(1));
			assert!(reader.poll_varint(&mut noop()).is_pending());

			assert!(pressure.evict());
			assert!(matches!(
				reader.poll_varint(&mut noop()),
				Poll::Ready(Err(Error::Evicted))
			));
			assert_eq!(pressure.held(), 0);
			assert!(matches!(
				send.poll_write(&mut noop(), &[0x05]),
				Poll::Ready(Err(memory::Error::Stopped(_)))
			));
		}

		/// An evicted reader mid-frame keeps the credit for the payload already in its
		/// partial frame, which is still allocated, until it gives the frame up.
		#[test]
		fn a_partial_frame_keeps_its_credit_through_eviction() {
			let (mut send, mut reader, pressure, _sessions) = tracked(None);
			let (mut frame, _payload, _waiter, _wakes) = parked_frame(9);

			// Six bytes read ahead into the buffer, then moved into the frame, and two more
			// read straight into it.
			assert_eq!(write(&mut send, b"abcdef"), Poll::Ready(6));
			assert!(reader.poll_has_more(&mut noop()).is_ready());
			assert!(reader.poll_read_frame(&mut noop(), &mut frame).is_pending());
			assert_eq!(write(&mut send, b"gh"), Poll::Ready(2));
			assert!(reader.poll_read_frame(&mut noop(), &mut frame).is_pending());
			assert_eq!(pressure.held(), 8);

			assert!(pressure.evict());
			assert_eq!(
				pressure.held(),
				8,
				"the partial frame's credit returned while it was allocated"
			);
			assert!(matches!(
				reader.poll_read_frame(&mut noop(), &mut frame),
				Poll::Ready(Err(Error::Evicted))
			));
			reader.abort(&Error::Evicted);
			assert_eq!(pressure.held(), 8, "the reader returned its frame's credit");
			frame.abort(Error::Evicted).unwrap();
			assert_eq!(pressure.held(), 0);
		}

		/// A FIN mid-payload fails the frame, whose credit returns only once its owner drops
		/// it: the payload is still allocated until then.
		#[test]
		fn a_truncated_frame_keeps_its_credit_until_dropped() {
			let (mut send, mut reader, pressure, _sessions) = tracked(None);
			let (mut frame, _payload, _waiter, _wakes) = parked_frame(9);

			assert_eq!(write(&mut send, b"abc"), Poll::Ready(3));
			assert!(reader.poll_read_frame(&mut noop(), &mut frame).is_pending());
			send.finish().expect("finish");
			assert!(matches!(
				reader.poll_read_frame(&mut noop(), &mut frame),
				Poll::Ready(Err(Error::WrongSize))
			));
			assert_eq!(pressure.held(), 3, "the credit returned before its frame dropped");
			drop(frame);
			assert_eq!(pressure.held(), 0);
		}

		/// Parked readers filled and evicted in turn never buffer more than the window. An
		/// eviction that returned the credit but left a parked reader its read-ahead would
		/// let the peer refill the window while every evicted reader kept its bytes.
		#[test]
		fn evicted_parked_readers_free_their_read_ahead() {
			const WINDOW: usize = 16;
			let (mut a, mut b) = memory::pair(memory::Config {
				receive_window: Some(WINDOW),
				..Default::default()
			});
			let pressure = Tracked::default();
			// One message, then the start of the next, which the reader keeps as read-ahead.
			const MESSAGE: &[u8] = &[0x07, 0x40, 0x01, 0x40, 0x02, 0x40, 0x03, 0x40];
			let mut parked = Vec::new();
			for _ in 0..64 {
				let Poll::Ready(Ok(mut send)) = a.poll_open_uni(&mut noop()) else {
					panic!("open")
				};
				let mut rest = MESSAGE;
				while !rest.is_empty() {
					match write(&mut send, rest) {
						Poll::Ready(n) => rest = &rest[n..],
						// Full of parked read-ahead: evict one, as the monitor would.
						Poll::Pending => assert!(pressure.evict(), "a full window has a holder"),
					}
				}
				let Poll::Ready(Ok(recv)) = b.poll_accept_uni(&mut noop()) else {
					panic!("accept")
				};
				let mut reader = Reader::new(recv, Version::Lite05);
				reader.track(&pressure, Rank::Unidentified);
				assert_eq!(varint(reader.poll_varint(&mut noop())), 7);
				parked.push((send, reader));

				let buffered: usize = parked.iter_mut().map(|(_, reader)| reader.buffered()).sum();
				assert!(
					buffered <= WINDOW,
					"{buffered} bytes buffered past a {WINDOW}-byte window"
				);
			}
		}
	}
}
