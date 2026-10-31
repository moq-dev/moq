use bytes::Bytes;
use moq_net::kio;
use moq_net::transport::{self as moq, poll};
use std::task::{Poll, ready};
use web_transport_wasm as backend;

/// A browser failure, keeping the session and stream code registries apart.
#[derive(Debug, thiserror::Error)]
pub enum Error {
	/// The browser reported a failure.
	#[error(transparent)]
	Backend(#[from] backend::Error),
	/// The peer stopped or reset a stream with this code.
	#[error("stream closed by peer: code={0}")]
	Stream(u32),
}

impl moq::Error for Error {
	/// The browser reports one numeric code either way, so the variant says which registry
	/// it belongs to. Answering both would decode a stream reset against the session table.
	fn session_error(&self) -> Option<(u32, String)> {
		match self {
			Self::Backend(backend::Error::Session { code, reason }) => Some((*code, reason.clone())),
			_ => None,
		}
	}
	fn stream_error(&self) -> Option<u32> {
		match self {
			Self::Stream(code) => Some(*code),
			Self::Backend(err @ backend::Error::Stream(_)) => err.code(),
			_ => None,
		}
	}
}

/// A stream's closed result, with a peer's code as an error.
fn closed(res: Result<Option<u32>, backend::Error>) -> Result<(), Error> {
	match res? {
		None => Ok(()),
		Some(code) => Err(Error::Stream(code)),
	}
}

/// A browser WebTransport session adapted to moq-net.
///
/// The browser's polls keep one waker per handle and operation, so each operation that may
/// wait polls its own clone of the session, bridged to every waiter parked on it.
pub struct Session {
	accept_uni: kio::Bridge<backend::Session>,
	accept_bi: kio::Bridge<backend::Session>,
	open_uni: kio::Bridge<backend::Session>,
	open_bi: kio::Bridge<backend::Session>,
	datagram: kio::Bridge<backend::Session>,
	closed: kio::Bridge<backend::Session>,
	/// The operations that never wait.
	session: backend::Session,
}
impl Session {
	/// Wrap a connected browser session.
	pub fn new(session: backend::Session) -> Self {
		let bridge = || kio::Bridge::new(session.clone());
		Self {
			accept_uni: bridge(),
			accept_bi: bridge(),
			open_uni: bridge(),
			open_bi: bridge(),
			datagram: bridge(),
			closed: bridge(),
			session,
		}
	}
}
impl poll::Session for Session {
	type SendStream = SendStream;
	// The browser cannot hold its receive window for moq-net, so reads are buffered here.
	type RecvStream = poll::Buffered<RecvStream>;
	type Error = Error;
	fn poll_accept_uni(&self, waiter: &kio::Waiter) -> Poll<Result<Self::RecvStream, Error>> {
		let recv = ready!(self.accept_uni.poll(waiter, |session, cx| session.poll_accept_uni(cx)))?;
		Poll::Ready(Ok(poll::Buffered::new(RecvStream(recv))))
	}
	fn poll_accept_bi(&self, waiter: &kio::Waiter) -> Poll<Result<poll::BiStreams<Self>, Error>> {
		let (send, recv) = ready!(self.accept_bi.poll(waiter, |session, cx| session.poll_accept_bi(cx)))?;
		Poll::Ready(Ok((SendStream::new(send), poll::Buffered::new(RecvStream(recv)))))
	}
	fn poll_open_uni(
		&self,
		waiter: &kio::Waiter,
		options: poll::SendOptions<'_, poll::Ungrouped>,
	) -> Poll<Result<SendStream, Error>> {
		let send = ready!(self.open_uni.poll(waiter, |session, cx| session.poll_open_uni(cx)))?;
		Poll::Ready(Ok(SendStream::new(send).scheduled(options)))
	}
	fn poll_open_bi(
		&self,
		waiter: &kio::Waiter,
		options: poll::SendOptions<'_, poll::Ungrouped>,
	) -> Poll<Result<poll::BiStreams<Self>, Error>> {
		let (send, recv) = ready!(self.open_bi.poll(waiter, |session, cx| session.poll_open_bi(cx)))?;
		Poll::Ready(Ok((
			SendStream::new(send).scheduled(options),
			poll::Buffered::new(RecvStream(recv)),
		)))
	}

