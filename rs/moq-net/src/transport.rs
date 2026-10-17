//! The poll transport interface owned by `moq-net`.
//!
//! Implement these traits for a transport, or wrap a backend at the application
//! boundary. `moq-tokio`, `moq-wasm`, and `moq-uring` provide their own adapters, and
//! [`memory`] links two peers in one process.
//! The async helpers only drive the required poll methods. Thread-local transports
//! stay supported; [`poll::Boxable`] adds native thread bounds for boxed drivers.

use std::time::Duration;

pub mod memory;

/// A trait that is Send on native targets and empty on WASM.
#[cfg(not(target_family = "wasm"))]
pub trait MaybeSend: Send {}

/// A trait that is Sync on native targets and empty on WASM.
#[cfg(not(target_family = "wasm"))]
pub trait MaybeSync: Sync {}

#[cfg(not(target_family = "wasm"))]
impl<T: Send> MaybeSend for T {}

#[cfg(not(target_family = "wasm"))]
impl<T: Sync> MaybeSync for T {}

/// A trait that is Send on native targets and empty on WASM.
#[cfg(target_family = "wasm")]
pub trait MaybeSend {}

/// A trait that is Sync on native targets and empty on WASM.
#[cfg(target_family = "wasm")]
pub trait MaybeSync {}

#[cfg(target_family = "wasm")]
impl<T> MaybeSend for T {}

#[cfg(target_family = "wasm")]
impl<T> MaybeSync for T {}

/// Where the transport carries the request target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Binding {
	/// The request target is carried by the transport URI.
	Uri,
	/// The request target is carried by the MoQ SETUP.
	InBand,
}

/// One connection's cumulative wire counters, which its transport updates as it sends and receives.
///
/// Clones share the counters, so stats read them without polling the session or keeping the
/// connection alive, and the final totals stay readable after it closes. Each counter is
/// independent: a transport records the ones it tracks, and the rest stay zero.
#[derive(Clone, Debug, Default)]
pub struct Wire(std::sync::Arc<WireCounters>);

#[derive(Debug, Default)]
struct WireCounters {
	sent_bytes: std::sync::atomic::AtomicU64,
	received_bytes: std::sync::atomic::AtomicU64,
}

impl Wire {
	/// Create counters that start at zero.
	pub fn new() -> Self {
		Self::default()
	}

	/// The bytes sent so far, including transport framing and retransmissions.
	pub fn sent_bytes(&self) -> u64 {
		self.0.sent_bytes.load(std::sync::atomic::Ordering::Relaxed)
	}

	/// The bytes received so far, including transport framing.
	pub fn received_bytes(&self) -> u64 {
		self.0.received_bytes.load(std::sync::atomic::Ordering::Relaxed)
	}

	/// Record the cumulative bytes sent so far.
	pub fn set_sent_bytes(&self, total: u64) {
		self.0.sent_bytes.store(total, std::sync::atomic::Ordering::Relaxed);
	}

	/// Record the cumulative bytes received so far.
	pub fn set_received_bytes(&self, total: u64) {
		self.0.received_bytes.store(total, std::sync::atomic::Ordering::Relaxed);
	}
}

/// Connection-level statistics.
///
/// Methods return `Option`: `None` means the implementation doesn't track
/// this metric, while `Some(0)` means actually zero.
pub trait Stats {
	/// Total bytes sent over the connection, including retransmissions and overhead.
	fn bytes_sent(&self) -> Option<u64> {
		None
	}

	/// Total bytes received over the connection, including duplicate and overhead.
	fn bytes_received(&self) -> Option<u64> {
		None
	}

	/// Total bytes lost (detected via retransmission or acknowledgement).
	fn bytes_lost(&self) -> Option<u64> {
		None
	}

	/// Total number of datagrams sent.
	fn packets_sent(&self) -> Option<u64> {
		None
	}

	/// Total number of datagrams received.
	fn packets_received(&self) -> Option<u64> {
		None
	}

	/// Total number of datagrams detected as lost.
	fn packets_lost(&self) -> Option<u64> {
		None
	}

	/// Smoothed round-trip time estimate.
	fn rtt(&self) -> Option<Duration> {
		None
	}

	/// Estimated available send bandwidth, in bits per second.
	fn estimated_send_rate(&self) -> Option<u64> {
		None
	}
}

/// Default stats implementation that returns `None` for all metrics.
pub struct StatsUnavailable;
impl Stats for StatsUnavailable {}

