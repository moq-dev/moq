//! The poll transport interface owned by `moq-net`.
//!
//! Implement these traits for a transport, or wrap a backend at the application
//! boundary. `moq-tokio`, `moq-wasm`, and `moq-uring` provide their own adapters, and
//! [`memory`] links two peers in one process.
//! The async helpers only drive the required poll methods. Sessions take a
//! [`poll::Boxable`] transport, which adds native thread bounds; wrap a thread-local
//! transport in a type that checks the thread on use (`moq-uring` has one).

use std::time::Duration;

#[cfg(feature = "async")]
pub(crate) mod boxed;
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

/// Where a session's request target travels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Address {
	/// The transport's own request URI carries it (WebTransport, qmux over WebSocket).
	Uri,
	/// The MoQ SETUP carries it, because the transport has no request URI.
	Setup,
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

/// The rate a transport's sender probes up to, which the session sets and the transport reads.
///
/// A sender raises the congestion estimate while the connection is app-limited below
/// this target, so a rate the session wants to send can be funded once the path has room.
/// The session writes its [`Allocator`](crate::bandwidth::Allocator)'s target here, the
/// reservations' ceilings summed unless overridden; the transport reads it whenever it
/// decides how to fill its pacing.
/// Probing to it is local, so it never reaches the wire.
///
/// Clones share one value, which starts at zero: nothing to probe for.
#[derive(Clone, Debug, Default)]
pub struct Probe(std::sync::Arc<std::sync::atomic::AtomicU64>);

impl Probe {
	/// Create a target of zero.
	pub fn new() -> Self {
		Self::default()
	}

	/// The rate to probe up to; zero means don't probe.
	pub fn target(&self) -> crate::bandwidth::Rate {
		crate::bandwidth::Rate::from_bps(self.0.load(std::sync::atomic::Ordering::Relaxed))
	}

	/// Set the rate to probe up to.
	pub fn set_target(&self, rate: crate::bandwidth::Rate) {
		self.0.store(rate.as_bps(), std::sync::atomic::Ordering::Relaxed);
	}
}

/// Connection-level metrics a transport reports, beyond the byte counts its [`Wire`] holds.
///
/// Methods return `Option`: `None` means the implementation doesn't track
/// this metric, while `Some(0)` means actually zero.
pub trait Metrics {
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

	/// Total receive streams evicted because incomplete messages exhausted the receive window.
	fn streams_evicted(&self) -> Option<u64> {
		None
	}
}

/// The metrics of a transport that tracks none.
struct Untracked;
impl Metrics for Untracked {}

