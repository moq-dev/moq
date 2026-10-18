//! An in-memory [`poll::Session`] pair: two peers in one process, no sockets or crypto.
//!
//! Each stream is a channel of [`Bytes`] messages: a write queues one message and a read
//! hands it back, the same allocation, so a payload crosses without a copy. A read with a
//! smaller limit splits the message, still without a copy.
//!
//! Each stream's channel is bounded by [`Config::stream_buffer`] queued bytes, and a write
//! pends while it is full, so a slow reader throttles only its own stream. A write is
//! never split: one accepted while the channel has room may overshoot the bound.
//!
//! [`finish`](poll::SendStream::finish) closes the channel cleanly once its messages are
//! read. A [`reset`](poll::SendStream::reset) or [`stop`](poll::RecvStream::stop) closes it
//! with a code and drops what it queued. Closing the session keeps queued messages
//! readable, and leaves streams as they were, so the peer reads them, then the close.
//!
//! Datagrams ignore their send options, and are dropped when oversized or when the
//! peer's queue is full, as a network would.

use std::{
	collections::VecDeque,
	sync::Arc,
	task::{Context, Poll, ready},
};

use bytes::{Buf, BufMut, Bytes};

use super::{StatsUnavailable, poll};

/// The largest datagram either side sends: QUIC's minimum MTU, so a payload that fits
/// here fits any QUIC path.
const MAX_DATAGRAM_SIZE: usize = 1200;

/// Unread datagrams one side holds before new ones are dropped.
const DATAGRAM_QUEUE: usize = 1024;

/// The options both sides of a [`pair`] share.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// Queued bytes one stream holds before its writer pends. An empty stream always takes
	/// one write, so 0 holds one message at a time.
	pub stream_buffer: usize,
	/// The application protocol both sides report, standing in for a negotiated ALPN.
	pub protocol: Option<String>,
}

impl Default for Config {
	/// A 1 MiB stream buffer: about a QUIC stream's receive window (quinn's is 1.25 MB), so
	/// a writer pends about where it would against a zero-latency QUIC peer.
	fn default() -> Self {
		Self {
			stream_buffer: 1024 * 1024,
			protocol: None,
		}
	}
}

