//! Iroh sessions and streams, implementing moq-net's poll traits.
//!
//! Iroh's session operations are async, so each one that waits runs as a boxed future
//! shared by every waiter. Streams write and read through their tokio poll forms. Only a
//! send stream's close watch is a future, since iroh offers it as one borrowing the
//! stream, and any other use of the stream takes it back at once.

use std::{
	io,
	pin::Pin,
	task::{Context, Poll, Waker, ready},
};

use bytes::{BufMut, Bytes, BytesMut};
use futures::{
	FutureExt,
	channel::oneshot,
	future::{BoxFuture, Either},
};
use moq_net::{
	kio,
	transport::{self as moq, poll},
};
use web_transport_iroh::{
	self as backend, ReadError, SessionError, WebTransportError, WriteError,
	iroh::endpoint::{self, ConnectionError},
};

use crate::transport::{OpSlot, poll_op};

/// How much a stream read may take at once.
const READ_SIZE: usize = 16 * 1024;

/// A failure on an iroh session or stream, keeping the session and stream code registries apart.
#[derive(Clone, Debug, thiserror::Error)]
pub(crate) enum Error {
	#[error(transparent)]
	Session(#[from] SessionError),
	#[error(transparent)]
	Write(#[from] WriteError),
	#[error(transparent)]
	Read(#[from] ReadError),
}

impl Error {
	/// The session failure behind this, if any.
	fn session(&self) -> Option<&SessionError> {
		match self {
			Self::Session(err)
			| Self::Write(WriteError::SessionError(err))
			| Self::Read(ReadError::SessionError(err)) => Some(err),
			_ => None,
		}
	}
}

impl moq::Error for Error {
	fn close_kind(&self) -> moq::CloseKind {
		match self.session() {
			Some(SessionError::ConnectionError(ConnectionError::TimedOut)) => moq::CloseKind::IdleTimeout,
			Some(SessionError::ConnectionError(ConnectionError::LocallyClosed)) => moq::CloseKind::Closed,
			_ => moq::CloseKind::Transport,
		}
	}

	fn session_error(&self) -> Option<(u32, String)> {
		match self.session() {
			Some(SessionError::WebTransportError(WebTransportError::Closed { code, reason })) => {
				Some((*code, reason.clone()))
			}
			_ => None,
		}
	}

	fn stream_error(&self) -> Option<u32> {
		match self {
			Self::Write(WriteError::Stopped(code)) | Self::Read(ReadError::Reset(code)) => Some(*code),
			_ => None,
		}
	}
}

/// The application code of a peer's stop or reset. Raw QUIC carries it as is, while
/// HTTP/3 maps it into its own code space.
fn decode(code: endpoint::VarInt, raw: bool) -> Option<u32> {
	let code = code.into_inner();
	// An older raw peer maps its codes into the HTTP/3 range too, which no u32 reaches.
	match web_transport_proto::error_from_http3(code) {
		Some(code) => Some(code),
		None if raw => u32::try_from(code).ok(),
		None => None,
	}
}

/// The typed failure behind a stream's tokio write error.
fn write_error(err: io::Error, raw: bool) -> WriteError {
	let Some(err) = err
		.into_inner()
		.and_then(|err| err.downcast::<endpoint::WriteError>().ok())
	else {
		return WriteError::ClosedStream;
	};
	match *err {
		endpoint::WriteError::Stopped(code) => match decode(code, raw) {
			Some(code) => WriteError::Stopped(code),
			None => WriteError::InvalidStopped(code),
		},
		err => err.into(),
	}
}

/// The typed failure behind a stream's tokio read error.
fn read_error(err: io::Error, raw: bool) -> ReadError {
	let Some(err) = err
		.into_inner()
		.and_then(|err| err.downcast::<endpoint::ReadError>().ok())
	else {
		return ReadError::ClosedStream;
	};
	match *err {
		endpoint::ReadError::Reset(code) => match decode(code, raw) {
			Some(code) => ReadError::Reset(code),
			None => ReadError::InvalidReset(code),
		},
		err => err.into(),
	}
}

/// An iroh session, over WebTransport or raw QUIC.
pub(crate) struct Session {
	session: backend::Session,
	address: moq::Address,
	/// Raw QUIC, which carries stream codes as is.
	raw: bool,
	accept_uni: OpSlot<Result<backend::RecvStream, SessionError>>,
	accept_bi: OpSlot<Result<(backend::SendStream, backend::RecvStream), SessionError>>,
	open_uni: OpSlot<Result<backend::SendStream, SessionError>>,
	open_bi: OpSlot<Result<(backend::SendStream, backend::RecvStream), SessionError>>,
	recv_datagram: OpSlot<Result<Bytes, SessionError>>,
	closed: OpSlot<SessionError>,
	/// Iroh only reports bytes in its stats, so these are refreshed whenever metrics are read.
	wire: moq::Wire,
}

impl Session {
	/// Wrap an established session whose request target travels as `address`.
	pub(crate) fn new(session: backend::Session, address: moq::Address) -> Self {
		Self {
			// Only a WebTransport session has a CONNECT request.
			raw: session.request().is_none(),
			session,
			address,
			accept_uni: OpSlot::default(),
			accept_bi: OpSlot::default(),
			open_uni: OpSlot::default(),
			open_bi: OpSlot::default(),
			recv_datagram: OpSlot::default(),
			closed: OpSlot::default(),
			wire: moq::Wire::new(),
		}
	}

	fn send(&self, stream: backend::SendStream, options: poll::SendOptions<'_, poll::Ungrouped>) -> SendStream {
		let mut stream = SendStream(kio::Bridge::new(Outgoing {
			stream: Some(stream),
			watch: None,
			raw: self.raw,
		}));
		poll::SendStream::set_priority(&mut stream, options.priority);
		stream
	}

	fn recv(&self, stream: backend::RecvStream) -> poll::Buffered<RecvStream> {
		poll::Buffered::new(RecvStream {
			stream,
			buffer: BytesMut::new(),
			raw: self.raw,
		})
	}
}

impl poll::Session for Session {
	type SendStream = SendStream;
	// Iroh returns credit as it reads, so reads are buffered here.
	type RecvStream = poll::Buffered<RecvStream>;
	type Error = Error;

	fn poll_accept_uni(&self, waiter: &kio::Waiter) -> Poll<Result<Self::RecvStream, Error>> {
		let recv = ready!(poll_op(&self.accept_uni, waiter, || {
			let session = self.session.clone();
			async move { session.accept_uni().await }.boxed()
		}))?;
		Poll::Ready(Ok(self.recv(recv)))
	}

	fn poll_accept_bi(&self, waiter: &kio::Waiter) -> Poll<Result<poll::BiStreams<Self>, Error>> {
		let (send, recv) = ready!(poll_op(&self.accept_bi, waiter, || {
			let session = self.session.clone();
			async move { session.accept_bi().await }.boxed()
		}))?;
		Poll::Ready(Ok((self.send(send, Default::default()), self.recv(recv))))
	}

	fn poll_open_uni(
		&self,
		waiter: &kio::Waiter,
		options: poll::SendOptions<'_, poll::Ungrouped>,
	) -> Poll<Result<SendStream, Error>> {
		let send = ready!(poll_op(&self.open_uni, waiter, || {
			let session = self.session.clone();
			async move { session.open_uni().await }.boxed()
		}))?;
		Poll::Ready(Ok(self.send(send, options)))
	}

	fn poll_open_bi(
		&self,
		waiter: &kio::Waiter,
		options: poll::SendOptions<'_, poll::Ungrouped>,
	) -> Poll<Result<poll::BiStreams<Self>, Error>> {
		let (send, recv) = ready!(poll_op(&self.open_bi, waiter, || {
			let session = self.session.clone();
			async move { session.open_bi().await }.boxed()
		}))?;
		Poll::Ready(Ok((self.send(send, options), self.recv(recv))))
	}

	/// Iroh cannot inspect noq's stream credit, so a request waits for it like any open.
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

	/// Iroh cannot rank a datagram against a stream, so `options` are ignored.
	fn send_datagram(&self, payload: Bytes, _: poll::SendOptions<'_, poll::Ungrouped>) -> Result<(), Error> {
		Ok(self.session.send_datagram(payload)?)
	}

	fn poll_recv_datagram(&self, waiter: &kio::Waiter) -> Poll<Result<Bytes, Error>> {
		let datagram = ready!(poll_op(&self.recv_datagram, waiter, || {
			let session = self.session.clone();
			async move { session.read_datagram().await }.boxed()
		}))?;
		Poll::Ready(Ok(datagram))
	}

	fn max_datagram_size(&self) -> usize {
		self.session.max_datagram_size()
	}

	fn protocol(&self) -> Option<&str> {
		self.session.protocol()
	}

	fn address(&self) -> moq::Address {
		self.address
	}

	fn close(&self, code: u32, reason: &str) {
		self.session.close(code, reason.as_bytes())
	}

	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Error> {
		poll_op(&self.closed, waiter, || {
			let session = self.session.clone();
			async move { session.closed().await }.boxed()
		})
		.map(Error::Session)
	}

	fn metrics(&self) -> impl moq::Metrics {
		let conn = self.session.conn();
		let stats = conn.stats();
		// moq-net reads the wire right after the metrics.
		self.wire.set_sent_bytes(stats.udp_tx.bytes);
		self.wire.set_received_bytes(stats.udp_rx.bytes);
		Metrics {
			stats,
			path: conn
				.paths()
				.iter()
				.find(|path| path.is_selected())
				.map(|path| path.stats()),
		}
	}

	fn wire(&self) -> Option<moq::Wire> {
		Some(self.wire.clone())
	}
}

/// The connection's stats and its selected path's.
struct Metrics {
	stats: endpoint::ConnectionStats,
	path: Option<endpoint::PathStats>,
}

impl moq::Metrics for Metrics {
	fn bytes_lost(&self) -> Option<u64> {
		Some(self.stats.lost_bytes)
	}

	fn packets_sent(&self) -> Option<u64> {
		Some(self.stats.udp_tx.datagrams)
	}

	fn packets_received(&self) -> Option<u64> {
		Some(self.stats.udp_rx.datagrams)
	}

	fn packets_lost(&self) -> Option<u64> {
		Some(self.stats.lost_packets)
	}

	fn rtt(&self) -> Option<std::time::Duration> {
		self.path.map(|path| path.rtt)
	}

	fn estimated_send_rate(&self) -> Option<u64> {
		let path = self.path?;
		let rtt = path.rtt.as_secs_f64();
		(path.cwnd > 0 && rtt > 0.0).then(|| (path.cwnd as f64 * 8.0 / rtt) as u64)
	}
}

/// An outgoing iroh stream.
///
/// Bridged, so its close can be watched through a shared borrow.
pub(crate) struct SendStream(kio::Bridge<Outgoing>);

struct Outgoing {
	/// The stream, unless a close watch holds it.
	stream: Option<backend::SendStream>,
	watch: Option<Watch>,
	raw: bool,
}

impl Outgoing {
	/// The stream, taken back from a close watch.
	fn stream(&mut self) -> &mut backend::SendStream {
		if let Some(watch) = self.watch.take() {
			self.stream = Some(watch.cancel());
		}
		self.stream.as_mut().expect("a close watch holds the stream otherwise")
	}

	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
		let watch = match &mut self.watch {
			Some(watch) => watch,
			None => self
				.watch
				.insert(Watch::new(self.stream.take().expect("idle without a watch"))),
		};
		let (stream, stopped) = ready!(watch.future.as_mut().poll(cx));
		self.watch = None;
		self.stream = Some(stream);
		Poll::Ready(match stopped.expect("only taking the stream back cancels a watch") {
			Ok(None) => Ok(()),
			Ok(Some(code)) => Err(WriteError::Stopped(code).into()),
			Err(err) => Err(err.into()),
		})
	}
}

/// A wait for the peer to stop the stream or acknowledge its FIN, holding the stream.
struct Watch {
	cancel: oneshot::Sender<()>,
	/// The stream back, with the stop code, or `None` if cancelled.
	#[expect(clippy::type_complexity, reason = "one future, named once")]
	future: BoxFuture<'static, (backend::SendStream, Option<Result<Option<u32>, SessionError>>)>,
}

impl Watch {
	fn new(mut stream: backend::SendStream) -> Self {
		let (cancel, cancelled) = oneshot::channel();
		let future = async move {
			let stopped = {
				let stopped = std::pin::pin!(stream.stopped());
				match futures::future::select(stopped, cancelled).await {
					Either::Left((stopped, _)) => Some(stopped),
					Either::Right(_) => None,
				}
			};
			(stream, stopped)
		}
		.boxed();
		Self { cancel, future }
	}

	/// Take the stream back.
	///
	/// Cancelling wakes the watch's waiters too, so a caller parked on the close polls
	/// again and watches anew.
	fn cancel(mut self) -> backend::SendStream {
		let _ = self.cancel.send(());
		match self.future.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
			Poll::Ready((stream, _)) => stream,
			Poll::Pending => unreachable!("a cancelled watch settles at once"),
		}
	}
}

impl poll::SendStream for SendStream {
	type Error = Error;
	type Group = poll::Ungrouped;