/// The smallest stream and connection receive window a transport that leases may offer.
///
/// A session keeps each unit it is still assembling leased, and a unit larger than the
/// window can never complete, so the window has to fit the largest one with its framing:
/// a legacy announcement set, past any frame payload.
pub const MIN_RECEIVE_WINDOW: u64 = {
	// A type and a size prefix, each a varint of at most 8 bytes.
	const PREFIXES: u64 = 16;
	let message = <crate::lite::AnnounceInit<'static> as crate::lite::Message>::MAX_SIZE as u64 + PREFIXES;
	let frame = crate::group::MAX_CACHE_BYTES;
	if message > frame { message } else { frame }
};

/// The typed transport condition when no application code was received.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum CloseKind {
	/// The transport failed without a more specific classification.
	#[default]
	Transport,
	/// The connection reached its idle deadline.
	IdleTimeout,
	/// This endpoint closed the connection locally.
	Closed,
	/// The transport evicted this stream: incomplete messages had spent the receive window,
	/// and this stream ranked lowest.
	Evicted,
}

/// A transport failure with optional session and stream application codes.
///
/// Session codes and stream codes belong to separate registries.
pub trait Error: std::error::Error + MaybeSend + MaybeSync + 'static {
	/// The typed transport condition, independent of application close codes.
	fn close_kind(&self) -> CloseKind {
		CloseKind::Transport
	}
	/// Returns the error code and reason if this was an application error.
	///
	/// Close reasons are exposed as UTF-8 strings at the transport boundary.
	fn session_error(&self) -> Option<(u32, String)>;

	/// Returns the error code if this was a stream error.
	fn stream_error(&self) -> Option<u32> {
		None
	}
}

/// Poll operations and their async helpers.
pub mod poll {
	/// The outgoing and incoming halves of a bidirectional stream.
	pub type BiStreams<S> = (<S as Session>::SendStream, <S as Session>::RecvStream);
	use std::task::{Context, Poll, ready};

	use std::collections::VecDeque;

	use super::{Error, MaybeSend, MaybeSync, Stats};
	use bytes::{Buf, Bytes};

	/// A cloneable transport session with independent progress on each handle.
	///
	/// Poll operations may retain progress, but never borrow caller buffers between
	/// calls. A pending write may be retried with a shorter buffer. Terminal
	/// operations release retained resources; shared resources must not be held
	/// across a wait. Implementations register the supplied waker before Pending.
	pub trait Session: Clone + 'static {
		/// The outgoing stream type. Only the poll half is required, so a `!Send`
		/// session can hang `!Send` streams off it.
		type SendStream: SendStream;

		/// The incoming stream type. Only the poll half is required, so a `!Send`
		/// session can hang `!Send` streams off it.
		type RecvStream: RecvStream;

		/// The error type for every operation on this session.
		type Error: Error;

		/// Poll for a unidirectional stream created by the peer.
		fn poll_accept_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<Self::RecvStream, Self::Error>>;

