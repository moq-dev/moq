//! An in-memory [`poll::Session`] pair: two peers in one process, no sockets or crypto.
//!
//! It behaves like a QUIC connection with zero latency and unlimited bandwidth. A write
//! lands in the peer's receive buffer at once, so a stream payload crosses as the same
//! [`Bytes`] it was written as, and each write is read back as one chunk (or split by a
//! smaller read). Flow control bounds what that buffer holds: a writer pends once its
//! stream, or every stream in its direction together, holds a window of unread bytes,
//! until the reader consumes some. A slow reader therefore throttles its own streams and
//! nothing else.
//!
//! Like QUIC, a reset or a stop discards what the stream still buffers, and a stream
//! whose FIN was written counts as received, so a later reset is ignored and
//! [`poll::SendStream::poll_closed`] resolves at once. Unlike QUIC, a session close keeps
//! what was already written readable: the peer reads it, then the close.
//!
//! Datagrams are copied, since [`poll::Session::poll_send_datagram`] borrows them, and
//! dropped when the peer's queue is full, as a network would.

use std::{
	collections::VecDeque,
	sync::Arc,
	task::{Context, Poll, ready},
};

use bytes::{Buf, BufMut, Bytes};

use super::{StatsUnavailable, poll};

/// The limits both directions of a [`pair`] share.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// Unread bytes one stream may hold before its writer pends.
	pub stream_window: usize,
	/// Unread bytes every stream in one direction may hold together before their writers pend.
	pub session_window: usize,
	/// Unread datagrams one side may hold before new ones are dropped.
	pub datagram_queue: usize,
	/// The largest datagram either side may send.
	pub max_datagram_size: usize,
	/// The application protocol both sides report, standing in for a negotiated ALPN.
	pub protocol: Option<String>,
}

impl Default for Config {
	/// Quinn's default windows, and a datagram the size of QUIC's minimum MTU.
	fn default() -> Self {
		Self {
			stream_window: 1_250_000,
			session_window: 10_000_000,
			datagram_queue: 1024,
			max_datagram_size: 1200,
			protocol: None,
		}
	}
}

/// Create two connected sessions: streams one opens, the other accepts.
///
/// # Panics
///
/// Panics if either window is zero, since no write could ever complete.
pub fn pair(config: Config) -> (Session, Session) {
	assert!(config.stream_window > 0, "stream window must be non-zero");
	assert!(config.session_window > 0, "session window must be non-zero");

	let conn = Arc::new(Conn {
		close: Default::default(),
		windows: Default::default(),
		incoming: [Incoming::new(config.datagram_queue), Incoming::new(config.datagram_queue)],
		config,
	});

	let side = |local| Session {
		side: Arc::new(Side {
			conn: conn.clone(),
			local,
		}),
		accept_uni: Default::default(),
		accept_bi: Default::default(),
		datagram: Default::default(),
		closed: Default::default(),
	};

	(side(0), side(1))
}

/// A transport failure: a session close, or a stream's reset or stop.
#[derive(Clone, Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// Either side closed the session, or every handle to one side dropped.
	#[error("session closed: code={code} reason={reason}")]
	Session {
		/// The application close code.
		code: u32,
		/// The application close reason.
		reason: String,
	},

	/// The peer reset the stream with this code.
	#[error("stream reset: code={0}")]
	Reset(u32),

	/// The peer stopped reading the stream with this code.
	#[error("stream stopped: code={0}")]
	Stopped(u32),

	/// This side already finished, reset, or stopped the stream.
	#[error("stream closed")]
	Closed,

	/// The datagram is larger than [`Config::max_datagram_size`].
	#[error("datagram too large")]
	TooLarge,
}

impl super::Error for Error {
	fn session_error(&self) -> Option<(u32, String)> {
		match self {
			Self::Session { code, reason } => Some((*code, reason.clone())),
			_ => None,
		}
	}

	fn stream_error(&self) -> Option<u32> {
		match self {
			Self::Reset(code) | Self::Stopped(code) => Some(*code),
			_ => None,
		}
	}
}

