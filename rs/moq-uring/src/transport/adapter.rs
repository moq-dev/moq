use bytes::{Buf, Bytes};
use moq_net::transport::{self as moq, poll};
use std::task::{Context, Poll};
use web_transport_trait::{self as wt, poll as backend};

/// A backend failure preserving the session and stream code registries.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct Error<E>(pub(super) E);

impl<E: wt::Error> moq::Error for Error<E> {
	fn close_kind(&self) -> moq::CloseKind {
		close_kind(&self.0)
	}
	fn session_error(&self) -> Option<(u32, String)> {
		self.0.session_error()
	}
	fn stream_error(&self) -> Option<u32> {
		self.0.stream_error()
	}
}

struct Stats<S> {
	inner: S,
	streams_evicted: Option<u64>,
}
impl<S: wt::Stats> moq::Stats for Stats<S> {
	fn bytes_sent(&self) -> Option<u64> {
		self.inner.bytes_sent()
	}
	fn bytes_received(&self) -> Option<u64> {
		self.inner.bytes_received()
	}
	fn bytes_lost(&self) -> Option<u64> {
		self.inner.bytes_lost()
	}
	fn packets_sent(&self) -> Option<u64> {
		self.inner.packets_sent()
	}
	fn packets_received(&self) -> Option<u64> {
		self.inner.packets_received()
	}
	fn packets_lost(&self) -> Option<u64> {
		self.inner.packets_lost()
	}
	fn rtt(&self) -> Option<std::time::Duration> {
		self.inner.rtt()
	}
	fn estimated_send_rate(&self) -> Option<u64> {
		self.inner.estimated_send_rate()
	}
	fn streams_evicted(&self) -> Option<u64> {
		self.streams_evicted
	}
}
/// A backend session whose send streams may join send groups, as native QUIC's do.
///
/// `pub` only because `pub(crate)` is refused (E0446): the adapter's `type Group` projects
/// through [`GroupedSend`]. The module is private and the trait is not re-exported, so
/// other crates can neither name nor implement it.
///
/// The backend interface has no groups, wire counters, or probe target, so moq-net's
/// [`send_group`](poll::Session::send_group), [`wire`](poll::Session::wire), and
/// [`probe`](poll::Session::probe) reach a backend through this. A backend without them
/// implements it empty.
pub trait Grouped: backend::Session<SendStream: GroupedSend> {
	/// The receive stream moq-net reads: the backend's own lease where it has one, or
	/// [`Buffered`](poll::Buffered) reads otherwise.
	type Recv: poll::RecvStream;

	/// Adapt one of the backend's receive streams, freshly accepted or opened.
	fn recv(stream: RecvStream<Self::RecvStream>) -> Self::Recv;

	/// Open a request without waiting for stream credit.
	fn poll_open_request(
		&mut self,
		cx: &mut Context<'_>,
	) -> Poll<Result<Option<backend::BiStreams<Self>>, Self::Error>> {
		backend::Session::poll_open_bi(self, cx).map_ok(Some)
	}

	/// The wire counters the backend updates as it sends and receives, or `None` when it has none.
	fn wire(&self) -> Option<moq::Wire> {
		None
	}

	/// The target the backend's sender probes up to, or `None` when it cannot probe.
	fn probe(&self) -> Option<moq::Probe> {
		None
	}

	/// Receive streams evicted to free a stuck receive window, or `None` when the backend never
	/// evicts.
	fn streams_evicted(&self) -> Option<u64> {
		None
	}

	/// Open a send group at `priority`, or `None` when the backend has none.
	fn send_group(&mut self, priority: i64) -> Option<<Self::SendStream as GroupedSend>::Group> {
		let _ = priority;
		None
	}

	/// Send a datagram scheduled by `options`; a backend without groups ignores them and
	/// drops the datagram when it has no room.
	fn send_datagram(
		&mut self,
		payload: Bytes,
		options: poll::SendOptions<'_, <Self::SendStream as GroupedSend>::Group>,
	) -> Result<(), Self::Error> {
		let _ = options;
		let mut cx = Context::from_waker(std::task::Waker::noop());
		match backend::Session::poll_send_datagram(self, &mut cx, &payload) {
			Poll::Ready(res) => res,
			Poll::Pending => Ok(()),
		}
	}
}

