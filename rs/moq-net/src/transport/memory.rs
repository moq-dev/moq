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
//! [`Config::max_concurrent_uni_streams`] and [`Config::max_concurrent_bi_streams`] bound
//! the streams each side has open toward its peer, as QUIC's stream credit does: an open
//! pends until the peer is done with one.
//!
//! [`finish`](poll::SendStream::finish) closes the channel cleanly once its messages are
//! read. A [`reset`](poll::SendStream::reset) or [`stop`](poll::RecvStream::stop) closes it
//! with a code and drops what it queued. Closing the session keeps queued messages
//! readable, and leaves streams as they were, so the peer reads them, then the close.
//!
//! Reads return the channel's room as they go, so streams lease through
//! [`Buffered`](poll::Buffered) and are not bounded by a receive window.
//!
//! Datagrams ignore their send options, and are dropped when oversized or when the
//! peer's queue is full, as a network would.
//!
//! [`Config::latency`] delays each stream message and finish on the ambient clock, so
//! a mesh of pairs orders its traffic as a network of that latency would. Opens,
//! resets, stops, datagrams, and the session close skip it.

use std::{
	collections::VecDeque,
	pin::Pin,
	sync::Arc,
	task::{Poll, ready},
	time::Duration,
};

use bytes::{Buf, Bytes};

use super::poll;

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
	/// Uni streams one side may have open toward its peer at once, as QUIC's stream credit.
	/// A stream is open until its reader reaches the end, stops, or drops it, accepted or
	/// not. `None` grants credit without limit.
	pub max_concurrent_uni_streams: Option<usize>,
	/// Bidi streams one side may have open toward its peer at once, as QUIC's stream credit.
	/// A stream is open until the peer is done with both halves: its reader reaches the end,
	/// stops, or drops it, and its writer finishes, resets, or drops it. `None` grants credit
	/// without limit.
	pub max_concurrent_bi_streams: Option<usize>,
	/// How long each stream message and finish takes to reach its reader, on the ambient
	/// clock. Zero delivers at once.
	pub latency: Duration,
}

impl Default for Config {
	/// A 1 MiB stream buffer: about a QUIC stream's receive window (quinn's is 1.25 MB), so
	/// a writer pends about where it would against a zero-latency QUIC peer.
	fn default() -> Self {
		Self {
			stream_buffer: 1024 * 1024,
			protocol: None,
			max_concurrent_uni_streams: None,
			max_concurrent_bi_streams: None,
			latency: Duration::ZERO,
		}
	}
}