	fn poll_write(&mut self, waiter: &kio::Waiter, buf: &[u8]) -> Poll<Result<usize, Error>> {
		let send = self.0.get_mut();
		let raw = send.raw;
		let written = ready!(tokio::io::AsyncWrite::poll_write(
			Pin::new(send.stream()),
			&mut waiter.context(),
			buf
		));
		Poll::Ready(written.map_err(|err| write_error(err, raw).into()))
	}

	fn set_priority(&mut self, priority: i64) {
		let priority = priority.clamp(i32::MIN.into(), i32::MAX.into()) as i32;
		// A closed stream has nothing left to schedule.
		self.0.get_mut().stream().set_priority(priority).ok();
	}

	fn set_group(&mut self, group: &poll::Ungrouped) {
		match *group {}
	}

	fn finish(&mut self) -> Result<(), Error> {
		self.0
			.get_mut()
			.stream()
			.finish()
			.map_err(|_| WriteError::ClosedStream)?;
		Ok(())
	}

	fn reset(&mut self, code: u32) {
		// A closed stream has nothing left to reset.
		self.0.get_mut().stream().reset(code).ok();
	}

	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		self.0.poll(waiter, Outgoing::poll_closed)
	}
}

/// An incoming iroh stream.
pub(crate) struct RecvStream {
	stream: backend::RecvStream,
	/// The spare capacity reads land in, split off as each read completes.
	buffer: BytesMut,
	raw: bool,
}