/// A backend send stream that may join its [`Grouped`] session's send groups.
pub trait GroupedSend: backend::SendStream {
	/// The backend's send group, [`Ungrouped`](poll::Ungrouped) when it has none.
	type Group: poll::SendGroup;

	/// Move the stream into `group`.
	fn set_group(&mut self, group: &Self::Group);

	/// Set the priority within the stream's send group, or session-wide when ungrouped.
	///
	/// Saturated into the backend's narrower priority by default.
	fn set_priority(&mut self, priority: i64) {
		backend::SendStream::set_priority(self, priority.clamp(i32::MIN.into(), i32::MAX.into()) as i32);
	}
}

/// A WebTransport session adapted to moq-net.
#[derive(Clone)]
pub struct Session<S>(S, moq::Address);
impl<S> Session<S> {
	/// Wrap a backend poll session.
	pub fn new(session: S, address: moq::Address) -> Self {
		Self(session, address)
	}
}
impl<S: Grouped<SendStream: 'static, RecvStream: 'static> + Clone + 'static> poll::Session for Session<S> {
	type SendStream = SendStream<S::SendStream>;
	type RecvStream = S::Recv;
	type Error = Error<S::Error>;
	fn poll_accept_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<Self::RecvStream, Self::Error>> {
		backend::Session::poll_accept_uni(&mut self.0, cx)
			.map_err(Error)
			.map(|res| res.map(|recv| S::recv(RecvStream(recv))))
	}
	fn poll_accept_bi(&mut self, cx: &mut Context<'_>) -> Poll<Result<poll::BiStreams<Self>, Self::Error>> {
		backend::Session::poll_accept_bi(&mut self.0, cx)
			.map_err(Error)
			.map(|res| res.map(|(send, recv)| (SendStream(send), S::recv(RecvStream(recv)))))
	}
	fn poll_open_uni(
		&mut self,
		cx: &mut Context<'_>,
		options: poll::SendOptions<'_, <S::SendStream as GroupedSend>::Group>,
	) -> Poll<Result<Self::SendStream, Self::Error>> {
		backend::Session::poll_open_uni(&mut self.0, cx)
			.map_err(Error)
			.map(|res| res.map(|send| SendStream(send).scheduled(options)))
	}
	fn poll_open_bi(
		&mut self,
		cx: &mut Context<'_>,
		options: poll::SendOptions<'_, <S::SendStream as GroupedSend>::Group>,
	) -> Poll<Result<poll::BiStreams<Self>, Self::Error>> {
		backend::Session::poll_open_bi(&mut self.0, cx)
			.map_err(Error)
			.map(|res| res.map(|(send, recv)| (SendStream(send).scheduled(options), S::recv(RecvStream(recv)))))
	}

	fn poll_open_request(
		&mut self,
		cx: &mut Context<'_>,
		options: poll::SendOptions<'_, <S::SendStream as GroupedSend>::Group>,
	) -> Poll<Result<Option<poll::BiStreams<Self>>, Self::Error>> {
		Grouped::poll_open_request(&mut self.0, cx)
			.map_err(Error)
			.map_ok(|streams| {
				streams.map(|(send, recv)| (SendStream(send).scheduled(options), S::recv(RecvStream(recv))))
			})
	}
	fn send_group(&mut self, priority: i64) -> Option<<S::SendStream as GroupedSend>::Group> {
		Grouped::send_group(&mut self.0, priority)
	}
	fn send_datagram(
		&mut self,
		payload: Bytes,
		options: poll::SendOptions<'_, <S::SendStream as GroupedSend>::Group>,
	) -> Result<(), Self::Error> {
		Grouped::send_datagram(&mut self.0, payload, options).map_err(Error)
	}
	fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, Self::Error>> {
		backend::Session::poll_recv_datagram(&mut self.0, cx).map_err(Error)
	}
	fn max_datagram_size(&self) -> usize {
		backend::Session::max_datagram_size(&self.0)
	}
	fn address(&self) -> moq::Address {
		self.1
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
		Stats {
			inner: backend::Session::stats(&self.0),
			streams_evicted: Grouped::streams_evicted(&self.0),
		}
	}
	fn wire(&self) -> Option<moq::Wire> {
		Grouped::wire(&self.0)
	}
	fn probe(&self) -> Option<moq::Probe> {
		Grouped::probe(&self.0)
	}
}
/// An outgoing WebTransport stream adapted to moq-net.
pub struct SendStream<S>(S);
impl<S: GroupedSend + 'static> SendStream<S> {
	/// Schedule a newly opened stream by its options.
	fn scheduled(mut self, options: poll::SendOptions<'_, S::Group>) -> Self {
		GroupedSend::set_priority(&mut self.0, options.priority);
		if let Some(group) = options.group {
			GroupedSend::set_group(&mut self.0, group);
		}
		self
	}
}
impl<S: GroupedSend + 'static> poll::SendStream for SendStream<S> {
	type Error = Error<S::Error>;
	type Group = S::Group;
	fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
		backend::SendStream::poll_write(&mut self.0, cx, buf).map_err(Error)
	}
	fn poll_write_buf<B: Buf>(&mut self, cx: &mut Context<'_>, buf: &mut B) -> Poll<Result<usize, Self::Error>> {
		backend::SendStream::poll_write_buf(&mut self.0, cx, buf).map_err(Error)
	}
	fn set_priority(&mut self, priority: i64) {
		GroupedSend::set_priority(&mut self.0, priority)
	}
	fn set_group(&mut self, group: &Self::Group) {
		GroupedSend::set_group(&mut self.0, group)
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
pub struct RecvStream<S>(pub(super) S);
impl<S: backend::RecvStream + 'static> poll::Chunks for RecvStream<S> {
	type Error = Error<S::Error>;
	fn poll_read_chunk(&mut self, cx: &mut Context<'_>, max: usize) -> Poll<Result<Option<Bytes>, Self::Error>> {
		backend::RecvStream::poll_read_chunk(&mut self.0, cx, max).map_err(Error)
	}
	fn stop(&mut self, code: u32) {
		backend::RecvStream::stop(&mut self.0, code)
	}
}