/// The close both sides see: set once, by whichever side closes first.
type Close = kio::Shared<Option<(u32, String)>>;

/// Ready once the session is closed.
fn is_closed(close: &kio::Ref<'_, Option<(u32, String)>>) -> Poll<()> {
	match close.is_some() {
		true => Poll::Ready(()),
		false => Poll::Pending,
	}
}

/// Poll for the session close, registering `waiter` until it happens.
fn poll_close(close: &Close, waiter: &kio::Waiter) -> Poll<Error> {
	let close = ready!(close.poll(waiter, is_closed));
	let (code, reason) = close.as_ref().expect("closed").clone();
	Poll::Ready(Error::Session { code, reason })
}

/// The session close, if it happened.
fn closed(close: &Close) -> Option<Error> {
	let close = close.read();
	let (code, reason) = close.as_ref()?.clone();
	Some(Error::Session { code, reason })
}

/// State shared by both sides of a pair.
struct Conn {
	config: Config,
	close: Close,
	/// Unread bytes across every stream flowing to each side.
	windows: [kio::Shared<usize>; 2],
	/// What each side has yet to accept.
	incoming: [Incoming; 2],
}

impl Conn {
	fn close(&self, code: u32, reason: &str) {
		let mut close = self.close.lock();
		if close.is_none() {
			*close = Some((code, reason.to_string()));
		}
	}

	/// A stream flowing to side `to`.
	fn stream(&self, to: usize) -> (SendStream, RecvStream) {
		let state = kio::Shared::<State>::default();
		let send = SendStream {
			state: state.clone(),
			window: self.windows[to].clone(),
			close: self.close.clone(),
			stream_window: self.config.stream_window,
			session_window: self.config.session_window,
			done: None,
			park: Default::default(),
		};
		let recv = RecvStream {
			state,
			window: self.windows[to].clone(),
			close: self.close.clone(),
			done: None,
			park: Default::default(),
		};
		(send, recv)
	}
}

/// Streams and datagrams one side has yet to accept.
struct Incoming {
	uni: kio::Queue<RecvStream>,
	bi: kio::Queue<(SendStream, RecvStream)>,
	datagrams: kio::Queue<Bytes>,
}

impl Incoming {
	fn new(datagrams: usize) -> Self {
		Self {
			uni: kio::Queue::new(),
			bi: kio::Queue::new(),
			datagrams: kio::Queue::bounded(datagrams),
		}
	}

	/// Drop everything queued and refuse more, once nothing can accept it.
	fn close(&self) {
		self.uni.close();
		self.bi.close();
		self.datagrams.close();
		while let Ok(Some(_)) = self.uni.try_pop() {}
		while let Ok(Some(_)) = self.bi.try_pop() {}
		while let Ok(Some(_)) = self.datagrams.try_pop() {}
	}
}

/// One end of a pair. Dropping every clone closes the session with code 0, as QUIC
/// closes an abandoned connection.
struct Side {
	conn: Arc<Conn>,
	/// This side's index; the peer is `1 - local`.
	local: usize,
}

impl Side {
	fn incoming(&self) -> &Incoming {
		&self.conn.incoming[self.local]
	}

	fn peer(&self) -> usize {
		1 - self.local
	}
}

impl Drop for Side {
	fn drop(&mut self) {
		self.conn.close(0, "");
		// Queued streams would otherwise wait for an accept that can never come.
		self.incoming().close();
	}
}

/// One side of an in-memory session, created by [`pair`].
#[derive(Clone)]
pub struct Session {
	side: Arc<Side>,
	// One park per pending-operation class: a park holds a single waiter, and a clone
	// polling two classes at once must not clobber its own registration.
	accept_uni: kio::Park,
	accept_bi: kio::Park,
	datagram: kio::Park,
	closed: kio::Park,
}

impl Session {
	fn conn(&self) -> &Conn {
		&self.side.conn
	}