/// Create two connected sessions: streams one opens, the other accepts.
pub fn pair(config: Config) -> (Session, Session) {
	let mut secret = [0; 32];
	rand::RngExt::fill(&mut kio::clock::rng(), &mut secret);
	let conn = Arc::new(Conn {
		secret: rind::hkdf::Salt::new(&rind::hkdf::HKDF_SHA256, &[]).extract(&secret),
		close: Default::default(),
		incoming: [Incoming::default(), Incoming::default()],
		config,
	});

	let side = |local| Session {
		side: Arc::new(Side {
			conn: conn.clone(),
			local,
		}),
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
	/// What both sides export keying material from, standing in for a TLS handshake's secret.
	secret: rind::hkdf::Prk,
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

	/// A one-way stream, whose halves hold `send` and `recv` until each is done.
	fn stream(&self, send: Option<Credit>, recv: Option<Credit>) -> (SendStream, RecvStream) {
		let channel = kio::Shared::<Channel>::default();
		let writer = SendStream {
			channel: channel.clone(),
			close: self.close.clone(),
			buffer: self.config.stream_buffer,
			latency: self.config.latency,
			done: false,
			credit: send,
		};
		let reader = poll::Buffered::new(Chunks {
			channel,
			close: self.close.clone(),
			stopped: false,
			credit: recv,
			landing: None,
		});
		(writer, reader)
	}
}

/// One open stream's place in its opener's credit, returned once every half holding it is
/// done.
type Credit = Arc<Opened>;

/// The streams one side has open toward its peer in one direction.
#[derive(Default)]
struct Open {
	count: usize,
}

/// Counted in an open-stream total until dropped.
struct Opened(kio::Shared<Open>);

impl Drop for Opened {
	fn drop(&mut self) {
		let mut open = self.0.lock();
		open.count -= 1;
	}
}

/// Take one stream credit of `max` from `open`, pending until the peer is done with one.
fn poll_credit(
	open: &kio::Shared<Open>,
	max: Option<usize>,
	close: &Close,
	waiter: &kio::Waiter,
) -> Poll<Result<Option<Credit>, Error>> {
	let Some(max) = max else {
		return Poll::Ready(Ok(None));
	};
	// Check the close first, so a credit granted before it is still taken below.
	let closed = poll_close(close, waiter);
	let Poll::Ready(mut taken) = open.poll(waiter, |open| {
		if open.count < max {
			return Poll::Ready(());
		}
		Poll::Pending
	}) else {
		return closed.map(Err);
	};
	taken.count += 1;
	Poll::Ready(Ok(Some(Arc::new(Opened(open.clone())))))
}

/// Streams and datagrams one side has yet to accept.
struct Incoming {
	uni: kio::Queue<RecvStream>,
	/// The uni streams toward this side still open, against [`Config::max_concurrent_uni_streams`].
	uni_open: kio::Shared<Open>,
	bi: kio::Queue<(SendStream, RecvStream)>,
	/// The bidi streams toward this side still open, against [`Config::max_concurrent_bi_streams`].
	bi_open: kio::Shared<Open>,
	datagrams: kio::Queue<Bytes>,
}

impl Default for Incoming {
	fn default() -> Self {
		Self {
			uni: kio::Queue::new(),
			uni_open: Default::default(),
			bi: kio::Queue::new(),
			bi_open: Default::default(),
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

/// What one side of a pair still holds, read without keeping it alive, for the session fuzz
/// target's leak check.
#[cfg(all(feature = "async", any(test, feature = "fuzz")))]
pub(crate) struct Held {
	side: std::sync::Weak<Side>,
	conn: Arc<Conn>,
	local: usize,
}

#[cfg(all(feature = "async", any(test, feature = "fuzz")))]
impl Held {
	/// Whether any handle to this side's session is alive.
	pub(crate) fn session(&self) -> bool {
		self.side.strong_count() > 0
	}

	/// Streams the peer opened toward this side that this side is not done with, counted only
	/// under [`Config::max_concurrent_uni_streams`] and [`Config::max_concurrent_bi_streams`].
	pub(crate) fn streams(&self) -> usize {
		let incoming = &self.conn.incoming[self.local];
		incoming.uni_open.read().count + incoming.bi_open.read().count
	}
}

#[cfg(all(feature = "async", any(test, feature = "fuzz")))]
impl Session {
	/// Watch what this side holds; see [`Held`].
	pub(crate) fn held(&self) -> Held {
		Held {
			side: Arc::downgrade(&self.side),
			conn: self.side.conn.clone(),
			local: self.side.local,
		}
	}
}

impl poll::Session for Session {
	type SendStream = SendStream;
	type RecvStream = RecvStream;
	type Error = Error;

	fn poll_accept_uni(&self, waiter: &kio::Waiter) -> Poll<Result<RecvStream, Error>> {
		Self::poll_accept(&self.side.incoming().uni, &self.side.conn.close, waiter)
	}

	fn poll_accept_bi(&self, waiter: &kio::Waiter) -> Poll<Result<(SendStream, RecvStream), Error>> {
		Self::poll_accept(&self.side.incoming().bi, &self.side.conn.close, waiter)
	}

	fn poll_open_uni(
		&self,
		waiter: &kio::Waiter,
		_: poll::SendOptions<'_, poll::Ungrouped>,
	) -> Poll<Result<SendStream, Error>> {
		let outgoing = self.side.outgoing();
		let max = self.conn().config.max_concurrent_uni_streams;
		let credit = ready!(poll_credit(&outgoing.uni_open, max, &self.side.conn.close, waiter))?;
		let (send, recv) = self.conn().stream(None, credit);
		Poll::Ready(self.open(&outgoing.uni, recv).map(|()| send))
	}

	fn poll_open_bi(
		&self,
		waiter: &kio::Waiter,
		_: poll::SendOptions<'_, poll::Ungrouped>,
	) -> Poll<Result<(SendStream, RecvStream), Error>> {
		let outgoing = self.side.outgoing();
		let max = self.conn().config.max_concurrent_bi_streams;
		// The peer's halves both hold the credit, so it returns once the peer is done with both.
		let credit = ready!(poll_credit(&outgoing.bi_open, max, &self.side.conn.close, waiter))?;
		let (send, peer_recv) = self.conn().stream(None, credit.clone());
		let (peer_send, recv) = self.conn().stream(credit, None);
		let opened = self.open(&outgoing.bi, (peer_send, peer_recv));
		Poll::Ready(opened.map(|()| (send, recv)))
	}

	fn send_group(&self, _: i64) -> Option<poll::Ungrouped> {
		None
	}

	fn send_datagram(&self, payload: Bytes, _: poll::SendOptions<'_, poll::Ungrouped>) -> Result<(), Error> {
		if let Some(err) = closed(&self.conn().close) {
			return Err(err);
		}
		// An oversized datagram, or one the peer has no room for, is lost as on a network.
		if payload.len() <= MAX_DATAGRAM_SIZE {
			let _ = self.side.outgoing().datagrams.try_push(payload);
		}
		Ok(())
	}

	fn poll_recv_datagram(&self, waiter: &kio::Waiter) -> Poll<Result<Bytes, Error>> {
		Self::poll_accept(&self.side.incoming().datagrams, &self.side.conn.close, waiter)
	}

	fn max_datagram_size(&self) -> usize {
		MAX_DATAGRAM_SIZE
	}

	fn protocol(&self) -> Option<&str> {
		self.conn().config.protocol.as_deref()
	}

	fn address(&self) -> super::Address {
		// Like raw QUIC, the pair has no request URI, so the target travels in SETUP.
		super::Address::Setup
	}

	fn close(&self, code: u32, reason: &str) {
		self.conn().close(code, reason);
	}

	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Error> {
		poll_close(&self.side.conn.close, waiter)
	}

	/// Streams share no connection window, and one holds at most its buffer.
	fn receive_window(&self) -> Option<super::ReceiveWindow> {
		Some(super::ReceiveWindow::new(None, self.conn().config.stream_buffer as u64))
	}

	fn export_keying_material(&self, label: &[u8], context: &[u8]) -> Option<[u8; 32]> {
		// Length-prefixed, so no label and context pair collides with another.
		let lengths = [u8::try_from(label.len()).ok()?, u8::try_from(context.len()).ok()?];
		let mut output = [0; 32];
		self.conn()
			.secret
			.expand(&[&lengths, label, context], output.len())
			.fill(&mut output)
			.expect("32 bytes fit HKDF-SHA256");
		Some(output)
	}
}

/// One direction of a stream: the messages its writer queued for its reader.
#[derive(Default)]
struct Channel {
	messages: VecDeque<Message>,
	/// The bytes in `messages`.
	queued: usize,
	/// The bytes the reader took, which stand in for delivered ones.
	taken: u64,
	/// How the channel closed, once it has.
	end: Option<End>,
	/// When a finish reaches the reader under [`Config::latency`], until the reader
	/// takes it: the writer counts it acknowledged only then.
	finished: Option<kio::clock::Instant>,
}

/// One write, and when it reaches the reader under [`Config::latency`].
struct Message {
	bytes: Bytes,
	lands: Option<kio::clock::Instant>,
}

/// When something a writer sent `latency` ago reaches the reader: `None` at once.
fn landing(latency: Duration) -> Option<kio::clock::Instant> {
	(!latency.is_zero()).then(|| kio::clock::now().saturating_add(latency))
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
	/// Whether a read would return now: a queued message or the end.
	fn readable(&self) -> Poll<()> {
		match !self.messages.is_empty() || self.end.is_some() {
			true => Poll::Ready(()),
			false => Poll::Pending,
		}
	}

	/// When the next thing the reader takes lands, if it is still in flight.
	fn in_flight(&self) -> Option<kio::clock::Instant> {
		let lands = match self.messages.front() {
			Some(message) => message.lands,
			None if matches!(self.end, Some(End::Fin)) => self.finished,
			None => None,
		}?;
		(lands > kio::clock::now()).then_some(lands)
	}

	/// Close with a reset or stop code and drop what is queued. An earlier end stands,
	/// unless it was a finish the reader has not fully read.
	fn abort(&mut self, end: End) {
		if self.end.is_none() || !self.messages.is_empty() {
			self.end = Some(end);
		}
		self.messages.clear();
		self.queued = 0;
		// Nothing is left in flight for the writer to wait on.
		self.finished = None;
	}

	/// How the writer sees the channel close: reset, stopped, or finished and drained.
	fn sent(&self) -> Poll<Result<(), Error>> {
		match self.end {
			Some(End::Fin) if self.messages.is_empty() && self.finished.is_none() => Poll::Ready(Ok(())),
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
	/// See [`Config::latency`].
	latency: Duration,
	/// This side finished or reset the stream.
	done: bool,
	/// The credit of a bidi stream the peer opened, held until this side is done.
	credit: Option<Credit>,
}

impl poll::SendStream for SendStream {
	type Error = Error;
	type Group = crate::transport::poll::Ungrouped;
	type Offset = ();

	fn offset(&self) -> Self::Offset {}

	fn poll_write(&mut self, waiter: &kio::Waiter, buf: &[u8]) -> Poll<Result<usize, Error>> {
		let mut buf = buf;
		self.poll_write_buf(waiter, &mut buf)
	}

	/// Queue all of `buf` as one message, waiting while the channel is full.
	fn poll_write_buf<B: Buf>(&mut self, waiter: &kio::Waiter, buf: &mut B) -> Poll<Result<usize, Error>> {
		if self.done {
			return Poll::Ready(Err(Error::Closed));
		}
		if !buf.has_remaining() {
			return Poll::Ready(Ok(0));
		}

		// Peek at the close rather than register on it: only a write that parks waits for it.
		if let Some(err) = closed(&self.close) {
			return Poll::Ready(Err(err));
		}

		let buffer = self.buffer;
		let Poll::Ready(mut channel) = self.channel.poll(waiter, |channel| {
			match channel.end.is_some() || channel.messages.is_empty() || channel.queued < buffer {
				true => Poll::Ready(()),
				false => Poll::Pending,
			}
		}) else {
			return poll_close(&self.close, waiter).map(Err);
		};
		if let Some(End::Stop(code)) = channel.end {
			return Poll::Ready(Err(Error::Stopped(code)));
		}

		// Take ownership rather than copy: free for a `Bytes` source.
		let size = buf.remaining();
		channel.messages.push_back(Message {
			bytes: buf.copy_to_bytes(size),
			lands: landing(self.latency),
		});
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
		channel.finished = landing(self.latency);
		drop(channel);
		self.done = true;
		self.credit = None;
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
		drop(channel);
		self.credit = None;
	}

	/// Ready once this side reset the stream, the reader stopped it, or the reader took
	/// every message of a finished stream and, under latency, the finish. That stands in for a FIN acknowledgement, so a
	/// writer that waits for it before closing the session knows the peer has the data.
	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		// Check the close first, so a drain that happened before it is still seen below.
		let closed = poll_close(&self.close, waiter);
		if let Poll::Ready(channel) = self.channel.poll(waiter, |channel| channel.sent().map(drop)) {
			return channel.sent();
		}
		closed.map(Err)
	}

	/// Whether the reader took every message written so far, which stands in for an
	/// acknowledgement as it does for [`poll_closed`](poll::SendStream::poll_closed).
	fn acknowledged(&self) -> Option<bool> {
		let channel = self.channel.lock();
		// A reset on a closed session leaves the channel as it was, so only `done` records it.
		let finished = matches!(channel.end, Some(End::Fin)) && channel.finished.is_none();
		let open = channel.end.is_none() && !self.done;
		Some((open || finished) && channel.messages.is_empty())
	}

	/// The bytes the reader took, standing in for delivered ones as it does for
	/// [`acknowledged`](poll::SendStream::acknowledged).
	fn delivered(&self) -> Option<u64> {
		Some(self.channel.lock().taken)
	}

	fn poll_delivered(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.channel
			.poll(waiter, |channel| match channel.queued {
				0 => Poll::Ready(()),
				_ => Poll::Pending,
			})
			.map(drop)
	}
}

impl Drop for SendStream {
	/// Reset rather than finish, so a writer that went away is never read as complete.
	fn drop(&mut self) {
		poll::SendStream::reset(self, 0);
	}
}

/// The reading half of an in-memory stream, leased through [`poll::Buffered`].
pub type RecvStream = poll::Buffered<Chunks>;

/// The plain reads of an in-memory stream.
pub struct Chunks {
	channel: kio::Shared<Channel>,
	close: Close,
	/// This side stopped the stream.
	stopped: bool,
	/// The open-stream count this stream holds a credit in, until it is done.
	credit: Option<Credit>,
	/// Armed while the next message or the finish is still in flight.
	landing: Option<Pin<Box<kio::clock::Sleep>>>,
}

impl Chunks {
	/// Let go of this stream's credit, returned to its writer's side once both halves have.
	fn release(&mut self) {
		self.credit = None;
	}
}

impl poll::Chunks for Chunks {
	type Error = Error;

	/// Pop the next message, or split off its first `max` bytes.
	fn poll_read_chunk(&mut self, waiter: &kio::Waiter, max: usize) -> Poll<Result<Option<Bytes>, Error>> {
		if self.stopped {
			return Poll::Ready(Err(Error::Closed));
		}
		if max == 0 {
			return Poll::Ready(Ok(Some(Bytes::new())));
		}

		// Only a read that parks registers on the close, so a ready read costs one lock.
		let mut channel = match self.channel.poll(waiter, |channel| channel.readable()) {
			Poll::Ready(channel) => channel,
			Poll::Pending => {
				let err = ready!(poll_close(&self.close, waiter));
				// Queued messages drain first, so look again: one may have landed before the close.
				match self.channel.poll(waiter, |channel| channel.readable()) {
					Poll::Ready(channel) => channel,
					Poll::Pending => return Poll::Ready(Err(err)),
				}
			}
		};

		if let Some(lands) = channel.in_flight() {
			drop(channel);
			// Wait on the channel too: a reset or stop drops what is in flight, and the
			// reader sees it at once rather than at the discarded message's landing.
			let changed = |channel: &kio::Ref<'_, Channel>| match channel.in_flight() {
				Some(_) => Poll::Pending,
				None => Poll::Ready(()),
			};
			if self.channel.poll(waiter, changed).is_ready() {
				return self.poll_read_chunk(waiter, max);
			}
			let landing = match &mut self.landing {
				Some(landing) => {
					// A reset wakes the task parked on the sleep, so resetting to the
					// same instant on every poll would spin without time advancing.
					if landing.deadline() != lands {
						landing.as_mut().reset(lands);
					}
					landing
				}
				None => self.landing.insert(Box::pin(kio::clock::sleep_until(lands))),
			};
			ready!(waiter.poll_future(landing.as_mut()));
			return self.poll_read_chunk(waiter, max);
		}

		let Some(front) = channel.messages.front_mut() else {
			let end = channel.end.expect("checked above");
			if matches!(end, End::Fin) && channel.finished.is_some() {
				// The finish landed: tell the writer, which waits on it as an acknowledgement.
				channel.finished = None;
			}
			drop(channel);
			self.release();
			return Poll::Ready(match end {
				End::Fin => Ok(None),
				End::Reset(code) => Err(Error::Reset(code)),
				End::Stop(_) => Err(Error::Closed),
			});
		};
		let chunk = match front.bytes.len() > max {
			true => front.bytes.split_to(max),
			false => channel.messages.pop_front().expect("front exists").bytes,
		};
		channel.queued -= chunk.len();
		channel.taken += chunk.len() as u64;

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
		drop(channel);
		self.release();
	}

	fn poll_reset(&mut self, waiter: &kio::Waiter) -> Poll<Error> {
		if self.stopped {
			return Poll::Ready(Error::Closed);
		}
		if let Poll::Ready(err) = poll_close(&self.close, waiter) {
			return Poll::Ready(err);
		}
		let channel = ready!(self.channel.poll(waiter, |channel| match channel.end {
			Some(End::Reset(_)) => Poll::Ready(()),
			_ => Poll::Pending,
		}));
		let Some(End::Reset(code)) = channel.end else {
			unreachable!("checked above")
		};
		Poll::Ready(Error::Reset(code))
	}
}

impl Drop for Chunks {
	/// Stop, so the writer learns nobody reads and the queued messages are freed.
	fn drop(&mut self) {
		poll::Chunks::stop(self, 0);
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

	fn noop() -> kio::Waiter {
		kio::Waiter::noop()
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

	fn uni(a: &Session, b: &Session) -> (SendStream, RecvStream) {
		let send = ready(a.poll_open_uni(&noop(), Default::default())).unwrap();
		let recv = ready(b.poll_accept_uni(&noop())).unwrap();
		(send, recv)
	}

	fn write(send: &mut SendStream, chunk: &Bytes) -> Poll<Result<usize, Error>> {
		send.poll_write_buf(&noop(), &mut chunk.clone())
	}

	/// Below the lease buffering, so each read sees the channel itself.
	fn read(recv: &mut RecvStream, max: usize) -> Poll<Result<Option<Bytes>, Error>> {
		poll::Chunks::poll_read_chunk(recv.get_mut(), &noop(), max)
	}

	fn text(chunk: Poll<Result<Option<Bytes>, Error>>) -> Bytes {
		ready(chunk).unwrap().unwrap()
	}

	#[test]
	fn bounds() {
		fn boxable<S: poll::Boxable>() {}
		boxable::<Session>();
	}

	#[test]
	fn one_write_is_one_read() {
		let (a, b) = pair(Config::default());
		let (mut send, mut recv) = uni(&a, &b);

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
		let (a, b) = pair(buffer(8));
		let (mut send, mut recv) = uni(&a, &b);
		let (mut other, mut other_recv) = uni(&a, &b);

		// A write is never split, so one larger than the buffer still lands whole.
		let large = Bytes::from_static(b"0123456789");
		assert_eq!(ready(write(&mut send, &large)).unwrap(), 10);

		let count = Arc::new(Count::default());
		let waiter = kio::Waiter::new(Waker::from(count.clone()));
		let mut next = Bytes::from_static(b"x");
		assert!(send.poll_write_buf(&waiter, &mut next).is_pending());
		assert_eq!(next, "x");

		// Only that stream is full.
		assert_eq!(ready(write(&mut other, &Bytes::from_static(b"y"))).unwrap(), 1);
		assert_eq!(text(read(&mut other_recv, usize::MAX)), "y");

		// A partial read leaves the channel full; reading the rest wakes the writer.
		assert_eq!(text(read(&mut recv, 2)), "01");
		assert!(write(&mut send, &next).is_pending());
		assert_eq!(text(read(&mut recv, usize::MAX)), "23456789");
		assert!(count.0.load(Ordering::Relaxed) > 0);
		assert_eq!(ready(send.poll_write_buf(&noop(), &mut next)).unwrap(), 1);
	}

	#[test]
	fn zero_buffer_holds_one_message() {
		let (a, b) = pair(buffer(0));
		let (mut send, mut recv) = uni(&a, &b);

		assert_eq!(ready(write(&mut send, &Bytes::from_static(b"one"))).unwrap(), 3);
		assert!(write(&mut send, &Bytes::from_static(b"two")).is_pending());
		assert_eq!(text(read(&mut recv, usize::MAX)), "one");
		assert_eq!(ready(write(&mut send, &Bytes::from_static(b"two"))).unwrap(), 3);
	}

	#[test]
	fn uni_credit_pends_opens_until_a_stream_is_done() {
		let (a, b) = pair(Config {
			max_concurrent_uni_streams: Some(2),
			..Default::default()
		});
		let open = |a: &Session, waiter: &kio::Waiter| a.poll_open_uni(waiter, Default::default());

		// Streams the peer has not accepted yet hold credit too.
		let mut finished = ready(open(&a, &noop())).unwrap();
		let _reset = ready(open(&a, &noop())).unwrap();
		let count = Arc::new(Count::default());
		let waiter = kio::Waiter::new(Waker::from(count.clone()));
		assert!(open(&a, &waiter).is_pending());
		assert_eq!(count.0.load(Ordering::Relaxed), 0, "being blocked wakes nobody");

		// Credit is per direction.
		let _back = ready(open(&b, &noop())).unwrap();

		// Reading to the end returns the credit, and wakes the parked open.
		ready(write(&mut finished, &Bytes::from_static(b"x"))).unwrap();
		finished.finish().unwrap();
		let mut recv = ready(b.poll_accept_uni(&noop())).unwrap();
		assert_eq!(text(read(&mut recv, usize::MAX)), "x");
		assert!(open(&a, &waiter).is_pending(), "an unread FIN keeps the credit");
		assert_eq!(ready(read(&mut recv, usize::MAX)).unwrap(), None);
		assert!(count.0.load(Ordering::Relaxed) > 0);
		let _third = ready(open(&a, &noop())).unwrap();
		assert!(open(&a, &noop()).is_pending());

		// So does dropping a stream, accepted or not.
		drop(recv);
		assert!(open(&a, &noop()).is_pending(), "the credit was returned once");
		drop(ready(b.poll_accept_uni(&noop())).unwrap());
		let _fourth = ready(open(&a, &noop())).unwrap();

		// A parked open fails once the session closes.
		b.close(1, "bye");
		assert!(matches!(ready(open(&a, &noop())), Err(Error::Session { code: 1, .. })));
	}

	/// A bidi stream holds its opener's credit until the peer is done with both halves.
	#[test]
	fn bi_credit_returns_once_the_peer_is_done_with_both_halves() {
		let (a, b) = pair(Config {
			max_concurrent_bi_streams: Some(1),
			..Default::default()
		});
		let open = |a: &Session, waiter: &kio::Waiter| a.poll_open_bi(waiter, Default::default());

		let (mut send, _recv) = ready(open(&a, &noop())).unwrap();
		let count = Arc::new(Count::default());
		let waiter = kio::Waiter::new(Waker::from(count.clone()));
		assert!(open(&a, &waiter).is_pending());

		// The peer reading our half to the end leaves its own half open.
		send.finish().unwrap();
		let (mut peer_send, mut peer_recv) = ready(b.poll_accept_bi(&noop())).unwrap();
		assert_eq!(ready(read(&mut peer_recv, usize::MAX)).unwrap(), None);
		assert!(open(&a, &waiter).is_pending(), "the peer still writes");

		peer_send.finish().unwrap();
		assert!(count.0.load(Ordering::Relaxed) > 0);
		let _second = ready(open(&a, &noop())).unwrap();
	}

	#[test]
	fn reset_drops_queued_messages() {
		let (a, b) = pair(buffer(8));
		let (mut send, mut recv) = uni(&a, &b);

		ready(write(&mut send, &Bytes::from_static(b"01234567"))).unwrap();
		send.reset(7);
		assert!(ready(send.poll_closed(&noop())).is_ok());
		assert!(matches!(ready(read(&mut recv, usize::MAX)), Err(Error::Reset(7))));
		assert!(matches!(
			ready(write(&mut send, &Bytes::from_static(b"x"))),
			Err(Error::Closed)
		));
	}

	/// The next chunk `recv` reads, once it lands.
	async fn landed(recv: &mut RecvStream) -> Option<Bytes> {
		kio::wait(|waiter| poll::Chunks::poll_read_chunk(recv.get_mut(), waiter, usize::MAX))
			.await
			.unwrap()
	}

	#[test]
	fn latency_delays_each_message_and_the_finish() {
		kio::clock::sim::run(async {
			const LATENCY: Duration = Duration::from_millis(10);
			let (a, b) = pair(Config {
				latency: LATENCY,
				..Default::default()
			});
			let (mut send, mut recv) = uni(&a, &b);
			let start = kio::clock::now();

			ready(write(&mut send, &Bytes::from_static(b"one"))).unwrap();
			// Polling again while it is in flight must not wake the reader, or a task
			// re-polling its stream would spin without simulated time moving.
			let count = Arc::new(Count::default());
			let waiter = kio::Waiter::new(Waker::from(count.clone()));
			for _ in 0..2 {
				let chunk = poll::Chunks::poll_read_chunk(recv.get_mut(), &waiter, usize::MAX);
				assert!(chunk.is_pending(), "a message landed at once");
			}
			assert_eq!(count.0.load(Ordering::Relaxed), 0, "a re-poll woke the reader");
			kio::clock::sleep(LATENCY / 2).await;
			ready(write(&mut send, &Bytes::from_static(b"two"))).unwrap();
			send.finish().unwrap();

			// Each lands its latency after it was sent, in order, and the finish last.
			assert_eq!(landed(&mut recv).await.unwrap(), "one");
			assert_eq!(start.elapsed(), LATENCY);
			assert_eq!(landed(&mut recv).await.unwrap(), "two");
			assert_eq!(start.elapsed(), LATENCY + LATENCY / 2);
			assert_eq!(landed(&mut recv).await, None);
			assert_eq!(start.elapsed(), LATENCY + LATENCY / 2);
		});
	}

	#[test]
	fn a_finish_is_acknowledged_once_the_reader_takes_it() {
		kio::clock::sim::run(async {
			const LATENCY: Duration = Duration::from_millis(10);
			let (a, b) = pair(Config {
				latency: LATENCY,
				..Default::default()
			});
			let (mut send, mut recv) = uni(&a, &b);
			ready(write(&mut send, &Bytes::from_static(b"one"))).unwrap();
			assert_eq!(landed(&mut recv).await.unwrap(), "one");

			// Every message is taken, but the finish is still in flight: a writer that
			// closed the session now would beat its finish to the reader.
			send.finish().unwrap();
			assert!(send.poll_closed(&noop()).is_pending());
			assert_eq!(send.acknowledged(), Some(false));
			let start = kio::clock::now();
			assert_eq!(landed(&mut recv).await, None);
			assert_eq!(start.elapsed(), LATENCY);
			assert!(ready(send.poll_closed(&noop())).is_ok());
			assert_eq!(send.acknowledged(), Some(true));

			// A reader that stops before the finish lands leaves the writer finished.
			let (mut send, recv) = uni(&a, &b);
			send.finish().unwrap();
			assert!(send.poll_closed(&noop()).is_pending());
			drop(recv);
			assert!(ready(send.poll_closed(&noop())).is_ok());
		});
	}

	#[test]
	fn a_reset_skips_the_latency() {
		kio::clock::sim::run(async {
			let (a, b) = pair(Config {
				latency: Duration::from_millis(10),
				..Default::default()
			});
			let (mut send, mut recv) = uni(&a, &b);
			let start = kio::clock::now();
			ready(write(&mut send, &Bytes::from_static(b"dropped"))).unwrap();

			// A reader parked on a message in flight wakes for the reset that drops it.
			let count = Arc::new(Count::default());
			let waiter = kio::Waiter::new(Waker::from(count.clone()));
			assert!(poll::Chunks::poll_read_chunk(recv.get_mut(), &waiter, usize::MAX).is_pending());
			send.reset(2);
			assert!(count.0.load(Ordering::Relaxed) > 0, "the reset did not wake the reader");
			let read = poll::Chunks::poll_read_chunk(recv.get_mut(), &waiter, usize::MAX);
			assert!(matches!(ready(read), Err(Error::Reset(2))));
			assert_eq!(start.elapsed(), Duration::ZERO);
		});
	}

	#[test]
	fn finish_closes_once_drained() {
		let (a, b) = pair(Config::default());
		let (mut send, mut recv) = uni(&a, &b);

		ready(write(&mut send, &Bytes::from_static(b"done"))).unwrap();
		send.finish().unwrap();
		assert!(matches!(send.finish(), Err(Error::Closed)));

		// The writer's close waits for the reader to take every message.
		assert!(send.poll_closed(&noop()).is_pending());
		assert_eq!(text(read(&mut recv, usize::MAX)), "done");
		assert!(ready(send.poll_closed(&noop())).is_ok());
		assert_eq!(ready(read(&mut recv, usize::MAX)).unwrap(), None);

		// A reset after the finish is ignored, so the reader still gets everything.
		let (mut send, mut recv) = uni(&a, &b);
		ready(write(&mut send, &Bytes::from_static(b"kept"))).unwrap();
		send.finish().unwrap();
		send.reset(2);
		assert_eq!(text(read(&mut recv, usize::MAX)), "kept");
		assert_eq!(ready(read(&mut recv, usize::MAX)).unwrap(), None);

		// A reader that stops after taking everything leaves the writer finished.
		drop(recv);
		assert!(ready(send.poll_closed(&noop())).is_ok());

		// A reader that stops with messages unread fails the writer, finished or not.
		let (mut send, recv) = uni(&a, &b);
		ready(write(&mut send, &Bytes::from_static(b"unread"))).unwrap();
		send.finish().unwrap();
		drop(recv);
		assert!(matches!(ready(send.poll_closed(&noop())), Err(Error::Stopped(0))));
	}

	#[test]
	fn stop_fails_the_writer() {
		let (a, b) = pair(Config::default());
		let (mut send, mut recv) = uni(&a, &b);

		ready(write(&mut send, &Bytes::from_static(b"01234567"))).unwrap();
		assert!(send.poll_closed(&noop()).is_pending());

		recv.stop(3);
		assert!(matches!(ready(send.poll_closed(&noop())), Err(Error::Stopped(3))));
		assert!(matches!(
			ready(write(&mut send, &Bytes::from_static(b"x"))),
			Err(Error::Stopped(3))
		));
		assert!(matches!(send.finish(), Err(Error::Stopped(3))));
		assert!(matches!(ready(read(&mut recv, usize::MAX)), Err(Error::Closed)));
	}

	#[test]
	fn drops_reset_and_stop() {
		let (a, b) = pair(Config::default());

		let (send, mut recv) = uni(&a, &b);
		drop(send);
		assert!(matches!(ready(read(&mut recv, usize::MAX)), Err(Error::Reset(0))));

		let (send, recv) = uni(&a, &b);
		drop(recv);
		assert!(matches!(ready(send.poll_closed(&noop())), Err(Error::Stopped(0))));
	}

	#[test]
	fn bidi_streams_pair_up() {
		let (a, b) = pair(Config::default());
		let (mut a_send, mut a_recv) = ready(a.poll_open_bi(&noop(), Default::default())).unwrap();
		let (mut b_send, mut b_recv) = ready(b.poll_accept_bi(&noop())).unwrap();

		ready(write(&mut a_send, &Bytes::from_static(b"ping"))).unwrap();
		assert_eq!(text(read(&mut b_recv, usize::MAX)), "ping");
		ready(write(&mut b_send, &Bytes::from_static(b"pong"))).unwrap();
		assert_eq!(text(read(&mut a_recv, usize::MAX)), "pong");
	}

	#[test]
	fn close_drains_queued_messages() {
		let (a, b) = pair(Config {
			protocol: Some("moq".into()),
			..Default::default()
		});
		assert_eq!(b.protocol(), Some("moq"));
		let (mut send, mut recv) = uni(&a, &b);
		let (mut unfinished, mut unfinished_recv) = uni(&a, &b);
		let late = ready(a.poll_open_uni(&noop(), Default::default())).unwrap();

		ready(write(&mut send, &Bytes::from_static(b"last"))).unwrap();
		send.finish().unwrap();
		ready(write(&mut unfinished, &Bytes::from_static(b"part"))).unwrap();
		assert!(b.poll_closed(&noop()).is_pending());

		a.close(4, "bye");
		let closed = |err: Error| matches!(err, Error::Session { code: 4, ref reason } if reason == "bye");
		assert!(closed(ready(b.poll_closed(&noop()))));
		assert!(closed(
			ready(a.poll_open_uni(&noop(), Default::default())).err().unwrap()
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
		ready(b.poll_accept_uni(&noop())).unwrap();
		assert!(closed(ready(b.poll_accept_uni(&noop())).err().unwrap()));
	}

	/// Only a parked read or write waits on the close: one that returns at once leaves no
	/// registration behind, as every frame of a session pays for one.
	#[test]
	fn only_parked_streams_wait_on_the_close() {
		let (a, b) = pair(buffer(0));
		let (mut send, mut recv) = uni(&a, &b);
		let counted = || {
			let count = Arc::new(Count::default());
			(kio::Waiter::new(Waker::from(count.clone())), count)
		};

		let (ready_write, ready_write_count) = counted();
		assert_eq!(
			ready(send.poll_write_buf(&ready_write, &mut Bytes::from_static(b"one"))).unwrap(),
			3
		);
		let (ready_read, ready_read_count) = counted();
		let chunk = ready(poll::Chunks::poll_read_chunk(recv.get_mut(), &ready_read, usize::MAX));
		assert_eq!(chunk.unwrap().unwrap(), "one");

		// The channel is empty, so the read parks; refill it so the write parks too.
		let (parked_read, parked_read_count) = counted();
		assert!(poll::Chunks::poll_read_chunk(recv.get_mut(), &parked_read, usize::MAX).is_pending());
		let (mut other, _other_recv) = uni(&a, &b);
		ready(write(&mut other, &Bytes::from_static(b"two"))).unwrap();
		let (parked_write, parked_write_count) = counted();
		assert!(
			other
				.poll_write_buf(&parked_write, &mut Bytes::from_static(b"x"))
				.is_pending()
		);

		a.close(4, "bye");
		assert_eq!(ready_write_count.0.load(Ordering::Relaxed), 0);
		assert_eq!(ready_read_count.0.load(Ordering::Relaxed), 0);
		assert!(parked_read_count.0.load(Ordering::Relaxed) > 0);
		assert!(parked_write_count.0.load(Ordering::Relaxed) > 0);

		let closed = |err: Error| matches!(err, Error::Session { code: 4, .. });
		assert!(closed(ready(read(&mut recv, usize::MAX)).unwrap_err()));
		assert!(closed(ready(write(&mut other, &Bytes::from_static(b"x"))).unwrap_err()));
	}

	#[test]
	fn dropping_a_side_closes() {
		let (a, b) = pair(Config::default());
		let clone = a.clone();
		drop(a);
		assert!(b.poll_closed(&noop()).is_pending());
		drop(clone);
		assert!(matches!(ready(b.poll_closed(&noop())), Error::Session { code: 0, .. }));
	}

	#[test]
	fn datagrams() {
		let (a, b) = pair(Config::default());
		a.send_datagram(vec![0; MAX_DATAGRAM_SIZE + 1].into(), Default::default())
			.unwrap();
		for _ in 0..DATAGRAM_QUEUE {
			a.send_datagram("one".into(), Default::default()).unwrap();
		}
		// The peer's queue is full, so this one is lost, like the oversized one.
		a.send_datagram("two".into(), Default::default()).unwrap();
		for _ in 0..DATAGRAM_QUEUE {
			assert_eq!(ready(b.poll_recv_datagram(&noop())).unwrap(), "one");
		}
		assert!(b.poll_recv_datagram(&noop()).is_pending());
	}
}