fn close_kind(error: &(dyn std::error::Error + 'static)) -> moq::CloseKind {
	#[cfg(all(target_os = "linux", feature = "quic"))]
	if let Some(error) = error.downcast_ref::<crate::quic::Error>() {
		return match error {
			crate::quic::Error::TimedOut => moq::CloseKind::IdleTimeout,
			crate::quic::Error::Closed => moq::CloseKind::Closed,
			crate::quic::Error::Evicted => moq::CloseKind::Evicted,
			_ => moq::CloseKind::Transport,
		};
	}
	error.source().map(close_kind).unwrap_or_default()
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
	impl GroupedSend for Stream {
		type Group = poll::Ungrouped;
		fn set_group(&mut self, group: &Self::Group) {
			match *group {}
		}
	}
	impl Grouped for Backend {
		type Recv = poll::Buffered<RecvStream<Stream>>;
		fn recv(stream: RecvStream<Stream>) -> Self::Recv {
			poll::Buffered::new(stream)
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
		let mut session = Session::new(Backend(state.clone()), moq::Address::Setup);
		let mut cx = Context::from_waker(std::task::Waker::noop());
		assert_eq!(poll::Session::protocol(&session), Some("moq-lite-06"));
		assert_eq!(
			poll::Session::stats(&session).rtt(),
			Some(std::time::Duration::from_millis(7))
		);
		let Poll::Ready(Ok((mut send, mut recv))) =
			poll::Session::poll_open_bi(&mut session, &mut cx, Default::default())
		else {
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

#[cfg(all(test, target_os = "linux", feature = "quic"))]
mod outcome_tests {
	use super::*;

	#[test]
	fn typed_ring_close_kinds_survive_the_adapter() {
		let idle = Error(crate::quic::Error::TimedOut);
		assert!(matches!(
			moq_net::Error::from_transport(idle),
			moq_net::Error::IdleTimeout
		));
		let local = Error(crate::quic::Error::Closed);
		assert!(matches!(
			moq_net::Error::from_transport(local),
			moq_net::Error::Session(moq_net::SessionError::Cancel)
		));
	}
}