	/// Pop from an incoming queue, failing with the close once it is empty.
	fn poll_accept<T>(queue: &kio::Queue<T>, close: &Close, waiter: &kio::Waiter) -> Poll<Result<T, Error>> {
		// Streams opened before a close are still handed out, like the data on them.
		if let Poll::Ready(Ok(item)) = queue.poll_pop(waiter) {
			return Poll::Ready(Ok(item));
		}
		Poll::Ready(Err(ready!(poll_close(close, waiter))))
	}
}

impl poll::Session for Session {
	type SendStream = SendStream;
	type RecvStream = RecvStream;
	type Error = Error;

	fn poll_accept_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<RecvStream, Error>> {
		let waiter = self.accept_uni.hold(cx);
		Self::poll_accept(&self.side.incoming().uni, &self.side.conn.close, waiter)
	}

	fn poll_accept_bi(&mut self, cx: &mut Context<'_>) -> Poll<Result<(SendStream, RecvStream), Error>> {
		let waiter = self.accept_bi.hold(cx);
		Self::poll_accept(&self.side.incoming().bi, &self.side.conn.close, waiter)
	}

	fn poll_open_uni(&mut self, _cx: &mut Context<'_>) -> Poll<Result<SendStream, Error>> {
		if let Some(err) = closed(&self.conn().close) {
			return Poll::Ready(Err(err));
		}
		let peer = self.side.peer();
		let (send, recv) = self.conn().stream(peer);
		match self.conn().incoming[peer].uni.try_push(recv) {
			Ok(()) => Poll::Ready(Ok(send)),
			Err(_) => Poll::Ready(Err(closed(&self.conn().close).expect("peer dropped"))),
		}
	}

	fn poll_open_bi(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(SendStream, RecvStream), Error>> {
		if let Some(err) = closed(&self.conn().close) {
			return Poll::Ready(Err(err));
		}
		let peer = self.side.peer();
		let (send, peer_recv) = self.conn().stream(peer);
		let (peer_send, recv) = self.conn().stream(self.side.local);
		match self.conn().incoming[peer].bi.try_push((peer_send, peer_recv)) {
			Ok(()) => Poll::Ready(Ok((send, recv))),
			Err(_) => Poll::Ready(Err(closed(&self.conn().close).expect("peer dropped"))),
		}
	}

	fn poll_send_datagram(&mut self, _cx: &mut Context<'_>, payload: &[u8]) -> Poll<Result<(), Error>> {
		if let Some(err) = closed(&self.conn().close) {
			return Poll::Ready(Err(err));
		}
		if payload.len() > self.conn().config.max_datagram_size {
			return Poll::Ready(Err(Error::TooLarge));
		}
		// A full queue drops the datagram, as a receiver that isn't reading would.
		let _ = self.conn().incoming[self.side.peer()]
			.datagrams
			.try_push(Bytes::copy_from_slice(payload));
		Poll::Ready(Ok(()))
	}

	fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, Error>> {
		let waiter = self.datagram.hold(cx);
		Self::poll_accept(&self.side.incoming().datagrams, &self.side.conn.close, waiter)
	}

	fn max_datagram_size(&self) -> usize {
		self.conn().config.max_datagram_size
	}

	fn protocol(&self) -> Option<&str> {
		self.conn().config.protocol.as_deref()
	}

	fn close(&mut self, code: u32, reason: &str) {
		self.conn().close(code, reason);
	}

	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Error> {
		poll_close(&self.side.conn.close, self.closed.hold(cx))
	}

	fn stats(&self) -> impl super::Stats {
		StatsUnavailable
	}
}

/// One direction of a stream, shared by its writer and reader.
#[derive(Default)]
struct State {
	/// Written and unread, each chunk as it was written.
	chunks: VecDeque<Bytes>,
	/// The bytes in `chunks`.
	held: usize,
	/// The writer finished: once `chunks` drains, the stream ends.
	fin: bool,
	/// The writer reset the stream with this code.
	reset: Option<u32>,
	/// The reader stopped the stream with this code.
	stop: Option<u32>,
}

impl State {
	/// Drop the unread chunks, returning their bytes to the session window.
	fn discard(&mut self, window: &kio::Shared<usize>) {
		if self.held > 0 {
			*window.lock() -= self.held;
			self.held = 0;
			self.chunks.clear();
		}
	}
}

/// How a stream handle ended, so later calls repeat the outcome.
#[derive(Clone, Copy)]
enum Done {
	/// Finished by the writer, or read to the end.
	Fin,
	/// Reset by this writer, or stopped by this reader.
	Local,
	/// Reset by the peer writer.
	Reset(u32),
}

/// The writing half of an in-memory stream.
pub struct SendStream {
	state: kio::Shared<State>,
	window: kio::Shared<usize>,
	close: Close,
	stream_window: usize,
	session_window: usize,
	done: Option<Done>,
	park: kio::Park,
}

impl poll::SendStream for SendStream {
	type Error = Error;

	fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Error>> {
		let mut buf = buf;
		self.poll_write_buf(cx, &mut buf)
	}

	fn poll_write_buf<B: Buf>(&mut self, cx: &mut Context<'_>, buf: &mut B) -> Poll<Result<usize, Error>> {
		if self.done.is_some() {
			return Poll::Ready(Err(Error::Closed));
		}
		if !buf.has_remaining() {
			return Poll::Ready(Ok(0));
		}

		let waiter = self.park.hold(cx);
		if let Poll::Ready(err) = poll_close(&self.close, waiter) {
			return Poll::Ready(Err(err));
		}

		let stream_window = self.stream_window;
		let mut state = ready!(
			self.state
				.poll(waiter, |state| match state.stop.is_some() || state.held < stream_window {
					true => Poll::Ready(()),
					false => Poll::Pending,
				})
		);
		if let Some(code) = state.stop {
			return Poll::Ready(Err(Error::Stopped(code)));
		}

		let session_window = self.session_window;
		let mut window = ready!(self.window.poll(waiter, |held| match **held < session_window {
			true => Poll::Ready(()),
			false => Poll::Pending,
		}));

		// Take ownership rather than copy: free for a `Bytes` source.
		let size = buf
			.remaining()
			.min(stream_window - state.held)
			.min(session_window - *window);
		let chunk = buf.copy_to_bytes(size);
		state.chunks.push_back(chunk);
		state.held += size;
		*window += size;

		Poll::Ready(Ok(size))
	}

	/// Ignored: with no link to share, there is nothing to schedule.
	fn set_priority(&mut self, _order: i32) {}

	fn finish(&mut self) -> Result<(), Error> {
		if self.done.is_some() {
			return Err(Error::Closed);
		}
		if let Some(err) = closed(&self.close) {
			return Err(err);
		}
		let mut state = self.state.lock();
		if let Some(code) = state.stop {
			return Err(Error::Stopped(code));
		}
		state.fin = true;
		self.done = Some(Done::Fin);
		Ok(())
	}

	fn reset(&mut self, code: u32) {
		// A finished stream was received in full, so a reset has nothing left to stop.
		if self.done.is_some() {
			return;
		}
		self.done = Some(Done::Local);

		let mut state = self.state.lock();
		state.reset = Some(code);
		state.discard(&self.window);
	}

	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
		if self.done.is_some() {
			return Poll::Ready(Ok(()));
		}
		let waiter = self.park.hold(cx);
		if let Poll::Ready(state) = self.state.poll(waiter, |state| match state.stop.is_some() {
			true => Poll::Ready(()),
			false => Poll::Pending,
		}) {
			return Poll::Ready(Err(Error::Stopped(state.stop.expect("stopped"))));
		}
		Poll::Ready(Err(ready!(poll_close(&self.close, waiter))))
	}
}

impl Drop for SendStream {
	/// Reset rather than finish, so a writer that went away is never read as complete.
	fn drop(&mut self) {
		poll::SendStream::reset(self, 0);
	}
}

