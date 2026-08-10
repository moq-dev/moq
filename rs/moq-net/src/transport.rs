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
	/// and `MaybeSend + 'static` so drivers can be boxed and sent to an executor.
	/// It is implemented automatically.
	///
	/// The async methods are helpers over the required `poll_*` methods, so callers
	/// can `.await` operations without giving up the ability to poll them.
	pub trait Session:
		poll::Session<SendStream: SendStream, RecvStream: RecvStream> + Clone + MaybeSend + MaybeSync + 'static
	{
		/// Accept the next unidirectional stream opened by the peer.
		fn accept_uni(&mut self) -> impl Future<Output = Result<Self::RecvStream, Self::Error>> + MaybeSend {
			std::future::poll_fn(|cx| self.poll_accept_uni(cx))
		}

		/// Accept the next bidirectional stream opened by the peer.
		fn accept_bi(&mut self) -> impl Future<Output = Result<poll::BiStreams<Self>, Self::Error>> + MaybeSend {
			std::future::poll_fn(|cx| self.poll_accept_bi(cx))
		}

		/// Open a unidirectional stream, waiting for stream credit if necessary.
		fn open_uni(&mut self) -> impl Future<Output = Result<Self::SendStream, Self::Error>> + MaybeSend {
			std::future::poll_fn(|cx| self.poll_open_uni(cx))
		}

		/// Open a bidirectional stream, waiting for stream credit if necessary.
		fn open_bi(&mut self) -> impl Future<Output = Result<poll::BiStreams<Self>, Self::Error>> + MaybeSend {
			std::future::poll_fn(|cx| self.poll_open_bi(cx))
		}

		/// Receive the next datagram from the peer.
		fn recv_datagram(&mut self) -> impl Future<Output = Result<Bytes, Self::Error>> + MaybeSend {
			std::future::poll_fn(|cx| self.poll_recv_datagram(cx))
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
		fn closed(&mut self) -> impl Future<Output = Self::Error> + MaybeSend {
			std::future::poll_fn(|cx| self.poll_closed(cx))
		}
	}

	impl<S> Session for S where
		S: poll::Session<SendStream: SendStream, RecvStream: RecvStream> + Clone + MaybeSend + MaybeSync + 'static
	{
	}

	/// An outgoing transport stream: [`web_transport_trait::poll::SendStream`] plus
	/// the bounds needed to move it into a boxed driver, with async helpers.
	pub trait SendStream: poll::SendStream + MaybeSend + 'static {
		/// Write some of the buffer, returning how many bytes were accepted.
		fn write(&mut self, buf: &[u8]) -> impl Future<Output = Result<usize, Self::Error>> + MaybeSend {
			std::future::poll_fn(|cx| self.poll_write(cx, buf))
		}

		/// Write some of the buffer, advancing it by the bytes accepted.
		fn write_buf<B: Buf + MaybeSend>(
			&mut self,
			buf: &mut B,
		) -> impl Future<Output = Result<usize, Self::Error>> + MaybeSend {
			std::future::poll_fn(|cx| self.poll_write_buf(cx, buf))
		}

		/// Write the entire chunk to the stream.
		fn write_chunk(&mut self, chunk: Bytes) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend {
			let mut chunk = chunk;
			std::future::poll_fn(move |cx| {
				while !chunk.is_empty() {
					ready!(self.poll_write_buf(cx, &mut chunk))?;
				}
				Poll::Ready(Ok(()))
			})
		}

		/// Wait until the stream is closed by either side.
		fn closed(&mut self) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend {
			std::future::poll_fn(|cx| self.poll_closed(cx))
		}
	}

	impl<S> SendStream for S where S: poll::SendStream + MaybeSend + 'static {}

	/// An incoming transport stream: [`web_transport_trait::poll::RecvStream`] plus
	/// the bounds needed to move it into a boxed driver, with async helpers.
	pub trait RecvStream: poll::RecvStream + MaybeSend + 'static {
		/// Read some bytes into the slice, or `None` once the stream is finished.
		fn read(&mut self, dst: &mut [u8]) -> impl Future<Output = Result<Option<usize>, Self::Error>> + MaybeSend {
			std::future::poll_fn(|cx| self.poll_read(cx, dst))
		}

		/// Read some bytes into the buffer, advancing it, or `None` once finished.
		fn read_buf<B: BufMut + MaybeSend>(
			&mut self,
			buf: &mut B,
		) -> impl Future<Output = Result<Option<usize>, Self::Error>> + MaybeSend {
			std::future::poll_fn(|cx| self.poll_read_buf(cx, buf))
		}

		/// Read the next chunk of data, up to `max` bytes, or `None` once finished.
		fn read_chunk(&mut self, max: usize) -> impl Future<Output = Result<Option<Bytes>, Self::Error>> + MaybeSend {
			std::future::poll_fn(move |cx| self.poll_read_chunk(cx, max))
		}

		/// Wait until the stream is closed by either side.
		fn closed(&mut self) -> impl Future<Output = Result<(), Self::Error>> + MaybeSend {
			std::future::poll_fn(|cx| self.poll_closed(cx))
		}
	}

	impl<S> RecvStream for S where S: poll::RecvStream + MaybeSend + 'static {}
}