impl poll::Chunks for RecvStream {
	type Error = Error;

	fn poll_read_chunk(&mut self, waiter: &kio::Waiter, max: usize) -> Poll<Result<Option<Bytes>, Error>> {
		if max == 0 {
			return Poll::Ready(Ok(Some(Bytes::new())));
		}
		self.buffer.reserve(READ_SIZE.min(max));
		let read = ready!(tokio_util::io::poll_read_buf(
			Pin::new(&mut self.stream),
			&mut waiter.context(),
			&mut (&mut self.buffer).limit(max),
		));
		let read = read.map_err(|err| read_error(err, self.raw))?;
		// An empty read into spare capacity is the FIN.
		Poll::Ready(Ok((read > 0).then(|| self.buffer.split().freeze())))
	}

	fn stop(&mut self, code: u32) {
		// A closed stream has nobody left to tell.
		self.stream.stop(code).ok();
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use moq_net::transport::Error as _;
	use poll::{RecvStream as _, SendStream as _, Session as _};
	use std::sync::{
		Arc,
		atomic::{AtomicUsize, Ordering},
	};
	use web_transport_iroh::iroh::{Endpoint, endpoint::presets};

	const ALPN: &[u8] = b"test";

	/// Two connected raw QUIC sessions, plus the endpoints that drive them.
	async fn pair() -> (Session, Session, [Endpoint; 2]) {
		let client = Endpoint::builder(presets::Minimal).bind().await.unwrap();
		let server = Endpoint::builder(presets::Minimal)
			.alpns(vec![ALPN.to_vec()])
			.bind()
			.await
			.unwrap();
		let (client_conn, server_conn) =
			tokio::join!(async { client.connect(server.addr(), ALPN).await.unwrap() }, async {
				server.accept().await.unwrap().await.unwrap()
			},);
		let session = |conn| Session::new(backend::Session::raw(conn), moq::Address::Setup);
		(session(client_conn), session(server_conn), [client, server])
	}

	/// Counts its wakes.
	#[derive(Default)]
	struct Wakes(AtomicUsize);
	impl std::task::Wake for Wakes {
		fn wake(self: Arc<Self>) {
			self.0.fetch_add(1, Ordering::SeqCst);
		}
	}

	#[tokio::test]
	async fn a_write_takes_the_stream_back_from_a_close_watch() {
		let (client, server, _endpoints) = pair().await;
		let mut send = client.open_uni(Default::default()).await.unwrap();

		let wakes = Arc::new(Wakes::default());
		let watcher = kio::Waiter::new(Waker::from(wakes.clone()));
		assert!(send.poll_closed(&watcher).is_pending());

		// The write cancels the watch, waking its waiter so it watches anew.
		send.write_chunk(Bytes::from_static(b"hi")).await.unwrap();
		assert!(wakes.0.load(Ordering::SeqCst) > 0);

		let mut recv = server.accept_uni().await.unwrap();
		assert_eq!(recv.read_chunk(16).await.unwrap().unwrap(), b"hi"[..]);
		recv.stop(9);
		let err = send.closed().await.unwrap_err();
		assert_eq!(err.stream_error(), Some(9));
	}

	#[test]
	fn typed_close_kinds_survive_stream_wrappers() {
		let idle = Error::Session(SessionError::ConnectionError(ConnectionError::TimedOut));
		assert!(matches!(
			moq_net::Error::from_transport(idle),
			moq_net::Error::IdleTimeout
		));
		let idle = Error::Read(ReadError::SessionError(SessionError::ConnectionError(
			ConnectionError::TimedOut,
		)));
		assert_eq!(idle.close_kind(), moq::CloseKind::IdleTimeout);
		let local = Error::Write(WriteError::SessionError(SessionError::ConnectionError(
			ConnectionError::LocallyClosed,
		)));
		assert!(matches!(
			moq_net::Error::from_transport(local),
			moq_net::Error::Session(moq_net::SessionError::Cancel)
		));
	}

	#[test]
	fn stream_codes_decode_in_the_session_code_space() {
		let mapped = endpoint::VarInt::from_u64(web_transport_proto::error_to_http3(7)).unwrap();
		assert_eq!(decode(mapped, false), Some(7));
		assert_eq!(decode(mapped, true), Some(7));
		assert_eq!(decode(endpoint::VarInt::from_u32(7), true), Some(7));
		assert_eq!(decode(endpoint::VarInt::from_u32(7), false), None);

		let stopped = io::Error::from(endpoint::WriteError::Stopped(endpoint::VarInt::from_u32(9)));
		let stopped = Error::from(write_error(stopped, true));
		assert_eq!(stopped.stream_error(), Some(9));
		let reset = io::Error::from(endpoint::ReadError::Reset(mapped));
		assert_eq!(Error::from(read_error(reset, false)).stream_error(), Some(7));
	}
}