/// The reading half of an in-memory stream.
pub struct RecvStream {
	state: kio::Shared<State>,
	window: kio::Shared<usize>,
	close: Close,
	done: Option<Done>,
	park: kio::Park,
}

impl RecvStream {
	/// The outcome repeated once the stream ended.
	fn ended(done: Done) -> Result<Option<Bytes>, Error> {
		match done {
			Done::Fin => Ok(None),
			Done::Local => Err(Error::Closed),
			Done::Reset(code) => Err(Error::Reset(code)),
		}
	}
}

impl poll::RecvStream for RecvStream {
	type Error = Error;

	fn poll_read(&mut self, cx: &mut Context<'_>, dst: &mut [u8]) -> Poll<Result<Option<usize>, Error>> {
		let Some(chunk) = ready!(self.poll_read_chunk(cx, dst.len()))? else {
			return Poll::Ready(Ok(None));
		};
		dst[..chunk.len()].copy_from_slice(&chunk);
		Poll::Ready(Ok(Some(chunk.len())))
	}

	fn poll_read_buf<B: BufMut>(&mut self, cx: &mut Context<'_>, buf: &mut B) -> Poll<Result<Option<usize>, Error>> {
		// The same cap the default uses, but without zeroing a destination first.
		let max = buf.remaining_mut().min(64 * 1024);
		let Some(chunk) = ready!(self.poll_read_chunk(cx, max))? else {
			return Poll::Ready(Ok(None));
		};
		let size = chunk.len();
		buf.put(chunk);
		Poll::Ready(Ok(Some(size)))
	}

	fn poll_read_chunk(&mut self, cx: &mut Context<'_>, max: usize) -> Poll<Result<Option<Bytes>, Error>> {
		if let Some(done) = self.done {
			return Poll::Ready(Self::ended(done));
		}
		if max == 0 {
			return Poll::Ready(Ok(Some(Bytes::new())));
		}

		let waiter = self.park.hold(cx);
		let Poll::Ready(mut state) = self.state.poll(waiter, |state| {
			match !state.chunks.is_empty() || state.fin || state.reset.is_some() {
				true => Poll::Ready(()),
				false => Poll::Pending,
			}
		}) else {
			// Everything written before a close stays readable; only then does it fail.
			return Poll::Ready(Err(ready!(poll_close(&self.close, waiter))));
		};

		if let Some(code) = state.reset {
			self.done = Some(Done::Reset(code));
			return Poll::Ready(Err(Error::Reset(code)));
		}

		let Some(front) = state.chunks.front_mut() else {
			// Finished, and every chunk read.
			self.done = Some(Done::Fin);
			return Poll::Ready(Ok(None));
		};
		let chunk = match front.len() > max {
			true => front.split_to(max),
			false => state.chunks.pop_front().expect("front exists"),
		};
		state.held -= chunk.len();
		*self.window.lock() -= chunk.len();

		Poll::Ready(Ok(Some(chunk)))
	}

	fn stop(&mut self, code: u32) {
		if self.done.is_some() {
			return;
		}
		self.done = Some(Done::Local);

		let mut state = self.state.lock();
		// A finished stream was received in full, so the writer has nothing left to stop.
		if !state.fin && state.reset.is_none() {
			state.stop = Some(code);
		}
		state.discard(&self.window);
	}

	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
		match self.done {
			Some(Done::Reset(code)) => return Poll::Ready(Err(Error::Reset(code))),
			Some(_) => return Poll::Ready(Ok(())),
			None => {}
		}
		let waiter = self.park.hold(cx);
		if let Poll::Ready(state) = self.state.poll(waiter, |state| match state.fin || state.reset.is_some() {
			true => Poll::Ready(()),
			false => Poll::Pending,
		}) {
			return Poll::Ready(match state.reset {
				Some(code) => Err(Error::Reset(code)),
				None => Ok(()),
			});
		}
		Poll::Ready(Err(ready!(poll_close(&self.close, waiter))))
	}
}