		/// Poll for a bidirectional stream created by the peer.
		fn poll_accept_bi(&mut self, cx: &mut Context<'_>) -> Poll<Result<BiStreams<Self>, Self::Error>>;

		/// Poll to open a unidirectional stream, which blocks while there are too many
		/// concurrent streams.
		fn poll_open_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<Self::SendStream, Self::Error>>;

		/// Poll to open a bidirectional stream, which blocks while there are too many
		/// concurrent streams.
		fn poll_open_bi(&mut self, cx: &mut Context<'_>) -> Poll<Result<BiStreams<Self>, Self::Error>>;

		/// Open a send group at `priority`, or `None` when the transport orders streams by
		/// their own priority alone.
		///
		/// Streams that [join](SendStream::set_group) a group share bytes with the session's
		/// other groups: a higher priority group preempts every lower one, and groups of equal
		/// priority get an equal share of bytes however many streams each has queued. Streams
		/// outside every group go before all of them. The group is released once its last
		/// handle is dropped and its streams finish.
		fn send_group(&mut self, priority: i64) -> Option<<Self::SendStream as SendStream>::Group>;

		/// Send a datagram in `group` at `order`, best-effort.
		///
		/// `order` ranks the datagram among the group's streams like a stream's own
		/// [priority](SendStream::set_priority), and a datagram goes ahead of a stream of equal
		/// order. Without a group it is ranked among the ungrouped streams. A transport that
		/// cannot rank datagrams against streams ignores both.
		///
		/// Never blocks: a transport with no room drops a queued datagram, the lowest priority
		/// first where it can tell, exactly as the network is allowed to.
		///
		/// Accepting a datagram is not delivery. QUIC datagrams may still be dropped:
		/// - Network congestion.
		/// - Random packet loss.
		/// - Payload is larger than `max_datagram_size()`
		/// - Peer is not receiving datagrams.
		fn send_datagram(
			&mut self,
			payload: Bytes,
			group: Option<&<Self::SendStream as SendStream>::Group>,
			order: i64,
		) -> Result<(), Self::Error>;

		/// Poll for a datagram from the network.
		fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, Self::Error>>;

		/// The maximum size of a datagram that can be sent.
		fn max_datagram_size(&self) -> usize;

		/// Return the application protocol negotiated for this session, if any.
		///
		/// For WebTransport over HTTP/3 this is the selected WebTransport subprotocol;
		/// for raw QUIC it is the negotiated ALPN. Return `None` if the transport does
		/// not negotiate either or the ALPN is not valid UTF-8. This is required rather
		/// than defaulted: a transport that negotiates an application protocol and
		/// forgets to report it is a silent bug, and the default hid that.
		fn protocol(&self) -> Option<&str>;

		/// Return where this transport carries the request target.
		fn binding(&self) -> super::Binding;

		/// Close the connection immediately with a code and reason.
		///
		/// Idempotent, and deliberately infallible: closing an already-closed connection
		/// achieved what the caller asked for, and there is nothing they could do with an
		/// error.
		fn close(&mut self, code: u32, reason: &str);

		/// Poll until the connection is closed by either side.
		fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Self::Error>;

		/// Return connection-level statistics.
		///
		/// Return [`super::StatsUnavailable`] if the transport does not track them. Required
		/// rather than defaulted for the same reason as [`protocol`](Self::protocol).
		fn stats(&self) -> impl Stats;

		/// Return the wire counters this transport updates, or `None` when it tracks none.
		fn wire(&self) -> Option<super::Wire> {
			None
		}

		/// Accept the next unidirectional stream opened by the peer.
		fn accept_uni(&mut self) -> AcceptUni<'_, Self> {
			AcceptUni(self)
		}

		/// Accept the next bidirectional stream opened by the peer.
		fn accept_bi(&mut self) -> AcceptBi<'_, Self> {
			AcceptBi(self)
		}

