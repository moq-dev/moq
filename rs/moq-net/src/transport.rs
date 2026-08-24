//! The transport interface a MoQ session runs over.
//!
//! This crate is poll-only: every entry point ([`crate::Client::connect`],
//! [`crate::Server::accept`]) is generic over the *poll* half of
//! `web_transport_trait` ([`web_transport_trait::poll`]), never its async half.
//! The [`poll::Session`], [`poll::SendStream`], and [`poll::RecvStream`] traits
//! here bundle those poll traits with the bounds the session machinery needs
//! (cloning, thread affinity) and provide async helper methods on top, the same
//! layering the rest of the crate uses (`async fn` wraps `poll_*`).
//!
//! A transport that only implements the async half cannot be used directly:
//! implement the poll interface for it (typically inside the transport itself,
//! where its wakers live) rather than wrapping futures around it here.

/// The poll-based transport traits this crate requires.
///
/// These mirror [`web_transport_trait::poll`], adding the bounds the session
/// machinery needs and async helper methods; they are implemented automatically
/// for any type implementing the upstream poll traits with those bounds.
pub mod poll {
	use std::task::{Context, Poll, ready};

	use bytes::{Buf, BufMut, Bytes};
	use web_transport_trait::{MaybeSend, MaybeSync, poll};

	/// The transport session a MoQ session runs over.
	///
	/// This is [`web_transport_trait::poll::Session`] plus the bounds the protocol
	/// drivers need: `Clone` so each concurrently pending operation gets its own
	/// handle (each clone carries its own in-progress state, per the poll contract),
	/// and `'static` so drivers can own it for the session's lifetime. There is
	/// deliberately no thread-affinity bound: a pinned `!Send` transport drives a
	/// `!Send` machine on its own thread. The [`Boxable`] subset is for the parts
	/// that still erase into `Send` boxes. It is implemented automatically.
	///
	/// The async methods are helpers over the required `poll_*` methods, so callers
	/// can `.await` operations without giving up the ability to poll them.
	pub trait Session: poll::Session<SendStream: SendStream, RecvStream: RecvStream> + Clone + 'static {
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

		/// Send a datagram, best-effort: if the transport has no room for it right
		/// now, the datagram is dropped, exactly as the network is allowed to do.
		fn send_datagram(&mut self, payload: &[u8]) -> Result<(), Self::Error> {
			let mut cx = Context::from_waker(std::task::Waker::noop());
			match self.poll_send_datagram(&mut cx, payload) {
				Poll::Ready(res) => res,
				Poll::Pending => Ok(()),
			}
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
		AcceptUni, poll::Session, poll_accept_uni, Result<S::RecvStream, S::Error>);
	poll_future!(
		/// A pending [`Session::accept_bi`].
		AcceptBi, poll::Session, poll_accept_bi, Result<poll::BiStreams<S>, S::Error>);
	poll_future!(
		/// A pending [`Session::open_uni`].
		OpenUni, poll::Session, poll_open_uni, Result<S::SendStream, S::Error>);
	poll_future!(
		/// A pending [`Session::open_bi`].
		OpenBi, poll::Session, poll_open_bi, Result<poll::BiStreams<S>, S::Error>);
	poll_future!(
		/// A pending [`Session::recv_datagram`].
		RecvDatagram, poll::Session, poll_recv_datagram, Result<Bytes, S::Error>);
	poll_future!(
		/// A pending [`Session::closed`].
		SessionClosed, poll::Session, poll_closed, S::Error);
	poll_future!(
		/// A pending [`SendStream::closed`].
		SendClosed, poll::SendStream, poll_closed, Result<(), S::Error>);
	poll_future!(
		/// A pending [`RecvStream::closed`].
		RecvClosed, poll::RecvStream, poll_closed, Result<(), S::Error>);

	impl<S> Session for S where S: poll::Session<SendStream: SendStream, RecvStream: RecvStream> + Clone + 'static {}

	/// A transport whose session, streams, and errors can be captured by the
	/// boxed drivers (`Send` boxes on native): what the moq-transport path
	/// requires until it too becomes named machines. Implemented automatically.
	pub trait Boxable:
		Session<SendStream: MaybeSend, RecvStream: MaybeSend, Error: MaybeSend> + MaybeSend + MaybeSync
	{
	}

	impl<S> Boxable for S where
		S: Session<SendStream: MaybeSend, RecvStream: MaybeSend, Error: MaybeSend> + MaybeSend + MaybeSync
	{
	}

	/// An outgoing transport stream: [`web_transport_trait::poll::SendStream`]
	/// plus the `'static` bound the drivers need, with async helpers.
	pub trait SendStream: poll::SendStream + 'static {
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

	impl<S: poll::SendStream> Future for Write<'_, S> {
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

	impl<S: poll::SendStream, B: Buf> Future for WriteBuf<'_, S, B> {
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

	impl<S: poll::SendStream> Future for WriteChunk<'_, S> {
		type Output = Result<(), S::Error>;

		fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
			let this = &mut *self;
			while !this.chunk.is_empty() {
				ready!(this.stream.poll_write_buf(cx, &mut this.chunk))?;
			}
			Poll::Ready(Ok(()))
		}
	}

	impl<S> SendStream for S where S: poll::SendStream + 'static {}

	/// An incoming transport stream: [`web_transport_trait::poll::RecvStream`]
	/// plus the `'static` bound the drivers need, with async helpers.
	pub trait RecvStream: poll::RecvStream + 'static {
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

	impl<S: poll::RecvStream> Future for Read<'_, S> {
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

	impl<S: poll::RecvStream, B: BufMut> Future for ReadBuf<'_, S, B> {
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

	impl<S: poll::RecvStream> Future for ReadChunk<'_, S> {
		type Output = Result<Option<Bytes>, S::Error>;

		fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
			let this = &mut *self;
			this.stream.poll_read_chunk(cx, this.max)
		}
	}

	impl<S> RecvStream for S where S: poll::RecvStream + 'static {}
}
