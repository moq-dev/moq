use std::{
	io,
	pin::Pin,
	sync::Arc,
	task::{Context, Poll},
};

use bytes::Bytes;

use crate::quic::web::{CloseReason, ReadError, ReadExactError, ReadToEndError, SessionError};

/// A stream that can be used to receive bytes. See [`crate::quic::driver::RecvStream`].
///
/// Dropping it before the end stops it with code 0.
#[derive(Debug)]
pub struct RecvStream {
	inner: crate::quic::driver::RecvStream,
	close: Arc<CloseReason>,
}

impl RecvStream {
	pub(crate) fn new(mut stream: crate::quic::driver::RecvStream, close: Arc<CloseReason>) -> Self {
		// The driver's raw 0 is outside the HTTP/3 range, so a WebTransport peer can't decode it.
		stream.set_drop_stop(close.encode_stream_code(0));
		Self { inner: stream, close }
	}

	fn map_error(&self, e: crate::quic::driver::ReadError) -> ReadError {
		map_error(&self.close, e)
	}

	/// Lease the stream's unreleased bytes. See [`crate::quic::driver::RecvStream::poll_lease`].
	pub fn poll_lease(
		&mut self,
		cx: &mut Context<'_>,
		want: usize,
	) -> Poll<Result<crate::quic::driver::Lease<'_>, ReadError>> {
		let close = &self.close;
		self.inner.poll_lease(cx, want).map_err(|e| map_error(close, e))
	}

	/// Rank this stream for eviction. See [`crate::quic::driver::RecvStream::set_priority`].
	pub fn set_priority(&mut self, priority: i64) {
		self.inner.set_priority(priority);
	}

	/// Tell the other end to stop sending data with the given error code. See
	/// [`crate::quic::driver::RecvStream::stop`]. This is a u32 with WebTransport since it shares the error
	/// space with HTTP/3. A raw QUIC session sends the code as is.
	pub fn stop(&mut self, code: u32) -> Result<(), crate::quic::driver::ClosedStream> {
		let code = self.close.encode_stream_code(code);
		self.inner.stop(code)
	}

	// Unfortunately, we have to wrap ReadError for a bunch of functions.

	/// Read some data into the buffer and return the amount read. See [`crate::quic::driver::RecvStream::read`].
	pub async fn read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, ReadError> {
		self.inner.read(buf).await.map_err(|e| self.map_error(e))
	}

	/// Fill the entire buffer with data. See [`crate::quic::driver::RecvStream::read_exact`].
	pub async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), ReadExactError> {
		self.inner.read_exact(buf).await.map_err(|e| match e {
			crate::quic::driver::ReadExactError::ReadError(e) => self.map_error(e).into(),
			e => e.into(),
		})
	}

	/// Read a chunk of data from the stream. See [`crate::quic::driver::RecvStream::read_chunk`].
	pub async fn read_chunk(&mut self, max_length: usize) -> Result<Option<Bytes>, ReadError> {
		self.inner
			.read_chunk(max_length, true)
			.await
			.map(|chunk| chunk.map(|chunk| chunk.bytes))
			.map_err(|e| self.map_error(e))
	}

	/// Read until the end of the stream or the limit is hit. See [`crate::quic::driver::RecvStream::read_to_end`].
	pub async fn read_to_end(&mut self, size_limit: usize) -> Result<Vec<u8>, ReadToEndError> {
		self.inner.read_to_end(size_limit).await.map_err(|e| match e {
			crate::quic::driver::ReadToEndError::Read(e) => self.map_error(e).into(),
			e => e.into(),
		})
	}

	/// Block until the stream has been reset and return the error code. See
	/// [`crate::quic::driver::RecvStream::received_reset`].
	///
	/// Unlike the driver, this returns a SessionError, not a ResetError, because 0-RTT is not supported.
	pub async fn received_reset(&mut self) -> Result<Option<u32>, SessionError> {
		match self.inner.received_reset().await {
			Ok(None) => Ok(None),
			Ok(Some(code)) => match self.close.session_gone(code) {
				Some(err) => Err(err),
				None => Ok(self.close.decode_stream_code(code)),
			},
			Err(crate::quic::driver::ResetError::ConnectionLost(conn_err)) => Err(self.close.map(conn_err.into())),
			Err(crate::quic::driver::ResetError::ZeroRttRejected) => unreachable!("0-RTT not supported"),
		}
	}

	/// Return the underlying QUIC stream ID.
	///
	/// > **Warning**
	/// >
	/// > WebTransport sessions share the QUIC connection with HTTP/3 and potentially other
	/// > sessions.
	/// > The [crate::quic::driver::StreamId::index] might not increment by 1 like expected when using raw QUIC.
	/// > This is why the Javascript WebTransport API does not expose the Stream ID.
	pub fn quic_id(&self) -> crate::quic::driver::StreamId {
		self.inner.id()
	}

	// We purposely don't expose the 0RTT because it's not valid with WebTransport
}

/// Decode the peer's reset code, and report a session error, or a reset for a session
/// that is gone, as the session's close reason.
fn map_error(close: &CloseReason, e: crate::quic::driver::ReadError) -> ReadError {
	match e {
		crate::quic::driver::ReadError::Reset(code) => match close.decode_stream_code(code) {
			Some(code) => ReadError::Reset(code),
			None => match close.session_gone(code) {
				Some(err) => ReadError::SessionError(err),
				None => ReadError::InvalidReset(code),
			},
		},
		e => match ReadError::from(e) {
			ReadError::SessionError(e) => ReadError::SessionError(close.map(e)),
			e => e,
		},
	}
}

impl tokio::io::AsyncRead for RecvStream {
	fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut tokio::io::ReadBuf) -> Poll<io::Result<()>> {
		Pin::new(&mut self.inner).poll_read(cx, buf)
	}
}

impl web_transport_trait::RecvStream for RecvStream {
	type Error = ReadError;

	fn stop(&mut self, code: u32) {
		Self::stop(self, code).ok();
	}

	async fn read(&mut self, dst: &mut [u8]) -> Result<Option<usize>, Self::Error> {
		self.read(dst).await
	}

	async fn read_chunk(&mut self, max: usize) -> Result<Option<Bytes>, Self::Error> {
		self.read_chunk(max).await
	}

	async fn closed(&mut self) -> Result<(), Self::Error> {
		self.received_reset().await?;
		Ok(())
	}
}