/// The smallest connection receive window a transport that leases may offer.
///
/// A session keeps each unit it is still assembling leased, and a unit larger than the
/// window can never complete, so the window has to fit the largest one with its framing:
/// a legacy announcement set, past any frame segment. A frame returns its credit a
/// segment at a time, so its size never matters here.
pub const MIN_RECEIVE_WINDOW: u64 = {
	// A type and a size prefix, each a varint of at most 8 bytes.
	const PREFIXES: u64 = 16;
	let message = <crate::lite::AnnounceInit as crate::lite::Message>::MAX_SIZE as u64 + PREFIXES;
	let segment = crate::frame::SEGMENT as u64;
	if message > segment { message } else { segment }
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

	use super::{Error, MaybeSend, MaybeSync, Metrics, Untracked};
	use bytes::{Buf, Bytes, BytesMut};

	/// A transport session, shared by reference between the tasks that use it.
	///
	/// Every operation takes `&self` and keeps no per-caller progress between polls: the
	/// caller owns the [`kio::Waiter`] it parks, so an abandoned wait leaves nothing behind.
	/// Share a session with [`Arc`](std::sync::Arc) rather than cloning it. Poll operations
	/// never borrow caller buffers between calls, and shared resources must not be held
	/// across a wait. Implementations register the supplied waiter before Pending.
	pub trait Session: 'static {
		/// The outgoing stream type.
		type SendStream: SendStream;

		/// The incoming stream type.
		type RecvStream: RecvStream;

		/// The error type for every operation on this session.
		type Error: Error;

		/// Poll for a unidirectional stream created by the peer.
		fn poll_accept_uni(&self, waiter: &kio::Waiter) -> Poll<Result<Self::RecvStream, Self::Error>>;

		/// Poll for a bidirectional stream created by the peer.
		fn poll_accept_bi(&self, waiter: &kio::Waiter) -> Poll<Result<BiStreams<Self>, Self::Error>>;

		/// Poll to open a unidirectional stream scheduled by `options`, which blocks while
		/// there are too many concurrent streams.
		fn poll_open_uni(
			&self,
			waiter: &kio::Waiter,
			options: SendOptions<'_, <Self::SendStream as SendStream>::Group>,
		) -> Poll<Result<Self::SendStream, Self::Error>>;

		/// Poll to open a bidirectional stream, its sending half scheduled by `options`, which
		/// blocks while there are too many concurrent streams.
		fn poll_open_bi(
			&self,
			waiter: &kio::Waiter,
			options: SendOptions<'_, <Self::SendStream as SendStream>::Group>,
		) -> Poll<Result<BiStreams<Self>, Self::Error>>;

		/// Open a request stream without waiting for stream credit.
		///
		/// `None` refuses the request before opening a stream. `Pending` means the stream
		/// already has credit but its transport header is waiting for send capacity.
		/// Native QUIC and qmux implement this admission check. Iroh retains its waiting
		/// open behavior, and browser-backed sessions retain their browser's behavior.
		fn poll_open_request(
			&self,
			waiter: &kio::Waiter,
			options: SendOptions<'_, <Self::SendStream as SendStream>::Group>,
		) -> Poll<Result<Option<BiStreams<Self>>, Self::Error>>;

		/// Open a send group at `priority`, or `None` when the transport orders streams by
		/// their own priority alone.
		///
		/// Streams that join a group, through [`SendOptions`] or [`SendStream::set_group`], and
		/// datagrams sent in it share bytes with the session's
		/// other groups: a higher priority group preempts every lower one, and groups of equal
		/// priority get an equal share of bytes however many streams each has queued. Streams
		/// outside every group go before all of them. The group is released once its last
		/// handle is dropped and its streams finish.
		fn send_group(&self, priority: i64) -> Option<<Self::SendStream as SendStream>::Group>;

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
			&self,
			payload: Bytes,
			options: SendOptions<'_, <Self::SendStream as SendStream>::Group>,
		) -> Result<(), Self::Error>;

		/// Poll for a datagram from the network.
		fn poll_recv_datagram(&self, waiter: &kio::Waiter) -> Poll<Result<Bytes, Self::Error>>;

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
		fn address(&self) -> super::Address;

		/// Close the connection immediately with a code and reason.
		///
		/// Idempotent, and deliberately infallible: closing an already-closed connection
		/// achieved what the caller asked for, and there is nothing they could do with an
		/// error.
		fn close(&self, code: u32, reason: &str);

		/// Poll until the connection is closed by either side.
		fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Self::Error>;

		/// Return connection-level metrics; by default, none are tracked.
		fn metrics(&self) -> impl Metrics {
			Untracked
		}

		/// Return the wire counters this transport updates, or `None` when it tracks none.
		fn wire(&self) -> Option<super::Wire> {
			None
		}

		/// Return the target this transport's sender probes up to, or `None` when it has no sender.
		///
		/// The session sets the target; see [`super::Probe`]. Without a sender nothing probes,
		/// so the congestion estimate stops rising while the connection is app-limited.
		fn probe(&self) -> Option<super::Probe> {
			None
		}

		/// Accept the next unidirectional stream opened by the peer.
		fn accept_uni(&self) -> AcceptUni<'_, Self> {
			AcceptUni::new(self)
		}

		/// Accept the next bidirectional stream opened by the peer.
		fn accept_bi(&self) -> AcceptBi<'_, Self> {
			AcceptBi::new(self)
		}

		/// Open a unidirectional stream scheduled by `options`, waiting for stream credit if
		/// necessary.
		fn open_uni<'a>(
			&'a self,
			options: SendOptions<'a, <Self::SendStream as SendStream>::Group>,
		) -> OpenUni<'a, Self> {
			OpenUni {
				session: self,
				options,
				park: kio::Park::default(),
			}
		}

		/// Open a bidirectional stream, its sending half scheduled by `options`, waiting for
		/// stream credit if necessary.
		fn open_bi<'a>(
			&'a self,
			options: SendOptions<'a, <Self::SendStream as SendStream>::Group>,
		) -> OpenBi<'a, Self> {
			OpenBi {
				session: self,
				options,
				park: kio::Park::default(),
			}
		}

		/// Receive the next datagram from the peer.
		fn recv_datagram(&self) -> RecvDatagram<'_, Self> {
			RecvDatagram::new(self)
		}

		/// Wait until the session is closed by either side, returning the reason.
		fn closed(&self) -> SessionClosed<'_, Self> {
			SessionClosed::new(self)
		}
	}

	/// One in-flight `poll_*` operation as a [`Future`]: the helpers below are
	/// named types (not `impl Future`) so their `Send`-ness stays inferred from
	/// the transport; an opaque return type in a trait would hide it. Each holds
	/// the [`kio::Park`] that keeps its waiter registered between polls.
	macro_rules! poll_future {
		($(#[$doc:meta])* $name:ident, $bound:path, $poll:ident, $out:ty) => {
			$(#[$doc])*
			pub struct $name<'a, S: ?Sized> {
				inner: &'a S,
				park: kio::Park,
			}

			impl<'a, S: ?Sized> $name<'a, S> {
				fn new(inner: &'a S) -> Self {
					Self {
						inner,
						park: kio::Park::default(),
					}
				}
			}

			impl<S: $bound + ?Sized> Future for $name<'_, S> {
				type Output = $out;

				fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
					let this = &mut *self;
					this.inner.$poll(this.park.hold(cx))
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
			pub struct $name<'a, S: Session + ?Sized> {
				session: &'a S,
				options: SendOptions<'a, <S::SendStream as SendStream>::Group>,
				park: kio::Park,
			}

			impl<S: Session + ?Sized> Future for $name<'_, S> {
				type Output = $out;

				fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
					let this = self.get_mut();
					this.session.$poll(this.park.hold(cx), this.options)
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

	/// A session shared with the client or server that runs it, so the caller can still close
	/// or watch it.
	impl<S: Session + ?Sized> Session for std::sync::Arc<S> {
		type SendStream = S::SendStream;
		type RecvStream = S::RecvStream;
		type Error = S::Error;

		fn poll_accept_uni(&self, waiter: &kio::Waiter) -> Poll<Result<Self::RecvStream, Self::Error>> {
			(**self).poll_accept_uni(waiter)
		}

		fn poll_accept_bi(&self, waiter: &kio::Waiter) -> Poll<Result<BiStreams<Self>, Self::Error>> {
			(**self).poll_accept_bi(waiter)
		}

		fn poll_open_uni(
			&self,
			waiter: &kio::Waiter,
			options: SendOptions<'_, <Self::SendStream as SendStream>::Group>,
		) -> Poll<Result<Self::SendStream, Self::Error>> {
			(**self).poll_open_uni(waiter, options)
		}

		fn poll_open_bi(
			&self,
			waiter: &kio::Waiter,
			options: SendOptions<'_, <Self::SendStream as SendStream>::Group>,
		) -> Poll<Result<BiStreams<Self>, Self::Error>> {
			(**self).poll_open_bi(waiter, options)
		}

		fn poll_open_request(
			&self,
			waiter: &kio::Waiter,
			options: SendOptions<'_, <Self::SendStream as SendStream>::Group>,
		) -> Poll<Result<Option<BiStreams<Self>>, Self::Error>> {
			(**self).poll_open_request(waiter, options)
		}

		fn send_group(&self, priority: i64) -> Option<<Self::SendStream as SendStream>::Group> {
			(**self).send_group(priority)
		}

		fn send_datagram(
			&self,
			payload: Bytes,
			options: SendOptions<'_, <Self::SendStream as SendStream>::Group>,
		) -> Result<(), Self::Error> {
			(**self).send_datagram(payload, options)
		}

		fn poll_recv_datagram(&self, waiter: &kio::Waiter) -> Poll<Result<Bytes, Self::Error>> {
			(**self).poll_recv_datagram(waiter)
		}

		fn max_datagram_size(&self) -> usize {
			(**self).max_datagram_size()
		}

		fn protocol(&self) -> Option<&str> {
			(**self).protocol()
		}

		fn address(&self) -> super::Address {
			(**self).address()
		}

		fn close(&self, code: u32, reason: &str) {
			(**self).close(code, reason)
		}

		fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Self::Error> {
			(**self).poll_closed(waiter)
		}

		fn metrics(&self) -> impl Metrics {
			(**self).metrics()
		}

		fn wire(&self) -> Option<super::Wire> {
			(**self).wire()
		}

		fn probe(&self) -> Option<super::Probe> {
			(**self).probe()
		}
	}

	/// A transport that the async client and server can box:
	/// the session, its streams, send groups, and errors are thread-safe on native.
	/// Implemented automatically.
	///
	/// Boxing is what compiles the session code once per binary rather than once per
	/// transport.
	pub trait Boxable:
		Session<
			SendStream: MaybeSend + MaybeSync + SendStream<Group: MaybeSend + MaybeSync>,
			RecvStream: MaybeSend + MaybeSync,
		> + MaybeSend
		+ MaybeSync
	{
	}

	impl<S> Boxable for S where
		S: Session<
				SendStream: MaybeSend + MaybeSync + SendStream<Group: MaybeSend + MaybeSync>,
				RecvStream: MaybeSend + MaybeSync,
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
		fn poll_write(&mut self, waiter: &kio::Waiter, buf: &[u8]) -> Poll<Result<usize, Self::Error>>;

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
		fn poll_write_buf<B: Buf>(&mut self, waiter: &kio::Waiter, buf: &mut B) -> Poll<Result<usize, Self::Error>> {
			let size = ready!(self.poll_write(waiter, buf.chunk()))?;
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
		fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Result<(), Self::Error>>;

		/// Write some of the buffer, returning how many bytes were accepted.
		fn write<'a>(&'a mut self, buf: &'a [u8]) -> Write<'a, Self> {
			Write {
				stream: self,
				buf,
				park: kio::Park::default(),
			}
		}

		/// Write some of the buffer, advancing it by the bytes accepted.
		fn write_buf<'a, B: Buf>(&'a mut self, buf: &'a mut B) -> WriteBuf<'a, Self, B> {
			WriteBuf {
				stream: self,
				buf,
				park: kio::Park::default(),
			}
		}

		/// Write the entire chunk to the stream.
		fn write_chunk(&mut self, chunk: Bytes) -> WriteChunk<'_, Self> {
			WriteChunk {
				stream: self,
				chunk,
				park: kio::Park::default(),
			}
		}

		/// Wait until the stream is closed by either side.
		fn closed(&self) -> SendClosed<'_, Self> {
			SendClosed::new(self)
		}
	}

	/// A pending [`SendStream::write`].
	pub struct Write<'a, S: ?Sized> {
		stream: &'a mut S,
		buf: &'a [u8],
		park: kio::Park,
	}

	impl<S: SendStream> Future for Write<'_, S> {
		type Output = Result<usize, S::Error>;

		fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
			let this = &mut *self;
			this.stream.poll_write(this.park.hold(cx), this.buf)
		}
	}

	/// A pending [`SendStream::write_buf`].
	pub struct WriteBuf<'a, S: ?Sized, B> {
		stream: &'a mut S,
		buf: &'a mut B,
		park: kio::Park,
	}

	impl<S: SendStream, B: Buf> Future for WriteBuf<'_, S, B> {
		type Output = Result<usize, S::Error>;

		fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
			let this = &mut *self;
			this.stream.poll_write_buf(this.park.hold(cx), this.buf)
		}
	}

	/// A pending [`SendStream::write_chunk`].
	pub struct WriteChunk<'a, S: ?Sized> {
		stream: &'a mut S,
		chunk: Bytes,
		park: kio::Park,
	}

	impl<S: SendStream> Future for WriteChunk<'_, S> {
		type Output = Result<(), S::Error>;

		fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
			let this = &mut *self;
			let waiter = this.park.hold(cx);
			while !this.chunk.is_empty() {
				ready!(this.stream.poll_write_buf(waiter, &mut this.chunk))?;
			}
			Poll::Ready(Ok(()))
		}
	}

	/// An incoming stream, read through leases of the bytes its transport buffers.
	///
	/// Leased bytes stay in the transport, counted against its flow control, until
	/// [`Lease::release`] returns the credit of those read, so an incomplete message holds
	/// receive credit rather than memory of its own. When incomplete messages leave no credit for
	/// any of them to finish, a transport that bounds them evicts the lowest
	/// [priority](Self::set_priority) holder, whose next lease fails. A transport that returns
	/// credit as it reads, adapted with [`Buffered`], is not bounded this way.
	pub trait RecvStream: 'static {
		/// The error type for every operation on this stream.
		type Error: Error;

		/// The unreleased bytes borrowed from this stream.
		type Lease<'a>: Lease
		where
			Self: 'a;

		/// Poll until at least `want` bytes are leased, or the peer finished the stream with
		/// fewer.
		///
		/// Reading the lease as a [`Buf`] moves past bytes for good, but they keep their
		/// flow-control credit until [`Lease::release`]. Bytes not read stay for the next lease.
		/// A `want` the stream's receive window can never hold is an error rather than a wait, and
		/// a `want` of 0 never waits.
		///
		/// A lease may borrow the transport's connection state, so release it before
		/// touching any other stream of the session.
		fn poll_lease(&mut self, waiter: &kio::Waiter, want: usize) -> Poll<Result<Self::Lease<'_>, Self::Error>>;

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
		fn poll_read_chunk(&mut self, waiter: &kio::Waiter, max: usize) -> Poll<Result<Option<Bytes>, Self::Error>> {
			if max == 0 {
				return Poll::Ready(Ok(Some(Bytes::new())));
			}
			let mut lease = ready!(self.poll_lease(waiter, 1))?;
			if !lease.has_remaining() {
				return Poll::Ready(Ok(None));
			}
			let chunk = lease.copy_to_bytes(lease.chunk().len().min(max));
			lease.release();
			Poll::Ready(Ok(Some(chunk)))
		}

		/// Read the next chunk, up to `max` bytes, or `None` once finished.
		fn read_chunk(&mut self, max: usize) -> ReadChunk<'_, Self> {
			ReadChunk {
				stream: self,
				max,
				park: kio::Park::default(),
			}
		}
	}

	/// Unreleased bytes borrowed from a [`RecvStream`] by [`poll_lease`](RecvStream::poll_lease),
	/// read as a [`Buf`].
	///
	/// Bytes read keep their credit until [`release`](Self::release), which is how a frame
	/// reaches its consumer before the whole of it has arrived. Implementations override
	/// [`Buf::copy_to_bytes`] to hand out bytes within one chunk without copying.
	pub trait Lease: Buf {
		/// The bytes not yet read, in order, one contiguous chunk at a time, without reading them.
		fn chunks(&self) -> impl Iterator<Item = &[u8]>;

		/// Whether the peer finished the stream right after the leased bytes.
		fn is_fin(&self) -> bool;

		/// Return the credit of every byte read so far, by this lease or an earlier one.
		fn release(self);
	}

	/// A pending [`RecvStream::read_chunk`].
	pub struct ReadChunk<'a, S: ?Sized> {
		stream: &'a mut S,
		max: usize,
		park: kio::Park,
	}

	impl<S: RecvStream> Future for ReadChunk<'_, S> {
		type Output = Result<Option<Bytes>, S::Error>;

		fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
			let this = &mut *self;
			this.stream.poll_read_chunk(this.park.hold(cx), this.max)
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
		fn poll_read_chunk(&mut self, waiter: &kio::Waiter, max: usize) -> Poll<Result<Option<Bytes>, Self::Error>>;

		/// Send a `STOP_SENDING` code. See [`RecvStream::stop`].
		///
		/// An implementation MUST also do this on Drop: [`Buffered`] forwards an explicit stop
		/// but leaves the drop to the wrapped stream.
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

		fn poll_lease(&mut self, waiter: &kio::Waiter, want: usize) -> Poll<Result<BufferedLease<'_>, S::Error>> {
			while self.len < want && !self.fin {
				// Whatever the transport has in one piece: a transport handing over whole
				// messages, like the memory one, keeps a large frame in a single chunk, so it
				// reaches the consumer without a copy. Each read still ends at what has arrived.
				match ready!(self.inner.poll_read_chunk(waiter, usize::MAX))? {
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

	impl Buf for BufferedLease<'_> {
		fn remaining(&self) -> usize {
			*self.len
		}

		fn chunk(&self) -> &[u8] {
			self.chunks.front().map_or(&[], |chunk| &chunk[..])
		}

		fn advance(&mut self, mut n: usize) {
			assert!(n <= *self.len, "advanced {n} past {} leased bytes", *self.len);
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

		fn copy_to_bytes(&mut self, n: usize) -> Bytes {
			assert!(n <= *self.len, "copied {n} of {} leased bytes", *self.len);
			let Some(front) = self.chunks.front_mut().filter(|front| front.len() >= n) else {
				// Spans chunks, so it needs a contiguous copy.
				let mut bytes = BytesMut::with_capacity(n);
				while bytes.len() < n {
					let take = self.chunk().len().min(n - bytes.len());
					bytes.extend_from_slice(&self.chunk()[..take]);
					self.advance(take);
				}
				return bytes.freeze();
			};
			*self.len -= n;
			match front.len() > n {
				true => front.split_to(n),
				false => self.chunks.pop_front().expect("front exists"),
			}
		}
	}

	impl Lease for BufferedLease<'_> {
		fn chunks(&self) -> impl Iterator<Item = &[u8]> {
			self.chunks.iter().map(|chunk| &chunk[..])
		}

		fn is_fin(&self) -> bool {
			self.fin
		}

		// The transport returned the credit as it read.
		fn release(self) {}
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
		fn poll_read_chunk(&mut self, _: &kio::Waiter, max: usize) -> Poll<Result<Option<Bytes>, Self::Error>> {
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

	/// A lease waits for `want` across chunks, and dropping it keeps every unread byte for the next.
	#[test]
	fn lease_waits_for_want_and_drop_keeps_unread() {
		let waiter = kio::Waiter::noop();
		let mut stream = buffered(&[b"ab", b"cd"], false);

		{
			let Poll::Ready(Ok(lease)) = stream.poll_lease(&waiter, 3) else {
				panic!("both chunks were ready")
			};
			assert_eq!(lease.remaining(), 4);
			assert_eq!(lease.chunk(), b"ab");
			assert_eq!(lease.chunks().collect::<Vec<_>>(), [b"ab", b"cd"]);
		}

		assert!(stream.poll_lease(&waiter, 5).is_pending(), "nothing more arrived");
		let Poll::Ready(Ok(lease)) = stream.poll_lease(&waiter, 1) else {
			panic!("the dropped lease's bytes are still there")
		};
		let mut lease = lease;
		assert_eq!(lease.remaining(), 4);
		lease.advance(3);
		lease.release();

		let Poll::Ready(Ok(lease)) = stream.poll_lease(&waiter, 0) else {
			panic!("want 0 never waits")
		};
		assert_eq!(lease.chunk(), b"d");
		assert!(!lease.is_fin());
	}

	/// Bytes within a chunk are handed out without a copy, bytes across chunks are joined, and a
	/// finished stream leases what is left with its FIN.
	#[test]
	fn copy_to_bytes_and_fin_ends_short() {
		let waiter = kio::Waiter::noop();
		let mut stream = buffered(&[b"abc", b"de"], true);

		let Poll::Ready(Ok(mut lease)) = stream.poll_lease(&waiter, 10) else {
			panic!("a finished stream leases short")
		};
		assert!(lease.is_fin());
		assert_eq!(lease.copy_to_bytes(2), Bytes::from_static(b"ab"));
		assert_eq!(
			lease.copy_to_bytes(2),
			Bytes::from_static(b"cd"),
			"joined across chunks"
		);
		assert_eq!(lease.remaining(), 1);
		lease.release();

		assert_eq!(
			stream.read_chunk(10).now_or_never_ready(),
			Some(Bytes::from_static(b"e"))
		);
		assert_eq!(stream.read_chunk(10).now_or_never_ready(), None);
		assert_eq!(stream.read_chunk(0).now_or_never_ready(), Some(Bytes::new()));
	}

	/// A large chunk the transport hands over whole reaches the reader as that same
	/// allocation, so a frame from the memory transport is never copied.
	#[test]
	fn a_large_chunk_is_leased_whole() {
		static FRAME: [u8; 1 << 20] = [7; 1 << 20];
		let waiter = kio::Waiter::noop();
		let mut stream = buffered(&[&FRAME], true);

		let Poll::Ready(Ok(mut lease)) = stream.poll_lease(&waiter, 1) else {
			panic!("the chunk was ready")
		};
		let chunk = lease.copy_to_bytes(FRAME.len());
		assert_eq!(chunk.len(), FRAME.len());
		assert_eq!(chunk.as_ptr(), FRAME.as_ptr(), "the chunk was not copied");
	}

	/// Stopping discards what was buffered and stops the transport.
	#[test]
	fn stop_discards_the_buffer() {
		let waiter = kio::Waiter::noop();
		let mut stream = buffered(&[b"abc"], false);
		assert!(stream.poll_lease(&waiter, 1).is_ready());
		stream.stop(7);
		assert_eq!(stream.get_ref().stopped, Some(7));
		assert!(stream.poll_lease(&waiter, 1).is_pending());
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