/// Create two connected sessions: streams one opens, the other accepts.
pub fn pair(config: Config) -> (Session, Session) {
	let conn = Arc::new(Conn {
		close: Default::default(),
		incoming: [Incoming::default(), Incoming::default()],
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

/// Poll for the session close, registering `waiter` until it happens.
fn poll_close(close: &Close, waiter: &kio::Waiter) -> Poll<Error> {
	let close = ready!(close.poll(waiter, |close| match close.is_some() {
		true => Poll::Ready(()),
		false => Poll::Pending,
	}));
	let (code, reason) = close.as_ref().expect("closed").clone();
	Poll::Ready(Error::Session { code, reason })
}

/// The session close, if it happened.
fn closed(close: &Close) -> Option<Error> {
	let (code, reason) = close.read().as_ref()?.clone();
	Some(Error::Session { code, reason })
}

/// State shared by both sides of a pair.
struct Conn {
	config: Config,
	close: Close,
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

	fn stream(&self) -> (SendStream, RecvStream) {
		let channel = kio::Shared::<Channel>::default();
		let send = SendStream {
			channel: channel.clone(),
			close: self.close.clone(),
			buffer: self.config.stream_buffer,
			done: false,
			park: Default::default(),
		};
		let recv = RecvStream {
			channel,
			close: self.close.clone(),
			stopped: false,
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

impl Default for Incoming {
	fn default() -> Self {
		Self {
			uni: kio::Queue::new(),
			bi: kio::Queue::new(),
			datagrams: kio::Queue::bounded(DATAGRAM_QUEUE),
		}
	}
}

impl Incoming {
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

	fn outgoing(&self) -> &Incoming {
		&self.conn.incoming[1 - self.local]
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
		// Check the close first, so anything queued before it is still handed out below.
		let closed = poll_close(close, waiter);
		if let Poll::Ready(Ok(item)) = queue.poll_pop(waiter) {
			return Poll::Ready(Ok(item));
		}
		closed.map(Err)
	}

	/// Hand `item` to the peer's `queue`, unless the session closed.
	fn open<T>(&self, queue: &kio::Queue<T>, item: T) -> Result<(), Error> {
		if let Some(err) = closed(&self.conn().close) {
			return Err(err);
		}
		// The queue only closes once the peer is gone, which closed the session first.
		queue
			.try_push(item)
			.map_err(|_| closed(&self.conn().close).expect("peer dropped"))
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

	fn poll_open_uni(
		&mut self,
		_cx: &mut Context<'_>,
		_: poll::SendOptions<'_, poll::Ungrouped>,
	) -> Poll<Result<SendStream, Error>> {
		let (send, recv) = self.conn().stream();
		Poll::Ready(self.open(&self.side.outgoing().uni, recv).map(|()| send))
	}

	fn poll_open_bi(
		&mut self,
		_cx: &mut Context<'_>,
		_: poll::SendOptions<'_, poll::Ungrouped>,
	) -> Poll<Result<(SendStream, RecvStream), Error>> {
		let (send, peer_recv) = self.conn().stream();
		let (peer_send, recv) = self.conn().stream();
		let opened = self.open(&self.side.outgoing().bi, (peer_send, peer_recv));
		Poll::Ready(opened.map(|()| (send, recv)))
	}

	fn send_group(&mut self, _: i64) -> Option<crate::transport::poll::Ungrouped> {
		None
	}

	fn send_datagram(&mut self, payload: Bytes, _: poll::SendOptions<'_, poll::Ungrouped>) -> Result<(), Error> {
		if let Some(err) = closed(&self.conn().close) {
			return Err(err);
		}
		// An oversized datagram, or one the peer has no room for, is lost as on a network.
		if payload.len() <= MAX_DATAGRAM_SIZE {
			let _ = self.side.outgoing().datagrams.try_push(payload);
		}
		Ok(())
	}

	fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, Error>> {
		let waiter = self.datagram.hold(cx);
		Self::poll_accept(&self.side.incoming().datagrams, &self.side.conn.close, waiter)
	}

	fn max_datagram_size(&self) -> usize {
		MAX_DATAGRAM_SIZE
	}

	fn protocol(&self) -> Option<&str> {
		self.conn().config.protocol.as_deref()
	}

	fn binding(&self) -> super::Binding {
		// Like raw QUIC, the pair has no request URI, so the target travels in SETUP.
		super::Binding::InBand
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

/// One direction of a stream: the messages its writer queued for its reader.
#[derive(Default)]
struct Channel {
	messages: VecDeque<Bytes>,
	/// The bytes in `messages`.
	queued: usize,
	/// How the channel closed, once it has.
	end: Option<End>,
}

#[derive(Clone, Copy)]
enum End {
	/// The writer finished: the reader gets what is queued, then the end of the stream.
	Fin,
	/// The writer reset the stream with this code.
	Reset(u32),
	/// The reader stopped the stream with this code.
	Stop(u32),
}

impl Channel {
	/// Close with a reset or stop code and drop what is queued. An earlier end stands,
	/// unless it was a finish the reader has not fully read.
	fn abort(&mut self, end: End) {
		if self.end.is_none() || !self.messages.is_empty() {
			self.end = Some(end);
		}
		self.messages.clear();
		self.queued = 0;
	}

	/// How the writer sees the channel close: reset, stopped, or finished and drained.
	fn sent(&self) -> Poll<Result<(), Error>> {
		match self.end {
			Some(End::Fin) if self.messages.is_empty() => Poll::Ready(Ok(())),
			Some(End::Reset(_)) => Poll::Ready(Ok(())),
			Some(End::Stop(code)) => Poll::Ready(Err(Error::Stopped(code))),
			_ => Poll::Pending,
		}
	}
}

/// The writing half of an in-memory stream.
pub struct SendStream {
	channel: kio::Shared<Channel>,
	close: Close,
	buffer: usize,
	/// This side finished or reset the stream.
	done: bool,
	park: kio::Park,
}

impl poll::SendStream for SendStream {
	type Error = Error;
	type Group = crate::transport::poll::Ungrouped;

	fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Error>> {
		let mut buf = buf;
		self.poll_write_buf(cx, &mut buf)
	}

	/// Queue all of `buf` as one message, waiting while the channel is full.
	fn poll_write_buf<B: Buf>(&mut self, cx: &mut Context<'_>, buf: &mut B) -> Poll<Result<usize, Error>> {
		if self.done {
			return Poll::Ready(Err(Error::Closed));
		}
		if !buf.has_remaining() {
			return Poll::Ready(Ok(0));
		}

		let waiter = self.park.hold(cx);
		if let Poll::Ready(err) = poll_close(&self.close, waiter) {
			return Poll::Ready(Err(err));
		}

		let buffer = self.buffer;
		let mut channel = ready!(self.channel.poll(waiter, |channel| {
			match channel.end.is_some() || channel.messages.is_empty() || channel.queued < buffer {
				true => Poll::Ready(()),
				false => Poll::Pending,
			}
		}));
		if let Some(End::Stop(code)) = channel.end {
			return Poll::Ready(Err(Error::Stopped(code)));
		}

		// Take ownership rather than copy: free for a `Bytes` source.
		let size = buf.remaining();
		channel.messages.push_back(buf.copy_to_bytes(size));
		channel.queued += size;

		Poll::Ready(Ok(size))
	}

	/// Ignored: with no link to share, there is nothing to schedule.
	fn set_priority(&mut self, _order: i64) {}

	fn set_group(&mut self, group: &Self::Group) {
		match *group {}
	}

	fn finish(&mut self) -> Result<(), Error> {
		if self.done {
			return Err(Error::Closed);
		}
		if let Some(err) = closed(&self.close) {
			return Err(err);
		}
		let mut channel = self.channel.lock();
		if let Some(End::Stop(code)) = channel.end {
			return Err(Error::Stopped(code));
		}
		channel.end = Some(End::Fin);
		self.done = true;
		Ok(())
	}

	fn reset(&mut self, code: u32) {
		if std::mem::replace(&mut self.done, true) {
			return;
		}
		// A closed session leaves its streams as they were, so the peer drains them. The
		// close is checked under the channel lock so a concurrent close can't land between.
		let mut channel = self.channel.lock();
		if closed(&self.close).is_none() {
			channel.abort(End::Reset(code));
		}
	}

	/// Ready once this side reset the stream, the reader stopped it, or the reader took
	/// every message of a finished stream. That stands in for a FIN acknowledgement, so a
	/// writer that waits for it before closing the session knows the peer has the data.
	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
		let waiter = self.park.hold(cx);
		// Check the close first, so a drain that happened before it is still seen below.
		let closed = poll_close(&self.close, waiter);
		if let Poll::Ready(channel) = self.channel.poll(waiter, |channel| channel.sent().map(drop)) {
			return channel.sent();
		}
		closed.map(Err)
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
	channel: kio::Shared<Channel>,
	close: Close,
	/// This side stopped the stream.
	stopped: bool,
	park: kio::Park,
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

	/// Pop the next message, or split off its first `max` bytes.
	fn poll_read_chunk(&mut self, cx: &mut Context<'_>, max: usize) -> Poll<Result<Option<Bytes>, Error>> {
		if self.stopped {
			return Poll::Ready(Err(Error::Closed));
		}
		if max == 0 {
			return Poll::Ready(Ok(Some(Bytes::new())));
		}

		let waiter = self.park.hold(cx);
		// Check the close first, so a message queued before it is still read below.
		let closed = poll_close(&self.close, waiter);
		let Poll::Ready(mut channel) = self.channel.poll(waiter, |channel| {
			match !channel.messages.is_empty() || channel.end.is_some() {
				true => Poll::Ready(()),
				false => Poll::Pending,
			}
		}) else {
			// Queued messages drain first; only an idle stream fails with the close.
			return closed.map(Err);
		};

		let Some(front) = channel.messages.front_mut() else {
			return Poll::Ready(match channel.end.expect("checked above") {
				End::Fin => Ok(None),
				End::Reset(code) => Err(Error::Reset(code)),
				End::Stop(_) => Err(Error::Closed),
			});
		};
		let chunk = match front.len() > max {
			true => front.split_to(max),
			false => channel.messages.pop_front().expect("front exists"),
		};
		channel.queued -= chunk.len();

		Poll::Ready(Ok(Some(chunk)))
	}

	fn stop(&mut self, code: u32) {
		if std::mem::replace(&mut self.stopped, true) {
			return;
		}
		// A closed session leaves its streams as they were; checked under the channel lock,
		// as in `reset`.
		let mut channel = self.channel.lock();
		if closed(&self.close).is_none() {
			channel.abort(End::Stop(code));
		}
	}

	/// Ready once the writer finished or reset the stream, or this side stopped it.
	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
		if self.stopped {
			return Poll::Ready(Ok(()));
		}
		let waiter = self.park.hold(cx);
		// Check the close first, so an end that came before it is still seen below.
		let closed = poll_close(&self.close, waiter);
		if let Poll::Ready(channel) = self.channel.poll(waiter, |channel| match channel.end.is_some() {
			true => Poll::Ready(()),
			false => Poll::Pending,
		}) {
			return Poll::Ready(match channel.end {
				Some(End::Reset(code)) => Err(Error::Reset(code)),
				_ => Ok(()),
			});
		}
		closed.map(Err)
	}
}

impl Drop for RecvStream {
	/// Stop, so the writer learns nobody reads and the queued messages are freed.
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

	fn buffer(stream_buffer: usize) -> Config {
		Config {
			stream_buffer,
			..Default::default()
		}
	}

	fn uni(a: &mut Session, b: &mut Session) -> (SendStream, RecvStream) {
		let send = ready(a.poll_open_uni(&mut noop(), Default::default())).unwrap();
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
	fn one_write_is_one_read() {
		let (mut a, mut b) = pair(Config::default());
		let (mut send, mut recv) = uni(&mut a, &mut b);

		let first = Bytes::from(vec![1u8; 4096]);
		let second = Bytes::from_static(b"second");
		assert_eq!(ready(write(&mut send, &first)).unwrap(), first.len());
		assert_eq!(ready(write(&mut send, &second)).unwrap(), second.len());

		// Each write is read back as one message, backed by the written allocation.
		let chunk = text(read(&mut recv, usize::MAX));
		assert_eq!(chunk.as_ptr(), first.as_ptr());
		assert_eq!(chunk.len(), first.len());

		// A smaller read splits the message, still without a copy.
		assert_eq!(text(read(&mut recv, 2)).as_ptr(), second.as_ptr());
		assert_eq!(text(read(&mut recv, usize::MAX)), "cond");

		send.finish().unwrap();
		assert_eq!(ready(read(&mut recv, usize::MAX)).unwrap(), None);
		assert_eq!(ready(read(&mut recv, usize::MAX)).unwrap(), None);
	}

	#[test]
	fn full_channel_pends_writer_until_read() {
		let (mut a, mut b) = pair(buffer(8));
		let (mut send, mut recv) = uni(&mut a, &mut b);
		let (mut other, mut other_recv) = uni(&mut a, &mut b);

		// A write is never split, so one larger than the buffer still lands whole.
		let large = Bytes::from_static(b"0123456789");
		assert_eq!(ready(write(&mut send, &large)).unwrap(), 10);

		let count = Arc::new(Count::default());
		let waker = Waker::from(count.clone());
		let mut next = Bytes::from_static(b"x");
		assert!(
			send.poll_write_buf(&mut Context::from_waker(&waker), &mut next)
				.is_pending()
		);
		assert_eq!(next, "x");

		// Only that stream is full.
		assert_eq!(ready(write(&mut other, &Bytes::from_static(b"y"))).unwrap(), 1);
		assert_eq!(text(read(&mut other_recv, usize::MAX)), "y");

		// A partial read leaves the channel full; reading the rest wakes the writer.
		assert_eq!(text(read(&mut recv, 2)), "01");
		assert!(write(&mut send, &next).is_pending());
		assert_eq!(text(read(&mut recv, usize::MAX)), "23456789");
		assert!(count.0.load(Ordering::Relaxed) > 0);
		assert_eq!(ready(send.poll_write_buf(&mut noop(), &mut next)).unwrap(), 1);
	}

	#[test]
	fn zero_buffer_holds_one_message() {
		let (mut a, mut b) = pair(buffer(0));
		let (mut send, mut recv) = uni(&mut a, &mut b);

		assert_eq!(ready(write(&mut send, &Bytes::from_static(b"one"))).unwrap(), 3);
		assert!(write(&mut send, &Bytes::from_static(b"two")).is_pending());
		assert_eq!(text(read(&mut recv, usize::MAX)), "one");
		assert_eq!(ready(write(&mut send, &Bytes::from_static(b"two"))).unwrap(), 3);
	}

	#[test]
	fn reset_drops_queued_messages() {
		let (mut a, mut b) = pair(buffer(8));
		let (mut send, mut recv) = uni(&mut a, &mut b);

		ready(write(&mut send, &Bytes::from_static(b"01234567"))).unwrap();
		send.reset(7);
		assert!(ready(send.poll_closed(&mut noop())).is_ok());
		assert!(matches!(ready(recv.poll_closed(&mut noop())), Err(Error::Reset(7))));
		assert!(matches!(ready(read(&mut recv, usize::MAX)), Err(Error::Reset(7))));
		assert!(matches!(
			ready(write(&mut send, &Bytes::from_static(b"x"))),
			Err(Error::Closed)
		));
	}

	#[test]
	fn finish_closes_once_drained() {
		let (mut a, mut b) = pair(Config::default());
		let (mut send, mut recv) = uni(&mut a, &mut b);

		ready(write(&mut send, &Bytes::from_static(b"done"))).unwrap();
		send.finish().unwrap();
		assert!(matches!(send.finish(), Err(Error::Closed)));
		assert!(ready(recv.poll_closed(&mut noop())).is_ok());

		// The writer's close waits for the reader to take every message.
		assert!(send.poll_closed(&mut noop()).is_pending());
		assert_eq!(text(read(&mut recv, usize::MAX)), "done");
		assert!(ready(send.poll_closed(&mut noop())).is_ok());
		assert_eq!(ready(read(&mut recv, usize::MAX)).unwrap(), None);

		// A reset after the finish is ignored, so the reader still gets everything.
		let (mut send, mut recv) = uni(&mut a, &mut b);
		ready(write(&mut send, &Bytes::from_static(b"kept"))).unwrap();
		send.finish().unwrap();
		send.reset(2);
		assert_eq!(text(read(&mut recv, usize::MAX)), "kept");
		assert_eq!(ready(read(&mut recv, usize::MAX)).unwrap(), None);

		// A reader that stops after taking everything leaves the writer finished.
		drop(recv);
		assert!(ready(send.poll_closed(&mut noop())).is_ok());

		// A reader that stops with messages unread fails the writer, finished or not.
		let (mut send, recv) = uni(&mut a, &mut b);
		ready(write(&mut send, &Bytes::from_static(b"unread"))).unwrap();
		send.finish().unwrap();
		drop(recv);
		assert!(matches!(ready(send.poll_closed(&mut noop())), Err(Error::Stopped(0))));
	}

	#[test]
	fn stop_fails_the_writer() {
		let (mut a, mut b) = pair(Config::default());
		let (mut send, mut recv) = uni(&mut a, &mut b);

		ready(write(&mut send, &Bytes::from_static(b"01234567"))).unwrap();
		assert!(send.poll_closed(&mut noop()).is_pending());

		recv.stop(3);
		assert!(matches!(ready(send.poll_closed(&mut noop())), Err(Error::Stopped(3))));
		assert!(matches!(
			ready(write(&mut send, &Bytes::from_static(b"x"))),
			Err(Error::Stopped(3))
		));
		assert!(matches!(send.finish(), Err(Error::Stopped(3))));
		assert!(matches!(ready(read(&mut recv, usize::MAX)), Err(Error::Closed)));
		assert!(ready(recv.poll_closed(&mut noop())).is_ok());
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
		let (mut a_send, mut a_recv) = ready(a.poll_open_bi(&mut noop(), Default::default())).unwrap();
		let (mut b_send, mut b_recv) = ready(b.poll_accept_bi(&mut noop())).unwrap();

		ready(write(&mut a_send, &Bytes::from_static(b"ping"))).unwrap();
		assert_eq!(text(read(&mut b_recv, usize::MAX)), "ping");
		ready(write(&mut b_send, &Bytes::from_static(b"pong"))).unwrap();
		assert_eq!(text(read(&mut a_recv, usize::MAX)), "pong");
	}

	#[test]
	fn close_drains_queued_messages() {
		let (mut a, mut b) = pair(Config {
			protocol: Some("moq".into()),
			..Default::default()
		});
		assert_eq!(b.protocol(), Some("moq"));
		let (mut send, mut recv) = uni(&mut a, &mut b);
		let (mut unfinished, mut unfinished_recv) = uni(&mut a, &mut b);
		let late = ready(a.poll_open_uni(&mut noop(), Default::default())).unwrap();

		ready(write(&mut send, &Bytes::from_static(b"last"))).unwrap();
		send.finish().unwrap();
		ready(write(&mut unfinished, &Bytes::from_static(b"part"))).unwrap();
		assert!(b.poll_closed(&mut noop()).is_pending());

		a.close(4, "bye");
		let closed = |err: Error| matches!(err, Error::Session { code: 4, ref reason } if reason == "bye");
		assert!(closed(ready(b.poll_closed(&mut noop()))));
		assert!(closed(
			ready(a.poll_open_uni(&mut noop(), Default::default())).err().unwrap()
		));
		assert!(closed(
			ready(write(&mut unfinished, &Bytes::from_static(b"x"))).unwrap_err()
		));
		// Dropping a stream after the close leaves it as it was, so it still drains.
		drop(send);
		drop(unfinished);

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
		assert!(matches!(
			ready(b.poll_closed(&mut noop())),
			Error::Session { code: 0, .. }
		));
	}

	#[test]
	fn datagrams() {
		let (mut a, mut b) = pair(Config::default());
		a.send_datagram(vec![0; MAX_DATAGRAM_SIZE + 1].into(), Default::default())
			.unwrap();
		for _ in 0..DATAGRAM_QUEUE {
			a.send_datagram("one".into(), Default::default()).unwrap();
		}
		// The peer's queue is full, so this one is lost, like the oversized one.
		a.send_datagram("two".into(), Default::default()).unwrap();
		for _ in 0..DATAGRAM_QUEUE {
			assert_eq!(ready(b.poll_recv_datagram(&mut noop())).unwrap(), "one");
		}
		assert!(b.poll_recv_datagram(&mut noop()).is_pending());
	}
}
