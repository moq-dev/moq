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

	use super::{Error, MaybeSend, MaybeSync, Stats};
	use bytes::BytesMut;
	use bytes::{Buf, BufMut, Bytes};

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

		/// Poll to open a unidirectional stream scheduled by `options`, which blocks while
		/// there are too many concurrent streams.
		fn poll_open_uni(
			&mut self,
			cx: &mut Context<'_>,
			options: SendOptions<'_, <Self::SendStream as SendStream>::Group>,
		) -> Poll<Result<Self::SendStream, Self::Error>>;

		/// Poll to open a bidirectional stream, its sending half scheduled by `options`, which
		/// blocks while there are too many concurrent streams.
		fn poll_open_bi(
			&mut self,
			cx: &mut Context<'_>,
			options: SendOptions<'_, <Self::SendStream as SendStream>::Group>,
		) -> Poll<Result<BiStreams<Self>, Self::Error>>;

		/// Open a send group at `priority`, or `None` when the transport orders streams by
		/// their own priority alone.
		///
		/// Streams that join a group, through [`SendOptions`] or [`SendStream::set_group`], and
		/// datagrams sent in it share bytes with the session's
		/// other groups: a higher priority group preempts every lower one, and groups of equal
		/// priority get an equal share of bytes however many streams each has queued. Streams
		/// outside every group go before all of them. The group is released once its last
		/// handle is dropped and its streams finish.
		fn send_group(&mut self, priority: i64) -> Option<<Self::SendStream as SendStream>::Group>;

		/// Send a datagram scheduled by `options`, best-effort.
		///
		/// The datagram is ranked among its group's streams by its priority, like a stream's
		/// own [priority](SendStream::set_priority), and goes ahead of a stream of equal
		/// priority. Without a group it is ranked among the ungrouped streams. A transport that
		/// cannot rank datagrams against streams ignores `options`.
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
			options: SendOptions<'_, <Self::SendStream as SendStream>::Group>,
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

		/// Open a unidirectional stream scheduled by `options`, waiting for stream credit if
		/// necessary.
		fn open_uni<'a>(
			&'a mut self,
			options: SendOptions<'a, <Self::SendStream as SendStream>::Group>,
		) -> OpenUni<'a, Self> {
			OpenUni { session: self, options }
		}

		/// Open a bidirectional stream, its sending half scheduled by `options`, waiting for
		/// stream credit if necessary.
		fn open_bi<'a>(
			&'a mut self,
			options: SendOptions<'a, <Self::SendStream as SendStream>::Group>,
		) -> OpenBi<'a, Self> {
			OpenBi { session: self, options }
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
	/// One in-flight open, which polls with the same options until it resolves.
	macro_rules! open_future {
		($(#[$doc:meta])* $name:ident, $poll:ident, $out:ty) => {
			$(#[$doc])*
			pub struct $name<'a, S: Session> {
				session: &'a mut S,
				options: SendOptions<'a, <S::SendStream as SendStream>::Group>,
			}

			impl<S: Session> Future for $name<'_, S> {
				type Output = $out;

				fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
					let this = self.get_mut();
					this.session.$poll(cx, this.options)
				}
			}
		};
	}

	open_future!(
		/// A pending [`Session::open_uni`].
		OpenUni, poll_open_uni, Result<S::SendStream, S::Error>);
	open_future!(
		/// A pending [`Session::open_bi`].
		OpenBi, poll_open_bi, Result<BiStreams<S>, S::Error>);
	poll_future!(
		/// A pending [`Session::recv_datagram`].
		RecvDatagram, Session, poll_recv_datagram, Result<Bytes, S::Error>);
	poll_future!(
		/// A pending [`Session::closed`].
		SessionClosed, Session, poll_closed, S::Error);
	poll_future!(
		/// A pending [`SendStream::closed`].
		SendClosed, SendStream, poll_closed, Result<(), S::Error>);
	poll_future!(
		/// A pending [`RecvStream::closed`].
		RecvClosed, RecvStream, poll_closed, Result<(), S::Error>);

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

	/// How an outgoing stream or datagram is scheduled, like W3C WebTransport's send options.
	///
	/// The default is ungrouped at priority 0.
	#[derive(Debug)]
	pub struct SendOptions<'a, G> {
		/// The send group to join, from [`Session::send_group`], or `None` for the ungrouped
		/// streams that go before every group.
		pub group: Option<&'a G>,
		/// The priority within the group, like [`SendStream::set_priority`]: higher goes first.
		pub priority: i64,
	}

	impl<G> Default for SendOptions<'_, G> {
		fn default() -> Self {
			Self {
				group: None,
				priority: 0,
			}
		}
	}

	impl<G> Clone for SendOptions<'_, G> {
		fn clone(&self) -> Self {
			*self
		}
	}

	impl<G> Copy for SendOptions<'_, G> {}

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

	/// An incoming stream with reads, stop codes, and closure notification.
	pub trait RecvStream: 'static {
		/// The error type for every operation on this stream.
		type Error: Error;

		/// Poll to read some data into the provided slice.
		///
		/// Returns the number of bytes read, or `None` once the peer has finished the
		/// stream. An empty `dst` reads nothing and returns `Some(0)`: asking for no
		/// bytes is not end of stream.
		fn poll_read(&mut self, cx: &mut Context<'_>, dst: &mut [u8]) -> Poll<Result<Option<usize>, Self::Error>>;

		/// Poll to read some data into the provided buffer, advancing it by the number
		/// of bytes read.
		///
		/// Override this to avoid a copy when the underlying transport already owns the
		/// bytes as a [`Bytes`], which can be handed to [`BufMut::put`] directly.
		fn poll_read_buf<B: BufMut>(
			&mut self,
			cx: &mut Context<'_>,
			buf: &mut B,
		) -> Poll<Result<Option<usize>, Self::Error>> {
			// Cap the slice: it is zeroed on every poll, Pending included, and a
			// read may be partial anyway.
			let len = buf.chunk_mut().len().min(64 * 1024);

			// A destination with no room is not a closed stream. Collapsing the two
			// would turn "buffer full" into "stream ended", which reads as truncation.
			if len == 0 {
				return Poll::Ready(Ok(Some(0)));
			}

			// `poll_read` is safe and may inspect its input, so initialize spare
			// capacity before exposing it as a byte slice.
			let chunk = buf.chunk_mut();
			let dst = unsafe {
				chunk.as_mut_ptr().write_bytes(0, len);
				std::slice::from_raw_parts_mut(chunk.as_mut_ptr(), len)
			};

			let size = match ready!(self.poll_read(cx, dst))? {
				Some(size) if size > 0 => size,
				Some(_) => return Poll::Ready(Ok(Some(0))),
				None => return Poll::Ready(Ok(None)),
			};

			assert!(size <= len, "transport read exceeded its destination");
			unsafe { buf.advance_mut(size) };

			Poll::Ready(Ok(Some(size)))
		}

		/// Poll for the next chunk of data, up to `max` bytes.
		///
		/// Override this when the transport can hand over a [`Bytes`] it already owns;
		/// the default allocates and copies.
		fn poll_read_chunk(&mut self, cx: &mut Context<'_>, max: usize) -> Poll<Result<Option<Bytes>, Self::Error>> {
			// As in `poll_read_buf`: asking for nothing is not end of stream.
			if max == 0 {
				return Poll::Ready(Ok(Some(Bytes::new())));
			}

			// Don't allocate too much. Override this to avoid the copy, or to use a
			// larger per-poll buffer.
			let capacity = max.min(8 * 1024);
			let mut buf = BytesMut::zeroed(capacity);

			let size = match ready!(self.poll_read(cx, &mut buf))? {
				Some(size) if size > 0 => size,
				Some(_) => return Poll::Ready(Ok(Some(Bytes::new()))),
				None => return Poll::Ready(Ok(None)),
			};

			assert!(size <= capacity, "transport read exceeded its destination");
			buf.truncate(size);

			Poll::Ready(Ok(Some(buf.freeze())))
		}

		/// Send a `STOP_SENDING` QUIC code, informing the peer that no more data will be
		/// read.
		///
		/// An implementation MUST do this on Drop otherwise flow control will be leaked.
		/// Call this method manually if you want to specify a code yourself.
		fn stop(&mut self, code: u32);

		/// Poll until the stream has been closed by either side.
		///
		/// This includes:
		/// - We received a RESET_STREAM via [`SendStream::reset`]
		/// - We sent a STOP_SENDING via [`stop`](Self::stop)
		/// - We received a FIN via [`SendStream::finish`] and read all data.
		fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>>;

		/// Read some bytes into the slice, or `None` once the stream is finished.
		fn read<'a>(&'a mut self, dst: &'a mut [u8]) -> Read<'a, Self> {
			Read { stream: self, dst }
		}

		/// Read some bytes into the buffer, advancing it, or `None` once finished.
		fn read_buf<'a, B: BufMut>(&'a mut self, buf: &'a mut B) -> ReadBuf<'a, Self, B> {
			ReadBuf { stream: self, buf }
		}

		/// Read the next chunk of data, up to `max` bytes, or `None` once finished.
		fn read_chunk(&mut self, max: usize) -> ReadChunk<'_, Self> {
			ReadChunk { stream: self, max }
		}

		/// Wait until the stream is closed by either side.
		fn closed(&mut self) -> RecvClosed<'_, Self> {
			RecvClosed(self)
		}
	}

	/// A pending [`RecvStream::read`].
	pub struct Read<'a, S: ?Sized> {
		stream: &'a mut S,
		dst: &'a mut [u8],
	}

	impl<S: RecvStream> Future for Read<'_, S> {
		type Output = Result<Option<usize>, S::Error>;

		fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
			let this = &mut *self;
			this.stream.poll_read(cx, this.dst)
		}
	}

	/// A pending [`RecvStream::read_buf`].
	pub struct ReadBuf<'a, S: ?Sized, B> {
		stream: &'a mut S,
		buf: &'a mut B,
	}

	impl<S: RecvStream, B: BufMut> Future for ReadBuf<'_, S, B> {
		type Output = Result<Option<usize>, S::Error>;

		fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
			let this = &mut *self;
			this.stream.poll_read_buf(cx, this.buf)
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
}

