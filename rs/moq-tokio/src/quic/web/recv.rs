//! Incoming WebTransport streams, their leases, and errors.

use std::{
	io,
	pin::Pin,
	sync::Arc,
	task::{Context, Poll, ready},
};

use bytes::{BufMut, Bytes};

use crate::quic::driver::recv;
use crate::quic::web::{CloseReason, SessionError};

/// A stream that can be used to receive bytes. See [`crate::quic::driver::recv::Stream`].
///
/// Dropping it before the end stops it with code 0.
#[derive(Debug)]
pub struct Stream {
	/// Taken when moq-net stops through its mutable poll trait.
	inner: Option<recv::Stream>,
	close: Arc<CloseReason>,
	id: crate::quic::driver::stream::Id,
}

impl Stream {
	pub(crate) fn new(mut stream: recv::Stream, close: Arc<CloseReason>) -> Self {
		// The driver's raw 0 is outside the HTTP/3 range, so a WebTransport peer can't decode it.
		stream.set_drop_stop(close.encode_stream_code(0));
		Self {
			id: stream.id(),
			inner: Some(stream),
			close,
		}
	}

	fn inner(&mut self) -> Result<&mut recv::Stream, Error> {
		self.inner.as_mut().ok_or(Error::ClosedStream)
	}

	/// Lease the stream's unreleased bytes. See [`recv::Stream::poll_lease`].
	pub fn poll_lease(&mut self, cx: &mut Context<'_>, want: usize) -> Poll<Result<recv::Lease<'_>, Error>> {
		let close = &self.close;
		match &mut self.inner {
			Some(inner) => inner.poll_lease(cx, want).map_err(|e| map_error(close, e)),
			None => Poll::Ready(Err(Error::ClosedStream)),
		}
	}

	/// Poll until the peer resets the stream, the receiver evicts it, or the session closes,
	/// without reading it; pending for good once every byte was read. See
	/// [`recv::Stream::received_reset`].
	pub fn poll_reset(&mut self, cx: &mut Context<'_>) -> Poll<Error> {
		let close = &self.close;
		let Some(inner) = &mut self.inner else {
			return Poll::Ready(Error::ClosedStream);
		};
		match ready!(inner.poll_received_reset(cx)) {
			Ok(None) => Poll::Pending,
			Ok(Some(code)) => Poll::Ready(map_error(close, recv::Error::Reset(code))),
			Err(err) => Poll::Ready(map_error(close, err)),
		}
	}

	/// Rank this stream for eviction. See [`recv::Stream::set_priority`].
	pub fn set_priority(&mut self, priority: i64) {
		if let Some(inner) = &mut self.inner {
			inner.set_priority(priority);
		}
	}

	/// Tell the other end to stop sending data with the given error code. See
	/// [`recv::Stream::stop`]. This is a u32 with WebTransport since it shares the error
	/// space with HTTP/3. A raw QUIC session sends the code as is. Does nothing once stopped.
	fn stop_mut(&mut self, code: u32) {
		if let Some(inner) = self.inner.take() {
			inner.stop(self.close.encode_stream_code(code));
		}
	}

	/// Stop receiving and tell the peer why.
	pub fn stop(mut self, code: u32) {
		self.stop_mut(code);
	}

	/// Wait until the stream ends after its bytes are read, the peer resets it, or the session closes.
	pub async fn closed(&mut self) -> Result<(), Error> {
		kio::wait(|waiter| self.poll_closed(waiter)).await
	}

	/// Poll until the stream ends after its bytes are read, the peer resets it, or the session closes.
	pub fn poll_closed(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let close = &self.close;
		let Some(inner) = &mut self.inner else {
			return Poll::Ready(Ok(()));
		};
		match ready!(inner.poll_closed(&waiter.context())) {
			Ok(None) => Poll::Ready(Ok(())),
			Ok(Some(code)) => Poll::Ready(Err(map_error(close, recv::Error::Reset(code)))),
			Err(err) => Poll::Ready(Err(map_error(close, err))),
		}
	}

	/// Poll for some bytes without retaining the destination buffer.
	pub fn poll_read(&mut self, waiter: &kio::Waiter, mut buf: &mut [u8]) -> Poll<Result<Option<usize>, Error>> {
		let close = self.close.clone();
		self.inner()?
			.poll_read_buf(waiter, &mut buf)
			.map_err(|err| map_error(&close, err))
	}

	/// Read some data into `buf` and return the amount read. See [`recv::Stream::read`].
	pub async fn read(&mut self, buf: &mut impl BufMut) -> Result<Option<usize>, Error> {
		let result = self.inner()?.read(buf).await;
		result.map_err(|e| map_error(&self.close, e))
	}

	/// Fill the entire buffer with data. See [`recv::Stream::read_exact`].
	pub async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), Error> {
		let result = self.inner()?.read_exact(buf).await;
		result.map_err(|e| map_error(&self.close, e))
	}

	/// Read a chunk of data from the stream. See [`recv::Stream::read_chunk`].
	pub async fn read_chunk(&mut self, max: usize) -> Result<Option<Bytes>, Error> {
		let result = self.inner()?.read_chunk(max).await;
		result.map_err(|e| map_error(&self.close, e))
	}

	/// Read until the end of the stream or the limit is hit. See [`recv::Stream::read_to_end`].
	pub async fn read_to_end(&mut self, limit: usize) -> Result<Vec<u8>, Error> {
		let result = self.inner()?.read_to_end(limit).await;
		result.map_err(|e| map_error(&self.close, e))
	}