		/// Open a unidirectional stream, waiting for stream credit if necessary.
		fn open_uni(&mut self) -> OpenUni<'_, Self> {
			OpenUni(self)
		}

		/// Open a bidirectional stream, waiting for stream credit if necessary.
		fn open_bi(&mut self) -> OpenBi<'_, Self> {
			OpenBi(self)
		}

		/// Receive the next datagram from the peer.
		fn recv_datagram(&mut self) -> RecvDatagram<'_, Self> {
			RecvDatagram(self)
		}

		/// Wait until the session is closed by either side, returning the reason.
		fn closed(&mut self) -> SessionClosed<'_, Self> {
			SessionClosed(self)
		}
	}

	/// One in-flight `poll_*` operation as a [`Future`]: the helpers below are
	/// named types (not `impl Future`) so their `Send`-ness stays inferred from
	/// the transport; an opaque return type in a trait would hide it.
	macro_rules! poll_future {
		($(#[$doc:meta])* $name:ident, $bound:path, $poll:ident, $out:ty) => {
			$(#[$doc])*
			pub struct $name<'a, S: ?Sized>(&'a mut S);

			impl<S: $bound> Future for $name<'_, S> {
				type Output = $out;

				fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
					self.0.$poll(cx)
				}
			}
		};
	}

	poll_future!(
		/// A pending [`Session::accept_uni`].
		AcceptUni, Session, poll_accept_uni, Result<S::RecvStream, S::Error>);
	poll_future!(
		/// A pending [`Session::accept_bi`].
		AcceptBi, Session, poll_accept_bi, Result<BiStreams<S>, S::Error>);
	poll_future!(
		/// A pending [`Session::open_uni`].
		OpenUni, Session, poll_open_uni, Result<S::SendStream, S::Error>);
	poll_future!(
		/// A pending [`Session::open_bi`].
		OpenBi, Session, poll_open_bi, Result<BiStreams<S>, S::Error>);
	poll_future!(
		/// A pending [`Session::recv_datagram`].
		RecvDatagram, Session, poll_recv_datagram, Result<Bytes, S::Error>);
	poll_future!(
		/// A pending [`Session::closed`].
		SessionClosed, Session, poll_closed, S::Error);
	poll_future!(
		/// A pending [`SendStream::closed`].
		SendClosed, SendStream, poll_closed, Result<(), S::Error>);

	/// A transport whose session, streams, and errors can be captured by the
	/// boxed drivers (`Send` boxes on native): what the moq-transport path
	/// requires until it too becomes named machines. Implemented automatically.
	pub trait Boxable:
		Session<
			SendStream: MaybeSend + SendStream<Group: MaybeSend + MaybeSync>,
			RecvStream: MaybeSend,
			Error: MaybeSend,
		> + MaybeSend
		+ MaybeSync
	{
	}

	impl<S> Boxable for S where
		S: Session<
				SendStream: MaybeSend + SendStream<Group: MaybeSend + MaybeSync>,
				RecvStream: MaybeSend,
				Error: MaybeSend,
			> + MaybeSend
			+ MaybeSync
	{
	}

	/// The send group of a session's streams.
	#[cfg(feature = "async")]
	pub(crate) type GroupOf<S> = <<S as Session>::SendStream as SendStream>::Group;

	/// A transport's send group, from [`Session::send_group`].
	///
	/// Clones share the group.
	pub trait SendGroup: Clone + 'static {
		/// Change the group's priority: higher preempts lower.
		fn set_priority(&mut self, priority: i64);
	}

	/// The send group of a transport that has none, which cannot be constructed.
	#[derive(Clone, Debug)]
	pub enum Ungrouped {}

	impl SendGroup for Ungrouped {
		fn set_priority(&mut self, _: i64) {
			match *self {}
		}
	}

	/// An outgoing stream with partial writes, priorities, and reset codes.
	pub trait SendStream: 'static {
		/// The error type for every operation on this stream.
		type Error: Error;

		/// The transport's send group, [`Ungrouped`] for a transport without them.
		type Group: SendGroup;

		/// Poll to write some of the buffer to the stream, returning how many bytes were
		/// written. See [`poll_write_buf`](Self::poll_write_buf) for the partial-write
		/// contract, which this shares.
		fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Self::Error>>;

		/// Poll to write some of the given buffer to the stream, advancing it by the
		/// number of bytes written. This may be less than the whole buffer, so callers
		/// loop.
		///
		/// # Partial writes
		///
		/// Implementations must not advance `buf` past the bytes they accepted for
		/// sending. (Whether those bytes reach the peer is a separate matter: a reset
		/// or a dead connection can still discard accepted bytes.) A returned
		/// [`Poll::Pending`] must leave `buf` exactly where the accepted bytes end.
		/// Callers race writes against other work, so a byte taken from `buf` but never
		/// accepted becomes a silent hole in the stream, which the peer decodes as a
		/// truncated or garbage frame. Wait for send capacity *before* consuming from
		/// `buf`, never after.
		///
		/// Override this to avoid a copy when the underlying transport can take
		/// ownership of `buf`'s bytes: see [`Buf::copy_to_bytes`], which is free for a
		/// [`Bytes`] source.
		fn poll_write_buf<B: Buf>(&mut self, cx: &mut Context<'_>, buf: &mut B) -> Poll<Result<usize, Self::Error>> {
			let size = ready!(self.poll_write(cx, buf.chunk()))?;
			buf.advance(size);
			Poll::Ready(Ok(size))
		}

		/// Set the priority within the stream's send group, or session-wide when ungrouped.
		///
		/// Streams with higher values will be sent first, but are not guaranteed to
		/// arrive first. This matches the W3C WebTransport `sendOrder` convention (and
		/// quinn's scheduler).
		///
		/// Backends with a narrower priority saturate or approximate it, so treat the
		/// ordering as best-effort.
		fn set_priority(&mut self, priority: i64);

		/// Move the stream into `group`, which belongs to this stream's session.
		fn set_group(&mut self, group: &Self::Group);

		/// Mark the stream as finished, erroring on any future writes.
		///
		/// [`reset`](Self::reset) can still be called to abandon any queued data.
		/// [`poll_closed`](Self::poll_closed) should resolve when the FIN is acknowledged
		/// by the peer.
		///
		/// NOTE: Quinn implicitly calls this on Drop, but it's a common footgun.
		/// Implementations SHOULD [`reset`](Self::reset) on Drop instead.
		fn finish(&mut self) -> Result<(), Self::Error>;

		/// Immediately closes the stream and discards any remaining data.
		///
		/// This translates into a RESET_STREAM QUIC code.
		/// The peer may not receive the reset code if the stream is already closed.
		///
		/// Takes `&mut self` rather than `self` even though it is terminal, so a caller
		/// can still [`poll_closed`](Self::poll_closed) afterwards to await the peer ,
		/// and so it matches [`finish`](Self::finish), which must not consume the stream
		/// for exactly that reason.
		fn reset(&mut self, code: u32);

		/// Poll until the stream is closed by either side.
		///
		/// This includes:
		/// - We sent a RESET_STREAM via [`reset`](Self::reset)
		/// - We received a STOP_SENDING via [`RecvStream::stop`]
		/// - A FIN is acknowledged by the peer via [`finish`](Self::finish)
		///
		/// Some implementations do not support FIN acknowledgement, in which case this
		/// resolves once the FIN is sent.
		fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>>;

		/// Write some of the buffer, returning how many bytes were accepted.
		fn write<'a>(&'a mut self, buf: &'a [u8]) -> Write<'a, Self> {
			Write { stream: self, buf }
		}

		/// Write some of the buffer, advancing it by the bytes accepted.
		fn write_buf<'a, B: Buf>(&'a mut self, buf: &'a mut B) -> WriteBuf<'a, Self, B> {
			WriteBuf { stream: self, buf }
		}

		/// Write the entire chunk to the stream.
		fn write_chunk(&mut self, chunk: Bytes) -> WriteChunk<'_, Self> {
			WriteChunk { stream: self, chunk }
		}

		/// Wait until the stream is closed by either side.
		fn closed(&mut self) -> SendClosed<'_, Self> {
			SendClosed(self)
		}
	}

	/// A pending [`SendStream::write`].
	pub struct Write<'a, S: ?Sized> {
		stream: &'a mut S,
		buf: &'a [u8],
	}

	impl<S: SendStream> Future for Write<'_, S> {
		type Output = Result<usize, S::Error>;

		fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
			let this = &mut *self;
			this.stream.poll_write(cx, this.buf)
		}
	}

	/// A pending [`SendStream::write_buf`].
	pub struct WriteBuf<'a, S: ?Sized, B> {
		stream: &'a mut S,
		buf: &'a mut B,
	}

	impl<S: SendStream, B: Buf> Future for WriteBuf<'_, S, B> {
		type Output = Result<usize, S::Error>;

		fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
			let this = &mut *self;
			this.stream.poll_write_buf(cx, this.buf)
		}
	}

	/// A pending [`SendStream::write_chunk`].
	pub struct WriteChunk<'a, S: ?Sized> {
		stream: &'a mut S,
		chunk: Bytes,
	}

	impl<S: SendStream> Future for WriteChunk<'_, S> {
		type Output = Result<(), S::Error>;

		fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
			let this = &mut *self;
			while !this.chunk.is_empty() {
				ready!(this.stream.poll_write_buf(cx, &mut this.chunk))?;
			}
			Poll::Ready(Ok(()))
		}
	}

	/// An incoming stream, read through leases of the bytes its transport buffers.
	///
	/// Leased bytes stay in the transport, counted against its flow control, until
	/// [`Lease::consume`] releases them, so an incomplete message holds receive credit
	/// rather than memory of its own. When incomplete messages leave no credit for any of
	/// them to finish, a transport that bounds them evicts the lowest
	/// [priority](Self::set_priority) holder, whose next lease fails. A transport that
	/// returns credit as it reads, adapted with [`Buffered`], is not bounded this way.
	pub trait RecvStream: 'static {
		/// The error type for every operation on this stream.
		type Error: Error;

		/// The unconsumed bytes borrowed from this stream.
		type Lease<'a>: Lease
		where
			Self: 'a;

		/// Poll until at least `want` bytes are leased, or the peer finished the stream with
		/// fewer.
		///
		/// The bytes keep their flow-control credit until [`Lease::consume`]. Dropping the
		/// lease consumes nothing, so the next poll leases them again. A `want` the stream's
		/// receive window can never hold is an error rather than a wait, and a `want` of 0
		/// never waits.
		///
		/// A lease may borrow the transport's connection state, so release it before
		/// touching any other stream of the session.
		fn poll_lease(&mut self, cx: &mut Context<'_>, want: usize) -> Poll<Result<Self::Lease<'_>, Self::Error>>;

		/// Rank this stream for eviction when incomplete messages exhaust the receive window.
		///
		/// Higher keeps it longer, the convention of [`SendStream::set_priority`]. A stream
		/// starts at 0, and ties evict the newest stream first. A transport without eviction
		/// ignores it.
		fn set_priority(&mut self, priority: i64);

		/// Send a `STOP_SENDING` QUIC code, informing the peer that no more data will be
		/// read, and discard what is buffered.
		///
		/// An implementation MUST do this on Drop otherwise flow control will be leaked.
		/// Call this method manually if you want to specify a code yourself.
		fn stop(&mut self, code: u32);

		/// Poll for the next chunk, up to `max` bytes, or `None` once the peer finished the
		/// stream. Its credit returns as it is read.
		///
		/// An empty `max` reads nothing and returns an empty chunk: asking for no bytes is
		/// not end of stream.
		fn poll_read_chunk(&mut self, cx: &mut Context<'_>, max: usize) -> Poll<Result<Option<Bytes>, Self::Error>> {
			if max == 0 {
				return Poll::Ready(Ok(Some(Bytes::new())));
			}
			let mut lease = ready!(self.poll_lease(cx, 1))?;
			if lease.is_empty() {
				return Poll::Ready(Ok(None));
			}
			let chunk = lease.take(max);
			lease.consume(0);
			Poll::Ready(Ok(Some(chunk)))
		}

		/// Read the next chunk, up to `max` bytes, or `None` once finished.
		fn read_chunk(&mut self, max: usize) -> ReadChunk<'_, Self> {
			ReadChunk { stream: self, max }
		}
	}

	/// Unconsumed bytes borrowed from a [`RecvStream`] by [`poll_lease`](RecvStream::poll_lease).
	pub trait Lease {
		/// The number of leased bytes.
		fn len(&self) -> usize;

		/// Whether the peer finished the stream right after the leased bytes.
		fn is_fin(&self) -> bool;

		/// The leased bytes in order, one contiguous chunk at a time.
		fn chunks(&self) -> impl Iterator<Item = &[u8]>;

		/// Hand out up to `max` leading bytes without copying, stopping at a chunk boundary.
		///
		/// They leave the lease but keep their credit until [`consume`](Self::consume), which
		/// is how a frame reaches its consumer before the whole of it has arrived.
		fn take(&mut self, max: usize) -> Bytes;

		/// Release the first `n` leased bytes, and the credit of every byte taken so far.
		///
		/// # Panics
		/// If `n` exceeds [`len`](Self::len).
		fn consume(self, n: usize);

		/// Whether nothing is leased.
		fn is_empty(&self) -> bool {
			self.len() == 0
		}

		/// The first contiguous chunk, empty when nothing is leased.
		fn chunk(&self) -> &[u8] {
			self.chunks().next().unwrap_or_default()
		}

		/// The leased bytes as a [`Buf`], across chunk boundaries.
		fn buf(&self) -> impl Buf + '_ {
			let mut buf = LeaseBuf {
				chunk: &[],
				rest: self.chunks(),
				remaining: self.len(),
			};
			buf.fill();
			buf
		}
	}

	/// A [`Lease`] read as a [`Buf`].
	struct LeaseBuf<'a, I> {
		chunk: &'a [u8],
		rest: I,
		remaining: usize,
	}

	impl<'a, I: Iterator<Item = &'a [u8]>> LeaseBuf<'a, I> {
		/// Move to the next non-empty chunk once this one is spent, so `chunk` is never empty
		/// while bytes remain.
		fn fill(&mut self) {
			while self.chunk.is_empty() {
				match self.rest.next() {
					Some(chunk) => self.chunk = chunk,
					None => return,
				}
			}
		}
	}

	impl<'a, I: Iterator<Item = &'a [u8]>> Buf for LeaseBuf<'a, I> {
		fn remaining(&self) -> usize {
			self.remaining
		}

		fn chunk(&self) -> &[u8] {
			self.chunk
		}

		fn advance(&mut self, mut cnt: usize) {
			assert!(cnt <= self.remaining, "advanced past the lease");
			self.remaining -= cnt;
			loop {
				self.fill();
				let step = cnt.min(self.chunk.len());
				self.chunk = &self.chunk[step..];
				cnt -= step;
				if cnt == 0 {
					self.fill();
					return;
				}
			}
		}
	}

	/// A pending [`RecvStream::read_chunk`].
	pub struct ReadChunk<'a, S: ?Sized> {
		stream: &'a mut S,
		max: usize,
	}

	impl<S: RecvStream> Future for ReadChunk<'_, S> {
		type Output = Result<Option<Bytes>, S::Error>;

		fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
			let this = &mut *self;
			this.stream.poll_read_chunk(cx, this.max)
		}
	}

	/// A receive stream read as plain chunks, whose flow-control credit returns as it reads.
	///
	/// [`Buffered`] turns one into a [`RecvStream`].
	pub trait Chunks: 'static {
		/// The error type for every operation on this stream.
		type Error: Error;

		/// Poll for the next chunk, up to `max` bytes, or `None` once the peer finished the
		/// stream.
		fn poll_read_chunk(&mut self, cx: &mut Context<'_>, max: usize) -> Poll<Result<Option<Bytes>, Self::Error>>;

		/// Send a `STOP_SENDING` code. See [`RecvStream::stop`].
		fn stop(&mut self, code: u32);
	}

	/// Leases over a transport that cannot keep bytes unconsumed, buffering what it reads.
	///
	/// The transport returns credit as it reads, so incomplete messages are bounded by the
	/// largest message each, not by its receive window, and nothing is evicted.
	pub struct Buffered<S> {
		inner: S,
		chunks: VecDeque<Bytes>,
		len: usize,
		fin: bool,
	}

	impl<S> Buffered<S> {
		/// Lease over `inner`.
		pub fn new(inner: S) -> Self {
			Self {
				inner,
				chunks: VecDeque::new(),
				len: 0,
				fin: false,
			}
		}

		/// The wrapped stream.
		pub fn get_ref(&self) -> &S {
			&self.inner
		}

		/// The wrapped stream.
		pub fn get_mut(&mut self) -> &mut S {
			&mut self.inner
		}
	}

	impl<S: Chunks> RecvStream for Buffered<S> {
		type Error = S::Error;
		type Lease<'a>
			= BufferedLease<'a>
		where
			S: 'a;

		fn poll_lease(&mut self, cx: &mut Context<'_>, want: usize) -> Poll<Result<BufferedLease<'_>, S::Error>> {
			while self.len < want && !self.fin {
				// Whatever the transport has in one piece: a transport handing over whole
				// messages, like the memory one, keeps a large frame in a single chunk, so it
				// reaches the consumer without a copy. Each read still ends at what has arrived.
				match ready!(self.inner.poll_read_chunk(cx, usize::MAX))? {
					Some(chunk) if chunk.is_empty() => {}
					Some(chunk) => {
						self.len += chunk.len();
						self.chunks.push_back(chunk);
					}
					None => self.fin = true,
				}
			}
			Poll::Ready(Ok(BufferedLease {
				chunks: &mut self.chunks,
				len: &mut self.len,
				fin: self.fin,
			}))
		}

		fn set_priority(&mut self, _priority: i64) {}

		fn stop(&mut self, code: u32) {
			self.chunks.clear();
			self.len = 0;
			self.inner.stop(code);
		}
	}

	/// A [`Lease`] of a [`Buffered`] stream.
	pub struct BufferedLease<'a> {
		chunks: &'a mut VecDeque<Bytes>,
		len: &'a mut usize,
		fin: bool,
	}

	impl Lease for BufferedLease<'_> {
		fn len(&self) -> usize {
			*self.len
		}

		fn is_fin(&self) -> bool {
			self.fin
		}

		fn chunks(&self) -> impl Iterator<Item = &[u8]> {
			self.chunks.iter().map(|chunk| &chunk[..])
		}

		fn take(&mut self, max: usize) -> Bytes {
			let Some(front) = self.chunks.front_mut() else {
				return Bytes::new();
			};
			let chunk = match front.len() > max {
				true => front.split_to(max),
				false => self.chunks.pop_front().expect("front exists"),
			};
			*self.len -= chunk.len();
			chunk
		}

		fn consume(self, mut n: usize) {
			assert!(n <= *self.len, "consumed {n} of {} leased bytes", *self.len);
			*self.len -= n;
			while n > 0 {
				let front = self.chunks.front_mut().expect("counted above");
				if front.len() > n {
					front.advance(n);
					return;
				}
				n -= front.len();
				self.chunks.pop_front();
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::poll::{Buffered, Chunks, Lease as _, RecvStream as _};
	use bytes::{Buf, Bytes};
	use std::collections::VecDeque;
	use std::task::{Context, Poll};

	/// Hands back one queued chunk per poll, then blocks, or finishes once told to.
	#[derive(Default)]
	struct Queue {
		chunks: VecDeque<&'static [u8]>,
		fin: bool,
		stopped: Option<u32>,
	}

	impl Chunks for Queue {
		type Error = crate::Error;
		fn poll_read_chunk(&mut self, _: &mut Context<'_>, max: usize) -> Poll<Result<Option<Bytes>, Self::Error>> {
			match self.chunks.pop_front() {
				Some(chunk) => {
					let n = chunk.len().min(max);
					if n < chunk.len() {
						self.chunks.push_front(&chunk[n..]);
					}
					Poll::Ready(Ok(Some(Bytes::from_static(&chunk[..n]))))
				}
				None if self.fin => Poll::Ready(Ok(None)),
				None => Poll::Pending,
			}
		}
		fn stop(&mut self, code: u32) {
			self.stopped = Some(code);
		}
	}

	fn buffered(chunks: &[&'static [u8]], fin: bool) -> Buffered<Queue> {
		Buffered::new(Queue {
			chunks: chunks.iter().copied().collect(),
			fin,
			stopped: None,
		})
	}

	/// A lease waits for `want` across chunks, and dropping it keeps every byte for the next.
	#[test]
	fn lease_waits_for_want_and_drop_consumes_nothing() {
		let mut cx = Context::from_waker(std::task::Waker::noop());
		let mut stream = buffered(&[b"ab", b"cd"], false);

		{
			let Poll::Ready(Ok(lease)) = stream.poll_lease(&mut cx, 3) else {
				panic!("both chunks were ready")
			};
			assert_eq!(lease.len(), 4);
			assert_eq!(lease.chunk(), b"ab");
			let mut all = [0u8; 4];
			lease.buf().copy_to_slice(&mut all);
			assert_eq!(&all, b"abcd");
		}

		assert!(stream.poll_lease(&mut cx, 5).is_pending(), "nothing more arrived");
		let Poll::Ready(Ok(lease)) = stream.poll_lease(&mut cx, 1) else {
			panic!("the dropped lease's bytes are still there")
		};
		assert_eq!(lease.len(), 4);
		lease.consume(3);

		let Poll::Ready(Ok(lease)) = stream.poll_lease(&mut cx, 0) else {
			panic!("want 0 never waits")
		};
		assert_eq!(lease.chunk(), b"d");
		assert!(!lease.is_fin());
	}

	/// Taken bytes leave the lease, and a finished stream leases what is left with its FIN.
	#[test]
	fn take_hands_out_chunks_and_fin_ends_short() {
		let mut cx = Context::from_waker(std::task::Waker::noop());
		let mut stream = buffered(&[b"abc", b"de"], true);

		let Poll::Ready(Ok(mut lease)) = stream.poll_lease(&mut cx, 10) else {
			panic!("a finished stream leases short")
		};
		assert!(lease.is_fin());
		assert_eq!(lease.take(2), Bytes::from_static(b"ab"));
		assert_eq!(
			lease.take(10),
			Bytes::from_static(b"c"),
			"take stops at a chunk boundary"
		);
		assert_eq!(lease.len(), 2);
		lease.consume(0);

		assert_eq!(
			stream.read_chunk(10).now_or_never_ready(),
			Some(Bytes::from_static(b"de"))
		);
		assert_eq!(stream.read_chunk(10).now_or_never_ready(), None);
		assert_eq!(stream.read_chunk(0).now_or_never_ready(), Some(Bytes::new()));
	}

	/// A large chunk the transport hands over whole reaches the reader as that same
	/// allocation, so a frame from the memory transport is never copied.
	#[test]
	fn a_large_chunk_is_leased_whole() {
		static FRAME: [u8; 1 << 20] = [7; 1 << 20];
		let mut cx = Context::from_waker(std::task::Waker::noop());
		let mut stream = buffered(&[&FRAME], true);

		let Poll::Ready(Ok(mut lease)) = stream.poll_lease(&mut cx, 1) else {
			panic!("the chunk was ready")
		};
		let chunk = lease.take(usize::MAX);
		assert_eq!(chunk.len(), FRAME.len());
		assert_eq!(chunk.as_ptr(), FRAME.as_ptr(), "the chunk was not copied");
	}

	/// Stopping discards what was buffered and stops the transport.
	#[test]
	fn stop_discards_the_buffer() {
		let mut cx = Context::from_waker(std::task::Waker::noop());
		let mut stream = buffered(&[b"abc"], false);
		assert!(stream.poll_lease(&mut cx, 1).is_ready());
		stream.stop(7);
		assert_eq!(stream.get_ref().stopped, Some(7));
		assert!(stream.poll_lease(&mut cx, 1).is_pending());
	}

	trait NowOrNever {
		fn now_or_never_ready(self) -> Option<Bytes>;
	}

	impl<F: Future<Output = Result<Option<Bytes>, crate::Error>>> NowOrNever for F {
		fn now_or_never_ready(self) -> Option<Bytes> {
			let mut cx = Context::from_waker(std::task::Waker::noop());
			match std::pin::pin!(self).poll(&mut cx) {
				Poll::Ready(res) => res.unwrap(),
				Poll::Pending => panic!("pending"),
			}
		}
	}
}