	fn poll_open_request(
		&self,
		waiter: &kio::Waiter,
		options: poll::SendOptions<'_, poll::Ungrouped>,
	) -> Poll<Result<Option<poll::BiStreams<Self>>, Error>> {
		self.poll_open_bi(waiter, options).map_ok(Some)
	}
	fn send_group(&self, _: i64) -> Option<poll::Ungrouped> {
		None
	}
	/// The browser cannot rank a datagram against a stream, so `options` are ignored, and a
	/// datagram it has no room for is dropped.
	fn send_datagram(&self, payload: Bytes, _: poll::SendOptions<'_, poll::Ungrouped>) -> Result<(), Error> {
		self.session.try_send_datagram(&payload)?;
		Ok(())
	}
	fn poll_recv_datagram(&self, waiter: &kio::Waiter) -> Poll<Result<Bytes, Error>> {
		let datagram = ready!(self.datagram.poll(waiter, |session, cx| session.poll_recv_datagram(cx)))?;
		Poll::Ready(Ok(datagram))
	}
	fn max_datagram_size(&self) -> usize {
		self.session.max_datagram_size()
	}
	fn address(&self) -> moq::Address {
		moq::Address::Uri
	}
	fn protocol(&self) -> Option<&str> {
		self.session.protocol()
	}
	fn close(&self, code: u32, reason: &str) {
		self.session.close(code, reason)
	}
	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Error> {
		self.closed
			.poll(waiter, |session, cx| session.poll_closed(cx))
			.map(Error::Backend)
	}
}
/// An outgoing browser stream adapted to moq-net.
///
/// Bridged, so its close can be watched through a shared borrow.
pub struct SendStream(kio::Bridge<backend::SendStream>);
impl SendStream {
	fn new(stream: backend::SendStream) -> Self {
		Self(kio::Bridge::new(stream))
	}

	/// Schedule a newly opened stream, which has no group to join.
	fn scheduled(mut self, options: poll::SendOptions<'_, poll::Ungrouped>) -> Self {
		poll::SendStream::set_priority(&mut self, options.priority);
		self
	}
}
impl poll::SendStream for SendStream {
	type Error = Error;
	type Group = poll::Ungrouped;
	fn poll_write(&mut self, waiter: &kio::Waiter, buf: &[u8]) -> Poll<Result<usize, Error>> {
		let written = ready!(self.0.get_mut().poll_write(&mut waiter.context(), buf))?;
		Poll::Ready(Ok(written))
	}
	fn set_priority(&mut self, order: i64) {
		let order = order.clamp(i32::MIN.into(), i32::MAX.into()) as i32;
		self.0.get_mut().set_priority(order)
	}
	fn set_group(&mut self, group: &Self::Group) {
		match *group {}
	}
	fn finish(&mut self) -> Result<(), Error> {
		self.0.get_mut().finish()?;
		Ok(())
	}
	fn reset(&mut self, code: u32) {
		self.0.get_mut().reset(code)
	}
	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		self.0.poll(waiter, |stream, cx| stream.poll_closed(cx)).map(closed)
	}
}
/// An incoming browser stream adapted to moq-net.
pub struct RecvStream(backend::RecvStream);
impl poll::Chunks for RecvStream {
	type Error = Error;
	fn poll_read_chunk(&mut self, waiter: &kio::Waiter, max: usize) -> Poll<Result<Option<Bytes>, Error>> {
		let chunk = ready!(self.0.poll_read_chunk(&mut waiter.context(), max))?;
		Poll::Ready(Ok(chunk))
	}
	fn stop(&mut self, code: u32) {
		self.0.stop(code)
	}
}
