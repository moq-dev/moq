use bytes::{Buf, BufMut, Bytes};
use moq_net::transport::{self as moq, poll};
use std::task::{Context, Poll};
use web_transport_trait::{self as wt, poll as backend};

/// A backend failure preserving the session and stream code registries.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct Error<E>(E);

impl<E: wt::Error> moq::Error for Error<E> {
	fn session_error(&self) -> Option<(u32, String)> {
		self.0.session_error()
	}
	fn stream_error(&self) -> Option<u32> {
		self.0.stream_error()
	}
}

struct Stats<S>(S);
impl<S: wt::Stats> moq::Stats for Stats<S> {
	fn bytes_sent(&self) -> Option<u64> {
		self.0.bytes_sent()
	}
	fn bytes_received(&self) -> Option<u64> {
		self.0.bytes_received()
	}
	fn bytes_lost(&self) -> Option<u64> {
		self.0.bytes_lost()
	}
	fn packets_sent(&self) -> Option<u64> {
		self.0.packets_sent()
	}
	fn packets_received(&self) -> Option<u64> {
		self.0.packets_received()
	}
	fn packets_lost(&self) -> Option<u64> {
		self.0.packets_lost()
	}
	fn rtt(&self) -> Option<std::time::Duration> {
		self.0.rtt()
	}
	fn estimated_send_rate(&self) -> Option<u64> {
		self.0.estimated_send_rate()
	}
}
/// A WebTransport session adapted to moq-net.
#[derive(Clone)]
pub struct Session<S>(S);
impl<S> Session<S> {
	/// Wrap a backend poll session.
	pub fn new(session: S) -> Self {
		Self(session)
	}
}
impl<S: backend::Session<SendStream: 'static, RecvStream: 'static> + Clone + 'static> poll::Session for Session<S> {
	type SendStream = SendStream<S::SendStream>;
	type RecvStream = RecvStream<S::RecvStream>;
	type Error = Error<S::Error>;
	fn poll_accept_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<Self::RecvStream, Self::Error>> {
		backend::Session::poll_accept_uni(&mut self.0, cx)
			.map_err(Error)
			.map(|res| res.map(RecvStream))
	}
	fn poll_accept_bi(&mut self, cx: &mut Context<'_>) -> Poll<Result<poll::BiStreams<Self>, Self::Error>> {
		backend::Session::poll_accept_bi(&mut self.0, cx)
			.map_err(Error)
			.map(|res| res.map(|(send, recv)| (SendStream(send), RecvStream(recv))))
	}
	fn poll_open_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<Self::SendStream, Self::Error>> {
		backend::Session::poll_open_uni(&mut self.0, cx)
			.map_err(Error)
			.map(|res| res.map(SendStream))
	}
	fn poll_open_bi(&mut self, cx: &mut Context<'_>) -> Poll<Result<poll::BiStreams<Self>, Self::Error>> {
		backend::Session::poll_open_bi(&mut self.0, cx)
			.map_err(Error)
			.map(|res| res.map(|(send, recv)| (SendStream(send), RecvStream(recv))))
	}
	fn poll_send_datagram(&mut self, cx: &mut Context<'_>, payload: &[u8]) -> Poll<Result<(), Self::Error>> {
		backend::Session::poll_send_datagram(&mut self.0, cx, payload).map_err(Error)
	}
	fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, Self::Error>> {
		backend::Session::poll_recv_datagram(&mut self.0, cx).map_err(Error)
	}
	fn max_datagram_size(&self) -> usize {
		backend::Session::max_datagram_size(&self.0)
	}
	fn protocol(&self) -> Option<&str> {
		backend::Session::protocol(&self.0)
	}
	fn close(&mut self, code: u32, reason: &str) {
		backend::Session::close(&mut self.0, code, reason)
	}
	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Self::Error> {
		backend::Session::poll_closed(&mut self.0, cx).map(Error)
	}
	fn stats(&self) -> impl moq::Stats {
		Stats(backend::Session::stats(&self.0))
	}
}
/// An outgoing WebTransport stream adapted to moq-net.
pub struct SendStream<S>(S);
impl<S: backend::SendStream + 'static> poll::SendStream for SendStream<S> {
	type Error = Error<S::Error>;
	fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
		backend::SendStream::poll_write(&mut self.0, cx, buf).map_err(Error)
	}
	fn poll_write_buf<B: Buf>(&mut self, cx: &mut Context<'_>, buf: &mut B) -> Poll<Result<usize, Self::Error>> {
		backend::SendStream::poll_write_buf(&mut self.0, cx, buf).map_err(Error)
	}
	fn set_priority(&mut self, order: i32) {
		backend::SendStream::set_priority(&mut self.0, order)
	}
	fn finish(&mut self) -> Result<(), Self::Error> {
		backend::SendStream::finish(&mut self.0).map_err(Error)
	}
	fn reset(&mut self, code: u32) {
		backend::SendStream::reset(&mut self.0, code)
	}
	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		backend::SendStream::poll_closed(&mut self.0, cx).map_err(Error)
	}
}
/// An incoming WebTransport stream adapted to moq-net.
pub struct RecvStream<S>(S);
impl<S: backend::RecvStream + 'static> poll::RecvStream for RecvStream<S> {
	type Error = Error<S::Error>;
	fn poll_read(&mut self, cx: &mut Context<'_>, dst: &mut [u8]) -> Poll<Result<Option<usize>, Self::Error>> {
		backend::RecvStream::poll_read(&mut self.0, cx, dst).map_err(Error)
	}
	fn poll_read_buf<B: BufMut>(
		&mut self,
		cx: &mut Context<'_>,
		buf: &mut B,
	) -> Poll<Result<Option<usize>, Self::Error>> {
		backend::RecvStream::poll_read_buf(&mut self.0, cx, buf).map_err(Error)
	}
	fn poll_read_chunk(&mut self, cx: &mut Context<'_>, max: usize) -> Poll<Result<Option<Bytes>, Self::Error>> {
		backend::RecvStream::poll_read_chunk(&mut self.0, cx, max).map_err(Error)
	}
	fn stop(&mut self, code: u32) {
		backend::RecvStream::stop(&mut self.0, code)
	}
	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		backend::RecvStream::poll_closed(&mut self.0, cx).map_err(Error)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use moq_net::transport::Error as _;
	use std::{cell::Cell, rc::Rc};

	#[derive(Debug, thiserror::Error)]
	#[error("peer failure")]
	struct Failure;
	impl wt::Error for Failure {
		fn session_error(&self) -> Option<(u32, String)> {
			None
		}
		fn stream_error(&self) -> Option<u32> {
			Some(0x33)
		}
	}

	#[derive(Clone)]
	struct Backend(Rc<Cell<u32>>);
	struct Stream(Rc<Cell<u32>>);
	impl backend::SendStream for Stream {
		type Error = Failure;
		fn poll_write(&mut self, _: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Failure>> {
			Poll::Ready(Ok(buf.len().min(2)))
		}
		fn set_priority(&mut self, order: i32) {
			self.0.set(order as u32);
		}
		fn finish(&mut self) -> Result<(), Failure> {
			Ok(())
		}
		fn reset(&mut self, code: u32) {
			self.0.set(code);
		}
		fn poll_closed(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Failure>> {
			Poll::Ready(Err(Failure))
		}
	}
	impl backend::RecvStream for Stream {
		type Error = Failure;
		fn poll_read(&mut self, _: &mut Context<'_>, dst: &mut [u8]) -> Poll<Result<Option<usize>, Failure>> {
			let size = dst.len().min(2);
			dst[..size].copy_from_slice(&b"hi"[..size]);
			Poll::Ready(Ok(Some(size)))
		}
		fn stop(&mut self, code: u32) {
			self.0.set(code);
		}
		fn poll_closed(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Failure>> {
			Poll::Ready(Err(Failure))
		}
	}
	struct Sample;
	impl wt::Stats for Sample {
		fn rtt(&self) -> Option<std::time::Duration> {
			Some(std::time::Duration::from_millis(7))
		}
	}
	impl backend::Session for Backend {
		type SendStream = Stream;
		type RecvStream = Stream;
		type Error = Failure;
		fn poll_accept_uni(&mut self, _: &mut Context<'_>) -> Poll<Result<Stream, Failure>> {
			Poll::Ready(Ok(Stream(self.0.clone())))
		}
		fn poll_accept_bi(&mut self, _: &mut Context<'_>) -> Poll<Result<backend::BiStreams<Self>, Failure>> {
			Poll::Ready(Ok((Stream(self.0.clone()), Stream(self.0.clone()))))
		}
		fn poll_open_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<Stream, Failure>> {
			backend::Session::poll_accept_uni(self, cx)
		}
		fn poll_open_bi(&mut self, cx: &mut Context<'_>) -> Poll<Result<backend::BiStreams<Self>, Failure>> {
			backend::Session::poll_accept_bi(self, cx)
		}
		fn poll_send_datagram(&mut self, _: &mut Context<'_>, payload: &[u8]) -> Poll<Result<(), Failure>> {
			self.0.set(payload.len() as u32);
			Poll::Ready(Ok(()))
		}
		fn poll_recv_datagram(&mut self, _: &mut Context<'_>) -> Poll<Result<Bytes, Failure>> {
			Poll::Ready(Ok(Bytes::from_static(b"data")))
		}
		fn max_datagram_size(&self) -> usize {
			1200
		}
		fn protocol(&self) -> Option<&str> {
			Some("moq-lite-06")
		}
		fn close(&mut self, code: u32, _: &str) {
			self.0.set(code);
		}
		fn poll_closed(&mut self, _: &mut Context<'_>) -> Poll<Failure> {
			Poll::Ready(Failure)
		}
		fn stats(&self) -> impl wt::Stats {
			Sample
		}
	}

	#[derive(Debug, thiserror::Error)]
	#[error("session failure")]
	struct SessionFailure;
	impl wt::Error for SessionFailure {
		fn session_error(&self) -> Option<(u32, String)> {
			Some((2, "unauthorized".into()))
		}
	}
	#[test]
	fn preserves_session_close_codes() {
		let error = Error(SessionFailure);
		assert_eq!(error.session_error(), Some((2, "unauthorized".into())));
		assert_eq!(error.stream_error(), None);
		assert!(matches!(
			moq_net::Error::from_transport(error),
			moq_net::Error::Session(moq_net::SessionError::Unauthorized)
		));
	}

	#[test]
	fn forwards_thread_local_streams_and_keeps_error_codes() {
		use moq_net::transport::Stats as _;
		let state = Rc::new(Cell::new(0));
		let mut session = Session::new(Backend(state.clone()));
		let mut cx = Context::from_waker(std::task::Waker::noop());
		assert_eq!(poll::Session::protocol(&session), Some("moq-lite-06"));
		assert_eq!(
			poll::Session::stats(&session).rtt(),
			Some(std::time::Duration::from_millis(7))
		);
		let Poll::Ready(Ok((mut send, mut recv))) = poll::Session::poll_open_bi(&mut session, &mut cx) else {
			panic!("open failed")
		};
		let mut buf = Bytes::from_static(b"abcd");
		assert!(matches!(
			poll::SendStream::poll_write_buf(&mut send, &mut cx, &mut buf),
			Poll::Ready(Ok(2))
		));
		assert_eq!(buf, b"cd"[..]);
		poll::SendStream::set_priority(&mut send, 9);
		assert_eq!(state.get(), 9);
		poll::SendStream::reset(&mut send, 11);
		assert_eq!(state.get(), 11);
		let Poll::Ready(Ok(Some(chunk))) = poll::RecvStream::poll_read_chunk(&mut recv, &mut cx, 2) else {
			panic!("read failed")
		};
		assert_eq!(chunk, b"hi"[..]);
		poll::RecvStream::stop(&mut recv, 13);
		assert_eq!(state.get(), 13);
		let Poll::Ready(Err(error)) = poll::SendStream::poll_closed(&mut send, &mut cx) else {
			panic!("missing stream failure")
		};
		assert_eq!(error.stream_error(), Some(0x33));
		assert_eq!(error.session_error(), None);
		assert!(matches!(
			moq_net::Error::from_transport(error),
			moq_net::Error::Stream(moq_net::StreamError::NotFound)
		));
		poll::Session::close(&mut session, 21, "done");
		assert_eq!(state.get(), 21);
	}
}