#[cfg(test)]
mod tests {
	use super::poll::RecvStream as _;
	use bytes::{Bytes, BytesMut};
	use std::task::{Context, Poll};
	struct Read;
	impl super::poll::RecvStream for Read {
		type Error = crate::Error;
		fn poll_read(&mut self, _: &mut Context<'_>, dst: &mut [u8]) -> Poll<Result<Option<usize>, Self::Error>> {
			// A safe transport may read its destination before overwriting it.
			assert!(dst.iter().all(|byte| *byte == 0));
			let size = dst.len().min(2);
			dst[..size].copy_from_slice(&b"hi"[..size]);
			Poll::Ready(Ok(Some(size)))
		}
		fn stop(&mut self, _: u32) {}
		fn poll_closed(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
			Poll::Ready(Ok(()))
		}
	}
	#[test]
	fn default_reads_initialize_spare_capacity_and_respect_limits() {
		let mut read = Read;
		let mut cx = Context::from_waker(std::task::Waker::noop());
		let mut buf = BytesMut::with_capacity(8);
		assert!(matches!(
			read.poll_read_buf(&mut cx, &mut buf),
			Poll::Ready(Ok(Some(2)))
		));
		assert_eq!(buf, b"hi"[..]);
		let Poll::Ready(Ok(Some(chunk))) = read.poll_read_chunk(&mut cx, 1) else {
			panic!("missing chunk")
		};
		assert_eq!(chunk, Bytes::from_static(b"h"));
		let Poll::Ready(Ok(Some(chunk))) = read.poll_read_chunk(&mut cx, 0) else {
			panic!("missing empty chunk")
		};
		assert!(chunk.is_empty());
		let mut empty: &mut [u8] = &mut [];
		assert!(matches!(
			read.poll_read_buf(&mut cx, &mut empty),
			Poll::Ready(Ok(Some(0)))
		));
	}