impl Drop for RecvStream {
	/// Stop, so the writer learns nobody reads and the buffered bytes free their window.
	fn drop(&mut self) {
		poll::RecvStream::stop(self, 0);
	}
}

#[cfg(test)]
mod tests {
	use std::{
		sync::atomic::{AtomicUsize, Ordering},
		task::{Wake, Waker},
	};

	use super::*;
	use poll::{RecvStream as _, SendStream as _, Session as _};

	/// Counts its wakes.
	#[derive(Default)]
	struct Count(AtomicUsize);

	impl Wake for Count {
		fn wake(self: Arc<Self>) {
			self.0.fetch_add(1, Ordering::Relaxed);
		}
	}

	fn noop() -> Context<'static> {
		Context::from_waker(Waker::noop())
	}

	fn ready<T>(poll: Poll<T>) -> T {
		match poll {
			Poll::Ready(value) => value,
			Poll::Pending => panic!("pending"),
		}
	}

	fn config(stream_window: usize, session_window: usize) -> Config {
		Config {
			stream_window,
			session_window,
			..Default::default()
		}
	}

	fn uni(a: &mut Session, b: &mut Session) -> (SendStream, RecvStream) {
		let send = ready(a.poll_open_uni(&mut noop())).unwrap();
		let recv = ready(b.poll_accept_uni(&mut noop())).unwrap();
		(send, recv)
	}

	fn write(send: &mut SendStream, chunk: &Bytes) -> Poll<Result<usize, Error>> {
		send.poll_write_buf(&mut noop(), &mut chunk.clone())
	}

	fn read(recv: &mut RecvStream, max: usize) -> Poll<Result<Option<Bytes>, Error>> {
		recv.poll_read_chunk(&mut noop(), max)
	}

	fn text(chunk: Poll<Result<Option<Bytes>, Error>>) -> Bytes {
		ready(chunk).unwrap().unwrap()
	}

	#[test]
	fn bounds() {
		fn boxable<S: poll::Boxable<SendStream: Sync, RecvStream: Sync>>() {}
		boxable::<Session>();
	}

	#[test]
	fn payload_crosses_without_copy() {
		let (mut a, mut b) = pair(Config::default());
		let (mut send, mut recv) = uni(&mut a, &mut b);

		let first = Bytes::from(vec![1u8; 4096]);
		let second = Bytes::from_static(b"second");
		assert_eq!(ready(write(&mut send, &first)).unwrap(), first.len());
		assert_eq!(ready(write(&mut send, &second)).unwrap(), second.len());

		// Each write is read back as one chunk, backed by the written allocation.
		let chunk = text(read(&mut recv, usize::MAX));
		assert_eq!(chunk.as_ptr(), first.as_ptr());
		assert_eq!(chunk.len(), first.len());

		// A smaller read splits the chunk, still without a copy.
		assert_eq!(text(read(&mut recv, 2)).as_ptr(), second.as_ptr());
		assert_eq!(text(read(&mut recv, usize::MAX)), "cond");

		send.finish().unwrap();
		assert_eq!(ready(read(&mut recv, usize::MAX)).unwrap(), None);
		assert_eq!(ready(read(&mut recv, usize::MAX)).unwrap(), None);
	}

	#[test]
	fn stream_window_pends_writer_until_read() {
		let (mut a, mut b) = pair(config(8, 1024));
		let (mut send, mut recv) = uni(&mut a, &mut b);

		// A write past the window is partial, then pends.
		let mut buf = Bytes::from_static(b"0123456789");
		assert_eq!(ready(send.poll_write_buf(&mut noop(), &mut buf)).unwrap(), 8);
		assert_eq!(buf, "89");

		let count = Arc::new(Count::default());
		let waker = Waker::from(count.clone());
		assert!(send.poll_write_buf(&mut Context::from_waker(&waker), &mut buf).is_pending());

		// Reading frees the window and wakes the writer.
		assert_eq!(text(read(&mut recv, 3)), "012");
		assert_eq!(count.0.load(Ordering::Relaxed), 1);
		assert_eq!(ready(send.poll_write_buf(&mut noop(), &mut buf)).unwrap(), 2);
	}

	#[test]
	fn session_window_spans_streams() {
		let (mut a, mut b) = pair(config(1024, 8));
		let (mut slow, mut slow_recv) = uni(&mut a, &mut b);
		let (mut fast, mut fast_recv) = uni(&mut a, &mut b);

		assert_eq!(ready(write(&mut slow, &Bytes::from_static(b"01234567"))).unwrap(), 8);
		assert!(write(&mut fast, &Bytes::from_static(b"x")).is_pending());

		// The slow stream's reader frees the direction for the other stream.
		text(read(&mut slow_recv, 4));
		assert_eq!(ready(write(&mut fast, &Bytes::from_static(b"xyz"))).unwrap(), 3);
		assert_eq!(text(read(&mut fast_recv, usize::MAX)), "xyz");

		// The other direction has its own window.
		let (mut back, mut back_recv) = uni(&mut b, &mut a);
		assert_eq!(ready(write(&mut back, &Bytes::from_static(b"01234567"))).unwrap(), 8);
		text(read(&mut back_recv, usize::MAX));
	}

	#[test]
	fn reset_discards_buffered_bytes() {
		let (mut a, mut b) = pair(config(1024, 8));
		let (mut send, mut recv) = uni(&mut a, &mut b);
		let (mut other, _other_recv) = uni(&mut a, &mut b);

		assert_eq!(ready(write(&mut send, &Bytes::from_static(b"01234567"))).unwrap(), 8);
		assert!(write(&mut other, &Bytes::from_static(b"x")).is_pending());

		send.reset(7);
		assert!(matches!(ready(read(&mut recv, usize::MAX)), Err(Error::Reset(7))));
		assert!(matches!(ready(recv.poll_closed(&mut noop())), Err(Error::Reset(7))));
		assert_eq!(ready(write(&mut other, &Bytes::from_static(b"x"))).unwrap(), 1);
		assert!(ready(send.poll_closed(&mut noop())).is_ok());
	}

	#[test]
	fn reset_after_finish_is_ignored() {
		let (mut a, mut b) = pair(Config::default());
		let (mut send, mut recv) = uni(&mut a, &mut b);

		ready(write(&mut send, &Bytes::from_static(b"done"))).unwrap();
		send.finish().unwrap();
		assert!(ready(send.poll_closed(&mut noop())).is_ok());
		send.reset(1);
		drop(send);

		assert!(ready(recv.poll_closed(&mut noop())).is_ok());
		assert_eq!(text(read(&mut recv, usize::MAX)), "done");
		assert_eq!(ready(read(&mut recv, usize::MAX)).unwrap(), None);
	}

	#[test]
	fn stop_fails_the_writer() {
		let (mut a, mut b) = pair(config(1024, 8));
		let (mut send, mut recv) = uni(&mut a, &mut b);
		let (mut other, _other_recv) = uni(&mut a, &mut b);

		assert_eq!(ready(write(&mut send, &Bytes::from_static(b"01234567"))).unwrap(), 8);
		assert!(send.poll_closed(&mut noop()).is_pending());

		recv.stop(3);
		assert!(matches!(ready(send.poll_closed(&mut noop())), Err(Error::Stopped(3))));
		assert!(matches!(
			ready(write(&mut send, &Bytes::from_static(b"x"))),
			Err(Error::Stopped(3))
		));
		assert!(matches!(send.finish(), Err(Error::Stopped(3))));
		assert!(matches!(ready(read(&mut recv, usize::MAX)), Err(Error::Closed)));

		// The stopped stream's bytes no longer count against the session.
		assert_eq!(ready(write(&mut other, &Bytes::from_static(b"x"))).unwrap(), 1);
	}

	#[test]
	fn drops_reset_and_stop() {
		let (mut a, mut b) = pair(Config::default());

		let (send, mut recv) = uni(&mut a, &mut b);
		drop(send);
		assert!(matches!(ready(read(&mut recv, usize::MAX)), Err(Error::Reset(0))));

		let (mut send, recv) = uni(&mut a, &mut b);
		drop(recv);
		assert!(matches!(ready(send.poll_closed(&mut noop())), Err(Error::Stopped(0))));
	}

	#[test]
	fn bidi_streams_pair_up() {
		let (mut a, mut b) = pair(Config::default());
		let (mut a_send, mut a_recv) = ready(a.poll_open_bi(&mut noop())).unwrap();
		let (mut b_send, mut b_recv) = ready(b.poll_accept_bi(&mut noop())).unwrap();

		ready(write(&mut a_send, &Bytes::from_static(b"ping"))).unwrap();
		assert_eq!(text(read(&mut b_recv, usize::MAX)), "ping");
		ready(write(&mut b_send, &Bytes::from_static(b"pong"))).unwrap();
		assert_eq!(text(read(&mut a_recv, usize::MAX)), "pong");
	}

	#[test]
	fn close_keeps_written_data_readable() {
		let (mut a, mut b) = pair(Config {
			protocol: Some("moq".into()),
			..Default::default()
		});
		assert_eq!(b.protocol(), Some("moq"));
		let (mut send, mut recv) = uni(&mut a, &mut b);
		let (mut unfinished, mut unfinished_recv) = uni(&mut a, &mut b);
		let late = ready(a.poll_open_uni(&mut noop())).unwrap();

		ready(write(&mut send, &Bytes::from_static(b"last"))).unwrap();
		send.finish().unwrap();
		ready(write(&mut unfinished, &Bytes::from_static(b"part"))).unwrap();
		assert!(b.poll_closed(&mut noop()).is_pending());

		a.close(4, "bye");
		let closed = |err: Error| matches!(err, Error::Session { code: 4, ref reason } if reason == "bye");
		assert!(closed(ready(b.poll_closed(&mut noop()))));
		assert!(closed(ready(a.poll_open_uni(&mut noop())).err().unwrap()));
		assert!(closed(
			ready(write(&mut unfinished, &Bytes::from_static(b"x"))).unwrap_err()
		));

		assert_eq!(text(read(&mut recv, usize::MAX)), "last");
		assert_eq!(ready(read(&mut recv, usize::MAX)).unwrap(), None);
		assert_eq!(text(read(&mut unfinished_recv, usize::MAX)), "part");
		assert!(closed(ready(read(&mut unfinished_recv, usize::MAX)).unwrap_err()));

		// A stream opened before the close is still accepted.
		drop(late);
		ready(b.poll_accept_uni(&mut noop())).unwrap();
		assert!(closed(ready(b.poll_accept_uni(&mut noop())).err().unwrap()));
	}

	#[test]
	fn dropping_a_side_closes() {
		let (a, mut b) = pair(Config::default());
		let clone = a.clone();
		drop(a);
		assert!(b.poll_closed(&mut noop()).is_pending());
		drop(clone);
		assert!(matches!(ready(b.poll_closed(&mut noop())), Error::Session { code: 0, .. }));
	}

	#[test]
	fn datagrams() {
		let (mut a, mut b) = pair(Config {
			datagram_queue: 1,
			max_datagram_size: 4,
			..Default::default()
		});
		assert!(matches!(
			ready(a.poll_send_datagram(&mut noop(), b"large")),
			Err(Error::TooLarge)
		));
		ready(a.poll_send_datagram(&mut noop(), b"one")).unwrap();
		// The peer's queue is full, so this one is lost.
		ready(a.poll_send_datagram(&mut noop(), b"two")).unwrap();
		assert_eq!(ready(b.poll_recv_datagram(&mut noop())).unwrap(), "one");
		assert!(b.poll_recv_datagram(&mut noop()).is_pending());
	}
}