	/// Block until the stream has been reset and return the error code. See
	/// [`recv::Stream::received_reset`].
	///
	/// Returns the stream's reset code, or fails with its session error or
	/// [`Error::Evicted`] once the receiver evicted it to free a stuck window.
	pub async fn received_reset(&mut self) -> Result<Option<u32>, Error> {
		let Some(inner) = &mut self.inner else {
			return Ok(None);
		};
		match inner.received_reset().await {
			Ok(None) => Ok(None),
			Ok(Some(code)) => match map_error(&self.close, recv::Error::Reset(code)) {
				Error::Reset(code) => Ok(Some(code)),
				err => Err(err),
			},
			Err(err) => Err(map_error(&self.close, err)),
		}
	}

	/// Return the underlying QUIC stream ID.
	///
	/// > **Warning**
	/// >
	/// > WebTransport sessions share the QUIC connection with HTTP/3 and potentially other
	/// > sessions.
	/// > The [crate::quic::driver::stream::Id::index] might not increment by 1 like expected when using raw QUIC.
	/// > This is why the Javascript WebTransport API does not expose the Stream ID.
	pub fn id(&self) -> crate::quic::driver::stream::Id {
		self.id
	}

	// We purposely don't expose the 0RTT because it's not valid with WebTransport
}

/// Decode the peer's reset code, and report a session error, or a reset for a session
/// that is gone, as the session's close reason.
fn map_error(close: &CloseReason, e: recv::Error) -> Error {
	match e {
		recv::Error::Reset(code) => match close.decode_stream_code(code) {
			Some(code) => Error::Reset(code),
			None => match close.session_gone(code) {
				Some(err) => Error::SessionError(err),
				None => Error::InvalidReset(code),
			},
		},
		e => match Error::from(e) {
			Error::SessionError(e) => Error::SessionError(close.map(e)),
			e => e,
		},
	}
}

impl tokio::io::AsyncRead for Stream {
	fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut tokio::io::ReadBuf) -> Poll<io::Result<()>> {
		match &mut self.inner {
			Some(inner) => Pin::new(inner).poll_read(cx, buf),
			None => Poll::Ready(Err(io::ErrorKind::NotConnected.into())),
		}
	}
}

impl moq_net::transport::poll::RecvStream for Stream {
	type Error = Error;
	type Lease<'a> = recv::Lease<'a>;
	fn poll_lease(&mut self, waiter: &kio::Waiter, want: usize) -> Poll<Result<Self::Lease<'_>, Self::Error>> {
		Self::poll_lease(self, &mut waiter.context(), want)
	}
	fn set_priority(&mut self, priority: i64) {
		Self::set_priority(self, priority);
	}
	fn stop(&mut self, code: u32) {
		Self::stop_mut(self, code);
	}
	fn poll_reset(&mut self, waiter: &kio::Waiter) -> Poll<Self::Error> {
		Self::poll_reset(self, &mut waiter.context())
	}
}
impl moq_net::transport::poll::Lease for recv::Lease<'_> {
	fn is_fin(&self) -> bool {
		Self::is_fin(self)
	}
	fn chunks(&self) -> impl Iterator<Item = &[u8]> {
		Self::chunks(self)
	}
	fn release(self) {
		Self::release(self);
	}
}

/// An error when reading from [`crate::quic::web::recv::Stream`]. Similar to [`crate::quic::driver::recv::Error`].
#[derive(Clone, thiserror::Error, Debug)]
#[non_exhaustive]
pub enum Error {
	/// The session carrying the stream failed.
	#[error("session error: {0}")]
	SessionError(#[from] SessionError),

	/// The peer reset this stream with its application code.
	#[error("RESET_STREAM: {0}")]
	Reset(u32),

	/// The peer reset the stream with a code outside the WebTransport registry.
	#[error("invalid RESET_STREAM: {0}")]
	InvalidReset(crate::quic::driver::VarInt),

	/// The stream was already stopped.
	#[error("stream already closed")]
	ClosedStream,

	/// The connection's receive window was stuck and this stream was discarded to free it.
	#[error("evicted")]
	Evicted,

	/// The stream finished before [`read_exact`](crate::quic::web::recv::Stream::read_exact)
	/// filled its buffer, after this many bytes.
	#[error("finished early")]
	FinishedEarly(usize),

	/// The stream is longer than [`read_to_end`](crate::quic::web::recv::Stream::read_to_end)'s limit.
	#[error("too long")]
	TooLong,

	/// A lease wanted more than the connection receive window can hold.
	#[error("want exceeds the receive window")]
	TooLarge,
}

impl From<crate::quic::driver::recv::Error> for Error {
	fn from(value: crate::quic::driver::recv::Error) -> Self {
		use crate::quic::driver::recv::Error as DriverError;
		match value {
			DriverError::Reset(code) => match web_transport_proto::error_from_http3(code.into_inner()) {
				Some(code) => Error::Reset(code),
				None => Error::InvalidReset(code),
			},
			DriverError::Connection(e) => Error::SessionError(e.into()),
			DriverError::Evicted => Error::Evicted,
			DriverError::FinishedEarly(n) => Error::FinishedEarly(n),
			DriverError::TooLong => Error::TooLong,
			DriverError::TooLarge => Error::TooLarge,
		}
	}
}

impl moq_net::transport::Error for Error {
	fn close_kind(&self) -> moq_net::transport::CloseKind {
		match self {
			Self::Evicted => moq_net::transport::CloseKind::Evicted,
			Self::SessionError(e) => e.close_kind(),
			_ => moq_net::transport::CloseKind::Transport,
		}
	}
	fn session_error(&self) -> Option<(u32, String)> {
		if let Error::SessionError(e) = self {
			return e.session_error();
		}

		None
	}

	fn stream_error(&self) -> Option<u32> {
		match self {
			Error::Reset(code) => Some(*code),
			_ => None,
		}
	}
}

/// The driver's receive lease, which retains flow-control credit until released.
pub use crate::quic::driver::recv::Lease;