	#[test]
	fn default_read_buf_caps_the_initialized_slice_on_ready_and_pending() {
		struct ReadSpan(bool);
		impl super::poll::RecvStream for ReadSpan {
			type Error = crate::Error;
			fn poll_read(&mut self, _: &mut Context<'_>, dst: &mut [u8]) -> Poll<Result<Option<usize>, Self::Error>> {
				assert_eq!(
					dst.len(),
					64 * 1024,
					"one poll must not initialize the whole spare capacity"
				);
				assert!(dst.iter().all(|byte| *byte == 0));
				if self.0 {
					return Poll::Pending;
				}
				dst.fill(7);
				Poll::Ready(Ok(Some(dst.len())))
			}
			fn stop(&mut self, _: u32) {}
			fn poll_closed(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
				Poll::Pending
			}
		}
		for pending in [false, true] {
			let mut buf = BytesMut::with_capacity(256 * 1024);
			let mut cx = Context::from_waker(std::task::Waker::noop());
			let result = ReadSpan(pending).poll_read_buf(&mut cx, &mut buf);
			if pending {
				assert!(result.is_pending());
				assert!(buf.is_empty());
			} else {
				assert!(matches!(result, Poll::Ready(Ok(Some(65536)))));
				assert_eq!(buf.len(), 64 * 1024);
				assert!(buf.iter().all(|byte| *byte == 7));
			}
		}
	}
}
